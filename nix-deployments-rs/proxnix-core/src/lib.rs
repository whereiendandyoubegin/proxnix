#![allow(clippy::missing_errors_doc)]

mod build;
mod changes;
mod cohort;
mod common;
mod desired;
mod effect;
mod guest;
mod ids;
mod layout;
mod memo;
mod observation;
mod pair;
mod project;
mod spec;
mod step;
mod store;
mod strategy;
mod tags;
mod task;
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
    MountMode, PathFault, PathPart, Port, Privilege, Resources, Sockets,
};
#[pure_only]
pub use layout::{
    BadSegment, Dataset, HostEffect, Layout, Owner, Segment, StateLabel, Storage, StorageFault, StorageSpec, StoreMode, store_cutover,
};
#[pure_only]
pub use ids::{SameIdInBothSlots, Slot, SlotId, SlotPair, UnknownSlot, Vmid};
#[pure_only]
pub use memo::{Action, Failure, Memo, Progress};
#[pure_only]
pub use observation::{
    Anomaly, Audited, Grant, Guest, LockKind, Managed, Observation, Occupant, Permissions, Settled, Sighting, SlotState,
    Unsettled, Vacant, VisibilityFault,
};
#[pure_only]
pub use pair::{Fence, Keep, Overlap, PairPhase, StopStart, pair_phase, pair_plan};
#[pure_only]
pub use project::{Projection, Scope, assume, placeholder_address, project};
#[pure_only]
pub use spec::{BridgeName, Cutover, GuestName, Hostname, ImageType, KindSpec, ProxySpec, Purity, Timeouts, WorkloadSpec};
#[pure_only]
pub use step::{Builtin, Input, Planned, Report, Step, WorkloadReport, step};
#[pure_only]
pub use store::{
    BuildState, HydraBuild, Key, Ledger, Policy, Retain, RootHolder, Source, StoreEffect, StoreEvent, SyncFault, SyncInput, SyncStep, Toplevel,
    roots, sync_step,
};
#[pure_only]
pub use strategy::{Blocker, Context, Intent, Plan, Registry, SkipReason, Stage, Strategy, drive, refused};
#[pure_only]
pub use tags::{
    BadGeneration, BadRole, Commit, CommitHash, Digest, Generation, HashFault, HashFormat, ManagedTags, Nix, NixHash, Ownership,
    RawTags, RoleName, TagFault,
};
#[pure_only]
pub use task::{TaskExit, TaskState, Upid, UpidFault};
#[pure_only]
pub use tick::{Moment, Pacing, Push, Tick};
