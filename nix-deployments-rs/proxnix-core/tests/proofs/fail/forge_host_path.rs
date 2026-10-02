use proxnix_core::{HostPath, PathPart};

fn main() {
    let _ = HostPath(vec![PathPart(String::from("ZFS"))]);
}
