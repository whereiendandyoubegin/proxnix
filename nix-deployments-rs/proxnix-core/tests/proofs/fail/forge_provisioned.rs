use proxnix_core::{GuestKind, Provisioned, Vmid};

fn main() {
    let _ = Provisioned { id: Vmid::new(844), kind: GuestKind::Lxc };
}
