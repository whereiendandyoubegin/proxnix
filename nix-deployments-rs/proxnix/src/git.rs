use crate::types::{AppError, Result};
use git2::{FetchOptions, Oid, RemoteCallbacks, Repository, build::RepoBuilder};
use std::path::Path;
use tracing::info;

const SSH_KEY_CANDIDATES: &[&str] = &[
    "/root/.ssh/id_ed25519",
    "/root/.ssh/id_rsa",
    "/root/.ssh/id_ecdsa",
];

fn find_ssh_key() -> Option<&'static str> {
    SSH_KEY_CANDIDATES.iter().copied().find(|p| Path::new(p).exists())
}

pub fn git_clone(repo_url: &str, dest_path: &str) -> Result<Repository> {
    info!("Cloning {} to {}", repo_url, dest_path);
    let key_path = find_ssh_key()
        .ok_or_else(|| AppError::GitError("No SSH private key found in /root/.ssh/".to_string()))?;
    info!("Using SSH key: {}", key_path);
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |_url, username, _allowed| {
        git2::Cred::ssh_key(
            username.unwrap_or("git"),
            None,
            Path::new(key_path),
            None,
        )
    });
    let mut fetch_opts = FetchOptions::new();
    fetch_opts.remote_callbacks(callbacks);
    let mut builder = RepoBuilder::new();
    builder.fetch_options(fetch_opts);
    let repo = builder
        .clone(repo_url, Path::new(dest_path))
        .map_err(|e| AppError::GitError(e.to_string()))?;
    info!("Clone complete: {}", dest_path);
    Ok(repo)
}

pub fn git_checkout(repo: &Repository, commit_hash: &str) -> Result<()> {
    info!("Checking out commit {}", commit_hash);
    let commit_oid = Oid::from_str(commit_hash)?;
    let _commit = repo.find_commit(commit_oid)?;
    repo.set_head_detached(commit_oid)?;
    repo.checkout_head(None)?;
    info!("Checkout complete");
    Ok(())
}

pub fn git_ensure_commit(repo_url: &str, dest_path: &str, commit_hash: &str) -> Result<Repository> {
    let repo = if Path::new(dest_path).exists() {
        info!("Repo already exists at {}, opening", dest_path);
        Repository::open(dest_path).map_err(|e| AppError::GitError(e.to_string()))?
    } else {
        git_clone(repo_url, dest_path)?
    };
    git_checkout(&repo, commit_hash)?;

    Ok(repo)
}
