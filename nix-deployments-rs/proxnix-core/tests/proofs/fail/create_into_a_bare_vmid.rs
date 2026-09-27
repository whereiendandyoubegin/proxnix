use proxnix_core::{GuestEffect, Vmid};

fn main() {
    let _ = GuestEffect::Create { target: Vmid::new(844), spec: todo!(), tags: todo!() };
}
