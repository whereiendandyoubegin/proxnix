use proxnix_pure::pure_only;

#[pure_only]
fn exists() -> bool {
    std::fs::metadata("/").is_ok()
}

fn main() {}
