use proxnix_pure::pure_only;

#[pure_only]
fn length(text: &str) -> usize {
    text.len()
}

fn main() {}
