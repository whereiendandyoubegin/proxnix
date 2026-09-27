#[pure_only]
use crate::guest::{Cores, DiskGib, DurationMs, GuestKind, MemoryMb, Mount, Port, Privilege, Resources, Sockets};
#[pure_only]
use crate::ids::SlotPair;
use proxnix_pure::pure_only;
#[pure_only]
use std::net::Ipv4Addr;

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GuestName(pub String);

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hostname(pub String);

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageType(pub String);

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BridgeName(pub String);

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    Protected,
    Unprotected,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxySpec {
    pub hostname: Hostname,
    pub service_address: Option<Ipv4Addr>,
    pub backend_port: Port,
    pub tcp_ports: Vec<Port>,
    pub bridge: BridgeName,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub dhcp: DurationMs,
    pub health_check: DurationMs,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KindSpec {
    Qemu { sockets: Sockets },
    Lxc { privilege: Privilege, mounts: Vec<Mount> },
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkloadSpec {
    pub name: GuestName,
    pub slots: SlotPair,
    pub image: ImageType,
    pub resources: Resources,
    pub protection: Protection,
    pub proxy: ProxySpec,
    pub timeouts: Timeouts,
    pub kind: KindSpec,
}

#[pure_only]
impl WorkloadSpec {
    #[must_use]
    pub fn memory(&self) -> MemoryMb {
        self.resources.memory
    }

    #[must_use]
    pub fn disk(&self) -> DiskGib {
        self.resources.disk
    }

    #[must_use]
    pub fn cores(&self) -> Cores {
        self.resources.cores
    }

    #[must_use]
    pub fn kind(&self) -> GuestKind {
        match self.kind {
            KindSpec::Qemu { .. } => GuestKind::Qemu,
            KindSpec::Lxc { .. } => GuestKind::Lxc,
        }
    }
}
