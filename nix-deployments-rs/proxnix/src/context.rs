use std::{borrow::Borrow, collections::HashMap, fmt};

use crate::types::{AppError, Result};

/// The hash segment extracted from a nix store path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct NixHash(String);

impl NixHash {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NixHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<&str> for NixHash {
    type Error = AppError;
    fn try_from(s: &str) -> Result<Self> {
        if s.is_empty() {
            Err(AppError::CmdError("nix hash cannot be empty".to_string()))
        } else {
            Ok(NixHash(s.to_string()))
        }
    }
}

/// A full path to a nix build output (e.g. /nix/store/abc123...-name).
#[derive(Debug, Clone)]
pub struct StorePath(String);

impl StorePath {
    pub fn nix_hash(&self) -> Option<NixHash> {
        self.0
            .strip_prefix("/nix/store/")
            .and_then(|s| s.split('-').next())
            .map(|s| NixHash(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StorePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<String> for StorePath {
    type Error = AppError;
    fn try_from(s: String) -> Result<Self> {
        if s.starts_with("/nix/store/") {
            Ok(StorePath(s))
        } else {
            Err(AppError::CmdError(format!(
                "not a valid nix store path: {}",
                s
            )))
        }
    }
}

/// Sozu backend identifier constructed from a cluster name and its nix hash.
#[derive(Debug, Clone, PartialEq)]
pub struct BackendId(String);

impl BackendId {
    pub fn new(cluster_name: &str, hash: &NixHash) -> Self {
        Self(format!("{}-{}", cluster_name, hash.as_str()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BackendId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The image type name used as the key in build maps.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ImageType(String);

impl ImageType {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ImageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for ImageType {
    fn from(s: String) -> Self {
        ImageType(s)
    }
}

impl From<&str> for ImageType {
    fn from(s: &str) -> Self {
        ImageType(s.to_string())
    }
}

impl AsRef<str> for ImageType {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

// Allows HashMap<ImageType, _>::get(&str) without cloning
impl Borrow<str> for ImageType {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// A git commit hash, borrowed from its owner.
#[derive(Debug, Clone, Copy)]
pub struct CommitHash<'a>(&'a str);

impl<'a> CommitHash<'a> {
    pub fn as_str(self) -> &'a str {
        self.0
    }
}

impl<'a> TryFrom<&'a str> for CommitHash<'a> {
    type Error = AppError;
    fn try_from(s: &'a str) -> Result<Self> {
        if s.is_empty() {
            Err(AppError::CmdError("commit hash cannot be empty".to_string()))
        } else {
            Ok(CommitHash(s))
        }
    }
}

/// Path to the sozu control socket, borrowed from app config.
#[derive(Debug, Clone, Copy)]
pub struct SozuSocketPath<'a>(&'a str);

impl<'a> SozuSocketPath<'a> {
    pub fn as_str(self) -> &'a str {
        self.0
    }
}

impl<'a> TryFrom<&'a str> for SozuSocketPath<'a> {
    type Error = AppError;
    fn try_from(s: &'a str) -> Result<Self> {
        if s.is_empty() {
            Err(AppError::CmdError(
                "sozu socket path cannot be empty".to_string(),
            ))
        } else {
            Ok(SozuSocketPath(s))
        }
    }
}

/// Path to the template cache directory, borrowed from app config.
#[derive(Debug, Clone, Copy)]
pub struct TemplateCachePath<'a>(&'a str);

impl<'a> TemplateCachePath<'a> {
    pub fn as_str(self) -> &'a str {
        self.0
    }
}

impl<'a> TryFrom<&'a str> for TemplateCachePath<'a> {
    type Error = AppError;
    fn try_from(s: &'a str) -> Result<Self> {
        if s.is_empty() {
            Err(AppError::CmdError(
                "template cache path cannot be empty".to_string(),
            ))
        } else {
            Ok(TemplateCachePath(s))
        }
    }
}

/// Path to the cloned nix repository, borrowed from the pipeline.
#[derive(Debug, Clone, Copy)]
pub struct RepoPath<'a>(&'a str);

impl<'a> RepoPath<'a> {
    pub fn as_str(self) -> &'a str {
        self.0
    }
}

impl<'a> TryFrom<&'a str> for RepoPath<'a> {
    type Error = AppError;
    fn try_from(s: &'a str) -> Result<Self> {
        if s.is_empty() {
            Err(AppError::CmdError("repo path cannot be empty".to_string()))
        } else {
            Ok(RepoPath(s))
        }
    }
}

/// All inputs needed for a reconcile pass, fully typed.
pub struct ReconcileContext<'a> {
    /// Nix hash for each successfully built image type.
    pub image_hashes: &'a HashMap<ImageType, NixHash>,
    /// Store path for each successfully built image type.
    pub pre_built: &'a HashMap<ImageType, StorePath>,
    /// Build errors keyed by image type.
    pub image_type_errors: &'a HashMap<ImageType, String>,
    pub repo_path: RepoPath<'a>,
    pub commit_hash: CommitHash<'a>,
    pub template_cache_path: TemplateCachePath<'a>,
    pub sozu_socket_path: SozuSocketPath<'a>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL_PATH: &str = "/nix/store/0lmgpzmhq0d1yrpnl7fxpgnkqkgnxdq7-nixos-disk-image";

    #[test]
    fn store_path_extracts_its_hash() {
        let p = StorePath::try_from(REAL_PATH.to_string()).unwrap();
        assert_eq!(
            p.nix_hash().unwrap().as_str(),
            "0lmgpzmhq0d1yrpnl7fxpgnkqkgnxdq7"
        );
    }

    #[test]
    fn non_store_paths_are_rejected() {
        assert!(StorePath::try_from("/tmp/nixos.qcow2".to_string()).is_err());
        assert!(StorePath::try_from(String::new()).is_err());
    }

    #[test]
    fn backend_id_is_name_and_hash() {
        let hash = NixHash::try_from("abc123").unwrap();
        assert_eq!(
            BackendId::new("test-website", &hash).as_str(),
            "test-website-abc123"
        );
    }

    #[test]
    fn backend_id_differs_per_hash() {
        let old = BackendId::new("site", &NixHash::try_from("aaa").unwrap());
        let new = BackendId::new("site", &NixHash::try_from("bbb").unwrap());
        assert_ne!(old, new);
    }

    #[test]
    fn empty_nix_hash_is_rejected() {
        assert!(NixHash::try_from("").is_err());
    }

    #[test]
    fn image_type_looks_up_by_str() {
        let map = HashMap::from([(
            ImageType::from("build-qcow2-website"),
            NixHash::try_from("deadbeef").unwrap(),
        )]);
        assert!(map.get("build-qcow2-website").is_some());
        assert!(map.get("build-qcow2-other").is_none());
    }

    #[test]
    fn empty_borrowed_paths_are_rejected() {
        assert!(CommitHash::try_from("").is_err());
        assert!(SozuSocketPath::try_from("").is_err());
        assert!(TemplateCachePath::try_from("").is_err());
        assert!(RepoPath::try_from("").is_err());
    }
}
