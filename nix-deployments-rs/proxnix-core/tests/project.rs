mod sim;

use proxnix_core::{Builtin, Cutover, Desired, Effect, Images, Scope, Slot, Tick, project};
use sim::{COMMIT_B, NIX_A, NIX_B, World, built, images, kinds, lxc, pacing, push, qemu, run, spec};

fn projected_kinds(projection: &proxnix_core::Projection) -> Vec<sim::Kind> {
    kinds(&projection.effects)
}

fn without_addresses(effects: &[Effect]) -> Vec<sim::Kind> {
    kinds(effects)
}

#[test]
fn the_plan_predicts_exactly_what_a_clean_run_does() {
    for (kind, cutover) in [
        (qemu(), Cutover::Overlap),
        (lxc(), Cutover::Overlap),
        (lxc(), Cutover::StopStart),
        (lxc(), Cutover::Protected),
    ] {
        let workload = spec("forgejo", 844, 944, kind, cutover, true);
        let desired = Desired::validate(vec![workload.clone()]);
        let images = images(vec![built(&workload, NIX_B)]);
        let world = World::default().legacy(&workload, Slot::Blue, NIX_A);
        let plan = project::<Builtin>(&desired, &images, &world.observe(), &push(COMMIT_B), &pacing());
        let real = run::<Builtin>(world, &desired.workload(&workload.name), &images, &push(COMMIT_B), None);
        let planned = plan.iter().find(|projection| projection.scope == Scope::Workload(workload.name.clone())).unwrap();
        assert!(planned.settled);
        assert_eq!(projected_kinds(planned), without_addresses(&real.effects), "{cutover:?}");
        assert!(plan.iter().find(|projection| projection.scope == Scope::Teardown).unwrap().effects.is_empty());
    }
}

#[test]
fn the_plan_shows_orphans_only_in_the_teardown_scope() {
    let workload = spec("forgejo", 844, 944, lxc(), Cutover::Overlap, true);
    let world = World::default().legacy(&workload, Slot::Blue, NIX_A);
    let plan = project::<Builtin>(&Desired::default(), &Images::default(), &world.observe(), &push(COMMIT_B), &pacing());
    assert_eq!(plan.len(), 1);
    assert_eq!(kinds(&plan[0].effects), vec![sim::Kind::RemoveCluster, sim::Kind::Retire]);
    let periodic = project::<Builtin>(&Desired::default(), &Images::default(), &world.observe(), &Tick::Periodic, &pacing());
    assert!(periodic[0].effects.is_empty());
}

#[test]
fn planning_writes_nothing_so_the_observation_it_was_given_is_untouched() {
    let workload = spec("forgejo", 844, 944, lxc(), Cutover::Overlap, true);
    let world = World::default().legacy(&workload, Slot::Blue, NIX_A);
    let observed = world.observe();
    let before = observed.clone();
    let _ = project::<Builtin>(&Desired::validate(vec![workload.clone()]), &images(vec![built(&workload, NIX_B)]), &observed, &push(COMMIT_B), &pacing());
    assert_eq!(observed, before);
}
