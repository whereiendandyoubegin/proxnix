use crate::nixstore::store_text;
use proxnix_core::{BuildFault, BuildState, Ledger, Moment, Source, SyncFault};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    AwaitingHydra,
    Copying,
    Building,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Hydra,
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BuildView {
    pub job: String,
    pub rev: String,
    pub state: Phase,
    pub source: Option<Origin>,
    pub toplevel: Option<String>,
    pub since: Option<u64>,
    pub fault: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct WallClock(SystemTime);

impl WallClock {
    pub fn started_now() -> WallClock {
        WallClock(SystemTime::now())
    }

    fn unix(self, moment: Moment) -> Option<u64> {
        (self.0 + Duration::from_millis(moment.0))
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|since| since.as_secs())
    }
}

fn fault_text(fault: &SyncFault) -> String {
    match fault {
        SyncFault::Build(BuildFault::Checkout(detail)) => format!("checkout: {}", detail.0),
        SyncFault::Build(BuildFault::Eval(_, detail)) => format!("eval: {}", detail.0),
        SyncFault::Build(BuildFault::Build(_, detail)) => format!("build: {}", detail.0),
        SyncFault::Build(BuildFault::Output(fault)) => format!("output: {fault:?}"),
        SyncFault::Build(BuildFault::TimedOut(after)) => {
            format!("timed out after {}s", after.0 / 1000)
        }
    }
}

fn view(clock: WallClock, job: String, rev: String, state: &BuildState) -> BuildView {
    let blank = BuildView {
        job,
        rev,
        state: Phase::AwaitingHydra,
        source: None,
        toplevel: None,
        since: None,
        fault: None,
    };
    match state {
        BuildState::AwaitingHydra { since } => BuildView {
            since: clock.unix(*since),
            ..blank
        },
        BuildState::Copying { toplevel } => BuildView {
            state: Phase::Copying,
            toplevel: Some(store_text(toplevel.path())),
            ..blank
        },
        BuildState::Building { started } => BuildView {
            state: Phase::Building,
            since: clock.unix(*started),
            ..blank
        },
        BuildState::Ready {
            toplevel,
            source,
            at,
        } => BuildView {
            state: Phase::Ready,
            source: Some(match source {
                Source::Hydra => Origin::Hydra,
                Source::Local => Origin::Local,
            }),
            toplevel: Some(store_text(toplevel.path())),
            since: clock.unix(*at),
            ..blank
        },
        BuildState::Failed { fault } => BuildView {
            state: Phase::Failed,
            fault: Some(fault_text(fault)),
            ..blank
        },
    }
}

pub fn views(ledger: &Ledger, clock: WallClock) -> Vec<BuildView> {
    ledger
        .entries()
        .map(|(key, state)| {
            view(
                clock,
                key.job.0.clone(),
                String::from(key.rev.as_ref()),
                state,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxnix_core::{
        Detail, DurationMs, HydraBuild, ImageType, Key, Policy, Retain, StoreEvent, StorePath,
        SyncInput, Toplevel, sync_step,
    };
    use std::collections::{BTreeMap, BTreeSet};

    const REV: &str = "b44ce58f9c9d8565bbdd2990f54c3e91b2c8082e";
    const TOPLEVEL: &str = "/nix/store/78s0iadvjz6s48aqvx4rw78lwrzkjzlw-nixos-system-forgejo";

    fn key(job: &str) -> Key {
        Key {
            job: ImageType(String::from(job)),
            rev: REV.parse().unwrap(),
        }
    }

    fn ledger() -> Ledger {
        let ready = key("build-lxc-forgejo");
        let failed = key("build-lxc-hydra");
        let waiting = key("build-lxc-monitoring");
        let toplevel = Toplevel::from(TOPLEVEL.parse::<StorePath>().unwrap());
        let hydra: BTreeMap<Key, HydraBuild> = [
            (ready.clone(), HydraBuild::Succeeded(toplevel.clone())),
            (failed.clone(), HydraBuild::Failed),
        ]
        .into();
        let present: BTreeSet<Toplevel> = [toplevel].into();
        let first = sync_step(SyncInput {
            ledger: Ledger::default(),
            wanted: &[ready, failed.clone(), waiting],
            hydra: &hydra,
            present: &present,
            deployed: &[],
            rooted: &BTreeMap::new(),
            events: vec![],
            now: Moment(2_000),
            policy: Policy {
                hydra_grace: DurationMs(600_000),
                retain: Retain(3),
            },
        });
        let broken = StoreEvent::Built {
            key: failed,
            outcome: Err(BuildFault::Build(None, Detail(String::from("exit 1")))),
        };
        sync_step(SyncInput {
            ledger: first.ledger,
            wanted: &[],
            hydra: &hydra,
            present: &present,
            deployed: &[],
            rooted: &BTreeMap::new(),
            events: vec![broken],
            now: Moment(5_000),
            policy: Policy {
                hydra_grace: DurationMs(600_000),
                retain: Retain(3),
            },
        })
        .ledger
    }

    #[test]
    fn every_ledger_entry_becomes_a_json_row_with_wall_clock_times() {
        let clock = WallClock(UNIX_EPOCH + Duration::from_secs(1_790_000_000));
        let rows = views(&ledger(), clock);
        let json = serde_json::to_value(&rows).unwrap();
        assert_eq!(
            json,
            serde_json::json!([
                {"job": "build-lxc-forgejo", "rev": REV, "state": "ready", "source": "hydra", "toplevel": TOPLEVEL, "since": 1_790_000_002, "fault": null},
                {"job": "build-lxc-hydra", "rev": REV, "state": "failed", "source": null, "toplevel": null, "since": null, "fault": "build: exit 1"},
                {"job": "build-lxc-monitoring", "rev": REV, "state": "awaiting-hydra", "source": null, "toplevel": null, "since": 1_790_000_002, "fault": null},
            ])
        );
    }
}
