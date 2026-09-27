use proxnix_pure::pure_only;

#[pure_only]
struct Name(u8);

#[pure_only]
impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn main() {}
