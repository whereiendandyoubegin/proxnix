pub trait Workload {
    fn id(&self) -> u32;
    fn name(&self) -> &str;
    fn memory_mb(&self) -> u32;
    fn cores(&self) -> u16;
}
