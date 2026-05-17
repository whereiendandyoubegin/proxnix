use std::collections::HashMap;
use tracing::{info, warn};

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use crate::{
    build::{build_image_types, nix_store_hash},
    deployments::{self, Deployments},
    git::git_ensure_commit,
    nix::{BASE_REPO_PATH, eval_config},
    state::parse_config,
    types::{AppConfig, Outcome, Result},
};

type ReconcileFn = Box<
    dyn Fn(&HashMap<String, String>, &HashMap<String, String>, &HashMap<String, String>, &str, &str, &str, &str) -> Result<Vec<Outcome>>
        + Send
        + Sync,
>;

pub struct WorkloadGroup {
    pub image_type_attrs: HashMap<String, String>,
    reconcile_fn: ReconcileFn,
}

impl WorkloadGroup {
    pub fn new<T: Deployments + 'static>(configs: Vec<T>) -> Self {
        let image_type_attrs = configs
            .iter()
            .filter(|c| !c.impure())
            .map(|c| (c.image_type().to_string(), c.nix_build_attr().to_string()))
            .collect();
        Self {
            image_type_attrs,
            reconcile_fn: Box::new(move |hashes, pre_built, image_type_errors, repo_path, commit_hash, template_cache_path, sozu_socket_path| {
                deployments::reconcile(&configs, hashes, pre_built, image_type_errors, repo_path, commit_hash, template_cache_path, sozu_socket_path)
            }),
        }
    }

    pub fn reconcile(
        &self,
        hashes: &HashMap<String, String>,
        pre_built: &HashMap<String, String>,
        image_type_errors: &HashMap<String, String>,
        repo_path: &str,
        commit_hash: &str,
        template_cache_path: &str,
        sozu_socket_path: &str,
    ) -> Result<Vec<Outcome>> {
        (self.reconcile_fn)(hashes, pre_built, image_type_errors, repo_path, commit_hash, template_cache_path, sozu_socket_path)
    }
}

pub fn run_pipeline(repo_url: &str, commit_hash: &str, app_config: &AppConfig) -> Result<()> {
    let dest_path = format!("{}/{}", BASE_REPO_PATH, commit_hash);
    info!(
        "Cloning {} at commit {} to {}",
        repo_url, commit_hash, dest_path
    );
    git_ensure_commit(repo_url, &dest_path, commit_hash, &app_config.ssh_key_candidates)?;
    let groups = parse_config(&eval_config(&dest_path)?)?.into_workload_groups();
    let image_type_attrs: HashMap<String, String> = groups
        .iter()
        .flat_map(|g| g.image_type_attrs.clone())
        .collect();
    let (built, image_type_errors) = build_image_types(&image_type_attrs, &dest_path);
    let image_hashes: HashMap<String, String> = built
        .iter()
        .filter_map(|(k, v)| nix_store_hash(v).map(|h| (k.clone(), h.to_string())))
        .collect();
    let outcomes: Vec<Outcome> = groups
        .par_iter()
        .flat_map(
            |g: &WorkloadGroup| match g.reconcile(&image_hashes, &built, &image_type_errors, &dest_path, commit_hash, &app_config.template_cache_path, &app_config.sozu_socket_path) {
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
