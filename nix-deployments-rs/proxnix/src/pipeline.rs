use std::collections::HashMap;
use tracing::{info, warn};

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use crate::{
    build::{build_image_types, nix_store_hash},
    deployments::{self, Deployments},
    git::git_ensure_commit,
    nix::{BASE_REPO_PATH, eval_config},
    state::parse_config,
    types::{AppError, Outcome},
};

pub struct WorkloadGroup {
    pub image_type_attrs: HashMap<String, String>,
    reconcile_fn: Box<
        dyn Fn(&HashMap<String, String>, &str, &str) -> Result<Vec<Outcome>, AppError>
            + Send
            + Sync,
    >,
}

impl WorkloadGroup {
    pub fn new<T: Deployments + 'static>(configs: Vec<T>) -> Self {
        let image_type_attrs = configs
            .iter()
            .map(|c| (c.image_type().to_string(), c.nix_build_attr().to_string()))
            .collect();
        Self {
            image_type_attrs,
            reconcile_fn: Box::new(move |hashes, repo_path, commit_hash| {
                deployments::reconcile(&configs, hashes, repo_path, commit_hash)
            }),
        }
    }

    pub fn reconcile(
        &self,
        hashes: &HashMap<String, String>,
        repo_path: &str,
        commit_hash: &str,
    ) -> Result<Vec<Outcome>, AppError> {
        (self.reconcile_fn)(hashes, repo_path, commit_hash)
    }
}

pub fn run_pipeline(repo_url: &str, commit_hash: &str) -> Result<(), AppError> {
    let dest_path = format!("{}/{}", BASE_REPO_PATH, commit_hash);
    info!(
        "Cloning {} at commit {} to {}",
        repo_url, commit_hash, dest_path
    );
    git_ensure_commit(repo_url, &dest_path, commit_hash)?;
    let groups = parse_config(&eval_config(&dest_path)?)?.into_workload_groups();
    let image_type_attrs: HashMap<String, String> = groups
        .iter()
        .flat_map(|g| g.image_type_attrs.clone())
        .collect();
    let built = build_image_types(&image_type_attrs, &dest_path)?;
    let image_hashes: HashMap<String, String> = built
        .iter()
        .filter_map(|(k, v)| nix_store_hash(v).map(|h| (k.clone(), h.to_string())))
        .collect();
    let outcomes: Vec<Outcome> = groups
        .par_iter()
        .flat_map(
            |g: &WorkloadGroup| match g.reconcile(&image_hashes, &dest_path, commit_hash) {
                Ok(o) => o,
                Err(e) => {
                    warn!("Reconcile failed: {}", e);
                    vec![]
                }
            },
        )
        .collect();

    outcomes.iter().for_each(|o: &Outcome| match &o.error {
        Some(e) => warn!("{}: {:?} failed: {}", o.name, o.kind, e),
        None => info!("{}: {:?}", o.name, o.kind),
    });
    Ok(())
}
