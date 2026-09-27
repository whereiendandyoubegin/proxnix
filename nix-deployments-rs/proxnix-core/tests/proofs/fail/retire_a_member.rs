use proxnix_core::{GuestEffect, Member};

fn doom(member: Member) -> GuestEffect {
    GuestEffect::Retire(member)
}

fn main() {}
