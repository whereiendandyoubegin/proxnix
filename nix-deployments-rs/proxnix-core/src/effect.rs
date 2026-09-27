#[pure_only]
use crate::build::Artifact;
#[pure_only]
use crate::cohort::{Expendable, Instance, Member, Promotion};
#[pure_only]
use crate::guest::{Cores, DurationMs, GuestKind, MemoryMb, Port, Sockets};
#[pure_only]
use crate::ids::{Slot, Vmid};
#[pure_only]
use crate::observation::Vacant;
#[pure_only]
use crate::spec::{GuestName, ProxySpec, WorkloadSpec};
#[pure_only]
use crate::tags::{CommitHash, Generation, NixHash, RoleName};
#[pure_only]
use crate::tick::Push;
use proxnix_pure::pure_only;
#[pure_only]
use std::net::Ipv4Addr;

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EffectId(pub u64);

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail(pub String);

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectError {
    Refused(Detail),
    TaskFailed(Detail),
    TimedOut(DurationMs),
    Unreachable(Detail),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done,
    AlreadyApplied,
    Address(Ipv4Addr),
    Failed(EffectError),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub effect: EffectId,
    pub outcome: Outcome,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fresh {
    nix: NixHash,
    commit: CommitHash,
    slot: Slot,
    role: Option<RoleName>,
}

#[pure_only]
impl Fresh {
    #[must_use]
    pub fn new(push: &Push, artifact: &Artifact, spec: &WorkloadSpec, target: Vacant, role: Option<RoleName>) -> Option<Fresh> {
        spec.slots.slot_of(target.id()).map(|slot| Fresh {
            nix: artifact.nix().clone(),
            commit: push.commit().clone(),
            slot,
            role,
        })
    }

    #[must_use]
    pub fn nix(&self) -> &NixHash {
        &self.nix
    }

    #[must_use]
    pub fn commit(&self) -> &CommitHash {
        &self.commit
    }

    #[must_use]
    pub fn slot(&self) -> Slot {
        self.slot
    }

    #[must_use]
    pub fn role(&self) -> Option<&RoleName> {
        self.role.as_ref()
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provisioned {
    id: Vmid,
    kind: GuestKind,
}

#[pure_only]
impl Provisioned {
    #[must_use]
    pub fn confirmed(request: &GuestEffect, outcome: &Outcome) -> Option<Provisioned> {
        match (request, outcome) {
            (GuestEffect::Create { target, spec, .. }, Outcome::Done) => Some(Provisioned {
                id: target.id(),
                kind: spec.kind(),
            }),
            _ => None,
        }
    }

    pub(crate) fn attempted(request: &GuestEffect) -> Option<Provisioned> {
        match request {
            GuestEffect::Create { target, spec, .. } => Some(Provisioned { id: target.id(), kind: spec.kind() }),
            _ => None,
        }
    }

    #[must_use]
    pub fn id(self) -> Vmid {
        self.id
    }

    #[must_use]
    pub fn kind(self) -> GuestKind {
        self.kind
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceChange {
    Memory(MemoryMb),
    Cores(Cores),
    Sockets(Sockets),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestEffect {
    Create { target: Vacant, artifact: Artifact, spec: Box<WorkloadSpec>, fresh: Fresh },
    Start(Member),
    Stop(Member),
    Record { guest: Member, address: Ipv4Addr },
    Role { guest: Member, role: RoleName },
    Commit(Promotion),
    Update { guest: Member, changes: Vec<ResourceChange> },
    Undo(Provisioned),
    Reclaim(Expendable),
    Retire(Expendable),
}

#[pure_only]
impl GuestEffect {
    #[must_use]
    pub fn id(&self) -> Vmid {
        match self {
            GuestEffect::Create { target, .. } => target.id(),
            GuestEffect::Start(guest)
            | GuestEffect::Stop(guest)
            | GuestEffect::Record { guest, .. }
            | GuestEffect::Role { guest, .. }
            | GuestEffect::Update { guest, .. } => guest.id(),
            GuestEffect::Commit(commit) => commit.guest().id(),
            GuestEffect::Undo(provisioned) => provisioned.id(),
            GuestEffect::Reclaim(doomed) | GuestEffect::Retire(doomed) => doomed.id(),
        }
    }

    #[must_use]
    pub fn instance(&self) -> Option<Instance> {
        match self {
            GuestEffect::Create { target, artifact, .. } => Some(Instance { id: target.id(), nix: artifact.nix().clone() }),
            GuestEffect::Start(guest)
            | GuestEffect::Stop(guest)
            | GuestEffect::Record { guest, .. }
            | GuestEffect::Role { guest, .. }
            | GuestEffect::Update { guest, .. } => Some(guest.instance()),
            GuestEffect::Commit(commit) => Some(commit.guest().instance()),
            GuestEffect::Reclaim(doomed) | GuestEffect::Retire(doomed) => Some(doomed.instance()),
            GuestEffect::Undo(_) => None,
        }
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Check {
    Address,
    Port,
    Guest,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeEffect {
    ReadAddress(Member),
    PortOpen { guest: Member, address: Ipv4Addr, port: Port },
    GuestCheck(Member),
}

#[pure_only]
impl ProbeEffect {
    #[must_use]
    pub fn check(&self) -> Check {
        match self {
            ProbeEffect::ReadAddress(_) => Check::Address,
            ProbeEffect::PortOpen { .. } => Check::Port,
            ProbeEffect::GuestCheck(_) => Check::Guest,
        }
    }

    #[must_use]
    pub fn guest(&self) -> &Member {
        match self {
            ProbeEffect::ReadAddress(guest) | ProbeEffect::PortOpen { guest, .. } | ProbeEffect::GuestCheck(guest) => guest,
        }
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Endpoint {
    Primary,
    Replicas,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Backend {
    endpoint: Endpoint,
    generation: Generation,
    nix: NixHash,
    address: Ipv4Addr,
}

#[pure_only]
impl Backend {
    #[must_use]
    pub fn of(member: &Member, endpoint: Endpoint) -> Option<Backend> {
        member.generation().zip(member.tags().service_ip).map(|(generation, address)| Backend {
            endpoint,
            generation,
            nix: member.nix().clone(),
            address,
        })
    }

    #[must_use]
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    #[must_use]
    pub fn generation(&self) -> Generation {
        self.generation
    }

    #[must_use]
    pub fn nix(&self) -> &NixHash {
        &self.nix
    }

    #[must_use]
    pub fn address(&self) -> Ipv4Addr {
        self.address
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteEffect {
    Point { name: GuestName, proxy: ProxySpec, to: Backend, from: Option<Backend> },
    Restore { name: GuestName, proxy: ProxySpec, to: Backend },
    RemoveCluster(GuestName),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Guest(GuestEffect),
    Probe(ProbeEffect),
    Route(RouteEffect),
}
