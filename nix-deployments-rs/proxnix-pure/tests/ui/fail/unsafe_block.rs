use proxnix_pure::pure_only;

#[pure_only]
fn raw() -> u8 {
    unsafe { *std::ptr::null::<u8>() }
}

fn main() {}
