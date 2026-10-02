use proxnix_pure::pure_only;

#[pure_only]
fn bump(value: &mut u8) {
    *value += 1;
}

fn main() {}
