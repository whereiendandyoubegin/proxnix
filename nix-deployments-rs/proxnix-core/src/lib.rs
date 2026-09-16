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
