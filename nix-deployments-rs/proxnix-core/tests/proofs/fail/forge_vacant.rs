use proxnix_core::{Vacant, Vmid};

fn main() {
    let _ = Vacant { id: Vmid::new(844) };
}
