use proxnix_pure::pure_only;

#[pure_only]
fn total(values: &[u8]) -> u8 {
    let mut sum = 0;
    for value in values {
        sum += value;
    }
    sum
}

fn main() {}
