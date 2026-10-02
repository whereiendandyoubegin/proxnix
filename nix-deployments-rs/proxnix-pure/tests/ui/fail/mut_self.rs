use proxnix_pure::pure_only;

#[pure_only]
struct Counter(u8);

#[pure_only]
impl Counter {
    fn bump(&mut self) {
        self.0 += 1;
    }
}

fn main() {}
