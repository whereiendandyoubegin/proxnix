#[pure_only]
use crate::cohort::Instance;
#[pure_only]
use crate::effect::{Backend, Check, Effect, EffectError, EffectId, Event, GuestEffect, Outcome, ProbeEffect, Provisioned, RouteEffect};
#[pure_only]
use crate::guest::{Attempt, DurationMs};
#[pure_only]
use crate::ids::Vmid;
#[pure_only]
use crate::spec::GuestName;
#[pure_only]
use crate::tags::NixHash;
#[pure_only]
use crate::tick::{Moment, Pacing};
use proxnix_pure::pure_only;
#[pure_only]
use std::collections::{BTreeMap, BTreeSet};
#[pure_only]
use std::iter::once;
#[pure_only]
use std::net::Ipv4Addr;

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    Create,
    Start,
    Stop,
    Record,
    Role,
    Commit,
    Update,
    Undo,
    Reclaim,
    Retire,
}

#[pure_only]
impl Action {
    #[must_use]
    pub fn of(effect: &GuestEffect) -> Action {
        match effect {
            GuestEffect::Create { .. } => Action::Create,
            GuestEffect::Start(_) => Action::Start,
            GuestEffect::Stop(_) => Action::Stop,
            GuestEffect::Record { .. } => Action::Record,
            GuestEffect::Role { .. } => Action::Role,
            GuestEffect::Commit(_) => Action::Commit,
            GuestEffect::Update { .. } => Action::Update,
            GuestEffect::Undo(_) => Action::Undo,
            GuestEffect::Reclaim(_) => Action::Reclaim,
            GuestEffect::Retire(_) => Action::Retire,
        }
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Expired { check: Check, attempts: Attempt, last: Option<EffectError> },
    Refused { action: Action, error: EffectError },
    Route(EffectError),
    NoAddress,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Passed(Option<Ipv4Addr>),
    Due,
    Waiting(Moment),
    Expired(Attempt, Option<EffectError>),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
enum Trial {
    Trying { deadline: Moment, retry_at: Moment, attempts: Attempt, last: Option<EffectError> },
    Passed(Option<Ipv4Addr>),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RouteKey {
    name: GuestName,
    backend: Backend,
}

#[pure_only]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Memo {
    next: u64,
    issued: BTreeMap<EffectId, (GuestName, Effect)>,
    deploys: BTreeSet<Instance>,
    provisioned: BTreeMap<Instance, Provisioned>,
    trials: BTreeMap<(Instance, Check), Trial>,
    routed: BTreeSet<RouteKey>,
    route_failures: BTreeMap<RouteKey, EffectError>,
    failures: BTreeMap<(Vmid, Action), EffectError>,
    given_up: BTreeMap<(GuestName, NixHash), Failure>,
    cleared: BTreeSet<GuestName>,
    updated: BTreeSet<Instance>,
}

#[pure_only]
fn put<K: Ord, V>(map: BTreeMap<K, V>, key: K, value: V) -> BTreeMap<K, V> {
    let kept: Vec<(K, V)> = map.into_iter().filter(|(existing, _)| *existing != key).collect();
    kept.into_iter().chain(once((key, value))).collect()
}

#[pure_only]
fn add<K: Ord>(set: BTreeSet<K>, key: K) -> BTreeSet<K> {
    set.into_iter().chain(once(key)).collect()
}

#[pure_only]
fn pace(pacing: &Pacing, check: Check) -> DurationMs {
    match check {
        Check::Address => pacing.address,
        Check::Port => pacing.port,
        Check::Guest => pacing.guest,
    }
}

#[pure_only]
fn usable(address: Ipv4Addr) -> bool {
    !address.is_link_local() && !address.is_unspecified() && !address.is_loopback()
}

#[pure_only]
fn verdict(check: Check, outcome: &Outcome) -> Option<Trial> {
    match (check, outcome) {
        (Check::Address, Outcome::Address(address)) if usable(*address) => Some(Trial::Passed(Some(*address))),
        (Check::Port | Check::Guest, Outcome::Done | Outcome::AlreadyApplied) => Some(Trial::Passed(None)),
        _ => None,
    }
}

#[pure_only]
fn failure_of(outcome: Outcome) -> Option<EffectError> {
    match outcome {
        Outcome::Failed(error) => Some(error),
        _ => None,
    }
}

#[pure_only]
impl Memo {
    pub(crate) fn absorb(self, events: Vec<Event>, now: Moment, pacing: &Pacing) -> Memo {
        let absorbed = events.into_iter().fold(self, |memo, event| memo.absorb_one(event, now, pacing));
        Memo { issued: BTreeMap::new(), ..absorbed }
    }

    fn absorb_one(self, event: Event, now: Moment, pacing: &Pacing) -> Memo {
        match self.issued.get(&event.effect).cloned() {
            None => self,
            Some((name, Effect::Guest(effect))) => self.guest_outcome(name, &effect, event.outcome),
            Some((_, Effect::Probe(probe))) => self.probe_outcome(&probe, event.outcome, now, pacing),
            Some((name, Effect::Route(route))) => self.route_outcome(name, route, event.outcome),
        }
    }

    fn guest_outcome(self, name: GuestName, effect: &GuestEffect, outcome: Outcome) -> Memo {
        match (effect, outcome) {
            (GuestEffect::Create { artifact, .. }, Outcome::Failed(error)) => {
                let nix = artifact.nix().clone();
                Memo {
                    failures: put(self.failures, (effect.id(), Action::Create), error.clone()),
                    given_up: put(self.given_up, (name, nix), Failure::Refused { action: Action::Create, error }),
                    ..self
                }
            }
            (GuestEffect::Create { .. }, outcome) => match (Provisioned::confirmed(effect, &outcome), effect.instance()) {
                (Some(provisioned), Some(instance)) => Memo { provisioned: put(self.provisioned, instance, provisioned), ..self },
                _ => self,
            },
            (GuestEffect::Update { guest, .. }, Outcome::Done | Outcome::AlreadyApplied) => {
                Memo { updated: add(self.updated, guest.instance()), ..self }
            }
            (_, Outcome::Failed(error)) => Memo { failures: put(self.failures, (effect.id(), Action::of(effect)), error), ..self },
            _ => self,
        }
    }

    fn probe_outcome(self, probe: &ProbeEffect, outcome: Outcome, now: Moment, pacing: &Pacing) -> Memo {
        let check = probe.check();
        let key = (probe.guest().instance(), check);
        let passed = verdict(check, &outcome);
        let trial = match (passed, self.trials.get(&key)) {
            (Some(passed), _) => passed,
            (None, Some(Trial::Trying { deadline, attempts, .. })) => Trial::Trying {
                deadline: *deadline,
                retry_at: now.after(pace(pacing, check)),
                attempts: attempts.next(),
                last: failure_of(outcome),
            },
            (None, _) => Trial::Trying { deadline: now, retry_at: now, attempts: Attempt(1), last: failure_of(outcome) },
        };
        Memo { trials: put(self.trials, key, trial), ..self }
    }

    fn route_outcome(self, name: GuestName, route: RouteEffect, outcome: Outcome) -> Memo {
        match (route, outcome) {
            (RouteEffect::RemoveCluster(name), _) => Memo { cleared: add(self.cleared, name), ..self },
            (RouteEffect::Point { to, .. } | RouteEffect::Restore { to, .. }, Outcome::Failed(error)) => {
                Memo { route_failures: put(self.route_failures, RouteKey { name, backend: to }, error), ..self }
            }
            (RouteEffect::Point { to, .. } | RouteEffect::Restore { to, .. }, _) => {
                Memo { routed: add(self.routed, RouteKey { name, backend: to }), ..self }
            }
        }
    }

    pub(crate) fn issue(self, name: &GuestName, effect: Effect, within: Option<DurationMs>, now: Moment) -> (Memo, EffectId) {
        let id = EffectId(self.next);
        let deploys = match &effect {
            Effect::Guest(create @ GuestEffect::Create { .. }) => create.instance().map_or(self.deploys.clone(), |instance| add(self.deploys.clone(), instance)),
            _ => self.deploys.clone(),
        };
        let trials = match &effect {
            Effect::Probe(probe) if !self.trials.contains_key(&(probe.guest().instance(), probe.check())) => put(
                self.trials.clone(),
                (probe.guest().instance(), probe.check()),
                Trial::Trying {
                    deadline: now.after(within.unwrap_or(DurationMs(0))),
                    retry_at: now,
                    attempts: Attempt(0),
                    last: None,
                },
            ),
            _ => self.trials.clone(),
        };
        (
            Memo {
                next: self.next.saturating_add(1),
                issued: put(self.issued, id, (name.clone(), effect)),
                deploys,
                trials,
                ..self
            },
            id,
        )
    }

    pub(crate) fn give_up(self, name: &GuestName, nix: NixHash, failure: Failure) -> Memo {
        Memo { given_up: put(self.given_up, (name.clone(), nix), failure), ..self }
    }

    pub(crate) fn aborted(&self, effect: &Effect) -> Option<Instance> {
        match effect {
            Effect::Guest(GuestEffect::Undo(provisioned)) => self
                .provisioned
                .iter()
                .find(|(_, candidate)| *candidate == provisioned)
                .map(|(instance, _)| instance.clone()),
            Effect::Guest(GuestEffect::Reclaim(doomed)) => Some(doomed.instance()),
            _ => None,
        }
    }

    pub(crate) fn progress(&self, instance: &Instance, check: Check, now: Moment) -> Progress {
        match self.trials.get(&(instance.clone(), check)) {
            None => Progress::Due,
            Some(Trial::Passed(found)) => Progress::Passed(*found),
            Some(Trial::Trying { deadline, attempts, last, .. }) if now >= *deadline => Progress::Expired(*attempts, last.clone()),
            Some(Trial::Trying { retry_at, .. }) if now >= *retry_at => Progress::Due,
            Some(Trial::Trying { retry_at, deadline, .. }) => Progress::Waiting((*retry_at).min(*deadline)),
        }
    }

    pub(crate) fn in_flight(&self, instance: &Instance) -> bool {
        self.deploys.contains(instance)
    }

    pub(crate) fn provisioned(&self, instance: &Instance) -> Option<Provisioned> {
        self.provisioned.get(instance).copied()
    }

    pub(crate) fn failed(&self, id: Vmid, action: Action) -> Option<&EffectError> {
        self.failures.get(&(id, action))
    }

    pub(crate) fn given_up(&self, name: &GuestName, nix: &NixHash) -> Option<&Failure> {
        self.given_up.get(&(name.clone(), nix.clone()))
    }

    pub(crate) fn routed(&self, name: &GuestName, backend: &Backend) -> bool {
        self.routed.contains(&RouteKey { name: name.clone(), backend: backend.clone() })
    }

    pub(crate) fn route_failed(&self, name: &GuestName, backend: &Backend) -> Option<&EffectError> {
        self.route_failures.get(&RouteKey { name: name.clone(), backend: backend.clone() })
    }

    pub(crate) fn cleared(&self, name: &GuestName) -> bool {
        self.cleared.contains(name)
    }

    pub(crate) fn updated(&self, instance: &Instance) -> bool {
        self.updated.contains(instance)
    }
}
