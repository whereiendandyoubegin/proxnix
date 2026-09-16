use std::collections::HashMap;
use tracing::{info, warn};

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use crate::{
    build::build_image_types,
    context::{CommitHash, ImageType, NixHash, PoolFit, ReconcileContext, RepoPath, SozuSocketPath, TemplateCachePath},
    deployments,
    git::git_ensure_commit,
    materialise::Materialise,
    nix::{BASE_REPO_PATH, eval_config},
    state::parse_config,
    types::{AppConfig, ContainerConfig, Outcome, Result, VMConfig},
};

pub enum WorkloadGroup {
    Vms(Vec<VMConfig>),
    Containers(Vec<ContainerConfig>),
}

impl WorkloadGroup {
    pub fn image_type_attrs(&self) -> HashMap<ImageType, String> {
        match self {
            WorkloadGroup::Vms(configs) => configs
                .iter()
                .filter(|c| !c.impure())
                .map(|c| (c.image_type.clone(), c.nix_build_attr().to_string()))
                .collect(),
            WorkloadGroup::Containers(configs) => configs
                .iter()
                .filter(|c| !c.impure())
                .map(|c| (c.image_type.clone(), c.nix_build_attr().to_string()))
                .collect(),
        }
    }

    pub fn reconcile(&self, ctx: &ReconcileContext<'_>) -> Result<Vec<Outcome>> {
        match self {
            WorkloadGroup::Vms(configs) => deployments::reconcile(configs, ctx),
            WorkloadGroup::Containers(configs) => deployments::reconcile(configs, ctx),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            WorkloadGroup::Vms(configs) => configs.len(),
            WorkloadGroup::Containers(configs) => configs.len(),
        }
    }

    pub fn ensure_running(&self) {
        match self {
            WorkloadGroup::Vms(configs) => deployments::ensure_running(configs),
            WorkloadGroup::Containers(configs) => deployments::ensure_running(configs),
        }
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

    let service_count = groups.iter().map(|g| g.len() as u32).sum::<u32>();
    match app_config.backend_pool.as_ref().map(|p| p.fits(service_count)) {
        Some(PoolFit::TooSmall { capacity, required }) => warn!(
            "backend pool holds {} addresses but {} services need {} to deploy; a rebuild may have nowhere to place its new instance",
            capacity, service_count, required
        ),
        Some(PoolFit::Sufficient) | None => {}
    }

    let image_type_attrs: HashMap<ImageType, String> = groups
        .iter()
        .flat_map(|g| g.image_type_attrs())
        .collect();

    let (built, image_type_errors) = build_image_types(&image_type_attrs, &dest_path);

    let image_hashes: HashMap<ImageType, NixHash> = built
        .iter()
        .filter_map(|(k, v)| v.nix_hash().map(|h| (k.clone(), h)))
        .collect();

    let ctx = ReconcileContext {
        image_hashes: &image_hashes,
        pre_built: &built,
        image_type_errors: &image_type_errors,
        repo_path: RepoPath::try_from(dest_path.as_str())?,
        commit_hash: CommitHash::try_from(commit_hash)?,
        template_cache_path: TemplateCachePath::try_from(app_config.template_cache_path.as_str())?,
        sozu_socket_path: SozuSocketPath::try_from(app_config.sozu_socket_path.as_str())?,
        backend_pool: app_config.backend_pool.as_ref(),
    };

    let outcomes: Vec<Outcome> = groups
        .par_iter()
        .flat_map(|g| match g.reconcile(&ctx) {
            Ok(o) => o,
            Err(e) => {
                warn!("Reconcile failed: {}", e);
                vec![]
            }
        })
        .collect();

    outcomes.iter().for_each(|o: &Outcome| match &o.error {
        Some(e) => warn!("{}: {:?} failed: {}", o.name, o.kind, e),
        None => info!("{}: {:?}", o.name, o.kind),
    });
    Ok(())
}
