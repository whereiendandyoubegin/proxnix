use proxnix_core::{GuestEffect, Vmid};

fn main() {
    let _ = GuestEffect::Undo(Vmid::new(844));
}
