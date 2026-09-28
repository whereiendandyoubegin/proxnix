mod sim;

use proxnix_core::{
    Blocker, Builtin, Check, Cutover, Desired, Failure, Images, LockKind, MemoryMb, Slot, Stage, Tick, Unsettled, Vmid, WorkloadSpec,
};
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

#[test]
fn a_guest_proxmox_is_still_creating_blocks_only_its_own_slot() {
    let forgejo = forgejo(Cutover::Overlap);
    let website = website();
    let desired = Desired::validate(vec![forgejo.clone(), website.clone()]);
    let world = World::default()
        .legacy(&website, Slot::Blue, NIX_A)
        .half_made(Vmid::new(844), Unsettled::Locked(LockKind::Create));
    let done = run::<Builtin>(world, &desired, &images(vec![built(&forgejo, NIX_B), built(&website, NIX_B)]), &push(COMMIT_B), None);
    assert_eq!(done.world.serving("website").map(|(id, _)| id), Some(Vmid::new(923)));
    settled(&done.world, &one(&website));
    assert!(done.world.members("forgejo").is_empty());
    assert_eq!(done.world.unsettled.get(&Vmid::new(844)), Some(&Unsettled::Locked(LockKind::Create)));
    assert_eq!(
        last_stage(&done, "forgejo"),
        Stage::Blocked(Blocker::Unsettled(Vmid::new(844), Unsettled::Locked(LockKind::Create)))
    );
}

#[test]
fn a_half_written_guest_beside_a_serving_one_is_never_created_over_or_destroyed() {
    let forgejo = forgejo(Cutover::Overlap);
    let world = World::default().legacy(&forgejo, Slot::Blue, NIX_A).half_made(Vmid::new(944), Unsettled::Incomplete);
    let pushed = run::<Builtin>(world, &one(&forgejo), &images(vec![built(&forgejo, NIX_B)]), &push(COMMIT_B), None);
    assert!(!kinds(&pushed.effects).contains(&Kind::Create), "{:?}", kinds(&pushed.effects));
    assert_eq!(last_stage(&pushed, "forgejo"), Stage::Blocked(Blocker::Unsettled(Vmid::new(944), Unsettled::Incomplete)));
    assert!(pushed.effects.is_empty(), "{:?}", kinds(&pushed.effects));
    let untouched: Vec<(Vmid, bool)> = pushed.world.members("forgejo").into_iter().map(|(id, guest)| (id, guest.running)).collect();
    assert_eq!(untouched, vec![(Vmid::new(844), true)]);
    assert_eq!(pushed.world.unsettled.get(&Vmid::new(944)), Some(&Unsettled::Incomplete));
    let periodic = run::<Builtin>(pushed.world, &one(&forgejo), &Images::default(), &Tick::Periodic, None);
    assert!(periodic.effects.is_empty(), "{:?}", kinds(&periodic.effects));
    assert_eq!(last_stage(&periodic, "forgejo"), Stage::Blocked(Blocker::Unsettled(Vmid::new(944), Unsettled::Incomplete)));
}

#[test]
fn an_unreadable_guest_is_never_torn_down_as_an_orphan() {
    let world = World::default().half_made(Vmid::new(823), Unsettled::Unreadable);
    let done = run::<Builtin>(world, &Desired::default(), &Images::default(), &push(COMMIT_B), None);
    assert!(done.effects.is_empty(), "{:?}", kinds(&done.effects));
    assert_eq!(done.world.unsettled.get(&Vmid::new(823)), Some(&Unsettled::Unreadable));
}

#[test]
fn a_pair_with_an_unreadable_half_is_frozen_whole_so_nothing_is_created_or_rerouted() {
    let forgejo = forgejo(Cutover::StopStart);
    let world = World::default().legacy(&forgejo, Slot::Green, NIX_A);
    let adopted = run::<Builtin>(world, &one(&forgejo), &Images::default(), &Tick::Periodic, None);
    let blipped = World { guests: std::collections::BTreeMap::new(), ..adopted.world.clone() }
        .half_made(Vmid::new(944), Unsettled::Unreadable);
    let pushed = run::<Builtin>(blipped, &one(&forgejo), &images(vec![built(&forgejo, NIX_B)]), &push(COMMIT_B), None);
    assert!(pushed.effects.is_empty(), "{:?}", kinds(&pushed.effects));
    assert_eq!(last_stage(&pushed, "forgejo"), Stage::Blocked(Blocker::Unsettled(Vmid::new(944), Unsettled::Unreadable)));
}

fn forgejo_at(host: &str) -> WorkloadSpec {
    let state = proxnix_core::Mount {
        host: proxnix_core::HostPath::try_from(host).unwrap(),
        guest: proxnix_core::GuestPath(String::from("/var/lib/forgejo")),
        mode: proxnix_core::MountMode::ReadWrite,
    };
    WorkloadSpec {
        kind: proxnix_core::KindSpec::Lxc { privilege: proxnix_core::Privilege::Unprivileged, mounts: vec![state] },
        ..forgejo(Cutover::Overlap)
    }
}

#[test]
fn a_rebuild_waits_until_the_serving_guests_state_has_been_moved_to_where_the_spec_says() {
    let old = forgejo_at("/var/lib/proxnix/forgejo");
    let new = forgejo_at("/ZFS/proxnix/state/forgejo/forgejo");
    let world = World::default().legacy(&old, Slot::Blue, NIX_A);
    let early = run::<Builtin>(world.clone(), &one(&new), &images(vec![built(&new, NIX_B)]), &push(COMMIT_B), None);
    assert!(early.effects.is_empty(), "{:?}", kinds(&early.effects));
    assert_eq!(
        last_stage(&early, "forgejo"),
        Stage::Blocked(Blocker::StateMoved {
            at: proxnix_core::GuestPath(String::from("/var/lib/forgejo")),
            from: proxnix_core::HostPath::try_from("/var/lib/proxnix/forgejo").unwrap(),
            to: proxnix_core::HostPath::try_from("/ZFS/proxnix/state/forgejo/forgejo").unwrap(),
        })
    );
    let migrated = World::default().legacy(&new, Slot::Blue, NIX_A);
    let rebuilt = run::<Builtin>(migrated, &one(&new), &images(vec![built(&new, NIX_B)]), &push(COMMIT_B), None);
    assert!(kinds(&rebuilt.effects).contains(&Kind::Create));
    settled(&rebuilt.world, &one(&new));
}

#[test]
fn a_transient_api_failure_is_retried_instead_of_ending_the_deploy() {
    let forgejo = forgejo(Cutover::StopStart);
    let world = World::default().legacy(&forgejo, Slot::Blue, NIX_A);
    let clean = run::<Builtin>(world.clone(), &one(&forgejo), &images(vec![built(&forgejo, NIX_B)]), &push(COMMIT_B), None);
    let stop = clean.effects.iter().position(|effect| sim::kind(effect) == Kind::Stop).unwrap();
    let blip = World { faults: sim::Faults { flaky: [stop].into(), ..sim::Faults::default() }, ..world };
    let done = run::<Builtin>(blip, &one(&forgejo), &images(vec![built(&forgejo, NIX_B)]), &push(COMMIT_B), None);
    assert_eq!(done.effects.iter().filter(|effect| sim::kind(effect) == Kind::Stop).count(), 2);
    assert_eq!(done.world.serving("forgejo").map(|(id, _)| id), Some(Vmid::new(944)));
    settled(&done.world, &one(&forgejo));
}

#[test]
fn an_api_that_stays_unreachable_is_given_up_on_after_three_attempts() {
    let forgejo = forgejo(Cutover::StopStart);
    let world = World::default().legacy(&forgejo, Slot::Blue, NIX_A);
    let clean = run::<Builtin>(world.clone(), &one(&forgejo), &images(vec![built(&forgejo, NIX_B)]), &push(COMMIT_B), None);
    let stop = clean.effects.iter().position(|effect| sim::kind(effect) == Kind::Stop).unwrap();
    let down = World { faults: sim::Faults { flaky: (stop..stop + 3).collect(), ..sim::Faults::default() }, ..world };
    let done = run::<Builtin>(down, &one(&forgejo), &images(vec![built(&forgejo, NIX_B)]), &push(COMMIT_B), None);
    assert_eq!(done.effects.iter().filter(|effect| sim::kind(effect) == Kind::Stop).count(), 3);
    assert!(!done.effects.iter().any(|effect| matches!(effect, proxnix_core::Effect::Guest(proxnix_core::GuestEffect::Start(member)) if member.id() == Vmid::new(944))));
    assert!(done.world.guests.get(&Vmid::new(844)).is_some_and(|guest| guest.running));
}
