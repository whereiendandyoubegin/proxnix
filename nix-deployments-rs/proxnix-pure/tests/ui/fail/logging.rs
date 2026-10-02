use proxnix_pure::pure_only;

#[pure_only]
fn note() {
    tracing::info!("hello");
}

fn main() {}
