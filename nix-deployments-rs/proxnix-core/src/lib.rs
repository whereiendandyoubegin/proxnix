#![allow(clippy::missing_errors_doc)]

mod effect;
mod guest;
mod ids;
mod observation;
mod spec;
mod tags;

use proxnix_pure::pure_only;

#[pure_only]
pub use effect::{
    Backend, Detail, Effect, EffectError, EffectId, Event, GuestEffect, Outcome, Owned, ProbeEffect, Provisioned,
    ResourceChange, RouteEffect,
};
#[pure_only]
pub use guest::{
    Attempt, Cores, DiskGib, DurationMs, GuestKind, GuestPath, GuestStatus, HostPath, KindFacts, MemoryMb, Mount,
    MountMode, Port, Privilege, Resources, Sockets,
};
#[pure_only]
pub use ids::{SameIdInBothSlots, Slot, SlotId, SlotPair, UnknownSlot, Vmid};
#[pure_only]
pub use observation::{
    Anomaly, Audited, Grant, Guest, Managed, Observation, Occupant, Permissions, Sighting, SlotState, Vacant,
    VisibilityFault,
};
#[pure_only]
pub use spec::{BridgeName, GuestName, Hostname, ImageType, KindSpec, Protection, ProxySpec, Timeouts, WorkloadSpec};
#[pure_only]
pub use tags::{Commit, CommitHash, Digest, HashFault, HashFormat, ManagedTags, Nix, NixHash, Ownership, RawTags, TagFault};
