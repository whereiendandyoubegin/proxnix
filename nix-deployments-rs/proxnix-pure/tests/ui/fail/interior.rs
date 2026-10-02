use proxnix_pure::pure_only;

#[pure_only]
fn cell() -> u8 {
    std::cell::RefCell::new(1).into_inner()
}

fn main() {}
