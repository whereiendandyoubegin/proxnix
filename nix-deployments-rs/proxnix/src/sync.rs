use crate::engine::Clock;
use crate::hydra::{Hydra, HydraConfig};
use crate::interpret::Declared;
use crate::nixstore::{Flake, NixStore};
use crate::types::{AppConfig, Result};
use proxnix_core::{
    CommitHash, DurationMs, GuestName, HydraBuild, Key, Ledger, NixHash, Observation, Policy,
    Retain, RootHolder, SlotState, StoreEffect, StoreEvent, SyncInput, Toplevel, Vmid,
    root_effects, sync_step,
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
        Policy {
            hydra_grace: DurationMs(self.hydra_grace_ms),
            retain: Retain(self.retain),
        }
    }
}

pub fn wanted(declared: &BTreeMap<GuestName, Declared>, rev: &CommitHash) -> Vec<Key> {
    declared
        .values()
        .filter_map(|declared| match declared {
            Declared::Container(placed)
                if placed.config.store == crate::types::StoreChoice::Shared =>
            {
                Some(Key {
                    job: declared.image(),
                    rev: rev.clone(),
                })
            }
            Declared::Container(_) | Declared::Vm(_) => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub fn deployed(observed: &Observation) -> Vec<(Vmid, NixHash)> {
    observed
        .managed()
        .iter()
        .map(|managed| (managed.id(), managed.tags().nix.clone()))
        .collect()
}

fn still_occupied(effect: &StoreEffect, observed: &Observation) -> bool {
    match effect {
        StoreEffect::Unroot(RootHolder::Guest(id)) => {
            !matches!(observed.slot(*id), SlotState::Vacant(_))
        }
        _ => false,
    }
}

pub struct World<'a> {
    pub wanted: &'a [Key],
    pub hydra: &'a BTreeMap<Key, HydraBuild>,
    pub observe: &'a dyn Fn() -> Result<Observation>,
    pub policy: Policy,
}

pub trait Store {
    type Hold;
    fn present(&self) -> Result<BTreeSet<Toplevel>>;
    fn rooted(&self) -> Result<BTreeMap<RootHolder, Toplevel>>;
    fn apply(&self, effects: &[StoreEffect]) -> Vec<StoreEvent>;
    fn hold_roots(&self) -> Result<Self::Hold>;
}

impl Store for NixStore {
    type Hold = std::fs::File;

    fn present(&self) -> Result<BTreeSet<Toplevel>> {
        NixStore::present(self)
    }

    fn rooted(&self) -> Result<BTreeMap<RootHolder, Toplevel>> {
        NixStore::rooted(self)
    }

    fn apply(&self, effects: &[StoreEffect]) -> Vec<StoreEvent> {
        NixStore::apply(self, effects)
    }

    fn hold_roots(&self) -> Result<std::fs::File> {
        NixStore::hold_roots(self)
    }
}

fn reroot(ledger: &Ledger, world: &World<'_>, store: &impl Store) -> Result<()> {
    let _held = store.hold_roots()?;
    let observed = (world.observe)()?;
    let effects: Vec<StoreEffect> = root_effects(
        ledger,
        &deployed(&observed),
        &store.present()?,
        &store.rooted()?,
        world.policy.retain,
    )
    .into_iter()
    .filter(|effect| !still_occupied(effect, &observed))
    .collect();
    store.apply(&effects);
    Ok(())
}

const SETTLE_LIMIT: usize = 64;

pub fn settle(
    ledger: Ledger,
    world: &World<'_>,
    store: &impl Store,
    clock: &Clock,
) -> Result<Ledger> {
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
            deployed: &[],
            rooted: &rooted,
            events,
            now: clock.now(),
            policy: world.policy,
        });
        let transfers: Vec<StoreEffect> = step
            .effects
            .into_iter()
            .filter(|effect| matches!(effect, StoreEffect::Copy { .. } | StoreEffect::Build { .. }))
            .collect();
        let heard = store.apply(&transfers);
        if let Err(error) = reroot(&step.ledger, world, store) {
            return std::ops::ControlFlow::Break(Err(error));
        }
        if heard.is_empty() {
            std::ops::ControlFlow::Break(Ok(step.ledger))
        } else {
            std::ops::ControlFlow::Continue((step.ledger, heard))
        }
    });
    match settled {
        std::ops::ControlFlow::Break(result) => result,
        std::ops::ControlFlow::Continue((ledger, _)) => {
            warn!(
                "the store sync did not settle within {SETTLE_LIMIT} rounds; continuing next pass"
            );
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
    pub fn new(
        settings: &AppConfig,
        config: StoreSyncConfig,
        root: PathBuf,
        runtime: tokio::runtime::Handle,
    ) -> Syncer {
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
        crate::builds::views(
            &self.ledger.lock().unwrap_or_else(PoisonError::into_inner),
            self.wall,
        )
    }

    pub fn pass(
        &self,
        repo: PathBuf,
        declared: &BTreeMap<GuestName, Declared>,
        rev: &CommitHash,
        observe: &dyn Fn() -> Result<Observation>,
    ) -> Result<()> {
        let store = NixStore::new(
            self.root.clone(),
            self.config.cache.clone(),
            Flake { repo, dir: None },
            self.timeout,
        );
        let wanted = wanted(declared, rev);
        let hydra = self.hydra.status(&wanted)?;
        let world = World {
            wanted: &wanted,
            hydra: &hydra,
            observe,
            policy: self.config.policy(),
        };
        let before = self
            .ledger
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let after = settle(before, &world, &store, &self.clock)?;
        let ready = wanted
            .iter()
            .filter(|key| after.ready(key).is_some())
            .count();
        info!(
            "store sync: {ready} of {} images ready for {}",
            wanted.len(),
            rev.as_ref()
        );
        *self.ledger.lock().unwrap_or_else(PoisonError::into_inner) = after;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxnix_core::{
        Audited, BuildState, Cores, DiskGib, Grant, GuestStatus, ImageType, KindFacts, MemoryMb,
        Permissions, Privilege, RawTags, Resources, Settled, Sighting, Source, StorePath,
        Unsettled,
    };
    use std::cell::{Cell, RefCell};

    const REV: &str = "b44ce58f9c9d8565bbdd2990f54c3e91b2c8082e";

    fn key(job: &str) -> Key {
        Key {
            job: ImageType(String::from(job)),
            rev: REV.parse().unwrap(),
        }
    }

    fn toplevel(hash: &str) -> Toplevel {
        Toplevel::from(
            format!("/nix/store/{hash}-nixos-system")
                .parse::<StorePath>()
                .unwrap(),
        )
    }

    struct Fake {
        present: RefCell<BTreeSet<Toplevel>>,
        rooted: RefCell<BTreeMap<RootHolder, Toplevel>>,
        applied: RefCell<Vec<StoreEffect>>,
        builds_to: Toplevel,
        deploys_during_build: Option<(Vmid, Toplevel)>,
        held: Cell<u32>,
    }

    impl Store for Fake {
        type Hold = ();

        fn hold_roots(&self) -> Result<()> {
            self.held.set(self.held.get() + 1);
            Ok(())
        }

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
                        Some(StoreEvent::Copied {
                            key: key.clone(),
                            outcome: Ok(()),
                        })
                    }
                    StoreEffect::Build { key } => {
                        if let Some((id, toplevel)) = &self.deploys_during_build {
                            self.present.borrow_mut().insert(toplevel.clone());
                            self.rooted
                                .borrow_mut()
                                .insert(RootHolder::Guest(*id), toplevel.clone());
                        }
                        self.present.borrow_mut().insert(self.builds_to.clone());
                        Some(StoreEvent::Built {
                            key: key.clone(),
                            outcome: Ok(self.builds_to.clone()),
                        })
                    }
                    StoreEffect::Root { holder, toplevel } => {
                        self.rooted
                            .borrow_mut()
                            .insert(holder.clone(), toplevel.clone());
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
            deploys_during_build: None,
            held: Cell::new(0),
        }
    }

    const DEPLOYED: &str = "0l5zcg4wgcizg2136hm7yc0y9psfjamp";

    fn seen(sightings: Vec<Sighting>) -> Observation {
        Observation::new(
            Audited::try_from(Permissions {
                vm_audit: Grant::Granted,
            })
            .unwrap(),
            sightings,
        )
    }

    fn container(id: u32, nix: &str) -> Sighting {
        Sighting::Settled(Settled {
            id: Vmid::new(id),
            name: GuestName(String::from("forgejo")),
            status: GuestStatus::Running,
            tags: RawTags::from(format!("proxnix;nix-{nix};commit-{REV};slot-green;pending")),
            resources: Resources {
                memory: MemoryMb(512),
                disk: DiskGib(8),
                cores: Cores(1),
            },
            facts: KindFacts::Lxc {
                privilege: Privilege::Unprivileged,
                mounts: vec![],
            },
        })
    }

    fn nothing() -> Result<Observation> {
        Ok(seen(vec![]))
    }

    fn policy() -> Policy {
        Policy {
            hydra_grace: DurationMs(600_000),
            retain: Retain(2),
        }
    }

    #[test]
    fn one_pass_copies_what_hydra_built_builds_what_it_failed_and_roots_both() {
        let store = fake();
        let wanted = [key("build-lxc-forgejo"), key("build-lxc-hydra")];
        let hydra: BTreeMap<Key, HydraBuild> = [
            (
                wanted[0].clone(),
                HydraBuild::Succeeded(toplevel("78s0iadvjz6s48aqvx4rw78lwrzkjzlw")),
            ),
            (wanted[1].clone(), HydraBuild::Failed),
        ]
        .into();
        let world = World {
            wanted: &wanted,
            hydra: &hydra,
            observe: &nothing,
            policy: policy(),
        };
        let ledger = settle(Ledger::default(), &world, &store, &Clock::start()).unwrap();
        assert!(matches!(
            ledger.state(&wanted[0]),
            Some(BuildState::Ready {
                source: Source::Hydra,
                ..
            })
        ));
        assert!(matches!(
            ledger.state(&wanted[1]),
            Some(BuildState::Ready {
                source: Source::Local,
                ..
            })
        ));
        assert_eq!(store.rooted.borrow().len(), 2);
        let again = settle(ledger, &world, &store, &Clock::start()).unwrap();
        let before = store.applied.borrow().len();
        settle(again, &world, &store, &Clock::start()).unwrap();
        assert_eq!(
            store.applied.borrow().len(),
            before,
            "a settled store is left alone"
        );
    }

    #[test]
    fn a_push_hydra_has_not_evaluated_waits_instead_of_building_at_once() {
        let store = fake();
        let wanted = [key("build-lxc-forgejo")];
        let world = World {
            wanted: &wanted,
            hydra: &BTreeMap::new(),
            observe: &nothing,
            policy: policy(),
        };
        let ledger = settle(Ledger::default(), &world, &store, &Clock::start()).unwrap();
        assert!(matches!(
            ledger.state(&wanted[0]),
            Some(BuildState::AwaitingHydra { .. })
        ));
        assert!(store.applied.borrow().is_empty());
    }

    #[test]
    fn only_shared_store_images_are_synced_once_per_image() {
        let settings =
            crate::state::parse_appconfig(crate::state::tests_support::NIXOLOGY_APPCONFIG).unwrap();
        let eval = r#"{"vms": {"web": {"name": "web", "hostname": "web", "blue_id": 823, "green_id": 923, "dhcp_timeout_seconds": 1,
            "health_check_timeout_seconds": 1, "image_type": "build-qcow2-website", "cores": 1, "sockets": 1, "memory_mb": 512, "disk_gb": 8,
            "storage_location": "local-lvm", "protected": false, "impure": false}},
            "containers": {
              "a": {"name": "a", "hostname": "a", "blue_id": 830, "green_id": 930, "dhcp_timeout_seconds": 1, "health_check_timeout_seconds": 1,
                    "image_type": "build-lxc", "cores": 1, "memory_mb": 512, "disk_gb": 8, "storage_location": "ZFS", "protected": false, "impure": false,
                    "store": "shared"},
              "b": {"name": "b", "hostname": "b", "blue_id": 831, "green_id": 931, "dhcp_timeout_seconds": 1, "health_check_timeout_seconds": 1,
                    "image_type": "build-lxc", "cores": 1, "memory_mb": 512, "disk_gb": 8, "storage_location": "ZFS", "protected": false, "impure": false,
                    "store": "shared"},
              "c": {"name": "c", "hostname": "c", "blue_id": 832, "green_id": 932, "dhcp_timeout_seconds": 1, "health_check_timeout_seconds": 1,
                    "image_type": "build-lxc-image", "cores": 1, "memory_mb": 512, "disk_gb": 8, "storage_location": "ZFS", "protected": false,
                    "impure": false},
              "d": {"name": "d", "hostname": "d", "blue_id": 833, "green_id": 933, "dhcp_timeout_seconds": 1, "health_check_timeout_seconds": 1,
                    "image_type": "build-lxc-private", "cores": 1, "memory_mb": 512, "disk_gb": 8, "storage_location": "ZFS", "protected": false,
                    "impure": false, "store": "private", "cutover": "stop_start"}}}"#;
        let declared = crate::engine::declare(
            crate::state::parse_config(eval).unwrap(),
            crate::engine::layout(&settings).as_ref(),
        );
        assert_eq!(
            wanted(&declared, &REV.parse().unwrap()),
            vec![key("build-lxc")]
        );
    }

    #[test]
    fn the_sync_block_is_optional_and_defaults_what_it_can() {
        let parsed: StoreSyncConfig =
            serde_json::from_str(r#"{"hydra": {"url": "http://hydra.thesta.rs", "project": "nixology", "jobset": "main"}}"#).unwrap();
        assert_eq!(parsed.cache, "file:///ZFS/hydra-cache");
        assert_eq!(parsed.interval(), Duration::from_secs(600));
        assert_eq!(
            parsed.policy(),
            Policy {
                hydra_grace: DurationMs(600_000),
                retain: Retain(3)
            }
        );
        assert!(
            serde_json::from_str::<StoreSyncConfig>(
                r#"{"hydra": {"url": "u", "project": "p", "jobset": "j"}, "cach": "x"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn a_guest_deployed_while_the_sync_was_building_keeps_its_root() {
        let store = Fake {
            deploys_during_build: Some((Vmid::new(944), toplevel(DEPLOYED))),
            ..fake()
        };
        let wanted = [key("build-lxc-hydra")];
        let hydra: BTreeMap<Key, HydraBuild> = [(wanted[0].clone(), HydraBuild::Failed)].into();
        let deployed = Cell::new(false);
        let observe = || {
            deployed.set(!store.rooted.borrow().is_empty());
            Ok(if deployed.get() {
                seen(vec![container(944, DEPLOYED)])
            } else {
                seen(vec![])
            })
        };
        let world = World {
            wanted: &wanted,
            hydra: &hydra,
            observe: &observe,
            policy: policy(),
        };
        settle(Ledger::default(), &world, &store, &Clock::start()).unwrap();
        assert_eq!(
            store
                .rooted
                .borrow()
                .get(&RootHolder::Guest(Vmid::new(944))),
            Some(&toplevel(DEPLOYED))
        );
        assert!(
            store.held.get() > 0,
            "roots are only changed under the store lock"
        );
    }

    #[test]
    fn a_guest_root_is_kept_while_its_vmid_is_occupied_and_dropped_once_it_is_vacant() {
        let rooted_at = |observation: Observation| {
            let store = fake();
            store.present.borrow_mut().insert(toplevel(DEPLOYED));
            store
                .rooted
                .borrow_mut()
                .insert(RootHolder::Guest(Vmid::new(944)), toplevel(DEPLOYED));
            let observe = move || Ok(observation.clone());
            let world = World {
                wanted: &[],
                hydra: &BTreeMap::new(),
                observe: &observe,
                policy: policy(),
            };
            settle(Ledger::default(), &world, &store, &Clock::start()).unwrap();
            store
                .rooted
                .into_inner()
                .contains_key(&RootHolder::Guest(Vmid::new(944)))
        };
        assert!(
            rooted_at(seen(vec![Sighting::Unsettled(
                Vmid::new(944),
                Unsettled::Unreadable
            )])),
            "an unreadable guest is still there"
        );
        assert!(rooted_at(seen(vec![container(944, DEPLOYED)])));
        assert!(
            !rooted_at(seen(vec![])),
            "a vacant vmid's root is collectable"
        );
    }
}
