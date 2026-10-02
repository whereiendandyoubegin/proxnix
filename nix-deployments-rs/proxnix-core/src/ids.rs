use proxnix_pure::pure_only;

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Deserialize, serde::Serialize)]
pub enum Slot {
    Blue,
    Green,
}

#[pure_only]
impl Slot {
    #[must_use]
    pub fn switch_slot(self) -> Self {
        match self {
            Slot::Blue => Slot::Green,
            Slot::Green => Slot::Blue,
        }
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownSlot;

#[pure_only]
impl TryFrom<&str> for Slot {
    type Error = UnknownSlot;

    fn try_from(text: &str) -> Result<Self, UnknownSlot> {
        match text {
            "slot-blue" | "blue" => Ok(Slot::Blue),
            "slot-green" | "green" => Ok(Slot::Green),
            _ => Err(UnknownSlot),
        }
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct Vmid(u32);

#[pure_only]
impl Vmid {
    #[must_use]
    pub const fn new(id: u32) -> Self {
        Vmid(id)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[pure_only]
impl From<u32> for Vmid {
    fn from(id: u32) -> Self {
        Vmid(id)
    }
}

#[pure_only]
impl std::str::FromStr for Vmid {
    type Err = std::num::ParseIntError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Vmid)
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum SlotId {
    Blue(Vmid),
    Green(Vmid),
}

#[pure_only]
impl SlotId {
    #[must_use]
    pub fn inner(self) -> Vmid {
        match self {
            SlotId::Blue(id) | SlotId::Green(id) => id,
        }
    }

    #[must_use]
    pub fn slot(self) -> Slot {
        match self {
            SlotId::Blue(_) => Slot::Blue,
            SlotId::Green(_) => Slot::Green,
        }
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SameIdInBothSlots(pub Vmid);

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotPair {
    blue: Vmid,
    green: Vmid,
}

#[pure_only]
impl SlotPair {
    pub fn new(blue: Vmid, green: Vmid) -> Result<SlotPair, SameIdInBothSlots> {
        if blue == green {
            Err(SameIdInBothSlots(blue))
        } else {
            Ok(SlotPair { blue, green })
        }
    }

    #[must_use]
    pub fn id(self, slot: Slot) -> SlotId {
        match slot {
            Slot::Blue => SlotId::Blue(self.blue),
            Slot::Green => SlotId::Green(self.green),
        }
    }

    #[must_use]
    pub fn slot_of(self, id: Vmid) -> Option<Slot> {
        match id {
            blue if blue == self.blue => Some(Slot::Blue),
            green if green == self.green => Some(Slot::Green),
            _ => None,
        }
    }

    #[must_use]
    pub fn both(self) -> [SlotId; 2] {
        [SlotId::Blue(self.blue), SlotId::Green(self.green)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_slot_alternates() {
        assert_eq!(Slot::Blue.switch_slot(), Slot::Green);
        assert_eq!(Slot::Green.switch_slot(), Slot::Blue);
    }

    #[test]
    fn switch_slot_is_an_involution() {
        assert_eq!(Slot::Blue.switch_slot().switch_slot(), Slot::Blue);
        assert_eq!(Slot::Green.switch_slot().switch_slot(), Slot::Green);
    }

    #[test]
    fn slot_tags_are_read_with_or_without_their_prefix() {
        assert_eq!(Slot::try_from("slot-blue"), Ok(Slot::Blue));
        assert_eq!(Slot::try_from("green"), Ok(Slot::Green));
    }

    #[test]
    fn unknown_slot_tag_is_rejected() {
        assert_eq!(Slot::try_from("slot-purple"), Err(UnknownSlot));
        assert_eq!(Slot::try_from("nix-abc123"), Err(UnknownSlot));
        assert_eq!(Slot::try_from(""), Err(UnknownSlot));
    }

    #[test]
    fn slot_id_carries_its_slot() {
        assert_eq!(SlotId::Blue(Vmid::new(823)).slot(), Slot::Blue);
        assert_eq!(SlotId::Green(Vmid::new(824)).slot(), Slot::Green);
        assert_eq!(SlotId::Blue(Vmid::new(823)).inner(), Vmid::new(823));
    }

    #[test]
    fn same_id_in_different_slots_is_not_equal() {
        assert_ne!(SlotId::Blue(Vmid::new(823)), SlotId::Green(Vmid::new(823)));
    }

    #[test]
    fn a_slot_pair_rejects_one_id_in_both_slots() {
        assert_eq!(SlotPair::new(Vmid::new(844), Vmid::new(844)), Err(SameIdInBothSlots(Vmid::new(844))));
    }

    #[test]
    fn a_slot_pair_knows_which_slot_an_id_is_in() {
        let pair = SlotPair::new(Vmid::new(844), Vmid::new(944)).unwrap();
        assert_eq!(pair.slot_of(Vmid::new(944)), Some(Slot::Green));
        assert_eq!(pair.slot_of(Vmid::new(845)), None);
        assert_eq!(pair.id(Slot::Blue), SlotId::Blue(Vmid::new(844)));
    }
}
