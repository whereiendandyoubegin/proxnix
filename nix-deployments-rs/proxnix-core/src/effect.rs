#[pure_only]
use crate::guest::{Cores, DurationMs, GuestKind, MemoryMb, Port, Sockets};
#[pure_only]
use crate::ids::Vmid;
#[pure_only]
use crate::observation::{Managed, Vacant};
#[pure_only]
use crate::spec::{GuestName, ProxySpec, WorkloadSpec};
#[pure_only]
use crate::tags::{ManagedTags, NixHash};
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owned {
    Existing(Managed),
    New(Provisioned),
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
    Create { target: Vacant, spec: Box<WorkloadSpec>, tags: ManagedTags },
    Reclaim(Managed),
    Start(Owned),
    Tag { guest: Provisioned, tags: ManagedTags },
    Update { guest: Managed, changes: Vec<ResourceChange> },
    Undo(Provisioned),
    Retire(Managed),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeEffect {
    AwaitAddress { guest: Owned, within: DurationMs },
    PortOpen { address: Ipv4Addr, port: Port, within: DurationMs },
    GuestCheck { guest: Provisioned, within: DurationMs },
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backend {
    pub nix: NixHash,
    pub address: Ipv4Addr,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteEffect {
    Cutover { name: GuestName, proxy: ProxySpec, to: Backend, from: Option<Backend> },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest::{DiskGib, Privilege, Resources};
    use crate::ids::{Slot, SlotPair};
    use crate::observation::{Audited, Grant, Observation, Permissions, SlotState};
    use crate::spec::{BridgeName, Hostname, ImageType, KindSpec, Protection, Timeouts};

    fn spec() -> WorkloadSpec {
        WorkloadSpec {
            name: GuestName(String::from("forgejo")),
            slots: SlotPair::new(Vmid::new(844), Vmid::new(944)).unwrap(),
            image: ImageType(String::from("build-lxc-forgejo")),
            resources: Resources { memory: MemoryMb(2048), disk: DiskGib(16), cores: Cores(2) },
            protection: Protection::Unprotected,
            proxy: ProxySpec {
                hostname: Hostname(String::from("git.thesta.rs")),
                service_address: None,
                backend_port: Port(3000),
                tcp_ports: vec![],
                bridge: BridgeName(String::from("vmbr0")),
            },
            timeouts: Timeouts { dhcp: DurationMs(240_000), health_check: DurationMs(180_000) },
            kind: KindSpec::Lxc { privilege: Privilege::Unprivileged, mounts: vec![] },
        }
    }

    fn tags() -> ManagedTags {
        ManagedTags {
            nix: "78s0iadvjz6s48aqvx4rw78lwrzkjzlw".parse().unwrap(),
            commit: "66d0ba6b605de2703e0fb7bbf58b922d5b36597e".parse().unwrap(),
            slot: Slot::Blue,
            service_ip: None,
        }
    }

    fn create_into_vacant_944() -> GuestEffect {
        let observed = Observation::new(Audited::try_from(Permissions { vm_audit: Grant::Granted }).unwrap(), vec![]);
        match observed.slot(Vmid::new(944)) {
            SlotState::Vacant(target) => GuestEffect::Create { target, spec: Box::new(spec()), tags: tags() },
            SlotState::Occupied(_) => panic!("944 is vacant in an empty observation"),
        }
    }

    #[test]
    fn a_successful_create_proves_what_was_provisioned() {
        assert_eq!(
            Provisioned::confirmed(&create_into_vacant_944(), &Outcome::Done).map(|p| (p.id(), p.kind())),
            Some((Vmid::new(944), GuestKind::Lxc))
        );
    }

    #[test]
    fn a_failed_create_proves_nothing() {
        let refused = Outcome::Failed(EffectError::Refused(Detail(String::from("VM 944 already exists"))));
        assert_eq!(Provisioned::confirmed(&create_into_vacant_944(), &refused), None);
        assert_eq!(Provisioned::confirmed(&create_into_vacant_944(), &Outcome::AlreadyApplied), None);
    }
}
