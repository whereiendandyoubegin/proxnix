#![allow(clippy::missing_errors_doc)]

mod build;
mod changes;
mod cohort;
mod common;
mod desired;
mod effect;
mod guest;
mod ids;
mod memo;
mod observation;
mod pair;
mod spec;
mod step;
mod strategy;
mod tags;
mod tick;

use proxnix_pure::pure_only;

#[pure_only]
pub use build::{Artifact, BuildFault, Built, ExitCode, Images, Knowledge, StoreName, StorePath, StorePathFault};
#[pure_only]
pub use changes::{Changes, RebuildCause, changes};
#[pure_only]
pub use cohort::{Cohort, CohortFault, Expendable, Instance, Member, Promotion};
#[pure_only]
pub use common::{Health, Rebuilds, abort, begin, commit, dispose, health, point, serve, undeployed};
#[pure_only]
pub use desired::{ConfigFault, Desired, Orphan};
#[pure_only]
pub use effect::{
    Backend, Check, Detail, Effect, EffectError, EffectId, Endpoint, Event, Fresh, GuestEffect, Outcome, ProbeEffect,
    Provisioned, ResourceChange, RouteEffect,
};
#[pure_only]
pub use guest::{
    Attempt, Cores, DiskGib, DurationMs, GuestKind, GuestPath, GuestStatus, HostPath, KindFacts, MemoryMb, Mount,
    MountMode, Port, Privilege, Resources, Sockets,
};
#[pure_only]
pub use ids::{SameIdInBothSlots, Slot, SlotId, SlotPair, UnknownSlot, Vmid};
#[pure_only]
pub use memo::{Action, Failure, Memo, Progress};
#[pure_only]
pub use observation::{
    Anomaly, Audited, Grant, Guest, Managed, Observation, Occupant, Permissions, Sighting, SlotState, Vacant,
    VisibilityFault,
};
#[pure_only]
pub use pair::{Fence, Keep, Overlap, PairPhase, StopStart, pair_phase, pair_plan};
#[pure_only]
pub use spec::{BridgeName, Cutover, GuestName, Hostname, ImageType, KindSpec, ProxySpec, Purity, Timeouts, WorkloadSpec};
#[pure_only]
pub use step::{Builtin, Input, Planned, Report, Step, WorkloadReport, step};
#[pure_only]
pub use strategy::{Blocker, Context, Intent, Plan, Registry, SkipReason, Stage, Strategy, drive, refused};
#[pure_only]
pub use tags::{
    BadGeneration, BadRole, Commit, CommitHash, Digest, Generation, HashFault, HashFormat, ManagedTags, Nix, NixHash, Ownership,
    RawTags, RoleName, TagFault,
};
#[pure_only]
pub use tick::{Moment, Pacing, Push, Tick};
