#[pure_only]
use crate::build::{BuildFault, StorePath};
#[pure_only]
use crate::effect::Detail;
#[pure_only]
use crate::guest::DurationMs;
#[pure_only]
use crate::ids::Vmid;
#[pure_only]
use crate::spec::ImageType;
#[pure_only]
use crate::tags::{CommitHash, NixHash};
#[pure_only]
use crate::tick::Moment;
use proxnix_pure::pure_only;
#[pure_only]
use std::cmp::Reverse;
#[pure_only]
use std::collections::{BTreeMap, BTreeSet};

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Toplevel(StorePath);

#[pure_only]
impl From<StorePath> for Toplevel {
    fn from(path: StorePath) -> Toplevel {
        Toplevel(path)
    }
}

#[pure_only]
impl Toplevel {
    #[must_use]
    pub fn path(&self) -> &StorePath {
        &self.0
    }

    #[must_use]
    pub fn nix(&self) -> &NixHash {
        self.0.hash()
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
    pub job: ImageType,
    pub rev: CommitHash,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Hydra,
    Local,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HydraBuild {
    Unevaluated,
    Absent,
    Queued,
    Succeeded(Toplevel),
    Failed,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncFault {
    Build(BuildFault),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildState {
    AwaitingHydra {
        since: Moment,
    },
    Copying {
        toplevel: Toplevel,
    },
    Building {
        started: Moment,
    },
    Ready {
        toplevel: Toplevel,
        source: Source,
        at: Moment,
    },
    Failed {
        fault: SyncFault,
    },
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RootHolder {
    Guest(Vmid),
    Recent(Key),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreEffect {
    Copy {
        key: Key,
        toplevel: Toplevel,
    },
    Build {
        key: Key,
    },
    Root {
        holder: RootHolder,
        toplevel: Toplevel,
    },
    Unroot(RootHolder),
    Collect,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreEvent {
    Copied {
        key: Key,
        outcome: Result<(), Detail>,
    },
    Built {
        key: Key,
        outcome: Result<Toplevel, BuildFault>,
    },
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retain(pub u8);

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub hydra_grace: DurationMs,
    pub retain: Retain,
}

#[pure_only]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ledger(BTreeMap<Key, BuildState>);

#[pure_only]
impl Ledger {
    #[must_use]
    pub fn state(&self, key: &Key) -> Option<&BuildState> {
        self.0.get(key)
    }

    pub fn entries(&self) -> impl Iterator<Item = (&Key, &BuildState)> {
        self.0.iter()
    }

    #[must_use]
    pub fn ready(&self, key: &Key) -> Option<&Toplevel> {
        match self.0.get(key) {
            Some(BuildState::Ready { toplevel, .. }) => Some(toplevel),
            _ => None,
        }
    }

    fn with(&self, key: &Key, state: BuildState) -> Ledger {
        Ledger(
            self.0
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .chain([(key.clone(), state)])
                .collect(),
        )
    }
}

#[pure_only]
pub struct SyncInput<'a> {
    pub ledger: Ledger,
    pub wanted: &'a [Key],
    pub hydra: &'a BTreeMap<Key, HydraBuild>,
    pub present: &'a BTreeSet<Toplevel>,
    pub deployed: &'a [(Vmid, NixHash)],
    pub rooted: &'a BTreeMap<RootHolder, Toplevel>,
    pub events: Vec<StoreEvent>,
    pub now: Moment,
    pub policy: Policy,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStep {
    pub ledger: Ledger,
    pub effects: Vec<StoreEffect>,
    pub wake: Option<Moment>,
}

#[pure_only]
fn after(moment: Moment, wait: DurationMs) -> Moment {
    Moment(moment.0.saturating_add(wait.0))
}

#[pure_only]
fn obtain(
    key: &Key,
    toplevel: &Toplevel,
    present: &BTreeSet<Toplevel>,
    now: Moment,
) -> (BuildState, Option<StoreEffect>) {
    if present.contains(toplevel) {
        (
            BuildState::Ready {
                toplevel: toplevel.clone(),
                source: Source::Hydra,
                at: now,
            },
            None,
        )
    } else {
        (
            BuildState::Copying {
                toplevel: toplevel.clone(),
            },
            Some(StoreEffect::Copy {
                key: key.clone(),
                toplevel: toplevel.clone(),
            }),
        )
    }
}

#[pure_only]
fn build(key: &Key, now: Moment) -> (BuildState, Option<StoreEffect>) {
    (
        BuildState::Building { started: now },
        Some(StoreEffect::Build { key: key.clone() }),
    )
}

#[pure_only]
fn key_of(event: &StoreEvent) -> &Key {
    match event {
        StoreEvent::Copied { key, .. } | StoreEvent::Built { key, .. } => key,
    }
}

#[pure_only]
fn heard(
    ledger: &Ledger,
    event: &StoreEvent,
    now: Moment,
) -> Option<(Key, BuildState, Option<StoreEffect>)> {
    match (event, ledger.state(key_of(event))) {
        (
            StoreEvent::Copied {
                key,
                outcome: Ok(()),
            },
            Some(BuildState::Copying { toplevel }),
        ) => Some((
            key.clone(),
            BuildState::Ready {
                toplevel: toplevel.clone(),
                source: Source::Hydra,
                at: now,
            },
            None,
        )),
        (
            StoreEvent::Copied {
                key,
                outcome: Err(_),
            },
            Some(BuildState::Copying { .. }),
        ) => {
            let (state, effect) = build(key, now);
            Some((key.clone(), state, effect))
        }
        (
            StoreEvent::Built {
                key,
                outcome: Ok(toplevel),
            },
            Some(BuildState::Building { .. }),
        ) => Some((
            key.clone(),
            BuildState::Ready {
                toplevel: toplevel.clone(),
                source: Source::Local,
                at: now,
            },
            None,
        )),
        (
            StoreEvent::Built {
                key,
                outcome: Err(fault),
            },
            Some(BuildState::Building { .. }),
        ) => Some((
            key.clone(),
            BuildState::Failed {
                fault: SyncFault::Build(fault.clone()),
            },
            None,
        )),
        _ => None,
    }
}

#[pure_only]
fn advance(
    key: &Key,
    state: Option<&BuildState>,
    hydra: Option<&HydraBuild>,
    present: &BTreeSet<Toplevel>,
    now: Moment,
    policy: Policy,
) -> Option<(BuildState, Option<StoreEffect>)> {
    match (state, hydra) {
        (None | Some(BuildState::AwaitingHydra { .. }), Some(HydraBuild::Succeeded(toplevel))) => {
            Some(obtain(key, toplevel, present, now))
        }
        (
            None | Some(BuildState::AwaitingHydra { .. }),
            Some(HydraBuild::Failed | HydraBuild::Absent),
        ) => Some(build(key, now)),
        (Some(BuildState::AwaitingHydra { since }), _)
            if after(*since, policy.hydra_grace) <= now =>
        {
            Some(build(key, now))
        }
        (None, _) => Some((BuildState::AwaitingHydra { since: now }, None)),
        (Some(_), _) => None,
    }
}

#[pure_only]
#[must_use]
pub fn roots(
    ledger: &Ledger,
    deployed: &[(Vmid, NixHash)],
    present: &BTreeSet<Toplevel>,
    retain: Retain,
) -> BTreeMap<RootHolder, Toplevel> {
    let guests = deployed.iter().filter_map(|(id, nix)| {
        present
            .iter()
            .find(|toplevel| toplevel.nix() == nix)
            .map(|toplevel| (RootHolder::Guest(*id), toplevel.clone()))
    });
    let ready: Vec<(&Key, &Toplevel, Moment)> = ledger
        .entries()
        .filter_map(|(key, state)| match state {
            BuildState::Ready { toplevel, at, .. } => Some((key, toplevel, *at)),
            _ => None,
        })
        .collect();
    let jobs: BTreeSet<&ImageType> = ready.iter().map(|(key, _, _)| &key.job).collect();
    let recent = jobs.into_iter().flat_map(|job| {
        ready
            .iter()
            .filter(|(key, _, _)| key.job == *job)
            .map(|(key, toplevel, at)| (Reverse(*at), *key, *toplevel))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(usize::from(retain.0))
            .map(|(_, key, toplevel)| (RootHolder::Recent(key.clone()), toplevel.clone()))
            .collect::<Vec<_>>()
    });
    guests.chain(recent).collect()
}

#[pure_only]
fn rooting(
    wanted: &BTreeMap<RootHolder, Toplevel>,
    rooted: &BTreeMap<RootHolder, Toplevel>,
) -> Vec<StoreEffect> {
    let root = wanted
        .iter()
        .filter(|(holder, toplevel)| rooted.get(*holder) != Some(*toplevel))
        .map(|(holder, toplevel)| StoreEffect::Root {
            holder: holder.clone(),
            toplevel: toplevel.clone(),
        });
    let unroot: Vec<StoreEffect> = rooted
        .keys()
        .filter(|holder| !wanted.contains_key(*holder))
        .map(|holder| StoreEffect::Unroot(holder.clone()))
        .collect();
    let collect = (!unroot.is_empty()).then_some(StoreEffect::Collect);
    root.chain(unroot).chain(collect).collect()
}

#[pure_only]
#[must_use]
pub fn root_effects(
    ledger: &Ledger,
    deployed: &[(Vmid, NixHash)],
    present: &BTreeSet<Toplevel>,
    rooted: &BTreeMap<RootHolder, Toplevel>,
    retain: Retain,
) -> Vec<StoreEffect> {
    rooting(&roots(ledger, deployed, present, retain), rooted)
}

#[pure_only]
#[must_use]
pub fn sync_step(input: SyncInput<'_>) -> SyncStep {
    let SyncInput {
        ledger,
        wanted,
        hydra,
        present,
        deployed,
        rooted,
        events,
        now,
        policy,
    } = input;
    let (ledger, heard_effects) = events.iter().fold(
        (ledger, Vec::new()),
        |(ledger, effects), event| match heard(&ledger, event, now) {
            Some((key, state, effect)) => (
                ledger.with(&key, state),
                effects.into_iter().chain(effect).collect::<Vec<_>>(),
            ),
            None => (ledger, effects),
        },
    );
    let (ledger, advanced_effects) = wanted.iter().fold(
        (ledger, Vec::new()),
        |(ledger, effects), key| match advance(
            key,
            ledger.state(key),
            hydra.get(key),
            present,
            now,
            policy,
        ) {
            Some((state, effect)) => (
                ledger.with(key, state),
                effects.into_iter().chain(effect).collect::<Vec<_>>(),
            ),
            None => (ledger, effects),
        },
    );
    let wake = ledger
        .entries()
        .filter_map(|(_, state)| match state {
            BuildState::AwaitingHydra { since } => Some(after(*since, policy.hydra_grace)),
            _ => None,
        })
        .min();
    let rooted_effects = root_effects(&ledger, deployed, present, rooted, policy.retain);
    SyncStep {
        effects: heard_effects
            .into_iter()
            .chain(advanced_effects)
            .chain(rooted_effects)
            .collect(),
        ledger,
        wake,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REV: &str = "66d0ba6b605de2703e0fb7bbf58b922d5b36597e";
    const NEXT: &str = "9d6c5a707946c3ccde19b6f5cd2f9bd365caadeb";

    fn key(job: &str, rev: &str) -> Key {
        Key {
            job: ImageType(String::from(job)),
            rev: rev.parse().unwrap(),
        }
    }

    fn toplevel(hash: &str) -> Toplevel {
        Toplevel::from(
            format!("/nix/store/{hash}-nixos-system-forgejo")
                .parse::<StorePath>()
                .unwrap(),
        )
    }

    const A: &str = "78s0iadvjz6s48aqvx4rw78lwrzkjzlw";
    const B: &str = "i3d00236fdkfw1v9cmasajkjhzl8zi5j";

    fn policy() -> Policy {
        Policy {
            hydra_grace: DurationMs(600_000),
            retain: Retain(2),
        }
    }

    struct World {
        hydra: BTreeMap<Key, HydraBuild>,
        present: BTreeSet<Toplevel>,
        deployed: Vec<(Vmid, NixHash)>,
        rooted: BTreeMap<RootHolder, Toplevel>,
    }

    fn world() -> World {
        World {
            hydra: BTreeMap::new(),
            present: BTreeSet::new(),
            deployed: vec![],
            rooted: BTreeMap::new(),
        }
    }

    fn step(
        world: &World,
        ledger: Ledger,
        wanted: &[Key],
        events: Vec<StoreEvent>,
        now: u64,
    ) -> SyncStep {
        sync_step(SyncInput {
            ledger,
            wanted,
            hydra: &world.hydra,
            present: &world.present,
            deployed: &world.deployed,
            rooted: &world.rooted,
            events,
            now: Moment(now),
            policy: policy(),
        })
    }

    fn syncing(effects: &[StoreEffect]) -> Vec<&StoreEffect> {
        effects
            .iter()
            .filter(|effect| matches!(effect, StoreEffect::Copy { .. } | StoreEffect::Build { .. }))
            .collect()
    }

    #[test]
    fn a_build_hydra_finished_is_copied_not_rebuilt() {
        let forgejo = key("build-lxc-forgejo", REV);
        let hydra = World {
            hydra: [(forgejo.clone(), HydraBuild::Succeeded(toplevel(A)))].into(),
            ..world()
        };
        let copying = step(
            &hydra,
            Ledger::default(),
            std::slice::from_ref(&forgejo),
            vec![],
            0,
        );
        assert_eq!(
            syncing(&copying.effects),
            vec![&StoreEffect::Copy {
                key: forgejo.clone(),
                toplevel: toplevel(A)
            }]
        );
        let copied = step(
            &hydra,
            copying.ledger,
            std::slice::from_ref(&forgejo),
            vec![StoreEvent::Copied {
                key: forgejo.clone(),
                outcome: Ok(()),
            }],
            5,
        );
        assert!(syncing(&copied.effects).is_empty());
        assert_eq!(
            copied.ledger.state(&forgejo),
            Some(&BuildState::Ready {
                toplevel: toplevel(A),
                source: Source::Hydra,
                at: Moment(5)
            })
        );
    }

    #[test]
    fn a_build_already_in_the_store_is_ready_at_once() {
        let forgejo = key("build-lxc-forgejo", REV);
        let held = World {
            hydra: [(forgejo.clone(), HydraBuild::Succeeded(toplevel(A)))].into(),
            present: [toplevel(A)].into(),
            ..world()
        };
        let done = step(
            &held,
            Ledger::default(),
            std::slice::from_ref(&forgejo),
            vec![],
            0,
        );
        assert!(syncing(&done.effects).is_empty());
        assert_eq!(done.ledger.ready(&forgejo), Some(&toplevel(A)));
    }

    #[test]
    fn a_fresh_push_waits_for_hydra_then_builds_locally_once_the_grace_runs_out() {
        let forgejo = key("build-lxc-forgejo", NEXT);
        let waiting = step(
            &world(),
            Ledger::default(),
            std::slice::from_ref(&forgejo),
            vec![],
            1_000,
        );
        assert!(syncing(&waiting.effects).is_empty());
        assert_eq!(waiting.wake, Some(Moment(601_000)));
        let still = step(
            &world(),
            waiting.ledger,
            std::slice::from_ref(&forgejo),
            vec![],
            600_999,
        );
        assert!(syncing(&still.effects).is_empty());
        let building = step(
            &world(),
            still.ledger,
            std::slice::from_ref(&forgejo),
            vec![],
            601_000,
        );
        assert_eq!(
            syncing(&building.effects),
            vec![&StoreEffect::Build {
                key: forgejo.clone()
            }]
        );
        assert_eq!(building.wake, None);
    }

    #[test]
    fn hydra_finishing_during_the_grace_wins_over_a_local_build() {
        let forgejo = key("build-lxc-forgejo", NEXT);
        let waiting = step(
            &world(),
            Ledger::default(),
            std::slice::from_ref(&forgejo),
            vec![],
            0,
        );
        let finished = World {
            hydra: [(forgejo.clone(), HydraBuild::Succeeded(toplevel(B)))].into(),
            ..world()
        };
        let copying = step(
            &finished,
            waiting.ledger,
            std::slice::from_ref(&forgejo),
            vec![],
            10,
        );
        assert_eq!(
            syncing(&copying.effects),
            vec![&StoreEffect::Copy {
                key: forgejo,
                toplevel: toplevel(B)
            }]
        );
    }

    #[test]
    fn a_failed_hydra_build_or_a_failed_copy_falls_back_to_building_locally() {
        let forgejo = key("build-lxc-forgejo", REV);
        let failed = World {
            hydra: [(forgejo.clone(), HydraBuild::Failed)].into(),
            ..world()
        };
        assert_eq!(
            syncing(
                &step(
                    &failed,
                    Ledger::default(),
                    std::slice::from_ref(&forgejo),
                    vec![],
                    0
                )
                .effects
            ),
            vec![&StoreEffect::Build {
                key: forgejo.clone()
            }]
        );
        let hydra = World {
            hydra: [(forgejo.clone(), HydraBuild::Succeeded(toplevel(A)))].into(),
            ..world()
        };
        let copying = step(
            &hydra,
            Ledger::default(),
            std::slice::from_ref(&forgejo),
            vec![],
            0,
        );
        let broken = StoreEvent::Copied {
            key: forgejo.clone(),
            outcome: Err(Detail(String::from("cache unreachable"))),
        };
        let fallback = step(
            &hydra,
            copying.ledger,
            std::slice::from_ref(&forgejo),
            vec![broken],
            1,
        );
        assert_eq!(
            syncing(&fallback.effects),
            vec![&StoreEffect::Build {
                key: forgejo.clone()
            }]
        );
        assert_eq!(
            fallback.ledger.state(&forgejo),
            Some(&BuildState::Building { started: Moment(1) })
        );
    }

    #[test]
    fn a_job_hydra_does_not_build_is_built_locally_without_waiting() {
        let forgejo = key("build-lxc-forgejo", REV);
        let absent = World {
            hydra: [(forgejo.clone(), HydraBuild::Absent)].into(),
            ..world()
        };
        let now = step(
            &absent,
            Ledger::default(),
            std::slice::from_ref(&forgejo),
            vec![],
            0,
        );
        assert_eq!(
            syncing(&now.effects),
            vec![&StoreEffect::Build { key: forgejo }]
        );
        assert_eq!(now.wake, None);
    }

    #[test]
    fn a_local_build_ends_ready_or_failed_and_either_is_final() {
        let forgejo = key("build-lxc-forgejo", REV);
        let failed = World {
            hydra: [(forgejo.clone(), HydraBuild::Failed)].into(),
            ..world()
        };
        let building = step(
            &failed,
            Ledger::default(),
            std::slice::from_ref(&forgejo),
            vec![],
            0,
        );
        let built = step(
            &failed,
            building.ledger.clone(),
            std::slice::from_ref(&forgejo),
            vec![StoreEvent::Built {
                key: forgejo.clone(),
                outcome: Ok(toplevel(A)),
            }],
            9,
        );
        assert_eq!(
            built.ledger.state(&forgejo),
            Some(&BuildState::Ready {
                toplevel: toplevel(A),
                source: Source::Local,
                at: Moment(9)
            })
        );
        let broken = StoreEvent::Built {
            key: forgejo.clone(),
            outcome: Err(BuildFault::TimedOut(DurationMs(1))),
        };
        let gave_up = step(
            &failed,
            building.ledger,
            std::slice::from_ref(&forgejo),
            vec![broken],
            9,
        );
        assert_eq!(
            gave_up.ledger.state(&forgejo),
            Some(&BuildState::Failed {
                fault: SyncFault::Build(BuildFault::TimedOut(DurationMs(1)))
            })
        );
        let later = World {
            hydra: [(forgejo.clone(), HydraBuild::Succeeded(toplevel(B)))].into(),
            ..world()
        };
        assert!(
            syncing(
                &step(
                    &later,
                    built.ledger,
                    std::slice::from_ref(&forgejo),
                    vec![],
                    99
                )
                .effects
            )
            .is_empty()
        );
        assert!(
            syncing(
                &step(
                    &later,
                    gave_up.ledger,
                    std::slice::from_ref(&forgejo),
                    vec![],
                    99
                )
                .effects
            )
            .is_empty()
        );
    }

    #[test]
    fn an_event_for_something_not_in_flight_changes_nothing() {
        let forgejo = key("build-lxc-forgejo", REV);
        let stray = step(
            &world(),
            Ledger::default(),
            &[],
            vec![StoreEvent::Built {
                key: forgejo.clone(),
                outcome: Ok(toplevel(A)),
            }],
            0,
        );
        assert_eq!(stray.ledger.state(&forgejo), None);
    }

    #[test]
    fn a_settled_store_emits_nothing_when_stepped_again() {
        let forgejo = key("build-lxc-forgejo", REV);
        let hydra = World {
            hydra: [(forgejo.clone(), HydraBuild::Succeeded(toplevel(A)))].into(),
            present: [toplevel(A)].into(),
            ..world()
        };
        let first = step(
            &hydra,
            Ledger::default(),
            std::slice::from_ref(&forgejo),
            vec![],
            0,
        );
        let rooted = World {
            rooted: roots(&first.ledger, &[], &hydra.present, policy().retain),
            ..hydra
        };
        let again = step(&rooted, first.ledger, &[forgejo], vec![], 100);
        assert!(again.effects.is_empty(), "{:?}", again.effects);
    }

    #[test]
    fn deployed_guests_and_the_newest_ready_revs_of_each_job_are_rooted() {
        let revs = [
            "1111111111111111111111111111111111111111",
            "2222222222222222222222222222222222222222",
            "3333333333333333333333333333333333333333",
        ];
        let hashes = [A, B, "0l5zcg4wgcizg2136hm7yc0y9psfjamp"];
        let ledger = revs.iter().zip(hashes).enumerate().fold(
            Ledger::default(),
            |ledger, (at, (rev, hash))| {
                ledger.with(
                    &key("build-lxc-forgejo", rev),
                    BuildState::Ready {
                        toplevel: toplevel(hash),
                        source: Source::Hydra,
                        at: Moment(at as u64),
                    },
                )
            },
        );
        let present: BTreeSet<Toplevel> = hashes.iter().map(|hash| toplevel(hash)).collect();
        let kept = roots(
            &ledger,
            &[(Vmid::new(844), A.parse().unwrap())],
            &present,
            Retain(2),
        );
        assert_eq!(
            kept.get(&RootHolder::Guest(Vmid::new(844))),
            Some(&toplevel(A))
        );
        let recent: BTreeSet<&Key> = kept
            .keys()
            .filter_map(|holder| match holder {
                RootHolder::Recent(key) => Some(key),
                RootHolder::Guest(_) => None,
            })
            .collect();
        assert_eq!(
            recent,
            [
                key("build-lxc-forgejo", revs[1]),
                key("build-lxc-forgejo", revs[2])
            ]
            .iter()
            .collect()
        );
    }

    #[test]
    fn roots_that_are_no_longer_wanted_are_dropped_and_then_collected() {
        let stale = RootHolder::Guest(Vmid::new(944));
        let old = World {
            rooted: [(stale.clone(), toplevel(A))].into(),
            ..world()
        };
        let step = step(&old, Ledger::default(), &[], vec![], 0);
        assert_eq!(
            step.effects,
            vec![StoreEffect::Unroot(stale), StoreEffect::Collect]
        );
    }

    #[test]
    fn a_guest_whose_toplevel_is_not_in_the_store_gets_no_root() {
        assert!(
            roots(
                &Ledger::default(),
                &[(Vmid::new(844), A.parse().unwrap())],
                &BTreeSet::new(),
                Retain(2)
            )
            .is_empty()
        );
    }
}
