use proxnix_pure::pure_only;

#[pure_only]
fn label(n: u8) -> usize {
    n.to_string().len()
}

fn main() {}
