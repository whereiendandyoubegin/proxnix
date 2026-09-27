use proxnix_core::{GuestEffect, ManagedTags};

fn smuggle(tags: ManagedTags) -> GuestEffect {
    GuestEffect::Create { target: todo!(), artifact: todo!(), spec: todo!(), fresh: tags }
}

fn main() {}
