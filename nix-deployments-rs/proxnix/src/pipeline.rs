use tracing::{error, info, warn};

use crate::{
    context::BackendPool,
    git::{git_ensure_commit, git_head_commit},
    host_net::{AddressesHeld, ServiceBinding, Uniqueness, by_bridge, check_uniqueness, choose_prober, ensure_service_addresses},
    types::{AppConfig, AppError, Result},
};

pub enum RepoSource<'a> {
    Remote { url: &'a str, commit: &'a str },
    Local { path: &'a str },
}

impl<'a> RepoSource<'a> {
    pub fn pushed(settings: &'a AppConfig, url: &'a str, commit: &'a str) -> RepoSource<'a> {
        match settings.local_repo.as_deref() {
            Some(path) => RepoSource::Local { path },
            None => RepoSource::Remote { url, commit },
        }
    }

    pub fn resolve(&self, settings: &AppConfig) -> Result<String> {
        match self {
            RepoSource::Local { path } => {
                let commit = git_head_commit(path)?;
                info!("Using local repo at {} (HEAD {})", path, commit);
                Ok((*path).to_string())
            }
            RepoSource::Remote { url, commit } => {
                let dest_path = format!("{}/{commit}", settings.repo_cache);
                info!("Cloning {} at commit {} to {}", url, commit, dest_path);
                git_ensure_commit(url, &dest_path, commit, &settings.ssh_key_candidates)?;
                Ok(dest_path)
            }
        }
    }
}

pub fn hold_service_addresses(bindings: &[ServiceBinding], backend_pool: Option<&BackendPool>, probe_wait: std::time::Duration) -> Result<()> {
    match check_uniqueness(bindings) {
        Uniqueness::Clashing { duplicates } => {
            for address in &duplicates {
                error!("service address {} is declared by more than one workload", address);
            }
            return match duplicates.first() {
                Some(first) => Err(AppError::DuplicateServiceAddress(*first)),
                None => Ok(()),
            };
        }
        Uniqueness::Unique => {}
    }

    if let Some(pool) = backend_pool {
        bindings.iter().filter(|b| pool.contains(b.address)).for_each(|b| {
            error!(
                "service address {} sits inside the backend pool {}-{}, so DHCP can hand the same address to a guest",
                b.address, pool.start, pool.end
            );
        });
    }

    let prober = choose_prober(probe_wait);

    for b in &by_bridge(bindings) {
        match ensure_service_addresses(prober, &b.bridge, &b.addresses) {
            Err(e) => warn!("could not inspect {}: {}", b.bridge, e),
            Ok(AddressesHeld { added: 0, conflicted: 0, failed: 0, .. }) => {}
            Ok(held) => info!(
                "{}: {} service addresses newly held, {} already held, {} refused because another host answers for them, {} otherwise unclaimed",
                b.bridge, held.added, held.already, held.conflicted, held.failed
            ),
        }
    }

    Ok(())
}
