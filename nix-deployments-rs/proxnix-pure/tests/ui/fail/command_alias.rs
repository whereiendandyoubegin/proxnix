use proxnix_pure::pure_only;

#[pure_only]
fn spawn() -> bool {
    use std::process::Command as Run;
    Run::new("ls").status().is_ok()
}

fn main() {}
