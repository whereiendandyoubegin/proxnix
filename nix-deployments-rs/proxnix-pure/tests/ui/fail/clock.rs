use proxnix_pure::pure_only;

#[pure_only]
fn now() -> std::time::Duration {
    std::time::Instant::now().elapsed()
}

fn main() {}
