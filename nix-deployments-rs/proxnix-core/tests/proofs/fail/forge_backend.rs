use proxnix_core::{Backend, Endpoint, Generation};

fn main() {
    let _ = Backend { endpoint: Endpoint::Primary, generation: Generation::FIRST, nix: todo!(), address: std::net::Ipv4Addr::LOCALHOST };
}
