mod sim;

use proptest::prelude::*;
use proxnix_core::{Builtin, Cutover, Desired, Images, KindSpec, Slot, Tick, Vmid, WorkloadSpec};
use sim::{COMMIT_A, COMMIT_B, Faults, Kind, NIX_A, NIX_B, Run, SimGuest, SimTags, World, built, images, kinds, lxc, push, qemu, run, settled, spec};
use std::collections::{BTreeMap, BTreeSet};

fn scenario(kind: KindSpec, cutover: Cutover) -> (WorkloadSpec, World) {
    let workload = spec("forgejo", 844, 944, kind, cutover, true);
    let world = World::default().legacy(&workload, Slot::Blue, NIX_A);
    (workload, world)
}

fn settle(done: Run, desired: &Desired) -> World {
    let calm = World { faults: Faults { fail: BTreeSet::new(), ..done.world.faults.clone() }, ..done.world };
    let first = run::<Builtin>(calm, desired, &Images::default(), &Tick::Periodic, None);
    let second = run::<Builtin>(first.world, desired, &Images::default(), &Tick::Periodic, None);
    assert!(
        kinds(&second.effects).iter().all(|kind| *kind == Kind::Restore),
        "the periodic loop did not converge: {:?}",
        kinds(&second.effects)
    );
    second.world
}

fn cases() -> Vec<(KindSpec, Cutover)> {
    [qemu(), lxc()]
        .into_iter()
        .flat_map(|kind| [Cutover::Overlap, Cutover::StopStart].map(|cutover| (kind.clone(), cutover)))
        .collect()
}

#[test]
fn every_single_injected_failure_still_leaves_one_serving_routed_guest() {
    for (kind, cutover) in cases() {
        let (workload, world) = scenario(kind, cutover);
        let desired = Desired::validate(vec![workload.clone()]);
        let images = images(vec![built(&workload, NIX_B)]);
        let clean = run::<Builtin>(world.clone(), &desired, &images, &push(COMMIT_B), None);
        for index in 0..=clean.effects.len() {
            let faulty = World { faults: Faults { fail: [index].into(), ..Faults::default() }, ..world.clone() };
            let pushed = run::<Builtin>(faulty, &desired, &images, &push(COMMIT_B), None);
            let world = settle(pushed, &desired);
            settled(&world, &desired);
            assert_eq!(world.members("forgejo").len(), 1, "failing effect {index} ({cutover:?}) left extra guests");
        }
    }
}

#[test]
fn a_crash_at_any_step_recovers_without_harming_the_serving_guest() {
    for (kind, cutover) in cases() {
        let (workload, world) = scenario(kind, cutover);
        let desired = Desired::validate(vec![workload.clone()]);
        let images = images(vec![built(&workload, NIX_B)]);
        let clean = run::<Builtin>(world.clone(), &desired, &images, &push(COMMIT_B), None);
        for crash in 0..=clean.steps {
            let pushed = run::<Builtin>(world.clone(), &desired, &images, &push(COMMIT_B), Some(crash));
            let world = settle(pushed, &desired);
            settled(&world, &desired);
            assert_eq!(world.members("forgejo").len(), 1);
        }
    }
}

#[test]
fn a_crash_then_only_periodic_ticks_rolls_back_or_forward_but_never_loses_the_service() {
    for (kind, cutover) in cases() {
        let (workload, world) = scenario(kind, cutover);
        let desired = Desired::validate(vec![workload.clone()]);
        let images = images(vec![built(&workload, NIX_B)]);
        let clean = run::<Builtin>(world.clone(), &desired, &images, &push(COMMIT_B), None);
        for cut in 0..clean.effects.len() {
            let replayed = clean.effects[..cut].iter().fold(world.clone(), |world, effect| world.apply(effect).0);
            let world = settle(Run { world: replayed, effects: vec![], reports: vec![], steps: 0 }, &desired);
            settled(&world, &desired);
            assert_eq!(world.members("forgejo").len(), 1, "cut after {cut} effects");
        }
    }
}

const LXC: &str = include_str!("../../proxnix/fixtures/api/lxc.json");
const QEMU: &str = include_str!("../../proxnix/fixtures/api/qemu.json");

fn fixture(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap()
}

fn parsed(tags: &str) -> Option<SimTags> {
    let parts: BTreeMap<&str, &str> =
        tags.split(';').filter_map(|tag| tag.split_once('-')).collect();
    tags.split(';').any(|tag| tag == "proxnix").then(|| SimTags {
        nix: String::from(parts["nix"]),
        commit: String::from(parts["commit"]),
        slot: if parts["slot"] == "blue" { Slot::Blue } else { Slot::Green },
        ip: parts.get("ip").map(|ip| ip.parse().unwrap()),
        generation: parts.get("gen").map(|generation| generation.parse().unwrap()),
        role: None,
    })
}

fn pve01() -> (World, Vec<WorkloadSpec>) {
    let listed: Vec<(serde_json::Value, KindSpec)> = fixture(LXC)
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item.clone(), lxc()))
        .chain(fixture(QEMU).as_array().unwrap().iter().map(|item| (item.clone(), qemu())))
        .collect();
    let workloads: Vec<WorkloadSpec> = listed
        .iter()
        .filter_map(|(item, kind)| {
            let tags = parsed(item["tags"].as_str().unwrap_or(""))?;
            let id = u32::try_from(item["vmid"].as_u64().unwrap()).unwrap();
            let blue = if tags.slot == Slot::Blue { id } else { id - 100 };
            Some(spec(item["name"].as_str().unwrap(), blue, blue + 100, kind.clone(), Cutover::Overlap, true))
        })
        .collect();
    let world = listed.iter().fold(World::default(), |world, (item, kind)| {
        let id = u32::try_from(item["vmid"].as_u64().unwrap()).unwrap();
        let tags = parsed(item["tags"].as_str().unwrap_or(""));
        let managed = workloads.iter().find(|workload| workload.name.0 == item["name"].as_str().unwrap_or("") && tags.is_some());
        world.with_guest(
            Vmid::new(id),
            SimGuest {
                name: String::from(item["name"].as_str().unwrap_or("")),
                running: item["status"] == "running",
                resources: managed.map_or(proxnix_core::Resources { memory: proxnix_core::MemoryMb(1024), disk: proxnix_core::DiskGib(8), cores: proxnix_core::Cores(1) }, |workload| workload.resources),
                facts: sim::facts(kind),
                tags,
            },
        )
    });
    (world, workloads)
}

#[test]
fn the_incident_push_against_the_real_cluster_creates_nothing_into_an_occupied_slot_and_destroys_nothing_serving() {
    let (world, workloads) = pve01();
    assert_eq!(workloads.len(), 8);
    let unmanaged: BTreeMap<Vmid, SimGuest> =
        world.guests.iter().filter(|(_, guest)| guest.tags.is_none()).map(|(id, guest)| (*id, guest.clone())).collect();
    let desired = Desired::validate(workloads.clone());
    let deployed: BTreeMap<String, String> = world
        .guests
        .values()
        .filter_map(|guest| guest.tags.as_ref().map(|tags| (guest.name.clone(), tags.nix.clone())))
        .collect();
    let every_image_changed = images(workloads.iter().map(|workload| built(workload, if deployed[&workload.name.0] == NIX_B { NIX_A } else { NIX_B })).collect());
    let done = run::<Builtin>(world, &desired, &every_image_changed, &push(COMMIT_B), None);
    let after: BTreeMap<Vmid, SimGuest> =
        done.world.guests.iter().filter(|(_, guest)| guest.tags.is_none()).map(|(id, guest)| (*id, guest.clone())).collect();
    assert_eq!(after, unmanaged, "an unmanaged guest was touched");
    assert_eq!(kinds(&done.effects).iter().filter(|kind| **kind == Kind::Create).count(), 8);
    let world = settle(done, &desired);
    settled(&world, &desired);
    for workload in &workloads {
        assert_eq!(world.members(&workload.name.0).len(), 1);
        assert_eq!(world.serving(&workload.name.0).unwrap().1.tags.as_ref().unwrap().generation, Some(2));
    }
}

#[test]
fn the_incident_push_with_nothing_changed_only_adopts() {
    let (world, workloads) = pve01();
    let desired = Desired::validate(workloads.clone());
    let unchanged = images(
        workloads
            .iter()
            .map(|workload| {
                let nix = world.members(&workload.name.0)[0].1.tags.as_ref().unwrap().nix.clone();
                built(workload, &nix)
            })
            .collect(),
    );
    let done = run::<Builtin>(world, &desired, &unchanged, &push(COMMIT_A), None);
    assert_eq!(kinds(&done.effects), vec![Kind::Commit; 8]);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    #[test]
    fn the_invariants_hold_under_any_failure_schedule(
        fail in prop::collection::btree_set(0usize..40, 0..5),
        crash in prop::option::of(0usize..80),
        die_after in prop::option::of(0usize..40),
        unhealthy in any::<bool>(),
        silent in any::<bool>(),
        stop_start in any::<bool>(),
        container in any::<bool>(),
    ) {
        let (workload, world) = scenario(
            if container { lxc() } else { qemu() },
            if stop_start { Cutover::StopStart } else { Cutover::Overlap },
        );
        let desired = Desired::validate(vec![workload.clone()]);
        let faults = Faults {
            fail,
            unhealthy: if unhealthy { [Vmid::new(944)].into() } else { BTreeSet::new() },
            silent: if silent { [Vmid::new(944)].into() } else { BTreeSet::new() },
            die_after,
        };
        let pushed = run::<Builtin>(World { faults, ..world }, &desired, &images(vec![built(&workload, NIX_B)]), &push(COMMIT_B), crash);
        let world = settle(pushed, &desired);
        settled(&world, &desired);
        prop_assert_eq!(world.members("forgejo").len(), 1);
    }
}
