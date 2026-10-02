use proxnix_core::{GuestEffect, Member};

fn doom(member: Member) -> GuestEffect {
    GuestEffect::Reclaim(member)
}

fn main() {}
