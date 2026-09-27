use proxnix_pure::pure_only;

#[pure_only]
fn spawn() -> std::process::Output {
    std::process::Command::new("ls").output().unwrap()
}

fn main() {}
