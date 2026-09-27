use proxnix_core::{
    Audited, Cores, DiskGib, GuestEffect, GuestName, GuestStatus, Grant, KindFacts, MemoryMb, Observation, Occupant,
    Permissions, Privilege, RawTags, Resources, Sighting, SlotState, Vmid,
};

fn main() {
    let audited = Audited::try_from(Permissions { vm_audit: Grant::Granted }).unwrap();
    let observed = Observation::new(
        audited,
        vec![Sighting {
            id: Vmid::new(844),
            name: GuestName(String::from("forgejo")),
            status: GuestStatus::Running,
            tags: RawTags::from(String::from("commit-66d0ba6b605de2703e0fb7bbf58b922d5b36597e;nix-78s0iadvjz6s48aqvx4rw78lwrzkjzlw;proxnix;slot-blue")),
            resources: Resources { memory: MemoryMb(2048), disk: DiskGib(16), cores: Cores(2) },
            facts: KindFacts::Lxc { privilege: Privilege::Unprivileged, mounts: vec![] },
        }],
    );
    let retire = match observed.slot(Vmid::new(844)) {
        SlotState::Occupied(Occupant::Managed(managed)) => Some(GuestEffect::Retire(managed)),
        _ => None,
    };
    assert!(retire.is_some());
    assert!(matches!(observed.slot(Vmid::new(944)), SlotState::Vacant(_)));
}
