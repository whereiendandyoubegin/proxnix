use proxnix_core::{GuestEffect, Vmid};

fn main() {
    let _ = GuestEffect::Retire(Vmid::new(844));
}
