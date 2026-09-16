use std::fmt;

pub trait Workload {
    fn name(&self) -> &str;
    fn memory_mb(&self) -> u32;
    fn cores(&self) -> u16;
    fn ip_for_slot(&self, s: Slot) -> &str;
    fn id_for_slot(&self, s: Slot) -> SlotId;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum Slot {
    Blue,
    Green,
}

impl Slot {
    pub fn switch_slot(self) -> Self {
        match self {
            Slot::Blue => Slot::Green,
            Slot::Green => Slot::Blue,
        }
    }
}

impl TryFrom<&str> for Slot {
    type Error = String;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "slot-blue" | "blue" => Ok(Slot::Blue),
            "slot-green" | "green" => Ok(Slot::Green),
            other => Err(format!("unknown slot tag: {}", other)),
        }
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Slot::Blue => write!(f, "slot-blue"),
            Slot::Green => write!(f, "slot-green"),
        }
    }
}

/// A VM or container identity that carries which slot it belongs to.
/// Guarantees at compile time that a Blue identity cannot be confused with a Green one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum SlotId {
    Blue(u32),
    Green(u32),
}

impl SlotId {
    pub fn inner(self) -> u32 {
        match self {
            SlotId::Blue(id) | SlotId::Green(id) => id,
        }
    }

    pub fn slot(self) -> Slot {
        match self {
            SlotId::Blue(_) => Slot::Blue,
            SlotId::Green(_) => Slot::Green,
        }
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
    fn slot_round_trips_through_its_tag() {
        for slot in [Slot::Blue, Slot::Green] {
            let tag = slot.to_string();
            assert_eq!(Slot::try_from(tag.as_str()), Ok(slot));
        }
    }

    #[test]
    fn slot_tag_format_is_prefixed() {
        assert_eq!(Slot::Blue.to_string(), "slot-blue");
        assert_eq!(Slot::Green.to_string(), "slot-green");
    }

    #[test]
    fn unknown_slot_tag_is_rejected() {
        assert!(Slot::try_from("slot-purple").is_err());
        assert!(Slot::try_from("nix-abc123").is_err());
        assert!(Slot::try_from("").is_err());
    }

    #[test]
    fn slot_id_carries_its_slot() {
        assert_eq!(SlotId::Blue(823).slot(), Slot::Blue);
        assert_eq!(SlotId::Green(824).slot(), Slot::Green);
        assert_eq!(SlotId::Blue(823).inner(), 823);
        assert_eq!(SlotId::Green(824).inner(), 824);
    }

    #[test]
    fn same_id_in_different_slots_is_not_equal() {
        assert_ne!(SlotId::Blue(823), SlotId::Green(823));
    }
}
