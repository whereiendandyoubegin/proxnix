mod sim;

use proxnix_core::{
    Context, Cutover, Desired, Effect, Endpoint, Fence, GuestEffect, Health, Member, PairPhase, Plan, Rebuilds, Registry,
    RoleName, Stage, Strategy, Vmid, abort, commit, drive, health, pair_phase, pair_plan, point,
};
use sim::{COMMIT_B, Faults, Kind, NIX_A, NIX_B, SimRoute, World, built, images, kinds, lxc, push, run, spec};

enum Promote {}

enum PromotePhase {
    Pair(PairPhase),
    Replicating { replica: Member },
    Promoted { writer: Member, reader: Member },
}

fn reader() -> RoleName {
    "reader".parse().unwrap()
}

impl Strategy for Promote {
    type Phase = PromotePhase;

    fn phase(ctx: &Context<'_>) -> PromotePhase {
        match pair_phase(ctx) {
            PairPhase::Deploying { serving: Some(_), fresh } => PromotePhase::Replicating { replica: fresh },
            PairPhase::Superseded { serving, loser } => PromotePhase::Promoted { writer: serving, reader: loser },
            other => PromotePhase::Pair(other),
        }
    }

    fn plan(ctx: &Context<'_>, phase: PromotePhase) -> Plan {
        match phase {
            PromotePhase::Pair(phase) => pair_plan(ctx, phase, Fence::Never, Rebuilds::Allowed),
            PromotePhase::Replicating { replica } => match health(ctx, &replica) {
                Health::Pending(plan) => plan,
                Health::Failed(failure) => abort(ctx, &replica, failure),
                Health::Healthy(_) => commit(ctx, &replica, Stage::Committing),
            },
            PromotePhase::Promoted { reader: demoted, .. } if demoted.role() != Some(&reader()) => {
                Plan::act(Stage::Demoting, Effect::Guest(GuestEffect::Role { guest: demoted, role: reader() }))
            }
            PromotePhase::Promoted { writer, reader } => point(ctx, &writer, Some(&reader), Endpoint::Primary)
                .or_else(|| point(ctx, &reader, None, Endpoint::Replicas))
                .unwrap_or_else(|| Plan::idle(Stage::Converged)),
        }
    }
}

enum OnlyPromote {}

impl Registry for OnlyPromote {
    fn plan(ctx: &Context<'_>) -> Plan {
        drive::<Promote>(ctx)
    }
}

#[test]
fn a_strategy_outside_the_crate_promotes_a_replica_and_keeps_the_old_writer_as_a_reader() {
    let postgres = spec("postgres", 842, 942, lxc(), Cutover::Overlap, true);
    let desired = Desired::validate(vec![postgres.clone()]);
    let world = World::default().legacy(&postgres, proxnix_core::Slot::Blue, NIX_A);
    let done = run::<OnlyPromote>(world, &desired, &images(vec![built(&postgres, NIX_B)]), &push(COMMIT_B), None);
    let seen = kinds(&done.effects);
    assert!(!seen.iter().any(|kind| matches!(kind, Kind::Retire | Kind::Reclaim | Kind::Undo)), "{seen:?}");
    assert_eq!(
        seen,
        vec![Kind::Commit, Kind::Create, Kind::Start, Kind::ReadAddress, Kind::Record, Kind::PortOpen, Kind::GuestCheck, Kind::Commit, Kind::Role, Kind::Point, Kind::Restore]
    );
    let writer = &done.world.guests[&Vmid::new(942)];
    let old = &done.world.guests[&Vmid::new(842)];
    assert_eq!(writer.tags.as_ref().unwrap().generation, Some(2));
    assert_eq!(old.tags.as_ref().unwrap().generation, Some(1));
    assert_eq!(old.tags.as_ref().unwrap().role.as_deref(), Some("reader"));
    assert!(writer.running && old.running);
    assert_eq!(
        done.world.routes.get(&(String::from("postgres"), Endpoint::Primary)),
        Some(&SimRoute { generation: 2, nix: String::from(NIX_B), address: writer.tags.as_ref().unwrap().ip.unwrap() })
    );
    assert_eq!(
        done.world.routes.get(&(String::from("postgres"), Endpoint::Replicas)),
        Some(&SimRoute { generation: 1, nix: String::from(NIX_A), address: old.tags.as_ref().unwrap().ip.unwrap() })
    );
}

#[test]
fn a_replica_that_never_becomes_healthy_is_undone_and_the_writer_is_untouched() {
    let postgres = spec("postgres", 842, 942, lxc(), Cutover::Overlap, true);
    let desired = Desired::validate(vec![postgres.clone()]);
    let world = World { faults: Faults { unhealthy: [Vmid::new(942)].into(), ..Faults::default() }, ..World::default() }
        .legacy(&postgres, proxnix_core::Slot::Blue, NIX_A);
    let done = run::<OnlyPromote>(world, &desired, &images(vec![built(&postgres, NIX_B)]), &push(COMMIT_B), None);
    assert_eq!(kinds(&done.effects).last(), Some(&Kind::Undo));
    assert_eq!(done.world.members("postgres").len(), 1);
    assert_eq!(done.world.serving("postgres").map(|(id, _)| id), Some(Vmid::new(842)));
}
