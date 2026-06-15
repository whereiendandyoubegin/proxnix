pub trait Workload {
    fn id(&self) -> u32;
    fn name(&self) -> &str;
    fn memory_mb(&self) -> u32;
    fn cores(&self) -> u16;
    fn ip_for_slot(&self, s: Slot) -> &str;
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
