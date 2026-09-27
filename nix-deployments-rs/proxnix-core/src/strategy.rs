#[pure_only]
use crate::build::{BuildFault, Knowledge};
#[pure_only]
use crate::cohort::{Cohort, CohortFault, Member};
#[pure_only]
use crate::desired::ConfigFault;
#[pure_only]
use crate::effect::{Backend, Check, Effect, EffectError, Provisioned};
#[pure_only]
use crate::guest::DurationMs;
#[pure_only]
use crate::ids::Vmid;
#[pure_only]
use crate::memo::{Action, Failure, Memo, Progress};
#[pure_only]
use crate::observation::Observation;
#[pure_only]
use crate::effect::ProbeEffect;
#[pure_only]
use crate::spec::WorkloadSpec;
#[pure_only]
use crate::tags::NixHash;
#[pure_only]
use crate::tick::{Moment, Push};
use proxnix_pure::pure_only;

#[pure_only]
pub struct Context<'a> {
    pub spec: &'a WorkloadSpec,
    pub cohort: &'a Cohort,
    pub image: Knowledge<'a>,
    pub push: Option<&'a Push>,
    pub observed: &'a Observation,
    pub now: Moment,
    memo: &'a Memo,
}

#[pure_only]
impl<'a> Context<'a> {
    pub(crate) fn new(
        spec: &'a WorkloadSpec,
        cohort: &'a Cohort,
        image: Knowledge<'a>,
        push: Option<&'a Push>,
        observed: &'a Observation,
        now: Moment,
        memo: &'a Memo,
    ) -> Context<'a> {
        Context { spec, cohort, image, push, observed, now, memo }
    }

    #[must_use]
    pub fn progress(&self, member: &Member, check: Check) -> Progress {
        self.memo.progress(&member.instance(), check, self.now)
    }

    #[must_use]
    pub fn in_flight(&self, member: &Member) -> bool {
        self.memo.in_flight(&member.instance())
    }

    #[must_use]
    pub fn provisioned(&self, member: &Member) -> Option<Provisioned> {
        self.memo.provisioned(&member.instance())
    }

    #[must_use]
    pub fn failed(&self, member: &Member, action: Action) -> Option<&EffectError> {
        self.memo.failed(member.id(), action)
    }

    #[must_use]
    pub fn given_up(&self, nix: &NixHash) -> Option<&Failure> {
        self.memo.given_up(&self.spec.name, nix)
    }

    #[must_use]
    pub fn routed(&self, backend: &Backend) -> bool {
        self.memo.routed(&self.spec.name, backend)
    }

    #[must_use]
    pub fn route_failed(&self, backend: &Backend) -> Option<&EffectError> {
        self.memo.route_failed(&self.spec.name, backend)
    }

    #[must_use]
    pub fn updated(&self, member: &Member) -> bool {
        self.memo.updated(&member.instance())
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    Act(Effect),
    Probe { probe: ProbeEffect, within: DurationMs },
    Wake(Moment),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    Protected,
    BuildFailed(BuildFault),
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocker {
    SlotTaken(Vmid),
    Ambiguous,
    NoAddress,
    NoProof,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    Converged,
    Undeployed,
    Skipped(SkipReason),
    Invalid(Vec<ConfigFault>),
    Conflict(CohortFault),
    Blocked(Blocker),
    Adopting,
    Creating,
    Fencing,
    Starting,
    Checking(Check),
    Recording,
    Committing,
    Demoting,
    Routing,
    Updating,
    Reclaiming,
    Retiring,
    Aborting(Failure),
    Failed(Failure),
    Orphaned,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub stage: Stage,
    pub intents: Vec<Intent>,
}

#[pure_only]
impl Plan {
    #[must_use]
    pub fn idle(stage: Stage) -> Plan {
        Plan { stage, intents: vec![] }
    }

    #[must_use]
    pub fn act(stage: Stage, effect: Effect) -> Plan {
        Plan { stage, intents: vec![Intent::Act(effect)] }
    }

    #[must_use]
    pub fn probe(check: Check, probe: ProbeEffect, within: DurationMs) -> Plan {
        Plan { stage: Stage::Checking(check), intents: vec![Intent::Probe { probe, within }] }
    }

    #[must_use]
    pub fn wake(stage: Stage, at: Moment) -> Plan {
        Plan { stage, intents: vec![Intent::Wake(at)] }
    }
}

#[pure_only]
pub trait Strategy {
    type Phase;
    fn phase(ctx: &Context<'_>) -> Self::Phase;
    fn plan(ctx: &Context<'_>, phase: Self::Phase) -> Plan;
}

#[pure_only]
#[must_use]
pub fn drive<S: Strategy>(ctx: &Context<'_>) -> Plan {
    S::plan(ctx, S::phase(ctx))
}

#[pure_only]
pub trait Registry {
    fn plan(ctx: &Context<'_>) -> Plan;
}

#[pure_only]
#[must_use]
pub fn refused(ctx: &Context<'_>, member: &Member, actions: &[Action]) -> Option<Failure> {
    actions
        .iter()
        .find_map(|action| ctx.failed(member, *action).map(|error| Failure::Refused { action: *action, error: error.clone() }))
}
