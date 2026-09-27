use proxnix_pure::pure_only;

#[pure_only]
fn wait() {
    std::thread::sleep(std::time::Duration::from_secs(1));
}

fn main() {}
