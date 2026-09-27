use proxnix_pure::pure_only;

#[pure_only]
fn label(n: u8) -> usize {
    format!("{n}").len()
}

fn main() {}
