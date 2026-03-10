use crate::types::Result;

pub trait Materialise {
    fn nix_build_attr(&self) -> &str;
    fn provision(&self, artifact_path: &str, commit_hash: &str) -> Result<()>;
}
