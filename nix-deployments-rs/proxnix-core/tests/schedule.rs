mod sim;

use proxnix_core::{Builtin, Check, Cutover, Desired, DurationMs, Failure, Images, Slot, Stage, Timeouts, Vmid, WorkloadSpec};
use sim::{COMMIT_B, Faults, NIX_A, NIX_B, World, built, images, last_stage, lxc, push, run, spec};

fn scenario() -> (Desired, World, Images) {
    let patient = WorkloadSpec {
        timeouts: Timeouts { dhcp: DurationMs(240_000), health_check: DurationMs(600_000) },
        ..spec("slowcheck", 844, 944, lxc(), Cutover::Overlap, true)
    };
    let dhcp = spec("dhcp", 845, 945, lxc(), Cutover::Overlap, true);
    let world = World {
        faults: Faults {
            failing_checks: [Vmid::new(944)].into(),
            address_after: [(Vmid::new(945), 10)].into(),
            ..Faults::default()
        },
        costed: true,
        ..World::default()
    }
    .legacy(&patient, Slot::Blue, NIX_A)
    .legacy(&dhcp, Slot::Blue, NIX_A);
    let images = images(vec![built(&patient, NIX_B), built(&dhcp, NIX_B)]);
    (Desired::validate(vec![patient, dhcp]), world, images)
}

#[test]
fn one_loop_running_every_workload_serially_lets_a_slow_check_starve_another_deadline() {
    let (desired, world, images) = scenario();
    let joint = run::<Builtin>(world, &desired, &images, &push(COMMIT_B), None);
    assert!(
        matches!(last_stage(&joint, "dhcp"), Stage::Failed(Failure::Expired { check: Check::Address, .. })),
        "dhcp answers on its tenth read, two seconds apart, yet it expired: {:?}",
        last_stage(&joint, "dhcp")
    );
}

#[test]
fn an_independent_loop_per_workload_keeps_each_deadline_its_own() {
    let (desired, world, images) = scenario();
    let deployed = desired.names().iter().fold(world, |world, name| {
        run::<Builtin>(world, &desired.workload(name), &images, &push(COMMIT_B), None).world
    });
    let (id, serving) = deployed.serving("dhcp").unwrap();
    assert_eq!(id, Vmid::new(945));
    assert_eq!(serving.tags.as_ref().unwrap().nix, NIX_B);
    assert_eq!(deployed.serving("slowcheck").map(|(id, _)| id), Some(Vmid::new(844)));
    let teardown = run::<Builtin>(deployed, &desired.teardown(), &images, &push(COMMIT_B), None);
    assert!(teardown.effects.is_empty(), "a scoped teardown pass must not redeploy anything");
}

#[test]
fn a_workload_loop_never_tears_down_another_workload() {
    let (desired, world, images) = scenario();
    let only_dhcp = run::<Builtin>(world.clone(), &desired.workload(&proxnix_core::GuestName(String::from("dhcp"))), &images, &push(COMMIT_B), None);
    assert!(only_dhcp.world.members("slowcheck").iter().all(|(id, _)| world.guests.contains_key(id)));
    assert_eq!(only_dhcp.world.members("slowcheck").len(), 1);
    let emptied = run::<Builtin>(world, &Desired::default().workload(&proxnix_core::GuestName(String::from("dhcp"))), &Images::default(), &push(COMMIT_B), None);
    assert_eq!(emptied.world.members("slowcheck").len(), 1, "a scoped loop must not treat other names as orphans");
}
