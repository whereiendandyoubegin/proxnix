use crate::engine::Clock;
use crate::hydra::{Hydra, HydraConfig};
use crate::interpret::Declared;
use crate::nixstore::{Flake, NixStore};
use crate::types::{AppConfig, Result};
use proxnix_core::{
    CommitHash, DurationMs, GuestName, HydraBuild, Key, Ledger, NixHash, Observation, Policy, Retain, RootHolder, StoreEffect, StoreEvent,
    SyncInput, Toplevel, Vmid, sync_step,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tracing::{info, warn};

fn default_cache() -> String {
    String::from("file:///ZFS/hydra-cache")
}

fn default_interval_ms() -> u64 {
    600_000
}

fn default_grace_ms() -> u64 {
    600_000
}

fn default_retain() -> u8 {
    3
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct StoreSyncConfig {
    pub hydra: HydraConfig,
    #[serde(default = "default_cache")]
    pub cache: String,
    #[serde(default = "default_interval_ms")]
    pub interval_ms: u64,
    #[serde(default = "default_grace_ms")]
    pub hydra_grace_ms: u64,
    #[serde(default = "default_retain")]
    pub retain: u8,
}

impl StoreSyncConfig {
    pub fn interval(&self) -> Duration {
        Duration::from_millis(self.interval_ms)
    }

    fn policy(&self) -> Policy {
        Policy { hydra_grace: DurationMs(self.hydra_grace_ms), retain: Retain(self.retain) }
    }
}

pub fn wanted(declared: &BTreeMap<GuestName, Declared>, rev: &CommitHash) -> Vec<Key> {
    declared
        .values()
        .filter_map(|declared| match declared {
            Declared::Container(_) => Some(Key { job: declared.image(), rev: rev.clone() }),
            Declared::Vm(_) => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn deployed(observed: &Observation) -> Vec<(Vmid, NixHash)> {
    observed.managed().iter().map(|managed| (managed.id(), managed.tags().nix.clone())).collect()
}

pub struct World<'a> {
    pub wanted: &'a [Key],
    pub hydra: &'a BTreeMap<Key, HydraBuild>,
    pub deployed: &'a [(Vmid, NixHash)],
    pub policy: Policy,
}

pub trait Store {
    fn present(&self) -> Result<BTreeSet<Toplevel>>;
    fn rooted(&self) -> Result<BTreeMap<RootHolder, Toplevel>>;
    fn apply(&self, effects: &[StoreEffect]) -> Vec<StoreEvent>;
}

impl Store for NixStore {
    fn present(&self) -> Result<BTreeSet<Toplevel>> {
        NixStore::present(self)
    }

    fn rooted(&self) -> Result<BTreeMap<RootHolder, Toplevel>> {
        NixStore::rooted(self)
    }

    fn apply(&self, effects: &[StoreEffect]) -> Vec<StoreEvent> {
        NixStore::apply(self, effects)
    }
}

const SETTLE_LIMIT: usize = 64;

pub fn settle(ledger: Ledger, world: &World<'_>, store: &impl Store, clock: &Clock) -> Result<Ledger> {
    let settled = (0..SETTLE_LIMIT).try_fold((ledger, Vec::new()), |(ledger, events), _| {
        let present = match store.present() {
            Ok(present) => present,
            Err(error) => return std::ops::ControlFlow::Break(Err(error)),
        };
        let rooted = match store.rooted() {
            Ok(rooted) => rooted,
            Err(error) => return std::ops::ControlFlow::Break(Err(error)),
        };
        let step = sync_step(SyncInput {
            ledger,
            wanted: world.wanted,
            hydra: world.hydra,
            present: &present,
            deployed: world.deployed,
            rooted: &rooted,
            events,
            now: clock.now(),
            policy: world.policy,
        });
        let heard = store.apply(&step.effects);
        if heard.is_empty() {
            std::ops::ControlFlow::Break(Ok(step.ledger))
        } else {
            std::ops::ControlFlow::Continue((step.ledger, heard))
        }
    });
    match settled {
        std::ops::ControlFlow::Break(result) => result,
        std::ops::ControlFlow::Continue((ledger, _)) => {
            warn!("the store sync did not settle within {SETTLE_LIMIT} rounds; continuing next pass");
            Ok(ledger)
        }
    }
}

pub struct Syncer {
    config: StoreSyncConfig,
    hydra: Hydra,
    root: PathBuf,
    timeout: Duration,
    ledger: Arc<Mutex<Ledger>>,
    clock: Clock,
    wall: crate::builds::WallClock,
}

impl Syncer {
    pub fn new(settings: &AppConfig, config: StoreSyncConfig, root: PathBuf, runtime: tokio::runtime::Handle) -> Syncer {
        Syncer {
            hydra: Hydra::new(config.hydra.clone(), runtime),
            root,
            timeout: settings.timings_ms.get(crate::types::Timing::NixBuild),
            config,
            ledger: Arc::new(Mutex::new(Ledger::default())),
            clock: Clock::start(),
            wall: crate::builds::WallClock::started_now(),
        }
    }

    pub fn views(&self) -> Vec<crate::builds::BuildView> {
        crate::builds::views(&self.ledger.lock().unwrap_or_else(PoisonError::into_inner), self.wall)
    }

    pub fn pass(&self, repo: PathBuf, declared: &BTreeMap<GuestName, Declared>, rev: &CommitHash, observed: &Observation) -> Result<()> {
        let store = NixStore::new(self.root.clone(), self.config.cache.clone(), Flake { repo, dir: None }, self.timeout);
        let wanted = wanted(declared, rev);
        let hydra = self.hydra.status(&wanted)?;
        let deployed = deployed(observed);
        let world = World { wanted: &wanted, hydra: &hydra, deployed: &deployed, policy: self.config.policy() };
        let before = self.ledger.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let after = settle(before, &world, &store, &self.clock)?;
        let ready = wanted.iter().filter(|key| after.ready(key).is_some()).count();
        info!("store sync: {ready} of {} images ready for {}", wanted.len(), rev.as_ref());
        *self.ledger.lock().unwrap_or_else(PoisonError::into_inner) = after;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxnix_core::{BuildState, ImageType, Source, StorePath};
    use std::cell::RefCell;

    const REV: &str = "b44ce58f9c9d8565bbdd2990f54c3e91b2c8082e";

    fn key(job: &str) -> Key {
        Key { job: ImageType(String::from(job)), rev: REV.parse().unwrap() }
    }

    fn toplevel(hash: &str) -> Toplevel {
        Toplevel::from(format!("/nix/store/{hash}-nixos-system").parse::<StorePath>().unwrap())
    }

    struct Fake {
        present: RefCell<BTreeSet<Toplevel>>,
        rooted: RefCell<BTreeMap<RootHolder, Toplevel>>,
        applied: RefCell<Vec<StoreEffect>>,
        builds_to: Toplevel,
    }

    impl Store for Fake {
        fn present(&self) -> Result<BTreeSet<Toplevel>> {
            Ok(self.present.borrow().clone())
        }

        fn rooted(&self) -> Result<BTreeMap<RootHolder, Toplevel>> {
            Ok(self.rooted.borrow().clone())
        }

        fn apply(&self, effects: &[StoreEffect]) -> Vec<StoreEvent> {
            self.applied.borrow_mut().extend(effects.iter().cloned());
            effects
                .iter()
                .filter_map(|effect| match effect {
                    StoreEffect::Copy { key, toplevel } => {
                        self.present.borrow_mut().insert(toplevel.clone());
                        Some(StoreEvent::Copied { key: key.clone(), outcome: Ok(()) })
                    }
                    StoreEffect::Build { key } => {
                        self.present.borrow_mut().insert(self.builds_to.clone());
                        Some(StoreEvent::Built { key: key.clone(), outcome: Ok(self.builds_to.clone()) })
                    }
                    StoreEffect::Root { holder, toplevel } => {
                        self.rooted.borrow_mut().insert(holder.clone(), toplevel.clone());
                        None
                    }
                    StoreEffect::Unroot(holder) => {
                        self.rooted.borrow_mut().remove(holder);
                        None
                    }
                    StoreEffect::Collect => None,
                })
                .collect()
        }
    }

    fn fake() -> Fake {
        Fake {
            present: RefCell::new(BTreeSet::new()),
            rooted: RefCell::new(BTreeMap::new()),
            applied: RefCell::new(vec![]),
            builds_to: toplevel("i3d00236fdkfw1v9cmasajkjhzl8zi5j"),
        }
    }

    fn policy() -> Policy {
        Policy { hydra_grace: DurationMs(600_000), retain: Retain(2) }
    }

    #[test]
    fn one_pass_copies_what_hydra_built_builds_what_it_failed_and_roots_both() {
        let store = fake();
        let wanted = [key("build-lxc-forgejo"), key("build-lxc-hydra")];
        let hydra: BTreeMap<Key, HydraBuild> = [
            (wanted[0].clone(), HydraBuild::Succeeded(toplevel("78s0iadvjz6s48aqvx4rw78lwrzkjzlw"))),
            (wanted[1].clone(), HydraBuild::Failed),
        ]
        .into();
        let world = World { wanted: &wanted, hydra: &hydra, deployed: &[], policy: policy() };
        let ledger = settle(Ledger::default(), &world, &store, &Clock::start()).unwrap();
        assert!(matches!(ledger.state(&wanted[0]), Some(BuildState::Ready { source: Source::Hydra, .. })));
        assert!(matches!(ledger.state(&wanted[1]), Some(BuildState::Ready { source: Source::Local, .. })));
        assert_eq!(store.rooted.borrow().len(), 2);
        let again = settle(ledger, &world, &store, &Clock::start()).unwrap();
        let before = store.applied.borrow().len();
        settle(again, &world, &store, &Clock::start()).unwrap();
        assert_eq!(store.applied.borrow().len(), before, "a settled store is left alone");
    }

    #[test]
    fn a_push_hydra_has_not_evaluated_waits_instead_of_building_at_once() {
        let store = fake();
        let wanted = [key("build-lxc-forgejo")];
        let world = World { wanted: &wanted, hydra: &BTreeMap::new(), deployed: &[], policy: policy() };
        let ledger = settle(Ledger::default(), &world, &store, &Clock::start()).unwrap();
        assert!(matches!(ledger.state(&wanted[0]), Some(BuildState::AwaitingHydra { .. })));
        assert!(store.applied.borrow().is_empty());
    }

    #[test]
    fn only_container_images_are_synced_once_per_image() {
        let settings = crate::state::parse_appconfig(crate::state::tests_support::NIXOLOGY_APPCONFIG).unwrap();
        let eval = r#"{"vms": {"web": {"name": "web", "hostname": "web", "blue_id": 823, "green_id": 923, "dhcp_timeout_seconds": 1,
            "health_check_timeout_seconds": 1, "image_type": "build-qcow2-website", "cores": 1, "sockets": 1, "memory_mb": 512, "disk_gb": 8,
            "storage_location": "local-lvm", "protected": false, "impure": false}},
            "containers": {
              "a": {"name": "a", "hostname": "a", "blue_id": 830, "green_id": 930, "dhcp_timeout_seconds": 1, "health_check_timeout_seconds": 1,
                    "image_type": "build-lxc", "cores": 1, "memory_mb": 512, "disk_gb": 8, "storage_location": "ZFS", "protected": false, "impure": false},
              "b": {"name": "b", "hostname": "b", "blue_id": 831, "green_id": 931, "dhcp_timeout_seconds": 1, "health_check_timeout_seconds": 1,
                    "image_type": "build-lxc", "cores": 1, "memory_mb": 512, "disk_gb": 8, "storage_location": "ZFS", "protected": false, "impure": false}}}"#;
        let declared = crate::engine::declare(crate::state::parse_config(eval).unwrap(), crate::engine::layout(&settings).as_ref());
        assert_eq!(wanted(&declared, &REV.parse().unwrap()), vec![key("build-lxc")]);
    }

    #[test]
    fn the_sync_block_is_optional_and_defaults_what_it_can() {
        let parsed: StoreSyncConfig =
            serde_json::from_str(r#"{"hydra": {"url": "http://hydra.thesta.rs", "project": "nixology", "jobset": "main"}}"#).unwrap();
        assert_eq!(parsed.cache, "file:///ZFS/hydra-cache");
        assert_eq!(parsed.interval(), Duration::from_secs(600));
        assert_eq!(parsed.policy(), Policy { hydra_grace: DurationMs(600_000), retain: Retain(3) });
        assert!(serde_json::from_str::<StoreSyncConfig>(r#"{"hydra": {"url": "u", "project": "p", "jobset": "j"}, "cach": "x"}"#).is_err());
    }
}
