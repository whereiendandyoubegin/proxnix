#[pure_only]
use crate::effect::Detail;
#[pure_only]
use crate::guest::DurationMs;
#[pure_only]
use crate::spec::ImageType;
#[pure_only]
use crate::tags::{HashFault, NixHash};
use proxnix_pure::pure_only;
#[pure_only]
use std::collections::BTreeMap;

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StoreName(String);

#[pure_only]
impl AsRef<str> for StoreName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StorePath {
    hash: NixHash,
    name: StoreName,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorePathFault {
    NotInStore,
    NoName,
    Hash(HashFault),
    BadName,
}

#[pure_only]
fn store_name_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.' | '_' | '?' | '=')
}

#[pure_only]
impl std::str::FromStr for StorePath {
    type Err = StorePathFault;

    fn from_str(text: &str) -> Result<Self, StorePathFault> {
        let entry = text
            .strip_prefix("/nix/store/")
            .ok_or(StorePathFault::NotInStore)?;
        let (hash, name) = entry.split_once('-').ok_or(StorePathFault::NoName)?;
        let valid_name =
            !name.is_empty() && !name.starts_with('.') && name.chars().all(store_name_character);
        match (hash.parse::<NixHash>(), valid_name) {
            (Err(fault), _) => Err(StorePathFault::Hash(fault)),
            (Ok(_), false) => Err(StorePathFault::BadName),
            (Ok(hash), true) => Ok(StorePath {
                hash,
                name: StoreName(String::from(name)),
            }),
        }
    }
}

#[pure_only]
impl StorePath {
    #[must_use]
    pub fn hash(&self) -> &NixHash {
        &self.hash
    }

    #[must_use]
    pub fn name(&self) -> &StoreName {
        &self.name
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub path: StorePath,
}

#[pure_only]
impl Artifact {
    #[must_use]
    pub fn nix(&self) -> &NixHash {
        self.path.hash()
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitCode(pub i32);

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildFault {
    Checkout(Detail),
    Eval(Option<ExitCode>, Detail),
    Build(Option<ExitCode>, Detail),
    Output(StorePathFault),
    TimedOut(DurationMs),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    pub image: ImageType,
    pub outcome: Result<Artifact, BuildFault>,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Knowledge<'a> {
    Built(&'a Artifact),
    Failed(&'a BuildFault),
    NotBuiltThisRun,
}

#[pure_only]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Images(BTreeMap<ImageType, Result<Artifact, BuildFault>>);

#[pure_only]
impl FromIterator<Built> for Images {
    fn from_iter<I: IntoIterator<Item = Built>>(built: I) -> Images {
        Images(built.into_iter().map(|b| (b.image, b.outcome)).collect())
    }
}

#[pure_only]
impl Images {
    #[must_use]
    pub fn knowledge(&self, image: &ImageType) -> Knowledge<'_> {
        match self.0.get(image) {
            Some(Ok(artifact)) => Knowledge::Built(artifact),
            Some(Err(fault)) => Knowledge::Failed(fault),
            None => Knowledge::NotBuiltThisRun,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATH: &str = "/nix/store/78s0iadvjz6s48aqvx4rw78lwrzkjzlw-nixos-system-forgejo-25.11";

    #[test]
    fn a_store_path_yields_its_nix_hash() {
        let path: StorePath = PATH.parse().unwrap();
        assert_eq!(path.hash().as_ref(), "78s0iadvjz6s48aqvx4rw78lwrzkjzlw");
        assert_eq!(path.name().as_ref(), "nixos-system-forgejo-25.11");
    }

    #[test]
    fn anything_that_is_not_a_store_path_is_rejected() {
        assert_eq!(
            "/tmp/78s0iadvjz6s48aqvx4rw78lwrzkjzlw-x".parse::<StorePath>(),
            Err(StorePathFault::NotInStore)
        );
        assert_eq!(
            "/nix/store/78s0iadvjz6s48aqvx4rw78lwrzkjzlw".parse::<StorePath>(),
            Err(StorePathFault::NoName)
        );
        assert_eq!(
            "/nix/store/abc-x".parse::<StorePath>(),
            Err(StorePathFault::Hash(HashFault::Length))
        );
        assert_eq!(
            "/nix/store/78s0iadvjz6s48aqvx4rw78lwrzkjzlw-a/b".parse::<StorePath>(),
            Err(StorePathFault::BadName)
        );
        assert_eq!(
            "/nix/store/78s0iadvjz6s48aqvx4rw78lwrzkjzlw-.hidden".parse::<StorePath>(),
            Err(StorePathFault::BadName)
        );
    }

    #[test]
    fn an_image_nobody_built_this_run_is_known_to_be_unbuilt() {
        let images: Images = [
            Built {
                image: ImageType(String::from("ok")),
                outcome: Ok(Artifact {
                    path: PATH.parse().unwrap(),
                }),
            },
            Built {
                image: ImageType(String::from("bad")),
                outcome: Err(BuildFault::TimedOut(DurationMs(1))),
            },
        ]
        .into_iter()
        .collect();
        assert!(matches!(
            images.knowledge(&ImageType(String::from("ok"))),
            Knowledge::Built(_)
        ));
        assert!(matches!(
            images.knowledge(&ImageType(String::from("bad"))),
            Knowledge::Failed(BuildFault::TimedOut(_))
        ));
        assert_eq!(
            images.knowledge(&ImageType(String::from("other"))),
            Knowledge::NotBuiltThisRun
        );
    }
}
