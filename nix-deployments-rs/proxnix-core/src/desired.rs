#[pure_only]
use crate::cohort::Expendable;
#[pure_only]
use crate::ids::{SlotId, Vmid};
#[pure_only]
use crate::layout::StorageFault;
#[pure_only]
use crate::observation::{Managed, Observation};
#[pure_only]
use crate::spec::{GuestName, WorkloadSpec};
#[pure_only]
use crate::tick::Push;
use proxnix_pure::pure_only;
#[pure_only]
use std::collections::{BTreeMap, BTreeSet};
#[pure_only]
use std::net::Ipv4Addr;

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigFault {
    SameIdInBothSlots(Vmid),
    DuplicateName,
    SharedVmid { id: Vmid, with: GuestName },
    SharedServiceAddress { address: Ipv4Addr, with: GuestName },
    Storage(StorageFault),
}

#[pure_only]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum Scope {
    #[default]
    Everything,
    Only(GuestName),
    Teardown,
}

#[pure_only]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Desired {
    valid: BTreeMap<GuestName, WorkloadSpec>,
    invalid: BTreeMap<GuestName, Vec<ConfigFault>>,
    scope: Scope,
}

#[pure_only]
fn ids(spec: &WorkloadSpec) -> [Vmid; 2] {
    spec.slots.both().map(SlotId::inner)
}

#[pure_only]
fn faults(index: usize, spec: &WorkloadSpec, all: &[WorkloadSpec]) -> Vec<ConfigFault> {
    let others = || {
        all.iter()
            .enumerate()
            .filter(move |(other, _)| *other != index)
            .map(|(_, other)| other)
    };
    let duplicate = others()
        .any(|other| other.name == spec.name)
        .then_some(ConfigFault::DuplicateName);
    let shared_ids = others()
        .filter(|other| other.name != spec.name)
        .flat_map(|other| {
            ids(spec)
                .into_iter()
                .filter(|id| ids(other).contains(id))
                .map(|id| ConfigFault::SharedVmid {
                    id,
                    with: other.name.clone(),
                })
                .collect::<Vec<_>>()
        });
    let shared_address = others()
        .filter(|other| other.name != spec.name)
        .filter_map(|other| {
            spec.proxy
                .service_address
                .filter(|address| other.proxy.service_address == Some(*address))
                .map(|address| ConfigFault::SharedServiceAddress {
                    address,
                    with: other.name.clone(),
                })
        });
    duplicate
        .into_iter()
        .chain(shared_ids)
        .chain(shared_address)
        .collect()
}

#[pure_only]
impl Desired {
    #[must_use]
    pub fn validate(specs: Vec<WorkloadSpec>) -> Desired {
        Desired::validate_with(specs, Vec::new())
    }

    #[must_use]
    pub fn validate_with(
        specs: Vec<WorkloadSpec>,
        rejected: Vec<(GuestName, ConfigFault)>,
    ) -> Desired {
        let found: Vec<Vec<ConfigFault>> = specs
            .iter()
            .enumerate()
            .map(|(index, spec)| faults(index, spec, &specs))
            .collect();
        let checked: Vec<(WorkloadSpec, Vec<ConfigFault>)> = specs.into_iter().zip(found).collect();
        let invalid: BTreeMap<GuestName, Vec<ConfigFault>> = checked
            .iter()
            .filter(|(_, found)| !found.is_empty())
            .map(|(spec, found)| (spec.name.clone(), found.clone()))
            .chain(
                rejected
                    .into_iter()
                    .map(|(name, fault)| (name, vec![fault])),
            )
            .collect();
        Desired {
            valid: checked
                .into_iter()
                .filter(|(spec, found)| found.is_empty() && !invalid.contains_key(&spec.name))
                .map(|(spec, _)| (spec.name.clone(), spec))
                .collect(),
            invalid,
            scope: Scope::Everything,
        }
    }

    #[must_use]
    pub fn workload(&self, name: &GuestName) -> Desired {
        Desired {
            scope: Scope::Only(name.clone()),
            ..self.clone()
        }
    }

    #[must_use]
    pub fn teardown(&self) -> Desired {
        Desired {
            scope: Scope::Teardown,
            ..self.clone()
        }
    }

    #[must_use]
    pub fn names(&self) -> Vec<GuestName> {
        self.valid.keys().cloned().collect()
    }

    pub fn valid(&self) -> impl Iterator<Item = &WorkloadSpec> {
        self.valid.values().filter(|spec| match &self.scope {
            Scope::Everything => true,
            Scope::Only(name) => spec.name == *name,
            Scope::Teardown => false,
        })
    }

    pub fn reported(&self) -> impl Iterator<Item = (&GuestName, &Vec<ConfigFault>)> {
        self.invalid
            .iter()
            .filter(|_| !matches!(self.scope, Scope::Only(_)))
    }

    #[must_use]
    pub fn invalid(&self) -> &BTreeMap<GuestName, Vec<ConfigFault>> {
        &self.invalid
    }

    #[must_use]
    pub fn declares(&self, name: &GuestName) -> bool {
        self.valid.contains_key(name) || self.invalid.contains_key(name)
    }

    #[must_use]
    pub fn orphans(&self, _push: &Push, observed: &Observation) -> Vec<Orphan> {
        if matches!(self.scope, Scope::Only(_)) {
            return Vec::new();
        }
        let names: BTreeSet<GuestName> = observed
            .managed()
            .iter()
            .map(|managed| managed.guest().name().clone())
            .filter(|name| !self.declares(name))
            .collect();
        names
            .into_iter()
            .map(|name| Orphan {
                guests: observed.managed_named(&name),
                name,
            })
            .collect()
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orphan {
    name: GuestName,
    guests: Vec<Managed>,
}

#[pure_only]
impl Orphan {
    #[must_use]
    pub fn name(&self) -> &GuestName {
        &self.name
    }

    #[must_use]
    pub fn expendable(&self) -> Vec<Expendable> {
        self.guests
            .iter()
            .cloned()
            .map(Expendable::orphaned)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest::{Cores, DiskGib, DurationMs, MemoryMb, Port, Resources, Sockets};
    use crate::ids::SlotPair;
    use crate::spec::{
        BridgeName, Cutover, Hostname, ImageType, KindSpec, ProxySpec, Purity, Timeouts,
    };

    fn spec(name: &str, blue: u32, green: u32, address: Option<Ipv4Addr>) -> WorkloadSpec {
        WorkloadSpec {
            name: GuestName(String::from(name)),
            slots: SlotPair::new(Vmid::new(blue), Vmid::new(green)).unwrap(),
            image: ImageType(String::from(name)),
            resources: Resources {
                memory: MemoryMb(1),
                disk: DiskGib(1),
                cores: Cores(1),
            },
            cutover: Cutover::Overlap,
            purity: Purity::Pure,
            proxy: ProxySpec {
                hostname: Hostname(String::from(name)),
                service_address: address,
                backend_port: Port(80),
                tcp_ports: vec![],
                bridge: BridgeName(String::from("vmbr0")),
            },
            timeouts: Timeouts {
                dhcp: DurationMs(1),
                health_check: DurationMs(1),
            },
            kind: KindSpec::Qemu {
                sockets: Sockets(1),
            },
        }
    }

    #[test]
    fn workloads_that_share_an_id_or_address_are_invalid_but_still_declared() {
        let desired = Desired::validate(vec![
            spec("website", 823, 923, Some(Ipv4Addr::new(192, 168, 1, 23))),
            spec("clash", 923, 999, None),
            spec("copycat", 700, 701, Some(Ipv4Addr::new(192, 168, 1, 23))),
            spec("fine", 850, 950, None),
        ]);
        assert_eq!(
            desired
                .valid()
                .map(|spec| spec.name.0.as_str())
                .collect::<Vec<_>>(),
            vec!["fine"]
        );
        assert_eq!(
            desired.invalid()[&GuestName(String::from("clash"))],
            vec![ConfigFault::SharedVmid {
                id: Vmid::new(923),
                with: GuestName(String::from("website"))
            }]
        );
        for name in ["website", "clash", "copycat", "fine"] {
            assert!(desired.declares(&GuestName(String::from(name))));
        }
    }

    #[test]
    fn a_name_declared_twice_is_invalid() {
        let desired = Desired::validate(vec![
            spec("website", 823, 923, None),
            spec("website", 824, 924, None),
        ]);
        assert_eq!(desired.valid().count(), 0);
        assert!(
            desired.invalid()[&GuestName(String::from("website"))]
                .contains(&ConfigFault::DuplicateName)
        );
    }
}
