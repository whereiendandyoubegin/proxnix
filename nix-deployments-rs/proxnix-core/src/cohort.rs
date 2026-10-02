#[pure_only]
use crate::guest::GuestStatus;
#[pure_only]
use crate::ids::{Slot, SlotId, SlotPair, Vmid};
#[pure_only]
use crate::observation::{Guest, Managed, Observation, Occupant, SlotState};
#[pure_only]
use crate::spec::{GuestName, WorkloadSpec};
#[pure_only]
use crate::tags::{Generation, ManagedTags, NixHash, RoleName, TagFault};
use proxnix_pure::pure_only;

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instance {
    pub id: Vmid,
    pub nix: NixHash,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    managed: Managed,
}

#[pure_only]
impl Member {
    #[must_use]
    pub fn id(&self) -> Vmid {
        self.managed.id()
    }

    #[must_use]
    pub fn guest(&self) -> &Guest {
        self.managed.guest()
    }

    #[must_use]
    pub fn tags(&self) -> &ManagedTags {
        self.managed.tags()
    }

    #[must_use]
    pub fn generation(&self) -> Option<Generation> {
        self.tags().generation
    }

    #[must_use]
    pub fn pending(&self) -> bool {
        self.tags().pending
    }

    #[must_use]
    pub fn role(&self) -> Option<&RoleName> {
        self.tags().role.as_ref()
    }

    #[must_use]
    pub fn nix(&self) -> &NixHash {
        &self.tags().nix
    }

    #[must_use]
    pub fn running(&self) -> bool {
        self.guest().status() == GuestStatus::Running
    }

    #[must_use]
    pub fn instance(&self) -> Instance {
        Instance { id: self.id(), nix: self.nix().clone() }
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CohortFault {
    Stray(Vmid),
    Malformed(Vmid, TagFault),
    SameGeneration(Generation),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cohort {
    name: GuestName,
    slots: SlotPair,
    members: Vec<Member>,
}

#[pure_only]
fn malformed_in(observed: &Observation, id: Vmid) -> Option<CohortFault> {
    match observed.slot(id) {
        SlotState::Occupied(Occupant::Malformed(at, fault)) => Some(CohortFault::Malformed(at, fault)),
        _ => None,
    }
}

#[pure_only]
impl Cohort {
    pub fn gather(observed: &Observation, spec: &WorkloadSpec) -> Result<Cohort, CohortFault> {
        let members: Vec<Member> =
            observed.managed_named(&spec.name).into_iter().map(|managed| Member { managed }).collect();
        let stray = members
            .iter()
            .find(|member| !spec.slots.both().map(SlotId::inner).contains(&member.id()))
            .map(|member| CohortFault::Stray(member.id()));
        let malformed = spec.slots.both().into_iter().find_map(|slot| malformed_in(observed, slot.inner()));
        let repeated = members.iter().enumerate().find_map(|(index, member)| {
            member
                .generation()
                .filter(|generation| members[..index].iter().any(|earlier| earlier.generation() == Some(*generation)))
                .map(CohortFault::SameGeneration)
        });
        match stray.or(malformed).or(repeated) {
            Some(fault) => Err(fault),
            None => Ok(Cohort { name: spec.name.clone(), slots: spec.slots, members }),
        }
    }

    #[must_use]
    pub fn name(&self) -> &GuestName {
        &self.name
    }

    #[must_use]
    pub fn slots(&self) -> SlotPair {
        self.slots
    }

    #[must_use]
    pub fn members(&self) -> &[Member] {
        &self.members
    }

    #[must_use]
    pub fn highest(&self) -> Option<&Member> {
        self.members
            .iter()
            .filter(|member| member.generation().is_some())
            .max_by_key(|member| member.generation())
    }

    #[must_use]
    pub fn others(&self, member: &Member) -> Vec<&Member> {
        self.members.iter().filter(|other| other.id() != member.id()).collect()
    }

    #[must_use]
    pub fn slot_of(&self, member: &Member) -> Option<Slot> {
        self.slots.slot_of(member.id())
    }

    #[must_use]
    pub fn beside(&self, member: &Member) -> Option<Vmid> {
        self.slot_of(member).map(|slot| self.slots.id(slot.switch_slot()).inner())
    }

    fn holds(&self, member: &Member) -> bool {
        self.members.iter().any(|candidate| candidate == member)
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promotion {
    guest: Member,
    generation: Generation,
}

#[pure_only]
impl Promotion {
    #[must_use]
    pub fn over(cohort: &Cohort, member: &Member) -> Option<Promotion> {
        let top = cohort.highest();
        let already_highest = top.is_some_and(|top| top.id() == member.id());
        (cohort.holds(member) && !already_highest).then(|| Promotion {
            guest: member.clone(),
            generation: top.and_then(Member::generation).map_or(Generation::FIRST, Generation::after),
        })
    }

    #[must_use]
    pub fn guest(&self) -> &Member {
        &self.guest
    }

    #[must_use]
    pub fn generation(&self) -> Generation {
        self.generation
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
enum Doom {
    Outranked(Member),
    Orphaned(Managed),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expendable {
    doom: Doom,
}

#[pure_only]
impl Expendable {
    #[must_use]
    pub fn outranked(cohort: &Cohort, member: &Member) -> Option<Expendable> {
        let outranked = cohort.others(member).into_iter().any(|other| {
            other.generation().is_some() && other.generation() > member.generation()
        });
        (cohort.holds(member) && outranked).then(|| Expendable { doom: Doom::Outranked(member.clone()) })
    }

    #[must_use]
    pub fn unproven(cohort: &Cohort, member: &Member) -> Option<Expendable> {
        (cohort.holds(member) && member.pending() && member.generation().is_none())
            .then(|| Expendable { doom: Doom::Outranked(member.clone()) })
    }

    pub(crate) fn orphaned(managed: Managed) -> Expendable {
        Expendable { doom: Doom::Orphaned(managed) }
    }

    #[must_use]
    pub fn id(&self) -> Vmid {
        match &self.doom {
            Doom::Outranked(member) => member.id(),
            Doom::Orphaned(managed) => managed.id(),
        }
    }

    #[must_use]
    pub fn instance(&self) -> Instance {
        match &self.doom {
            Doom::Outranked(member) => member.instance(),
            Doom::Orphaned(managed) => Instance { id: managed.id(), nix: managed.tags().nix.clone() },
        }
    }

    #[must_use]
    pub fn guest(&self) -> &Guest {
        match &self.doom {
            Doom::Outranked(member) => member.guest(),
            Doom::Orphaned(managed) => managed.guest(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest::{Cores, DiskGib, DurationMs, KindFacts, MemoryMb, Port, Resources, Sockets};
    use crate::observation::{Audited, Grant, Permissions, Settled, Sighting};
    use crate::spec::{BridgeName, Cutover, Hostname, ImageType, KindSpec, ProxySpec, Purity, Timeouts};
    use crate::tags::RawTags;

    const NIX: &str = "78s0iadvjz6s48aqvx4rw78lwrzkjzlw";
    const COMMIT: &str = "66d0ba6b605de2703e0fb7bbf58b922d5b36597e";

    fn spec() -> WorkloadSpec {
        WorkloadSpec {
            name: GuestName(String::from("forgejo")),
            slots: SlotPair::new(Vmid::new(844), Vmid::new(944)).unwrap(),
            image: ImageType(String::from("build-lxc-forgejo")),
            resources: Resources { memory: MemoryMb(2048), disk: DiskGib(16), cores: Cores(2) },
            cutover: Cutover::Overlap,
            purity: Purity::Pure,
            proxy: ProxySpec {
                hostname: Hostname(String::from("git.thesta.rs")),
                service_address: None,
                backend_port: Port(3000),
                tcp_ports: vec![],
                bridge: BridgeName(String::from("vmbr0")),
            },
            timeouts: Timeouts { dhcp: DurationMs(1), health_check: DurationMs(1) },
            kind: KindSpec::Qemu { sockets: Sockets(1) },
        }
    }

    fn sighting(id: u32, name: &str, tags: &str) -> Sighting {
        Sighting::Settled(Settled {
            id: Vmid::new(id),
            name: GuestName(String::from(name)),
            status: GuestStatus::Running,
            tags: RawTags::from(String::from(tags)),
            resources: Resources { memory: MemoryMb(2048), disk: DiskGib(16), cores: Cores(2) },
            facts: KindFacts::Qemu { sockets: Sockets(1) },
        })
    }

    fn managed(slot: &str, generation: Option<u64>) -> String {
        let generation = generation.map_or(String::new(), |g| format!(";gen-{g}"));
        format!("proxnix;nix-{NIX};commit-{COMMIT};slot-{slot}{generation}")
    }

    fn cohort(sightings: Vec<Sighting>) -> Result<Cohort, CohortFault> {
        let observed = Observation::new(Audited::try_from(Permissions { vm_audit: Grant::Granted }).unwrap(), sightings);
        Cohort::gather(&observed, &spec())
    }

    fn member(cohort: &Cohort, id: u32) -> Member {
        cohort.members().iter().find(|member| member.id() == Vmid::new(id)).unwrap().clone()
    }

    #[test]
    fn only_a_guest_outranked_by_a_committed_generation_is_expendable() {
        let pair = cohort(vec![sighting(844, "forgejo", &managed("blue", Some(3))), sighting(944, "forgejo", &managed("green", Some(2)))]).unwrap();
        assert!(Expendable::outranked(&pair, &member(&pair, 844)).is_none(), "the serving guest must never be expendable");
        assert_eq!(Expendable::outranked(&pair, &member(&pair, 944)).map(|doomed| doomed.id()), Some(Vmid::new(944)));

        let fresh = cohort(vec![sighting(844, "forgejo", &managed("blue", Some(1))), sighting(944, "forgejo", &managed("green", None))]).unwrap();
        assert!(Expendable::outranked(&fresh, &member(&fresh, 844)).is_none());
        assert!(Expendable::outranked(&fresh, &member(&fresh, 944)).is_some());

        let legacy = cohort(vec![sighting(844, "forgejo", &managed("blue", None)), sighting(944, "forgejo", &managed("green", None))]).unwrap();
        assert!(legacy.members().iter().all(|member| Expendable::outranked(&legacy, member).is_none()), "no gen outranks no gen");
    }

    #[test]
    fn a_member_of_another_cohort_is_never_expendable_or_promotable_here() {
        let here = cohort(vec![sighting(844, "forgejo", &managed("blue", Some(2)))]).unwrap();
        let elsewhere = cohort(vec![sighting(844, "forgejo", &managed("blue", Some(2))), sighting(944, "forgejo", &managed("green", None))]).unwrap();
        let outsider = member(&elsewhere, 944);
        assert!(Expendable::outranked(&here, &outsider).is_none());
        assert!(Promotion::over(&here, &outsider).is_none());
    }

    #[test]
    fn a_promotion_always_takes_the_next_generation_and_never_re_promotes_the_top() {
        let legacy = cohort(vec![sighting(844, "forgejo", &managed("blue", None))]).unwrap();
        assert_eq!(Promotion::over(&legacy, &member(&legacy, 844)).map(|p| p.generation()), Some(Generation::FIRST));

        let pair = cohort(vec![sighting(844, "forgejo", &managed("blue", Some(7))), sighting(944, "forgejo", &managed("green", Some(3)))]).unwrap();
        assert!(Promotion::over(&pair, &member(&pair, 844)).is_none());
        assert_eq!(Promotion::over(&pair, &member(&pair, 944)).map(|p| p.generation().get()), Some(8));
    }

    #[test]
    fn a_cohort_refuses_strays_malformed_slots_and_repeated_generations() {
        assert_eq!(cohort(vec![sighting(845, "forgejo", &managed("blue", Some(1)))]), Err(CohortFault::Stray(Vmid::new(845))));
        assert_eq!(
            cohort(vec![sighting(944, "whatever", "proxnix;slot-green")]),
            Err(CohortFault::Malformed(Vmid::new(944), TagFault::NoNixHash))
        );
        assert_eq!(
            cohort(vec![sighting(844, "forgejo", &managed("blue", Some(2))), sighting(944, "forgejo", &managed("green", Some(2)))]),
            Err(CohortFault::SameGeneration("2".parse().unwrap()))
        );
    }

    #[test]
    fn unmanaged_guests_and_other_names_are_not_members() {
        let found = cohort(vec![sighting(844, "forgejo", "k3s"), sighting(944, "postgres", &managed("green", Some(1)))]).unwrap();
        assert!(found.members().is_empty());
    }
}
