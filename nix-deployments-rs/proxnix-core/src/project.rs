#[pure_only]
use crate::build::Images;
#[pure_only]
use crate::desired::Desired;
#[pure_only]
use crate::effect::{Effect, Event, GuestEffect, Outcome, ProbeEffect, ResourceChange};
#[pure_only]
use crate::guest::{GuestStatus, KindFacts, Resources};
#[pure_only]
use crate::ids::Vmid;
#[pure_only]
use crate::memo::Memo;
#[pure_only]
use crate::observation::{Guest, Observation, Sighting};
#[pure_only]
use crate::spec::{GuestName, KindSpec};
#[pure_only]
use crate::step::{Input, Report, step};
#[pure_only]
use crate::strategy::Registry;
#[pure_only]
use crate::tags::{ManagedTags, Ownership, RawTags};
#[pure_only]
use crate::tick::{Moment, Pacing, Tick};
use proxnix_pure::pure_only;
#[pure_only]
use std::net::Ipv4Addr;

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Workload(GuestName),
    Teardown,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projection {
    pub scope: Scope,
    pub first: Report,
    pub effects: Vec<Effect>,
    pub last: Report,
    pub settled: bool,
}

#[pure_only]
#[must_use]
pub fn placeholder_address(id: Vmid) -> Ipv4Addr {
    Ipv4Addr::new(192, 0, 2, u8::try_from(id.get() % 250).unwrap_or(0).saturating_add(1))
}

#[pure_only]
fn facts(kind: &KindSpec) -> KindFacts {
    match kind {
        KindSpec::Qemu { sockets } => KindFacts::Qemu { sockets: *sockets },
        KindSpec::Lxc { privilege, mounts } => KindFacts::Lxc { privilege: *privilege, mounts: mounts.clone() },
    }
}

#[pure_only]
fn revise(observed: &Observation, id: Vmid, change: impl FnOnce(&Guest) -> Guest) -> Observation {
    match observed.guest(id) {
        Some(guest) => observed.replaced(id, Some(change(guest))),
        None => observed.clone(),
    }
}

#[pure_only]
fn retag(observed: &Observation, id: Vmid, change: impl FnOnce(ManagedTags) -> ManagedTags) -> Observation {
    revise(observed, id, |guest| match guest.ownership() {
        Ownership::Managed(tags) => guest.revised(guest.status(), guest.resources(), Ownership::Managed(change(tags.clone()))),
        _ => guest.clone(),
    })
}

#[pure_only]
fn restate(observed: &Observation, id: Vmid, status: GuestStatus) -> Observation {
    revise(observed, id, |guest| guest.revised(status, guest.resources(), guest.ownership().clone()))
}

#[pure_only]
fn resized(resources: Resources, changes: &[ResourceChange]) -> Resources {
    changes.iter().fold(resources, |resources, change| match change {
        ResourceChange::Memory(memory) => Resources { memory: *memory, ..resources },
        ResourceChange::Cores(cores) => Resources { cores: *cores, ..resources },
        ResourceChange::Sockets(_) => resources,
    })
}

#[pure_only]
fn assume_guest(observed: &Observation, effect: &GuestEffect) -> Observation {
    match effect {
        GuestEffect::Create { target, spec, fresh, .. } => observed.replaced(
            target.id(),
            Some(Guest::projected(
                Sighting {
                    id: target.id(),
                    name: spec.name.clone(),
                    status: GuestStatus::Stopped,
                    tags: RawTags::from(String::new()),
                    resources: spec.resources,
                    facts: facts(&spec.kind),
                },
                Ownership::Managed(ManagedTags {
                    nix: fresh.nix().clone(),
                    commit: fresh.commit().clone(),
                    slot: fresh.slot(),
                    service_ip: None,
                    generation: None,
                    role: fresh.role().cloned(),
                }),
            )),
        ),
        GuestEffect::Start(member) => restate(observed, member.id(), GuestStatus::Running),
        GuestEffect::Stop(member) => restate(observed, member.id(), GuestStatus::Stopped),
        GuestEffect::Record { guest, address } => retag(observed, guest.id(), |tags| ManagedTags { service_ip: Some(*address), ..tags }),
        GuestEffect::Role { guest, role } => retag(observed, guest.id(), |tags| ManagedTags { role: Some(role.clone()), ..tags }),
        GuestEffect::Commit(promotion) => {
            retag(observed, promotion.guest().id(), |tags| ManagedTags { generation: Some(promotion.generation()), ..tags })
        }
        GuestEffect::Update { guest, changes } => revise(observed, guest.id(), |seen| {
            seen.revised(seen.status(), resized(seen.resources(), changes), seen.ownership().clone())
        }),
        GuestEffect::Undo(provisioned) => observed.replaced(provisioned.id(), None),
        GuestEffect::Reclaim(doomed) | GuestEffect::Retire(doomed) => observed.replaced(doomed.id(), None),
    }
}

#[pure_only]
#[must_use]
pub fn assume(observed: &Observation, effect: &Effect) -> (Observation, Outcome) {
    match effect {
        Effect::Guest(guest) => (assume_guest(observed, guest), Outcome::Done),
        Effect::Probe(ProbeEffect::ReadAddress(member)) => (observed.clone(), Outcome::Address(placeholder_address(member.id()))),
        Effect::Probe(_) | Effect::Route(_) => (observed.clone(), Outcome::Done),
    }
}

#[pure_only]
struct Walk {
    observed: Observation,
    memo: Memo,
    events: Vec<Event>,
    now: Moment,
    effects: Vec<Effect>,
    first: Option<Report>,
    last: Option<Report>,
}

#[pure_only]
fn walk<R: Registry>(desired: &Desired, images: &Images, observed: &Observation, tick: &Tick, pacing: &Pacing, scope: Scope, limit: usize) -> Projection {
    let start = Walk { observed: observed.clone(), memo: Memo::default(), events: vec![], now: Moment(0), effects: vec![], first: None, last: None };
    let finished = (0..limit).try_fold(start, |walk, _| {
        let stepped = step::<R>(Input {
            memo: walk.memo,
            desired,
            images,
            observed: &walk.observed,
            events: walk.events,
            now: walk.now,
            tick,
            pacing,
        });
        let first = walk.first.or_else(|| Some(stepped.report.clone()));
        if stepped.quiescent() {
            return Err(Box::new(Projection {
                scope: scope.clone(),
                first: first.unwrap_or_else(|| stepped.report.clone()),
                effects: walk.effects,
                last: stepped.report,
                settled: true,
            }));
        }
        let (observed, events) = stepped.effects.iter().fold((walk.observed, Vec::new()), |(observed, events), planned| {
            let (observed, outcome) = assume(&observed, &planned.effect);
            (observed, events.into_iter().chain([Event { effect: planned.id, outcome }]).collect())
        });
        let now = match (stepped.effects.is_empty(), stepped.wake) {
            (true, Some(wake)) => wake.max(walk.now.after(crate::guest::DurationMs(1))),
            _ => walk.now.after(crate::guest::DurationMs(1000)),
        };
        Ok(Walk {
            observed,
            memo: stepped.memo,
            events,
            now,
            effects: walk.effects.into_iter().chain(stepped.effects.into_iter().map(|planned| planned.effect)).collect(),
            first,
            last: Some(stepped.report),
        })
    });
    match finished {
        Err(projection) => *projection,
        Ok(walk) => Projection {
            scope,
            first: walk.first.unwrap_or(Report { workloads: vec![], anomalies: vec![] }),
            effects: walk.effects,
            last: walk.last.unwrap_or(Report { workloads: vec![], anomalies: vec![] }),
            settled: false,
        },
    }
}

#[pure_only]
#[must_use]
pub fn project<R: Registry>(desired: &Desired, images: &Images, observed: &Observation, tick: &Tick, pacing: &Pacing) -> Vec<Projection> {
    let limit = 10_000;
    desired
        .names()
        .into_iter()
        .map(|name| walk::<R>(&desired.workload(&name), images, observed, tick, pacing, Scope::Workload(name), limit))
        .chain([walk::<R>(&desired.teardown(), images, observed, tick, pacing, Scope::Teardown, limit)])
        .collect()
}
