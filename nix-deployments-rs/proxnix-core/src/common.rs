#[pure_only]
use crate::build::{Artifact, Knowledge};
#[pure_only]
use crate::changes::changes;
#[pure_only]
use crate::cohort::{Expendable, Member, Promotion};
#[pure_only]
use crate::effect::{Backend, Effect, Endpoint, Fresh, GuestEffect, ProbeEffect, RouteEffect};
#[pure_only]
use crate::guest::GuestKind;
#[pure_only]
use crate::ids::Slot;
#[pure_only]
use crate::memo::{Action, Failure, Progress};
#[pure_only]
use crate::observation::SlotState;
#[pure_only]
use crate::strategy::{Blocker, Context, Plan, SkipReason, Stage, refused};
#[pure_only]
use crate::tags::RoleName;
#[pure_only]
use crate::tick::Push;
use proxnix_pure::pure_only;
#[pure_only]
use std::net::Ipv4Addr;

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Pending(Plan),
    Healthy(Ipv4Addr),
    Failed(Failure),
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rebuilds {
    Allowed,
    Protected,
}

#[pure_only]
#[must_use]
pub fn begin(ctx: &Context<'_>, push: &Push, artifact: &Artifact, beside: Option<&Member>, role: Option<RoleName>) -> Plan {
    let target = beside
        .and_then(|member| ctx.cohort.beside(member))
        .unwrap_or_else(|| ctx.spec.slots.id(Slot::Blue).inner());
    match (ctx.given_up(artifact.nix()), ctx.observed.slot(target)) {
        (Some(failure), _) => Plan::idle(Stage::Failed(failure.clone())),
        (None, SlotState::Vacant(vacant)) => match Fresh::new(push, artifact, ctx.spec, vacant, role) {
            Some(fresh) => Plan::act(
                Stage::Creating,
                Effect::Guest(GuestEffect::Create { target: vacant, artifact: artifact.clone(), spec: Box::new(ctx.spec.clone()), fresh }),
            ),
            None => Plan::idle(Stage::Blocked(Blocker::SlotTaken(target))),
        },
        (None, SlotState::Occupied(_)) => Plan::idle(Stage::Blocked(Blocker::SlotTaken(target))),
    }
}

#[pure_only]
enum Unsettled {
    Pending(Plan),
    Failed(Failure),
}

#[pure_only]
fn trial(ctx: &Context<'_>, member: &Member, probe: ProbeEffect, within: crate::guest::DurationMs) -> Result<Option<Ipv4Addr>, Unsettled> {
    let check = probe.check();
    match ctx.progress(member, check) {
        Progress::Passed(found) => Ok(found),
        Progress::Due => Err(Unsettled::Pending(Plan::probe(check, probe, within))),
        Progress::Waiting(at) => Err(Unsettled::Pending(Plan::wake(Stage::Checking(check), at))),
        Progress::Expired(attempts, last) => Err(Unsettled::Failed(Failure::Expired { check, attempts, last })),
    }
}

#[pure_only]
fn assess(ctx: &Context<'_>, member: &Member) -> Result<Ipv4Addr, Unsettled> {
    refused(ctx, member, &[Action::Start, Action::Record]).map_or(Ok(()), |failure| Err(Unsettled::Failed(failure)))?;
    if !member.running() {
        return Err(Unsettled::Pending(Plan::act(Stage::Starting, Effect::Guest(GuestEffect::Start(member.clone())))));
    }
    let timeouts = ctx.spec.timeouts;
    let address = trial(ctx, member, ProbeEffect::ReadAddress(member.clone()), timeouts.dhcp)?
        .ok_or(Unsettled::Failed(Failure::NoAddress))?;
    if member.tags().service_ip != Some(address) {
        return Err(Unsettled::Pending(Plan::act(Stage::Recording, Effect::Guest(GuestEffect::Record { guest: member.clone(), address }))));
    }
    trial(
        ctx,
        member,
        ProbeEffect::PortOpen { guest: member.clone(), address, port: ctx.spec.proxy.backend_port },
        timeouts.health_check,
    )?;
    match ctx.spec.kind() {
        GuestKind::Lxc => trial(ctx, member, ProbeEffect::GuestCheck(member.clone()), timeouts.health_check).map(|_| address),
        GuestKind::Qemu => Ok(address),
    }
}

#[pure_only]
#[must_use]
pub fn health(ctx: &Context<'_>, member: &Member) -> Health {
    match assess(ctx, member) {
        Ok(address) => Health::Healthy(address),
        Err(Unsettled::Pending(plan)) => Health::Pending(plan),
        Err(Unsettled::Failed(failure)) => Health::Failed(failure),
    }
}

#[pure_only]
#[must_use]
pub fn abort(ctx: &Context<'_>, member: &Member, failure: Failure) -> Plan {
    match (
        refused(ctx, member, &[Action::Undo, Action::Reclaim]),
        ctx.provisioned(member),
        Expendable::outranked(ctx.cohort, member),
    ) {
        (Some(refusal), _, _) => Plan::idle(Stage::Failed(refusal)),
        (None, Some(provisioned), _) => Plan::act(Stage::Aborting(failure), Effect::Guest(GuestEffect::Undo(provisioned))),
        (None, None, Some(doomed)) => Plan::act(Stage::Aborting(failure), Effect::Guest(GuestEffect::Reclaim(doomed))),
        (None, None, None) => Plan::idle(Stage::Failed(failure)),
    }
}

#[pure_only]
#[must_use]
pub fn commit(ctx: &Context<'_>, member: &Member, stage: Stage) -> Plan {
    match (refused(ctx, member, &[Action::Commit]), Promotion::over(ctx.cohort, member)) {
        (Some(failure), _) => abort(ctx, member, failure),
        (None, Some(commit)) => Plan::act(stage, Effect::Guest(GuestEffect::Commit(commit))),
        (None, None) => Plan::idle(Stage::Blocked(Blocker::NoProof)),
    }
}

#[pure_only]
#[must_use]
pub fn dispose(ctx: &Context<'_>, member: &Member, action: Action) -> Plan {
    match (refused(ctx, member, &[action]), Expendable::outranked(ctx.cohort, member), action) {
        (Some(failure), _, _) => Plan::idle(Stage::Failed(failure)),
        (None, Some(doomed), Action::Reclaim) => Plan::act(Stage::Reclaiming, Effect::Guest(GuestEffect::Reclaim(doomed))),
        (None, Some(doomed), _) => Plan::act(Stage::Retiring, Effect::Guest(GuestEffect::Retire(doomed))),
        (None, None, _) => Plan::idle(Stage::Blocked(Blocker::NoProof)),
    }
}

#[pure_only]
#[must_use]
pub fn point(ctx: &Context<'_>, to: &Member, from: Option<&Member>, endpoint: Endpoint) -> Option<Plan> {
    if !ctx.spec.routed() {
        return None;
    }
    match Backend::of(to, endpoint) {
        None => Some(Plan::idle(Stage::Blocked(Blocker::NoAddress))),
        Some(backend) if ctx.routed(&backend) => None,
        Some(backend) => Some(match ctx.route_failed(&backend) {
            Some(error) => Plan::idle(Stage::Failed(Failure::Route(error.clone()))),
            None => Plan::act(
                Stage::Routing,
                Effect::Route(match from.and_then(|from| Backend::of(from, endpoint)) {
                    Some(old) => RouteEffect::Point { name: ctx.spec.name.clone(), proxy: ctx.spec.proxy.clone(), to: backend, from: Some(old) },
                    None if from.is_some() => RouteEffect::Point { name: ctx.spec.name.clone(), proxy: ctx.spec.proxy.clone(), to: backend, from: None },
                    None => RouteEffect::Restore { name: ctx.spec.name.clone(), proxy: ctx.spec.proxy.clone(), to: backend },
                }),
            ),
        }),
    }
}

#[pure_only]
fn after_update(ctx: &Context<'_>, serving: &Member) -> Option<Plan> {
    let address = serving.tags().service_ip.filter(|_| ctx.updated(serving))?;
    let probe = ProbeEffect::PortOpen { guest: serving.clone(), address, port: ctx.spec.proxy.backend_port };
    match trial(ctx, serving, probe, ctx.spec.timeouts.health_check) {
        Ok(_) => None,
        Err(Unsettled::Pending(plan)) => Some(plan),
        Err(Unsettled::Failed(failure)) => Some(Plan::idle(Stage::Failed(failure))),
    }
}

#[pure_only]
fn pushed(ctx: &Context<'_>, push: &Push, serving: &Member, rebuilds: Rebuilds) -> Option<Plan> {
    let found = changes(ctx.spec, serving.guest(), serving.nix(), ctx.image);
    match (found.none(), found.rebuild.is_empty(), rebuilds, ctx.image) {
        (true, ..) => after_update(ctx, serving),
        (false, _, Rebuilds::Protected, _) => Some(Plan::idle(Stage::Skipped(SkipReason::Protected))),
        (false, false, Rebuilds::Allowed, Knowledge::Built(artifact)) => Some(begin(ctx, push, artifact, Some(serving), None)),
        (false, true, Rebuilds::Allowed, _) => Some(match refused(ctx, serving, &[Action::Update]) {
            Some(failure) => Plan::idle(Stage::Failed(failure)),
            None => Plan::act(Stage::Updating, Effect::Guest(GuestEffect::Update { guest: serving.clone(), changes: found.in_place })),
        }),
        (false, false, Rebuilds::Allowed, _) => None,
    }
}

#[pure_only]
#[must_use]
pub fn serve(ctx: &Context<'_>, serving: &Member, rebuilds: Rebuilds) -> Plan {
    if !serving.running() {
        return match refused(ctx, serving, &[Action::Start]) {
            Some(failure) => Plan::idle(Stage::Failed(failure)),
            None => Plan::act(Stage::Starting, Effect::Guest(GuestEffect::Start(serving.clone()))),
        };
    }
    let routed = match ctx.push {
        Some(_) if !ctx.in_flight(serving) => None,
        _ => point(ctx, serving, None, Endpoint::Primary),
    };
    let decided = match (ctx.push, ctx.image) {
        (Some(_), Knowledge::Failed(fault)) => Some(Plan::idle(Stage::Skipped(SkipReason::BuildFailed(fault.clone())))),
        (Some(push), _) => pushed(ctx, push, serving, rebuilds),
        (None, _) => None,
    };
    routed.or(decided).unwrap_or_else(|| Plan::idle(Stage::Converged))
}

#[pure_only]
#[must_use]
pub fn undeployed(ctx: &Context<'_>) -> Plan {
    match (ctx.push, ctx.image) {
        (Some(push), Knowledge::Built(artifact)) => begin(ctx, push, artifact, None, None),
        (Some(_), Knowledge::Failed(fault)) => Plan::idle(Stage::Skipped(SkipReason::BuildFailed(fault.clone()))),
        _ => Plan::idle(Stage::Undeployed),
    }
}
