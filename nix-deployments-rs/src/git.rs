use crate::types::{AppError, Result};
use git2::{Oid, Repository};

pub fn git_clone(repo_url: &str, dest_path: &str) -> Result<Repository> {
    let repo = Repository::clone(repo_url, dest_path)
        .map_err(|e| AppError::GitError(e.to_string()))?;
    Ok(repo)   
}

pub fn git_checkout(repo: &Repository, commit_hash: &str) -> Result<()> {
    let commit_oid = Oid::from_str(commit_hash)?;
    let _commit = repo.find_commit(commit_oid)?;
    repo.set_head_detached(commit_oid)?;
    repo.checkout_head(None)?;
    Ok(())
}

pub fn git_ensure_commit(repo_url: &str, dest_path: &str, commit_hash: &str) -> Result<Repository> {
    let repo = git_clone(repo_url, dest_path)?;
    git_checkout(&repo, commit_hash)?;

    Ok(repo)
}
