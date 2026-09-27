#[pure_only]
use crate::guest::{GuestKind, GuestStatus, KindFacts, Resources};
#[pure_only]
use crate::ids::Vmid;
#[pure_only]
use crate::spec::GuestName;
#[pure_only]
use crate::tags::{ManagedTags, Ownership, RawTags, TagFault};
use proxnix_pure::pure_only;
#[pure_only]
use std::collections::BTreeMap;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sighting {
    pub id: Vmid,
    pub name: GuestName,
    pub status: GuestStatus,
    pub tags: RawTags,
    pub resources: Resources,
    pub facts: KindFacts,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guest {
    sighting: Sighting,
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
}

#[pure_only]
impl From<Sighting> for Guest {
    fn from(sighting: Sighting) -> Guest {
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
    anomalies: Vec<Anomaly>,
}

#[pure_only]
fn occupant(guest: &Guest) -> Occupant {
    match guest.ownership() {
        Ownership::Managed(tags) => Occupant::Managed(Managed { guest: Box::new(guest.clone()), tags: tags.clone() }),
        Ownership::Unmanaged => Occupant::Unmanaged(guest.id()),
        Ownership::Malformed(fault) => Occupant::Malformed(guest.id(), *fault),
    }
}

#[pure_only]
impl Observation {
    #[must_use]
    pub fn new(_audited: Audited, sightings: Vec<Sighting>) -> Observation {
        let guests: Vec<Guest> = sightings.into_iter().map(Guest::from).collect();
        let duplicates = guests
            .iter()
            .enumerate()
            .filter(|(index, guest)| guests[..*index].iter().any(|earlier| earlier.id() == guest.id()))
            .map(|(_, guest)| Anomaly::DuplicateVmid(guest.id()));
        let malformed = guests.iter().filter_map(|guest| match guest.ownership() {
            Ownership::Malformed(fault) => Some(Anomaly::Malformed(guest.id(), *fault)),
            _ => None,
        });
        let anomalies = duplicates.chain(malformed).collect();
        Observation {
            guests: guests.into_iter().map(|guest| (guest.id(), guest)).collect(),
            anomalies,
        }
    }

    #[must_use]
    pub fn slot(&self, id: Vmid) -> SlotState {
        self.guests
            .get(&id)
            .map_or(SlotState::Vacant(Vacant { id }), |guest| SlotState::Occupied(occupant(guest)))
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
        self.managed().into_iter().filter(|managed| managed.guest().name() == name).collect()
    }

    #[must_use]
    pub fn anomalies(&self) -> &[Anomaly] {
        &self.anomalies
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest::{Cores, DiskGib, MemoryMb, Privilege};
    use crate::ids::Slot;

    fn audited() -> Audited {
        Audited::try_from(Permissions { vm_audit: Grant::Granted }).unwrap()
    }

    fn sighting(id: u32, name: &str, tags: &str) -> Sighting {
        Sighting {
            id: Vmid::new(id),
            name: GuestName(String::from(name)),
            status: GuestStatus::Running,
            tags: RawTags::from(String::from(tags)),
            resources: Resources { memory: MemoryMb(1024), disk: DiskGib(8), cores: Cores(2) },
            facts: KindFacts::Lxc { privilege: Privilege::Unprivileged, mounts: vec![] },
        }
    }

    const FORGEJO: &str = "commit-66d0ba6b605de2703e0fb7bbf58b922d5b36597e;ip-192.168.1.214;nix-78s0iadvjz6s48aqvx4rw78lwrzkjzlw;proxnix;slot-blue";

    #[test]
    fn a_denied_audit_permission_cannot_become_an_observation() {
        assert_eq!(
            Audited::try_from(Permissions { vm_audit: Grant::Denied }),
            Err(VisibilityFault::NoVmAudit)
        );
    }

    #[test]
    fn an_id_nobody_holds_is_vacant() {
        let observed = Observation::new(audited(), vec![sighting(844, "forgejo", FORGEJO)]);
        assert!(matches!(observed.slot(Vmid::new(944)), SlotState::Vacant(v) if v.id() == Vmid::new(944)));
    }

    #[test]
    fn a_managed_guest_occupies_its_slot() {
        let observed = Observation::new(audited(), vec![sighting(844, "forgejo", FORGEJO)]);
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
        let observed = Observation::new(audited(), vec![sighting(844, "someone-elses", "k3s")]);
        assert_eq!(observed.slot(Vmid::new(844)), SlotState::Occupied(Occupant::Unmanaged(Vmid::new(844))));
        assert!(observed.managed().is_empty());
    }

    #[test]
    fn a_malformed_guest_occupies_its_slot_and_is_reported() {
        let observed = Observation::new(audited(), vec![sighting(841, "flake-updater", "proxnix;slot-blue")]);
        assert_eq!(
            observed.slot(Vmid::new(841)),
            SlotState::Occupied(Occupant::Malformed(Vmid::new(841), TagFault::NoNixHash))
        );
        assert_eq!(observed.anomalies(), &[Anomaly::Malformed(Vmid::new(841), TagFault::NoNixHash)]);
    }

    #[test]
    fn a_vmid_seen_twice_is_reported() {
        let observed = Observation::new(
            audited(),
            vec![sighting(844, "forgejo", FORGEJO), sighting(844, "forgejo", FORGEJO)],
        );
        assert_eq!(observed.anomalies(), &[Anomaly::DuplicateVmid(Vmid::new(844))]);
    }

    #[test]
    fn managed_guests_are_found_by_name_in_both_slots() {
        let observed = Observation::new(
            audited(),
            vec![
                sighting(844, "forgejo", FORGEJO),
                sighting(944, "forgejo", "commit-66d0ba6b605de2703e0fb7bbf58b922d5b36597e;nix-i3d00236fdkfw1v9cmasajkjhzl8zi5j;proxnix;slot-green"),
                sighting(845, "cloudflared", "commit-66d0ba6b605de2703e0fb7bbf58b922d5b36597e;nix-i3d00236fdkfw1v9cmasajkjhzl8zi5j;proxnix;slot-blue"),
            ],
        );
        let ids: Vec<Vmid> = observed
            .managed_named(&GuestName(String::from("forgejo")))
            .iter()
            .map(Managed::id)
            .collect();
        assert_eq!(ids, vec![Vmid::new(844), Vmid::new(944)]);
    }
}
