#[pure_only]
use crate::ids::Slot;
use proxnix_pure::pure_only;
#[pure_only]
use std::marker::PhantomData;
#[pure_only]
use std::net::Ipv4Addr;

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TagFault {
    NoNixHash,
    BadNixHash(HashFault),
    NoCommit,
    BadCommit(HashFault),
    NoSlot,
    BadServiceIp,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashFault {
    Length,
    Character,
}

#[pure_only]
pub trait HashFormat {
    const LENGTH: usize;
    fn digit(character: char) -> bool;
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Nix {}

#[pure_only]
impl HashFormat for Nix {
    const LENGTH: usize = 32;

    fn digit(character: char) -> bool {
        matches!(character, '0'..='9' | 'a'..='d' | 'f'..='n' | 'p'..='s' | 'v'..='z')
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Commit {}

#[pure_only]
impl HashFormat for Commit {
    const LENGTH: usize = 40;

    fn digit(character: char) -> bool {
        matches!(character, '0'..='9' | 'a'..='f')
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest<F: HashFormat> {
    text: String,
    format: PhantomData<F>,
}

#[pure_only]
impl<F: HashFormat> std::str::FromStr for Digest<F> {
    type Err = HashFault;

    fn from_str(text: &str) -> Result<Self, HashFault> {
        match (text.len() == F::LENGTH, text.chars().all(F::digit)) {
            (false, _) => Err(HashFault::Length),
            (true, false) => Err(HashFault::Character),
            (true, true) => Ok(Digest { text: String::from(text), format: PhantomData }),
        }
    }
}

#[pure_only]
impl<F: HashFormat> AsRef<str> for Digest<F> {
    fn as_ref(&self) -> &str {
        &self.text
    }
}

#[pure_only]
pub type NixHash = Digest<Nix>;

#[pure_only]
pub type CommitHash = Digest<Commit>;

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTags(String);

#[pure_only]
impl From<String> for RawTags {
    fn from(text: String) -> Self {
        RawTags(text)
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedTags {
    pub nix: NixHash,
    pub commit: CommitHash,
    pub slot: Slot,
    pub service_ip: Option<Ipv4Addr>,
}

#[pure_only]
impl TryFrom<&RawTags> for ManagedTags {
    type Error = TagFault;

    fn try_from(raw: &RawTags) -> Result<ManagedTags, TagFault> {
        let tag = |prefix| raw.0.split(';').map(str::trim).find_map(|tag| tag.strip_prefix(prefix));
        Ok(ManagedTags {
            nix: tag("nix-")
                .filter(|text| !text.is_empty())
                .ok_or(TagFault::NoNixHash)
                .and_then(|text| text.parse().map_err(TagFault::BadNixHash))?,
            commit: tag("commit-")
                .filter(|text| !text.is_empty())
                .ok_or(TagFault::NoCommit)
                .and_then(|text| text.parse().map_err(TagFault::BadCommit))?,
            slot: tag("slot-")
                .ok_or(TagFault::NoSlot)
                .and_then(|slot| Slot::try_from(slot).map_err(|_| TagFault::NoSlot))?,
            service_ip: tag("ip-")
                .map(|ip| ip.parse().map_err(|_| TagFault::BadServiceIp))
                .transpose()?,
        })
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ownership {
    Unmanaged,
    Managed(ManagedTags),
    Malformed(TagFault),
}

#[pure_only]
impl From<&RawTags> for Ownership {
    fn from(raw: &RawTags) -> Ownership {
        if raw.0.split(';').map(str::trim).any(|tag| tag == "proxnix") {
            ManagedTags::try_from(raw).map_or_else(Ownership::Malformed, Ownership::Managed)
        } else {
            Ownership::Unmanaged
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NIX: &str = "78s0iadvjz6s48aqvx4rw78lwrzkjzlw";
    const COMMIT: &str = "66d0ba6b605de2703e0fb7bbf58b922d5b36597e";
    const FORGEJO: &str = "commit-66d0ba6b605de2703e0fb7bbf58b922d5b36597e;ip-192.168.1.214;nix-78s0iadvjz6s48aqvx4rw78lwrzkjzlw;proxnix;slot-blue";

    fn ownership(text: &str) -> Ownership {
        Ownership::from(&RawTags::from(String::from(text)))
    }

    fn managed(slot: &str) -> String {
        format!("proxnix;nix-{NIX};commit-{COMMIT};slot-{slot}")
    }

    #[test]
    fn a_nix_hash_is_32_characters_of_nix_base32() {
        assert_eq!(NIX.parse::<NixHash>().map(|h| String::from(h.as_ref())), Ok(String::from(NIX)));
        assert_eq!("78s0iadv".parse::<NixHash>(), Err(HashFault::Length));
        assert_eq!("e8s0iadvjz6s48aqvx4rw78lwrzkjzlw".parse::<NixHash>(), Err(HashFault::Character));
        assert_eq!("78s0iadvjz6s48aqvx4rw78lwrzkjzlu".parse::<NixHash>(), Err(HashFault::Character));
    }

    #[test]
    fn a_commit_hash_is_40_lowercase_hex_characters() {
        assert!(COMMIT.parse::<CommitHash>().is_ok());
        assert_eq!("66d0ba6b".parse::<CommitHash>(), Err(HashFault::Length));
        assert_eq!(
            "66D0BA6B605DE2703E0FB7BBF58B922D5B36597E".parse::<CommitHash>(),
            Err(HashFault::Character)
        );
    }

    #[test]
    fn tags_proxmox_returned_for_forgejo_parse_as_managed() {
        assert_eq!(
            ownership(FORGEJO),
            Ownership::Managed(ManagedTags {
                nix: NIX.parse().unwrap(),
                commit: COMMIT.parse().unwrap(),
                slot: Slot::Blue,
                service_ip: Some(Ipv4Addr::new(192, 168, 1, 214)),
            })
        );
    }

    #[test]
    fn a_guest_without_the_proxnix_tag_is_unmanaged() {
        assert_eq!(ownership(""), Ownership::Unmanaged);
        assert_eq!(ownership(&managed("blue").replace("proxnix;", "")), Ownership::Unmanaged);
        assert_eq!(ownership("proxnixish;k3s"), Ownership::Unmanaged);
    }

    #[test]
    fn a_managed_guest_missing_a_tag_is_malformed_not_defaulted() {
        assert_eq!(ownership(&format!("proxnix;commit-{COMMIT};slot-blue")), Ownership::Malformed(TagFault::NoNixHash));
        assert_eq!(ownership(&format!("proxnix;nix-{NIX};slot-blue")), Ownership::Malformed(TagFault::NoCommit));
        assert_eq!(ownership(&format!("proxnix;nix-{NIX};commit-{COMMIT}")), Ownership::Malformed(TagFault::NoSlot));
        assert_eq!(ownership(&managed("purple")), Ownership::Malformed(TagFault::NoSlot));
        assert_eq!(
            ownership(&format!("{};ip-999.1.1.1", managed("green"))),
            Ownership::Malformed(TagFault::BadServiceIp)
        );
    }

    #[test]
    fn a_managed_guest_with_a_badly_formed_hash_is_malformed() {
        assert_eq!(
            ownership(&format!("proxnix;nix-abc;commit-{COMMIT};slot-blue")),
            Ownership::Malformed(TagFault::BadNixHash(HashFault::Length))
        );
        assert_eq!(
            ownership(&format!("proxnix;nix-{NIX};commit-deadbeef;slot-blue")),
            Ownership::Malformed(TagFault::BadCommit(HashFault::Length))
        );
    }

    #[test]
    fn an_empty_value_after_a_prefix_is_missing() {
        assert_eq!(ownership(&format!("proxnix;nix-;commit-{COMMIT};slot-blue")), Ownership::Malformed(TagFault::NoNixHash));
    }

    #[test]
    fn tag_order_and_whitespace_do_not_matter() {
        assert_eq!(
            ownership(&format!(" slot-green ; proxnix ;nix-{NIX}; commit-{COMMIT}")),
            Ownership::Managed(ManagedTags {
                nix: NIX.parse().unwrap(),
                commit: COMMIT.parse().unwrap(),
                slot: Slot::Green,
                service_ip: None,
            })
        );
    }
}
