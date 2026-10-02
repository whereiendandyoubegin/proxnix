use proxnix_pure::pure_only;

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cores(u16);

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawTags(String);

#[pure_only]
impl From<String> for RawTags {
    fn from(text: String) -> Self {
        RawTags(text)
    }
}

#[pure_only]
impl AsRef<str> for RawTags {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[pure_only]
impl std::str::FromStr for Cores {
    type Err = std::num::ParseIntError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.trim().parse().map(Cores)
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ownership {
    Unmanaged,
    Managed,
}

#[pure_only]
fn ownership(tags: &RawTags) -> Ownership {
    if tags.0.split(';').any(|tag| tag.trim() == "proxnix") {
        Ownership::Managed
    } else {
        Ownership::Unmanaged
    }
}

#[pure_only]
fn total(cores: &[Cores]) -> Cores {
    Cores(cores.iter().map(|c| c.0).sum())
}

#[pure_only]
fn gateway(address: std::net::Ipv4Addr) -> std::net::Ipv4Addr {
    std::net::Ipv4Addr::from(u32::from(address) & 0xffff_ff00 | 1)
}

#[pure_only]
fn scaled(cores: Cores, factor: Cores) -> Cores {
    Cores(cores.0 * factor.0)
}

fn main() {
    assert_eq!(ownership(&RawTags::from(String::from("proxnix;slot-blue"))), Ownership::Managed);
    assert_eq!(total(&[Cores(2), Cores(4)]), Cores(6));
    assert_eq!(scaled(Cores(2), Cores(3)), Cores(6));
    assert_eq!(gateway(std::net::Ipv4Addr::new(10, 0, 0, 7)), std::net::Ipv4Addr::new(10, 0, 0, 1));
    assert_eq!("8".parse::<Cores>(), Ok(Cores(8)));
    assert_eq!(RawTags::from(String::from("proxnix")).as_ref(), "proxnix");
}
