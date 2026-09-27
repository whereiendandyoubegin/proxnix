use proxnix_pure::pure_only;

#[pure_only]
struct Name(u8);

#[pure_only]
impl AsRef<u8> for Name {
    fn as_ref(&self) -> &u8 {
        &self.0
    }
}

#[pure_only]
impl Name {
    fn text(&self) -> &str {
        "name"
    }
}

fn main() {}
