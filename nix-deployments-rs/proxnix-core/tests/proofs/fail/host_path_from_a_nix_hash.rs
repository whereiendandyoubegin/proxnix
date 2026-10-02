use proxnix_core::{HostPath, NixHash};

fn main() {
    let nix: NixHash = "78s0iadvjz6s48aqvx4rw78lwrzkjzlw".parse().unwrap();
    let _ = HostPath::from(nix);
}
