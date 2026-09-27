#[pure_only]
use crate::build::Images;
#[pure_only]
use crate::cohort::Cohort;
#[pure_only]
use crate::desired::{Desired, Orphan};
#[pure_only]
use crate::effect::{Effect, EffectId, Event, GuestEffect, RouteEffect};
#[pure_only]
use crate::memo::{Action, Failure, Memo};
#[pure_only]
use crate::observation::{Anomaly, Observation};
#[pure_only]
use crate::pair::{Keep, Overlap, StopStart};
#[pure_only]
use crate::spec::{Cutover, GuestName, WorkloadSpec};
#[pure_only]
use crate::strategy::{Context, Intent, Plan, Registry, Stage, drive};
#[pure_only]
use crate::tick::{Moment, Pacing, Tick};
use proxnix_pure::pure_only;

#[pure_only]
pub struct Input<'a> {
    pub memo: Memo,
    pub desired: &'a Desired,
    pub images: &'a Images,
    pub observed: &'a Observation,
    pub events: Vec<Event>,
    pub now: Moment,
    pub tick: &'a Tick,
    pub pacing: &'a Pacing,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub id: EffectId,
    pub workload: GuestName,
    pub effect: Effect,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkloadReport {
    pub name: GuestName,
    pub stage: Stage,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub workloads: Vec<WorkloadReport>,
    pub anomalies: Vec<Anomaly>,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub memo: Memo,
    pub effects: Vec<Planned>,
    pub wake: Option<Moment>,
    pub report: Report,
}

#[pure_only]
impl Step {
    #[must_use]
    pub fn quiescent(&self) -> bool {
        self.effects.is_empty() && self.wake.is_none()
    }
}

#[pure_only]
pub enum Builtin {}

#[pure_only]
impl Registry for Builtin {
    fn plan(ctx: &Context<'_>) -> Plan {
        match ctx.spec.cutover {
            Cutover::Overlap => drive::<Overlap>(ctx),
            Cutover::StopStart => drive::<StopStart>(ctx),
            Cutover::Protected => drive::<Keep>(ctx),
        }
    }
}

#[pure_only]
fn workload<R: Registry>(spec: &WorkloadSpec, images: &Images, observed: &Observation, tick: &Tick, now: Moment, memo: &Memo) -> Plan {
    match Cohort::gather(observed, spec) {
        Err(fault) => Plan::idle(Stage::Conflict(fault)),
        Ok(cohort) => R::plan(&Context::new(spec, &cohort, images.knowledge(&spec.image), tick.push(), observed, now, memo)),
    }
}

#[pure_only]
fn orphaned(orphan: &Orphan, memo: &Memo) -> Plan {
    if !memo.cleared(orphan.name()) {
        return Plan::act(Stage::Orphaned, Effect::Route(RouteEffect::RemoveCluster(orphan.name().clone())));
    }
    let doomed = orphan.expendable();
    let refusal = doomed.iter().find_map(|guest| memo.failed(guest.id(), Action::Retire).cloned());
    let retirable: Vec<Intent> = doomed
        .into_iter()
        .filter(|guest| memo.failed(guest.id(), Action::Retire).is_none())
        .map(|guest| Intent::Act(Effect::Guest(GuestEffect::Retire(guest))))
        .collect();
    match (retirable.is_empty(), refusal) {
        (false, _) => Plan { stage: Stage::Retiring, intents: retirable },
        (true, Some(error)) => Plan::idle(Stage::Failed(Failure::Refused { action: Action::Retire, error })),
        (true, None) => Plan::idle(Stage::Converged),
    }
}

#[pure_only]
fn record(memo: Memo, name: &GuestName, stage: &Stage, intent: &Intent, now: Moment) -> (Memo, Option<Planned>) {
    let (effect, within) = match intent {
        Intent::Act(effect) => (effect.clone(), None),
        Intent::Probe { probe, within } => (Effect::Probe(probe.clone()), Some(*within)),
        Intent::Wake(_) => return (memo, None),
    };
    let marked = match (stage, memo.aborted(&effect)) {
        (Stage::Aborting(failure), Some(instance)) => {
            let reason = failure.clone();
            memo.give_up(name, instance.nix, reason)
        }
        _ => memo,
    };
    let (issued, id) = marked.issue(name, effect.clone(), within, now);
    (issued, Some(Planned { id, workload: name.clone(), effect }))
}

#[pure_only]
#[must_use]
pub fn step<R: Registry>(input: Input<'_>) -> Step {
    let Input { memo, desired, images, observed, events, now, tick, pacing } = input;
    let memo = memo.absorb(events, now, pacing);
    let orphans = tick.push().map(|push| desired.orphans(push, observed)).unwrap_or_default();
    let plans: Vec<(GuestName, Plan)> = desired
        .valid()
        .map(|spec| (spec.name.clone(), workload::<R>(spec, images, observed, tick, now, &memo)))
        .chain(desired.reported().map(|(name, faults)| (name.clone(), Plan::idle(Stage::Invalid(faults.clone())))))
        .chain(orphans.iter().map(|orphan| (orphan.name().clone(), orphaned(orphan, &memo))))
        .collect();
    let wake = plans
        .iter()
        .flat_map(|(_, plan)| plan.intents.iter())
        .filter_map(|intent| match intent {
            Intent::Wake(at) => Some(*at),
            _ => None,
        })
        .min();
    let (memo, effects) = plans.iter().fold((memo, Vec::new()), |(memo, effects), (name, plan)| {
        plan.intents.iter().fold((memo, effects), |(memo, effects), intent| {
            let (memo, planned) = record(memo, name, &plan.stage, intent, now);
            (memo, effects.into_iter().chain(planned).collect())
        })
    });
    Step {
        memo,
        effects,
        wake,
        report: Report {
            workloads: plans.into_iter().map(|(name, plan)| WorkloadReport { name, stage: plan.stage }).collect(),
            anomalies: observed.anomalies().to_vec(),
        },
    }
}
