#[pure_only]
use crate::build::Knowledge;
#[pure_only]
use crate::effect::ResourceChange;
#[pure_only]
use crate::guest::KindFacts;
#[pure_only]
use crate::observation::Guest;
#[pure_only]
use crate::spec::{KindSpec, Purity, WorkloadSpec};
#[pure_only]
use crate::tags::NixHash;
use proxnix_pure::pure_only;

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildCause {
    DiskGrew,
    Image,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changes {
    pub rebuild: Vec<RebuildCause>,
    pub in_place: Vec<ResourceChange>,
}

#[pure_only]
impl Changes {
    #[must_use]
    pub fn none(&self) -> bool {
        self.rebuild.is_empty() && self.in_place.is_empty()
    }
}

#[pure_only]
fn image_changed(spec: &WorkloadSpec, deployed: &NixHash, image: Knowledge<'_>) -> bool {
    match image {
        Knowledge::Built(artifact) => artifact.nix() != deployed || spec.purity == Purity::Impure,
        Knowledge::Failed(_) | Knowledge::NotBuiltThisRun => false,
    }
}

#[pure_only]
fn sockets_change(spec: &WorkloadSpec, guest: &Guest) -> Option<ResourceChange> {
    match (&spec.kind, guest.facts()) {
        (KindSpec::Qemu { sockets }, KindFacts::Qemu { sockets: seen }) if sockets != seen => {
            Some(ResourceChange::Sockets(*sockets))
        }
        _ => None,
    }
}

#[pure_only]
#[must_use]
pub fn changes(spec: &WorkloadSpec, guest: &Guest, deployed: &NixHash, image: Knowledge<'_>) -> Changes {
    let seen = guest.resources();
    Changes {
        rebuild: [
            (spec.disk() > seen.disk, RebuildCause::DiskGrew),
            (image_changed(spec, deployed, image), RebuildCause::Image),
        ]
        .into_iter()
        .filter_map(|(changed, cause)| changed.then_some(cause))
        .collect(),
        in_place: [
            (spec.memory() != seen.memory).then_some(ResourceChange::Memory(spec.memory())),
            (spec.cores() != seen.cores).then_some(ResourceChange::Cores(spec.cores())),
            sockets_change(spec, guest),
        ]
        .into_iter()
        .flatten()
        .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::{Artifact, BuildFault};
    use crate::guest::{Cores, DiskGib, DurationMs, GuestStatus, MemoryMb, Port, Privilege, Resources, Sockets};
    use crate::ids::{SlotPair, Vmid};
    use crate::observation::Sighting;
    use crate::spec::{BridgeName, Cutover, GuestName, Hostname, ImageType, ProxySpec, Timeouts};
    use crate::tags::RawTags;

    const DEPLOYED: &str = "78s0iadvjz6s48aqvx4rw78lwrzkjzlw";
    const NEWER: &str = "i3d00236fdkfw1v9cmasajkjhzl8zi5j";

    fn spec(kind: KindSpec, resources: Resources) -> WorkloadSpec {
        WorkloadSpec {
            name: GuestName(String::from("website")),
            slots: SlotPair::new(Vmid::new(823), Vmid::new(923)).unwrap(),
            image: ImageType(String::from("build-qcow2-website")),
            resources,
            cutover: Cutover::Overlap,
            purity: Purity::Pure,
            proxy: ProxySpec {
                hostname: Hostname(String::from("thesta.rs")),
                service_address: None,
                backend_port: Port(80),
                tcp_ports: vec![],
                bridge: BridgeName(String::from("vmbr0")),
            },
            timeouts: Timeouts { dhcp: DurationMs(1), health_check: DurationMs(1) },
            kind,
        }
    }

    fn guest(facts: KindFacts, resources: Resources) -> Guest {
        Guest::from(Sighting {
            id: Vmid::new(823),
            name: GuestName(String::from("website")),
            status: GuestStatus::Running,
            tags: RawTags::from(String::new()),
            resources,
            facts,
        })
    }

    fn sized(memory: u32, disk: u32, cores: u16) -> Resources {
        Resources { memory: MemoryMb(memory), disk: DiskGib(disk), cores: Cores(cores) }
    }

    fn built(nix: &str) -> Artifact {
        Artifact { path: format!("/nix/store/{nix}-website").parse().unwrap() }
    }

    fn qemu(sockets: u8) -> (KindSpec, KindFacts) {
        (KindSpec::Qemu { sockets: Sockets(sockets) }, KindFacts::Qemu { sockets: Sockets(sockets) })
    }

    #[test]
    fn nothing_changes_when_the_image_and_resources_match() {
        let (kind, facts) = qemu(1);
        let artifact = built(DEPLOYED);
        let found = changes(&spec(kind, sized(2048, 10, 2)), &guest(facts, sized(2048, 10, 2)), &DEPLOYED.parse().unwrap(), Knowledge::Built(&artifact));
        assert!(found.none());
    }

    #[test]
    fn a_new_image_or_a_bigger_disk_needs_a_rebuild_and_resources_change_in_place() {
        let (kind, facts) = qemu(1);
        let artifact = built(NEWER);
        let found = changes(&spec(kind, sized(4096, 20, 4)), &guest(facts, sized(2048, 10, 2)), &DEPLOYED.parse().unwrap(), Knowledge::Built(&artifact));
        assert_eq!(found.rebuild, vec![RebuildCause::DiskGrew, RebuildCause::Image]);
        assert_eq!(found.in_place, vec![ResourceChange::Memory(MemoryMb(4096)), ResourceChange::Cores(Cores(4))]);
    }

    #[test]
    fn a_smaller_disk_is_not_a_change() {
        let (kind, facts) = qemu(1);
        assert!(changes(&spec(kind, sized(2048, 8, 2)), &guest(facts, sized(2048, 10, 2)), &DEPLOYED.parse().unwrap(), Knowledge::NotBuiltThisRun).none());
    }

    #[test]
    fn an_image_that_was_not_built_or_failed_is_not_a_change() {
        let (kind, facts) = qemu(1);
        let fault = BuildFault::TimedOut(DurationMs(1));
        assert!(changes(&spec(kind.clone(), sized(2048, 10, 2)), &guest(facts.clone(), sized(2048, 10, 2)), &DEPLOYED.parse().unwrap(), Knowledge::NotBuiltThisRun).none());
        assert!(changes(&spec(kind, sized(2048, 10, 2)), &guest(facts, sized(2048, 10, 2)), &DEPLOYED.parse().unwrap(), Knowledge::Failed(&fault)).none());
    }

    #[test]
    fn an_impure_workload_rebuilds_whenever_it_is_built() {
        let (kind, facts) = qemu(1);
        let artifact = built(DEPLOYED);
        let impure = WorkloadSpec { purity: Purity::Impure, ..spec(kind, sized(2048, 10, 2)) };
        assert_eq!(
            changes(&impure, &guest(facts, sized(2048, 10, 2)), &DEPLOYED.parse().unwrap(), Knowledge::Built(&artifact)).rebuild,
            vec![RebuildCause::Image]
        );
    }

    #[test]
    fn only_vms_compare_sockets() {
        let found = changes(
            &spec(KindSpec::Qemu { sockets: Sockets(2) }, sized(2048, 10, 2)),
            &guest(KindFacts::Qemu { sockets: Sockets(1) }, sized(2048, 10, 2)),
            &DEPLOYED.parse().unwrap(),
            Knowledge::NotBuiltThisRun,
        );
        assert_eq!(found.in_place, vec![ResourceChange::Sockets(Sockets(2))]);
        let container = changes(
            &spec(KindSpec::Lxc { privilege: Privilege::Unprivileged, mounts: vec![] }, sized(2048, 10, 2)),
            &guest(KindFacts::Lxc { privilege: Privilege::Unprivileged, mounts: vec![] }, sized(2048, 10, 2)),
            &DEPLOYED.parse().unwrap(),
            Knowledge::NotBuiltThisRun,
        );
        assert!(container.none());
    }
}
