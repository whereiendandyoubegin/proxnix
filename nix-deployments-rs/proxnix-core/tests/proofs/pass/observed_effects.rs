use proxnix_core::{
    Audited, BridgeName, Cohort, Cores, Cutover, DiskGib, DurationMs, Expendable, GuestEffect, GuestName, GuestStatus,
    Grant, Hostname, ImageType, KindFacts, KindSpec, MemoryMb, Observation, Permissions, Port, Privilege, Promotion,
    ProxySpec, Purity, RawTags, Resources, Settled, Sighting, SlotPair, SlotState, Timeouts, Vmid, WorkloadSpec,
};

fn sighting(id: Vmid, tags: &str) -> Sighting {
    Sighting::Settled(Settled {
        id,
        name: GuestName(String::from("forgejo")),
        status: GuestStatus::Running,
        tags: RawTags::from(String::from(tags)),
        resources: Resources { memory: MemoryMb(2048), disk: DiskGib(16), cores: Cores(2) },
        facts: KindFacts::Lxc { privilege: Privilege::Unprivileged, mounts: vec![] },
    })
}

fn main() {
    let spec = WorkloadSpec {
        name: GuestName(String::from("forgejo")),
        slots: SlotPair::new(Vmid::new(844), Vmid::new(944)).unwrap(),
        image: ImageType(String::from("build-lxc-forgejo")),
        resources: Resources { memory: MemoryMb(2048), disk: DiskGib(16), cores: Cores(2) },
        cutover: Cutover::Overlap,
        purity: Purity::Pure,
        proxy: ProxySpec {
            hostname: Hostname(String::from("git.thesta.rs")),
            service_address: None,
            backend_port: Port(3000),
            tcp_ports: vec![],
            bridge: BridgeName(String::from("vmbr0")),
        },
        timeouts: Timeouts { dhcp: DurationMs(1), health_check: DurationMs(1) },
        kind: KindSpec::Lxc { privilege: Privilege::Unprivileged, mounts: vec![] },
    };
    let observed = Observation::new(
        Audited::try_from(Permissions { vm_audit: Grant::Granted }).unwrap(),
        vec![
            sighting(Vmid::new(844), "commit-66d0ba6b605de2703e0fb7bbf58b922d5b36597e;nix-78s0iadvjz6s48aqvx4rw78lwrzkjzlw;proxnix;slot-blue;gen-2"),
            sighting(Vmid::new(944), "commit-66d0ba6b605de2703e0fb7bbf58b922d5b36597e;nix-i3d00236fdkfw1v9cmasajkjhzl8zi5j;proxnix;slot-green;gen-1"),
        ],
    );
    let cohort = Cohort::gather(&observed, &spec).unwrap();
    let serving = cohort.highest().unwrap();
    let loser = cohort.others(serving)[0];
    assert!(Expendable::outranked(&cohort, serving).is_none());
    let retire = Expendable::outranked(&cohort, loser).map(GuestEffect::Retire);
    assert!(retire.is_some());
    assert_eq!(Promotion::over(&cohort, loser).map(|promotion| promotion.generation().get()), Some(3));
    assert!(matches!(observed.slot(Vmid::new(845)), SlotState::Vacant(_)));
}
