#[pure_only]
use crate::cohort::Member;
#[pure_only]
use crate::common::{Health, Rebuilds, abort, commit, dispose, health, point, serve, undeployed};
#[pure_only]
use crate::effect::{Effect, Endpoint, GuestEffect};
#[pure_only]
use crate::memo::Action;
#[pure_only]
use crate::strategy::{Blocker, Context, Plan, Stage, Strategy, refused};
use proxnix_pure::pure_only;

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairPhase {
    Absent,
    Legacy(Member),
    Serving(Member),
    Deploying { serving: Option<Member>, fresh: Member },
    Leftover { serving: Member, stale: Member },
    Superseded { serving: Member, loser: Member },
    Ambiguous,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fence {
    Never,
    BeforeBoot,
}

#[pure_only]
#[must_use]
pub fn pair_phase(ctx: &Context<'_>) -> PairPhase {
    match ctx.cohort.highest() {
        None => match ctx.cohort.members() {
            [] => PairPhase::Absent,
            [only] if only.pending() || ctx.in_flight(only) => PairPhase::Deploying { serving: None, fresh: only.clone() },
            [only] => PairPhase::Legacy(only.clone()),
            _ => PairPhase::Ambiguous,
        },
        Some(top) => match ctx.cohort.others(top).as_slice() {
            [] => PairPhase::Serving(top.clone()),
            [other] if other.generation().is_some() => PairPhase::Superseded { serving: top.clone(), loser: (*other).clone() },
            [other] if ctx.in_flight(other) => PairPhase::Deploying { serving: Some(top.clone()), fresh: (*other).clone() },
            [other] => PairPhase::Leftover { serving: top.clone(), stale: (*other).clone() },
            _ => PairPhase::Ambiguous,
        },
    }
}

#[pure_only]
fn fence(ctx: &Context<'_>, serving: &Member, fresh: &Member) -> Option<Plan> {
    serving.running().then(|| match refused(ctx, serving, &[Action::Stop]) {
        Some(failure) => abort(ctx, fresh, failure),
        None => Plan::act(Stage::Fencing, Effect::Guest(GuestEffect::Stop(serving.clone()))),
    })
}

#[pure_only]
fn deploying(ctx: &Context<'_>, serving: Option<&Member>, fresh: &Member, fencing: Fence) -> Plan {
    let fenced = match (fencing, serving) {
        (Fence::BeforeBoot, Some(serving)) => fence(ctx, serving, fresh),
        _ => None,
    };
    if let Some(failure) = refused(ctx, fresh, &[Action::Create]) {
        return abort(ctx, fresh, failure);
    }
    fenced.unwrap_or_else(|| match health(ctx, fresh) {
        Health::Pending(plan) => plan,
        Health::Failed(failure) => abort(ctx, fresh, failure),
        Health::Healthy(_) => commit(ctx, fresh, Stage::Committing),
    })
}

#[pure_only]
fn supersede(ctx: &Context<'_>, serving: &Member, loser: &Member) -> Plan {
    if !serving.running() {
        return match refused(ctx, serving, &[Action::Start]) {
            Some(failure) => Plan::idle(Stage::Failed(failure)),
            None => Plan::act(Stage::Starting, Effect::Guest(GuestEffect::Start(serving.clone()))),
        };
    }
    point(ctx, serving, Some(loser), Endpoint::Primary).unwrap_or_else(|| dispose(ctx, loser, Action::Retire))
}

#[pure_only]
#[must_use]
pub fn pair_plan(ctx: &Context<'_>, phase: PairPhase, fencing: Fence, rebuilds: Rebuilds) -> Plan {
    match phase {
        PairPhase::Absent => undeployed(ctx),
        PairPhase::Legacy(only) => commit(ctx, &only, Stage::Adopting),
        PairPhase::Serving(serving) => serve(ctx, &serving, rebuilds),
        PairPhase::Deploying { serving, fresh } => deploying(ctx, serving.as_ref(), &fresh, fencing),
        PairPhase::Leftover { stale, .. } => dispose(ctx, &stale, Action::Reclaim),
        PairPhase::Superseded { serving, loser } => supersede(ctx, &serving, &loser),
        PairPhase::Ambiguous => Plan::idle(Stage::Blocked(Blocker::Ambiguous)),
    }
}

#[pure_only]
pub enum Overlap {}

#[pure_only]
impl Strategy for Overlap {
    type Phase = PairPhase;

    fn phase(ctx: &Context<'_>) -> PairPhase {
        pair_phase(ctx)
    }

    fn plan(ctx: &Context<'_>, phase: PairPhase) -> Plan {
        pair_plan(ctx, phase, Fence::Never, Rebuilds::Allowed)
    }
}

#[pure_only]
pub enum StopStart {}

#[pure_only]
impl Strategy for StopStart {
    type Phase = PairPhase;

    fn phase(ctx: &Context<'_>) -> PairPhase {
        pair_phase(ctx)
    }

    fn plan(ctx: &Context<'_>, phase: PairPhase) -> Plan {
        pair_plan(ctx, phase, Fence::BeforeBoot, Rebuilds::Allowed)
    }
}

#[pure_only]
pub enum Keep {}

#[pure_only]
impl Strategy for Keep {
    type Phase = PairPhase;

    fn phase(ctx: &Context<'_>) -> PairPhase {
        pair_phase(ctx)
    }

    fn plan(ctx: &Context<'_>, phase: PairPhase) -> Plan {
        pair_plan(ctx, phase, Fence::Never, Rebuilds::Protected)
    }
}
