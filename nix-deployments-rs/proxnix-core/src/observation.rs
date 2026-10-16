#[pure_only]
use crate::guest::{GuestKind, GuestStatus, KindFacts, Resources};
#[pure_only]
use crate::ids::{Slot, Vmid};
#[pure_only]
use crate::layout::{DataSnapshot, DataTag};
#[pure_only]
use crate::spec::GuestName;
#[pure_only]
use crate::tags::{ManagedTags, Ownership, RawTags, TagFault};
use proxnix_pure::pure_only;
#[pure_only]
use std::collections::{BTreeMap, BTreeSet};

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    Granted,
    Denied,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    pub vm_audit: Grant,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisibilityFault {
    NoVmAudit,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Audited {
    _proof: (),
}

#[pure_only]
impl TryFrom<Permissions> for Audited {
    type Error = VisibilityFault;

    fn try_from(permissions: Permissions) -> Result<Audited, VisibilityFault> {
        match permissions.vm_audit {
            Grant::Granted => Ok(Audited { _proof: () }),
            Grant::Denied => Err(VisibilityFault::NoVmAudit),
        }
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockKind {
    Backup,
    Clone,
    Create,
    Destroyed,
    Disk,
    Fstrim,
    Migrate,
    Mounted,
    Rollback,
    Snapshot,
    SnapshotDelete,
    Suspended,
    Suspending,
    Other,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsettled {
    Locked(LockKind),
    Incomplete,
    Unreadable,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settled {
    pub id: Vmid,
    pub name: GuestName,
    pub status: GuestStatus,
    pub tags: RawTags,
    pub resources: Resources,
    pub facts: KindFacts,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sighting {
    Settled(Settled),
    Unsettled(Vmid, Unsettled),
}

#[pure_only]
impl Sighting {
    #[must_use]
    pub fn id(&self) -> Vmid {
        match self {
            Sighting::Settled(settled) => settled.id,
            Sighting::Unsettled(id, _) => *id,
        }
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartState {
    Absent,
    Present,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalState {
    Absent,
    Current,
    Stale,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataFault {
    ForeignOrigin,
    PropertyDrift,
    OrphanSnapshot,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    origin: Option<DataSnapshot>,
    start: StartState,
    finalstate: FinalState,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotFacts {
    home: Option<Home>,
    aside: bool,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSlots {
    blue: SlotFacts,
    green: SlotFacts,
}

#[pure_only]
impl DataSlots {
    #[must_use]
    pub fn slot(&self, slot: Slot) -> &SlotFacts {
        match slot {
            Slot::Blue => &self.blue,
            Slot::Green => &self.green,
        }
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataFacts {
    Unobservable(DataFault),
    Observed(DataSlots),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataOrigin {
    None,
    Snapshot(DataSnapshot),
    Foreign,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AclType {
    Off,
    PosixAcl,
    Nfsv4,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Xattr {
    Off,
    On,
    Sa,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Atime {
    Off,
    On,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataProperties {
    pub acltype: AclType,
    pub xattr: Xattr,
    pub atime: Atime,
}

#[pure_only]
pub const EXPECTED_PROPERTIES: DataProperties = DataProperties {
    acltype: AclType::PosixAcl,
    xattr: Xattr::Sa,
    atime: Atime::Off,
};

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotDataset {
    Home(DataOrigin, DataProperties),
    Aside(DataOrigin, DataProperties),
    Start,
    Final(u64),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataSighting {
    pub name: GuestName,
    pub slot: Slot,
    pub dataset: SlotDataset,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guest {
    sighting: Settled,
    ownership: Ownership,
}

#[pure_only]
impl Guest {
    #[must_use]
    pub fn id(&self) -> Vmid {
        self.sighting.id
    }

    #[must_use]
    pub fn name(&self) -> &GuestName {
        &self.sighting.name
    }

    #[must_use]

    pub fn status(&self) -> GuestStatus {
        self.sighting.status
    }

    #[must_use]
    pub fn kind(&self) -> GuestKind {
        self.sighting.facts.kind()
    }

    #[must_use]
    pub fn resources(&self) -> Resources {
        self.sighting.resources
    }

    #[must_use]
    pub fn facts(&self) -> &KindFacts {
        &self.sighting.facts
    }

    #[must_use]
    pub fn ownership(&self) -> &Ownership {
        &self.ownership
    }

    pub(crate) fn projected(sighting: Settled, ownership: Ownership) -> Guest {
        Guest {
            sighting,
            ownership,
        }
    }

    pub(crate) fn revised(
        &self,
        status: GuestStatus,
        resources: Resources,
        ownership: Ownership,
    ) -> Guest {
        Guest {
            sighting: Settled {
                status,
                resources,
                ..self.sighting.clone()
            },
            ownership,
        }
    }
}

#[pure_only]
impl From<Settled> for Guest {
    fn from(sighting: Settled) -> Guest {
        Guest {
            ownership: Ownership::from(&sighting.tags),
            sighting,
        }
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Managed {
    guest: Box<Guest>,
    tags: ManagedTags,
}

#[pure_only]
impl Managed {
    #[must_use]
    pub fn guest(&self) -> &Guest {
        &self.guest
    }

    #[must_use]
    pub fn tags(&self) -> &ManagedTags {
        &self.tags
    }

    #[must_use]
    pub fn id(&self) -> Vmid {
        self.guest.id()
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vacant {
    id: Vmid,
}

#[pure_only]
impl Vacant {
    #[must_use]
    pub fn id(self) -> Vmid {
        self.id
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Occupant {
    Managed(Managed),
    Unmanaged(Vmid),
    Malformed(Vmid, TagFault),
    Unsettled(Vmid, Unsettled),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotState {
    Vacant(Vacant),
    Occupied(Occupant),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anomaly {
    DuplicateVmid(Vmid),
    Malformed(Vmid, TagFault),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    guests: BTreeMap<Vmid, Guest>,
    unsettled: BTreeMap<Vmid, Unsettled>,
    anomalies: Vec<Anomaly>,
    data: BTreeMap<GuestName, DataFacts>,
}

#[pure_only]
fn occupant(guest: &Guest) -> Occupant {
    match guest.ownership() {
        Ownership::Managed(tags) => Occupant::Managed(Managed {
            guest: Box::new(guest.clone()),
            tags: tags.clone(),
        }),
        Ownership::Unmanaged => Occupant::Unmanaged(guest.id()),
        Ownership::Malformed(fault) => Occupant::Malformed(guest.id(), *fault),
    }
}

#[pure_only]
fn blank() -> DataSlots {
    DataSlots {
        blue: SlotFacts {
            home: None,
            aside: false,
        },
        green: SlotFacts {
            home: None,
            aside: false,
        },
    }
}

#[pure_only]
fn origin_slot(name: &GuestName, snapshot: &DataSnapshot) -> Option<Slot> {
    match snapshot.dataset.segments() {
        [_, .., proxnix, state, workload, side]
            if proxnix.as_ref() == "proxnix"
                && state.as_ref() == "state"
                && workload.as_ref() == name.0 =>
        {
            match side.as_ref() {
                "blue" => Some(Slot::Blue),
                "green" => Some(Slot::Green),
                _ => None,
            }
        }
        _ => None,
    }
}

#[pure_only]
fn checked(
    origin: &DataOrigin,
    properties: DataProperties,
    ours: impl Fn(&DataSnapshot) -> bool,
) -> Option<DataFault> {
    let known = match origin {
        DataOrigin::None => true,
        DataOrigin::Snapshot(snapshot) => ours(snapshot),
        DataOrigin::Foreign => false,
    };
    match (known, properties == EXPECTED_PROPERTIES) {
        (false, _) => Some(DataFault::ForeignOrigin),
        (true, false) => Some(DataFault::PropertyDrift),
        (true, true) => None,
    }
}

#[pure_only]
fn fault(group: &[DataSighting]) -> Option<DataFault> {
    group.iter().find_map(|row| match &row.dataset {
        SlotDataset::Home(origin, properties) => checked(origin, *properties, |snapshot| {
            origin_slot(&row.name, snapshot) == Some(row.slot.switch_slot())
        }),
        SlotDataset::Aside(origin, properties) => checked(origin, *properties, |snapshot| {
            snapshot.tag == DataTag::Start && origin_slot(&row.name, snapshot).is_some()
        }),
        SlotDataset::Start | SlotDataset::Final(_) => (!group
            .iter()
            .any(|home| home.slot == row.slot && matches!(home.dataset, SlotDataset::Home(..))))
        .then_some(DataFault::OrphanSnapshot),
    })
}

#[pure_only]
impl DataSlots {
    fn with(&self, slot: Slot, facts: SlotFacts) -> DataSlots {
        match slot {
            Slot::Blue => DataSlots {
                blue: facts,
                green: self.green.clone(),
            },
            Slot::Green => DataSlots {
                blue: self.blue.clone(),
                green: facts,
            },
        }
    }

    fn rehomed(&self, slot: Slot, change: impl Fn(&Home) -> Home) -> DataSlots {
        self.with(
            slot,
            SlotFacts {
                home: self.slot(slot).home.as_ref().map(change),
                aside: self.slot(slot).aside,
            },
        )
    }
}

#[pure_only]
fn facts(group: &[DataSighting]) -> DataFacts {
    match fault(group) {
        Some(fault) => DataFacts::Unobservable(fault),
        None => {
            let placed = group.iter().fold(blank(), |slots, row| match &row.dataset {
                SlotDataset::Home(origin, _) => slots.with(
                    row.slot,
                    SlotFacts {
                        home: Some(Home {
                            origin: match origin {
                                DataOrigin::Snapshot(snapshot) => Some(snapshot.clone()),
                                DataOrigin::None | DataOrigin::Foreign => None,
                            },
                            start: StartState::Absent,
                            finalstate: FinalState::Absent,
                        }),
                        aside: slots.slot(row.slot).aside,
                    },
                ),
                SlotDataset::Aside(..) => slots.with(
                    row.slot,
                    SlotFacts {
                        home: slots.slot(row.slot).home.clone(),
                        aside: true,
                    },
                ),
                SlotDataset::Start | SlotDataset::Final(_) => slots,
            });
            DataFacts::Observed(group.iter().fold(placed, |slots, row| match row.dataset {
                SlotDataset::Start => slots.rehomed(row.slot, |home| Home {
                    start: StartState::Present,
                    ..home.clone()
                }),
                SlotDataset::Final(written) => slots.rehomed(row.slot, |home| Home {
                    finalstate: if written == 0 {
                        FinalState::Current
                    } else {
                        FinalState::Stale
                    },
                    ..home.clone()
                }),
                SlotDataset::Home(..) | SlotDataset::Aside(..) => slots,
            }))
        }
    }
}

#[pure_only]
impl Observation {
    #[must_use]
    pub fn new(
        _audited: Audited,
        sightings: Vec<Sighting>,
        data: Vec<DataSighting>,
    ) -> Observation {
        let duplicates: Vec<Anomaly> = sightings
            .iter()
            .enumerate()
            .filter(|(index, seen)| {
                sightings[..*index]
                    .iter()
                    .any(|earlier| earlier.id() == seen.id())
            })
            .map(|(_, seen)| Anomaly::DuplicateVmid(seen.id()))
            .collect();
        let (settled, unsettled): (Vec<Sighting>, Vec<Sighting>) = sightings
            .into_iter()
            .partition(|seen| matches!(seen, Sighting::Settled(_)));
        let guests: Vec<Guest> = settled
            .into_iter()
            .filter_map(|seen| match seen {
                Sighting::Settled(settled) => Some(Guest::from(settled)),
                Sighting::Unsettled(..) => None,
            })
            .collect();
        let unsettled: BTreeMap<Vmid, Unsettled> = unsettled
            .into_iter()
            .filter_map(|seen| match seen {
                Sighting::Settled(_) => None,
                Sighting::Unsettled(id, why) => Some((id, why)),
            })
            .collect();
        let malformed = guests.iter().filter_map(|guest| match guest.ownership() {
            Ownership::Malformed(fault) => Some(Anomaly::Malformed(guest.id(), *fault)),
            _ => None,
        });
        let anomalies = duplicates.into_iter().chain(malformed).collect();
        Observation {
            guests: guests
                .into_iter()
                .map(|guest| (guest.id(), guest))
                .collect(),
            unsettled,
            anomalies,
            data: data
                .iter()
                .map(|row| row.name.clone())
                .collect::<BTreeSet<GuestName>>()
                .into_iter()
                .map(|name| {
                    let group: Vec<DataSighting> = data
                        .iter()
                        .filter(|row| row.name == name)
                        .cloned()
                        .collect();
                    (name, facts(&group))
                })
                .collect(),
        }
    }

    #[must_use]
    pub fn slot(&self, id: Vmid) -> SlotState {
        match (self.guests.get(&id), self.unsettled.get(&id)) {
            (Some(guest), _) => SlotState::Occupied(occupant(guest)),
            (None, Some(why)) => SlotState::Occupied(Occupant::Unsettled(id, *why)),
            (None, None) => SlotState::Vacant(Vacant { id }),
        }
    }

    #[must_use]
    pub fn managed(&self) -> Vec<Managed> {
        self.guests
            .values()
            .filter_map(|guest| match occupant(guest) {
                Occupant::Managed(managed) => Some(managed),
                _ => None,
            })
            .collect()
    }

    #[must_use]
    pub fn managed_named(&self, name: &GuestName) -> Vec<Managed> {
        self.managed()
            .into_iter()
            .filter(|managed| managed.guest().name() == name)
            .collect()
    }

    #[must_use]
    pub fn anomalies(&self) -> &[Anomaly] {
        &self.anomalies
    }

    #[must_use]
    pub fn lookup(&self, name: &GuestName) -> DataFacts {
        self.data
            .get(name)
            .cloned()
            .unwrap_or_else(|| DataFacts::Observed(blank()))
    }

    pub(crate) fn guest(&self, id: Vmid) -> Option<&Guest> {
        self.guests.get(&id)
    }

    pub(crate) fn replaced(&self, id: Vmid, guest: Option<Guest>) -> Observation {
        Observation {
            guests: self
                .guests
                .iter()
                .filter(|(existing, _)| **existing != id)
                .map(|(existing, kept)| (*existing, kept.clone()))
                .chain(guest.map(|guest| (id, guest)))
                .collect(),
            unsettled: self
                .unsettled
                .iter()
                .filter(|(held, _)| **held != id)
                .map(|(held, why)| (*held, *why))
                .collect(),
            anomalies: self.anomalies.clone(),
            data: self.data.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest::{Cores, DiskGib, MemoryMb, Privilege};
    use crate::layout::Dataset;

    fn audited() -> Audited {
        Audited::try_from(Permissions {
            vm_audit: Grant::Granted,
        })
        .unwrap()
    }

    fn observed(sightings: Vec<Sighting>) -> Observation {
        Observation::new(audited(), sightings, Vec::new())
    }

    fn sighting(id: u32, name: &str, tags: &str) -> Sighting {
        Sighting::Settled(Settled {
            id: Vmid::new(id),
            name: GuestName(String::from(name)),
            status: GuestStatus::Running,
            tags: RawTags::from(String::from(tags)),
            resources: Resources {
                memory: MemoryMb(1024),
                disk: DiskGib(8),
                cores: Cores(2),
            },
            facts: KindFacts::Lxc {
                privilege: Privilege::Unprivileged,
                mounts: vec![],
            },
        })
    }

    const FORGEJO: &str = "commit-66d0ba6b605de2703e0fb7bbf58b922d5b36597e;ip-192.168.1.214;nix-78s0iadvjz6s48aqvx4rw78lwrzkjzlw;proxnix;slot-blue";

    fn row(name: &str, slot: Slot, dataset: SlotDataset) -> DataSighting {
        DataSighting {
            name: GuestName(String::from(name)),
            slot,
            dataset,
        }
    }

    fn snapshot(path: &str, tag: DataTag) -> DataSnapshot {
        DataSnapshot {
            dataset: Dataset::try_from(path).unwrap(),
            tag,
        }
    }

    fn cloned(path: &str, tag: DataTag) -> DataOrigin {
        DataOrigin::Snapshot(snapshot(path, tag))
    }

    fn home(origin: DataOrigin) -> SlotDataset {
        SlotDataset::Home(origin, EXPECTED_PROPERTIES)
    }

    fn data(rows: Vec<DataSighting>) -> Observation {
        Observation::new(audited(), vec![], rows)
    }

    #[test]
    fn data_rows_are_read_back_as_slot_facts() {
        let observed = data(vec![
            row("forgejo", Slot::Blue, home(DataOrigin::None)),
            row("forgejo", Slot::Blue, SlotDataset::Start),
            row("forgejo", Slot::Blue, SlotDataset::Final(0)),
            row(
                "forgejo",
                Slot::Green,
                home(cloned("tank/proxnix/state/forgejo/blue", DataTag::Final)),
            ),
            row(
                "forgejo",
                Slot::Green,
                SlotDataset::Aside(DataOrigin::None, EXPECTED_PROPERTIES),
            ),
            row("forgejo", Slot::Green, SlotDataset::Final(4096)),
        ]);
        assert_eq!(
            observed.lookup(&GuestName(String::from("forgejo"))),
            DataFacts::Observed(DataSlots {
                blue: SlotFacts {
                    home: Some(Home {
                        origin: None,
                        start: StartState::Present,
                        finalstate: FinalState::Current,
                    }),
                    aside: false,
                },
                green: SlotFacts {
                    home: Some(Home {
                        origin: Some(snapshot("tank/proxnix/state/forgejo/blue", DataTag::Final)),
                        start: StartState::Absent,
                        finalstate: FinalState::Stale,
                    }),
                    aside: true,
                },
            })
        );
    }

    #[test]
    fn each_data_fault_makes_only_its_own_workload_unobservable() {
        let drifted = DataProperties {
            atime: Atime::On,
            ..EXPECTED_PROPERTIES
        };
        let observed = data(vec![
            row("foreign", Slot::Blue, home(DataOrigin::Foreign)),
            row(
                "selfish",
                Slot::Green,
                home(cloned("tank/proxnix/state/selfish/green", DataTag::Start)),
            ),
            row(
                "stolen",
                Slot::Green,
                home(cloned("tank/proxnix/state/hydra/blue", DataTag::Start)),
            ),
            row(
                "rehearsed",
                Slot::Green,
                home(cloned(
                    "tank/proxnix/state/rehearsed/blue-rehearsal",
                    DataTag::Start,
                )),
            ),
            row(
                "unpooled",
                Slot::Green,
                home(cloned("proxnix/state/unpooled/blue", DataTag::Start)),
            ),
            row(
                "late",
                Slot::Blue,
                SlotDataset::Aside(
                    cloned("tank/proxnix/state/late/green", DataTag::Final),
                    EXPECTED_PROPERTIES,
                ),
            ),
            row("finalizing", Slot::Green, home(DataOrigin::None)),
            row("finalizing", Slot::Green, SlotDataset::Start),
            row(
                "finalizing",
                Slot::Green,
                SlotDataset::Aside(
                    cloned("tank/proxnix/state/finalizing/green", DataTag::Start),
                    EXPECTED_PROPERTIES,
                ),
            ),
            row(
                "drifted",
                Slot::Blue,
                SlotDataset::Home(DataOrigin::None, drifted),
            ),
            row("orphan", Slot::Blue, SlotDataset::Start),
            row("orphan", Slot::Green, home(DataOrigin::None)),
            row("fine", Slot::Blue, home(DataOrigin::None)),
        ]);
        let looked = |name: &str| observed.lookup(&GuestName(String::from(name)));
        assert_eq!(
            looked("foreign"),
            DataFacts::Unobservable(DataFault::ForeignOrigin)
        );
        assert_eq!(
            looked("selfish"),
            DataFacts::Unobservable(DataFault::ForeignOrigin)
        );
        assert_eq!(
            looked("stolen"),
            DataFacts::Unobservable(DataFault::ForeignOrigin)
        );
        assert_eq!(
            looked("rehearsed"),
            DataFacts::Unobservable(DataFault::ForeignOrigin)
        );
        assert_eq!(
            looked("unpooled"),
            DataFacts::Unobservable(DataFault::ForeignOrigin)
        );
        assert_eq!(
            looked("late"),
            DataFacts::Unobservable(DataFault::ForeignOrigin)
        );
        assert_eq!(
            looked("drifted"),
            DataFacts::Unobservable(DataFault::PropertyDrift)
        );
        assert_eq!(
            looked("orphan"),
            DataFacts::Unobservable(DataFault::OrphanSnapshot)
        );
        assert!(matches!(looked("fine"), DataFacts::Observed(_)));
        assert!(matches!(looked("finalizing"), DataFacts::Observed(_)));
    }

    #[test]
    fn an_unknown_name_has_both_slots_blank() {
        let observed = data(vec![row("forgejo", Slot::Blue, home(DataOrigin::None))]);
        assert_eq!(
            observed.lookup(&GuestName(String::from("nobody"))),
            DataFacts::Observed(blank())
        );
    }

    #[test]
    fn a_denied_audit_permission_cannot_become_an_observation() {
        assert_eq!(
            Audited::try_from(Permissions {
                vm_audit: Grant::Denied
            }),
            Err(VisibilityFault::NoVmAudit)
        );
    }

    #[test]
    fn an_id_nobody_holds_is_vacant() {
        let observed = observed(vec![sighting(844, "forgejo", FORGEJO)]);
        assert!(
            matches!(observed.slot(Vmid::new(944)), SlotState::Vacant(v) if v.id() == Vmid::new(944))
        );
    }

    #[test]
    fn a_managed_guest_occupies_its_slot() {
        let observed = observed(vec![sighting(844, "forgejo", FORGEJO)]);
        match observed.slot(Vmid::new(844)) {
            SlotState::Occupied(Occupant::Managed(managed)) => {
                assert_eq!(managed.id(), Vmid::new(844));
                assert_eq!(managed.tags().slot, Slot::Blue);
            }
            other => panic!("expected a managed occupant, got {other:?}"),
        }
    }

    #[test]
    fn an_unmanaged_guest_still_occupies_its_slot() {
        let observed = observed(vec![sighting(844, "someone-elses", "k3s")]);
        assert_eq!(
            observed.slot(Vmid::new(844)),
            SlotState::Occupied(Occupant::Unmanaged(Vmid::new(844)))
        );
        assert!(observed.managed().is_empty());
    }

    #[test]
    fn a_malformed_guest_occupies_its_slot_and_is_reported() {
        let observed = observed(vec![sighting(841, "flake-updater", "proxnix;slot-blue")]);
        assert_eq!(
            observed.slot(Vmid::new(841)),
            SlotState::Occupied(Occupant::Malformed(Vmid::new(841), TagFault::NoNixHash))
        );
        assert_eq!(
            observed.anomalies(),
            &[Anomaly::Malformed(Vmid::new(841), TagFault::NoNixHash)]
        );
    }

    #[test]
    fn a_vmid_seen_twice_is_reported() {
        let observed = observed(vec![
            sighting(844, "forgejo", FORGEJO),
            sighting(844, "forgejo", FORGEJO),
        ]);
        assert_eq!(
            observed.anomalies(),
            &[Anomaly::DuplicateVmid(Vmid::new(844))]
        );
    }

    #[test]
    fn managed_guests_are_found_by_name_in_both_slots() {
        let observed = observed(vec![
            sighting(844, "forgejo", FORGEJO),
            sighting(
                944,
                "forgejo",
                "commit-66d0ba6b605de2703e0fb7bbf58b922d5b36597e;nix-i3d00236fdkfw1v9cmasajkjhzl8zi5j;proxnix;slot-green",
            ),
            sighting(
                845,
                "cloudflared",
                "commit-66d0ba6b605de2703e0fb7bbf58b922d5b36597e;nix-i3d00236fdkfw1v9cmasajkjhzl8zi5j;proxnix;slot-blue",
            ),
        ]);
        let ids: Vec<Vmid> = observed
            .managed_named(&GuestName(String::from("forgejo")))
            .iter()
            .map(Managed::id)
            .collect();
        assert_eq!(ids, vec![Vmid::new(844), Vmid::new(944)]);
    }

    #[test]
    fn an_unsettled_guest_occupies_its_slot_and_is_never_managed() {
        let observed = observed(vec![
            sighting(844, "forgejo", FORGEJO),
            Sighting::Unsettled(Vmid::new(944), Unsettled::Locked(LockKind::Create)),
            Sighting::Unsettled(Vmid::new(947), Unsettled::Incomplete),
        ]);
        assert_eq!(
            observed.slot(Vmid::new(944)),
            SlotState::Occupied(Occupant::Unsettled(
                Vmid::new(944),
                Unsettled::Locked(LockKind::Create)
            ))
        );
        assert_eq!(
            observed.slot(Vmid::new(947)),
            SlotState::Occupied(Occupant::Unsettled(Vmid::new(947), Unsettled::Incomplete))
        );
        assert_eq!(
            observed
                .managed()
                .iter()
                .map(Managed::id)
                .collect::<Vec<_>>(),
            vec![Vmid::new(844)]
        );
        assert!(observed.anomalies().is_empty());
    }

    #[test]
    fn a_vmid_seen_settled_and_unsettled_is_reported_and_stays_occupied() {
        let observed = observed(vec![
            sighting(844, "forgejo", FORGEJO),
            Sighting::Unsettled(Vmid::new(844), Unsettled::Unreadable),
        ]);
        assert_eq!(
            observed.anomalies(),
            &[Anomaly::DuplicateVmid(Vmid::new(844))]
        );
        assert!(matches!(
            observed.slot(Vmid::new(844)),
            SlotState::Occupied(_)
        ));
    }
}
