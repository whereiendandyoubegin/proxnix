mod sim;

use proxnix_core::{Builtin, Check, Cutover, Desired, Failure, Images, MemoryMb, Slot, Stage, Tick, Vmid, WorkloadSpec};
use sim::{
    COMMIT_B, Kind, NIX_A, NIX_B, World, built, images, kinds, last_stage, lxc, push, qemu, run, settled, spec,
};

fn website() -> WorkloadSpec {
    spec("website", 823, 923, qemu(), Cutover::Overlap, true)
}

fn forgejo(cutover: Cutover) -> WorkloadSpec {
    spec("forgejo", 844, 944, lxc(), cutover, true)
}

fn one(spec: &WorkloadSpec) -> Desired {
    Desired::validate(vec![spec.clone()])
}

fn without(kinds: &[Kind], dropped: &[Kind]) -> Vec<Kind> {
    kinds.iter().copied().filter(|kind| !dropped.contains(kind)).collect()
}

#[test]
fn a_rebuild_follows_the_order_the_old_deploy_used() {
    let website = website();
    let world = World::default().legacy(&website, Slot::Blue, NIX_A);
    let done = run::<Builtin>(world, &one(&website), &images(vec![built(&website, NIX_B)]), &push(COMMIT_B), None);
    let seen = kinds(&done.effects);
    assert_eq!(
        seen,
        vec![Kind::Commit, Kind::Create, Kind::Start, Kind::ReadAddress, Kind::Record, Kind::PortOpen, Kind::Commit, Kind::Point, Kind::Retire]
    );
    assert_eq!(
        without(&seen, &[Kind::Commit]),
        vec![Kind::Create, Kind::Start, Kind::ReadAddress, Kind::Record, Kind::PortOpen, Kind::Point, Kind::Retire],
        "without the adoption and the commit this must be exactly DeployContext's rebuild order"
    );
    let (id, serving) = done.world.serving("website").unwrap();
    assert_eq!(id, Vmid::new(923));
    assert_eq!(serving.tags.as_ref().unwrap().generation, Some(2));
    assert_eq!(serving.tags.as_ref().unwrap().nix, NIX_B);
    assert_eq!(done.world.members("website").len(), 1);
    settled(&done.world, &one(&website));
}

#[test]
fn a_container_rebuild_also_runs_the_guest_check() {
    let forgejo = forgejo(Cutover::Overlap);
    let world = World::default().legacy(&forgejo, Slot::Blue, NIX_A);
    let done = run::<Builtin>(world, &one(&forgejo), &images(vec![built(&forgejo, NIX_B)]), &push(COMMIT_B), None);
    assert_eq!(
        without(&kinds(&done.effects), &[Kind::Commit]),
        vec![Kind::Create, Kind::Start, Kind::ReadAddress, Kind::Record, Kind::PortOpen, Kind::GuestCheck, Kind::Point, Kind::Retire]
    );
    settled(&done.world, &one(&forgejo));
}

#[test]
fn a_first_deploy_goes_into_the_blue_slot_and_is_routed() {
    let forgejo = forgejo(Cutover::Overlap);
    let done = run::<Builtin>(World::default(), &one(&forgejo), &images(vec![built(&forgejo, NIX_A)]), &push(COMMIT_B), None);
    assert_eq!(
        kinds(&done.effects),
        vec![Kind::Create, Kind::Start, Kind::ReadAddress, Kind::Record, Kind::PortOpen, Kind::GuestCheck, Kind::Commit, Kind::Restore]
    );
    assert_eq!(done.world.serving("forgejo").map(|(id, _)| id), Some(Vmid::new(844)));
    settled(&done.world, &one(&forgejo));
}

#[test]
fn an_unchanged_workload_is_left_alone_on_a_push() {
    let website = website();
    let adopted = run::<Builtin>(World::default().legacy(&website, Slot::Blue, NIX_A), &one(&website), &Images::default(), &Tick::Periodic, None);
    let done = run::<Builtin>(adopted.world, &one(&website), &images(vec![built(&website, NIX_A)]), &push(COMMIT_B), None);
    assert!(done.effects.is_empty(), "{:?}", kinds(&done.effects));
    assert_eq!(last_stage(&done, "website"), Stage::Converged);
}

#[test]
fn a_resource_change_is_applied_in_place_then_checked() {
    let website = website();
    let bigger = WorkloadSpec { resources: proxnix_core::Resources { memory: MemoryMb(4096), ..website.resources }, ..website.clone() };
    let world = World::default().legacy(&website, Slot::Blue, NIX_A);
    let done = run::<Builtin>(world, &one(&bigger), &images(vec![built(&bigger, NIX_A)]), &push(COMMIT_B), None);
    assert_eq!(without(&kinds(&done.effects), &[Kind::Commit]), vec![Kind::Update, Kind::PortOpen]);
    assert_eq!(done.world.serving("website").unwrap().1.resources.memory, MemoryMb(4096));
}

#[test]
fn a_workload_dropped_from_the_config_is_torn_down_on_a_push_only() {
    let website = website();
    let world = World::default().legacy(&website, Slot::Blue, NIX_A);
    let periodic = run::<Builtin>(world.clone(), &Desired::default(), &Images::default(), &Tick::Periodic, None);
    assert!(periodic.effects.is_empty());
    let pushed = run::<Builtin>(world, &Desired::default(), &Images::default(), &push(COMMIT_B), None);
    assert_eq!(kinds(&pushed.effects), vec![Kind::RemoveCluster, Kind::Retire]);
    assert!(pushed.world.guests.is_empty());
}

#[test]
fn a_workload_with_a_config_error_is_reported_and_never_treated_as_an_orphan() {
    let website = website();
    let clash = spec("clash", 823, 999, qemu(), Cutover::Overlap, false);
    let desired = Desired::validate(vec![website.clone(), clash]);
    let world = World::default().legacy(&website, Slot::Blue, NIX_A);
    let done = run::<Builtin>(world, &desired, &images(vec![built(&website, NIX_B)]), &push(COMMIT_B), None);
    assert!(done.effects.is_empty(), "{:?}", kinds(&done.effects));
    assert!(matches!(last_stage(&done, "website"), Stage::Invalid(_)));
    assert_eq!(done.world.members("website").len(), 1);
}

#[test]
fn the_periodic_tick_starts_a_stopped_guest_then_restores_its_route() {
    let website = website();
    let adopted = run::<Builtin>(World::default().legacy(&website, Slot::Blue, NIX_A), &one(&website), &Images::default(), &Tick::Periodic, None);
    let stopped = World {
        guests: adopted.world.guests.into_iter().map(|(id, guest)| (id, sim::SimGuest { running: false, ..guest })).collect(),
        ..adopted.world
    };
    let done = run::<Builtin>(stopped, &one(&website), &Images::default(), &Tick::Periodic, None);
    assert_eq!(kinds(&done.effects), vec![Kind::Start, Kind::Restore]);
    settled(&done.world, &one(&website));
}

#[test]
fn an_unhealthy_new_guest_is_undone_and_the_old_one_keeps_serving() {
    let forgejo = forgejo(Cutover::Overlap);
    let world = World { faults: sim::Faults { unhealthy: [Vmid::new(944)].into(), ..sim::Faults::default() }, ..World::default() }
        .legacy(&forgejo, Slot::Blue, NIX_A);
    let done = run::<Builtin>(world, &one(&forgejo), &images(vec![built(&forgejo, NIX_B)]), &push(COMMIT_B), None);
    let seen = kinds(&done.effects);
    assert_eq!(seen.last(), Some(&Kind::Undo), "{seen:?}");
    assert!(!seen.contains(&Kind::Point) && !seen.contains(&Kind::Retire));
    assert_eq!(done.world.serving("forgejo").map(|(id, _)| id), Some(Vmid::new(844)));
    assert_eq!(done.world.members("forgejo").len(), 1);
    assert!(matches!(last_stage(&done, "forgejo"), Stage::Failed(Failure::Expired { check: Check::Port, .. })));
}

#[test]
fn stop_start_fences_the_old_guest_first_and_restarts_it_if_the_new_one_fails() {
    let forgejo = forgejo(Cutover::StopStart);
    let healthy = run::<Builtin>(World::default().legacy(&forgejo, Slot::Blue, NIX_A), &one(&forgejo), &images(vec![built(&forgejo, NIX_B)]), &push(COMMIT_B), None);
    assert_eq!(
        without(&kinds(&healthy.effects), &[Kind::Commit]),
        vec![Kind::Create, Kind::Stop, Kind::Start, Kind::ReadAddress, Kind::Record, Kind::PortOpen, Kind::GuestCheck, Kind::Point, Kind::Retire]
    );
    settled(&healthy.world, &one(&forgejo));

    let failing = World { faults: sim::Faults { unhealthy: [Vmid::new(944)].into(), ..sim::Faults::default() }, ..World::default() }
        .legacy(&forgejo, Slot::Blue, NIX_A);
    let rolled_back = run::<Builtin>(failing, &one(&forgejo), &images(vec![built(&forgejo, NIX_B)]), &push(COMMIT_B), None);
    let tail: Vec<Kind> = kinds(&rolled_back.effects).into_iter().rev().take(2).collect();
    assert_eq!(tail, vec![Kind::Start, Kind::Undo], "destroy the new guest, then restart the old one");
    assert!(rolled_back.world.serving("forgejo").unwrap().1.running);
    let next_tick = run::<Builtin>(rolled_back.world, &one(&forgejo), &Images::default(), &Tick::Periodic, None);
    assert_eq!(kinds(&next_tick.effects), vec![Kind::Restore]);
    settled(&next_tick.world, &one(&forgejo));
}

#[test]
fn a_protected_workload_is_adopted_and_kept_running_but_never_rebuilt() {
    let postgres = spec("postgres", 842, 942, lxc(), Cutover::Protected, false);
    let done = run::<Builtin>(World::default().legacy(&postgres, Slot::Blue, NIX_A), &one(&postgres), &images(vec![built(&postgres, NIX_B)]), &push(COMMIT_B), None);
    assert_eq!(kinds(&done.effects), vec![Kind::Commit]);
    assert!(matches!(last_stage(&done, "postgres"), Stage::Skipped(proxnix_core::SkipReason::Protected)));
}

#[test]
fn a_slot_held_by_someone_else_blocks_the_create_instead_of_colliding() {
    let forgejo = forgejo(Cutover::Overlap);
    let world = World::default().unmanaged(Vmid::new(844), "somebody-elses");
    let done = run::<Builtin>(world, &one(&forgejo), &images(vec![built(&forgejo, NIX_A)]), &push(COMMIT_B), None);
    assert!(done.effects.is_empty());
    assert_eq!(last_stage(&done, "forgejo"), Stage::Blocked(proxnix_core::Blocker::SlotTaken(Vmid::new(844))));
}

#[test]
fn a_committed_guest_that_dies_before_the_route_moves_is_brought_back() {
    let forgejo = forgejo(Cutover::StopStart);
    let healthy = run::<Builtin>(World::default().legacy(&forgejo, Slot::Blue, NIX_A), &one(&forgejo), &images(vec![built(&forgejo, NIX_B)]), &push(COMMIT_B), None);
    let committed = healthy.effects.iter().rposition(|effect| sim::kind(effect) == Kind::Commit).unwrap();
    let replayed = healthy.effects[..=committed].iter().fold(World::default().legacy(&forgejo, Slot::Blue, NIX_A), |world, effect| world.apply(effect).0);
    let crashed = World {
        guests: replayed.guests.into_iter().map(|(id, guest)| (id, sim::SimGuest { running: false, ..guest })).collect(),
        ..replayed
    };
    let next_tick = run::<Builtin>(crashed, &one(&forgejo), &Images::default(), &Tick::Periodic, None);
    assert_eq!(kinds(&next_tick.effects), vec![Kind::Start, Kind::Point, Kind::Retire]);
    assert_eq!(next_tick.world.serving("forgejo").map(|(id, _)| id), Some(Vmid::new(944)));
    assert_eq!(next_tick.world.members("forgejo").len(), 1);
    settled(&next_tick.world, &one(&forgejo));
}
