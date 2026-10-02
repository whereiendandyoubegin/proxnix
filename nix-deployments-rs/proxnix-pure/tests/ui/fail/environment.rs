use proxnix_pure::pure_only;

#[pure_only]
fn home() -> bool {
    std::env::var_os("HOME").is_some()
}

fn main() {}
