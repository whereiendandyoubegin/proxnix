use std::collections::{HashMap, HashSet};
use tracing::{error, info, warn};

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use crate::{
    build::build_image_types,
    context::{BackendPool, CommitHash, ImageType, NixHash, PoolFit, ReconcileContext, RepoPath, SozuSocketPath, TemplateCachePath},
    deployments,
    git::{git_ensure_commit, git_head_commit},
    host_net::{
        AddressesHeld, ServiceBinding, Uniqueness, by_bridge, check_uniqueness, choose_prober,
        ensure_service_addresses,
    },
    materialise::Materialise,
    nix::{BASE_REPO_PATH, eval_config},
    pct::reap_template_cache,
    state::parse_config,
    types::{AppConfig, AppError, ContainerConfig, Outcome, Result, VMConfig},
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

    pub fn service_addresses(&self) -> Vec<ServiceBinding> {
        match self {
            WorkloadGroup::Vms(configs) => configs
                .iter()
                .filter_map(|c| {
                    c.service_address.map(|address| ServiceBinding {
                        bridge: c.network_bridge.clone(),
                        address,
                    })
                })
                .collect(),
            WorkloadGroup::Containers(configs) => configs
                .iter()
                .filter_map(|c| {
                    c.service_address.map(|address| ServiceBinding {
                        bridge: c.network_bridge.clone(),
                        address,
                    })
                })
                .collect(),
        }
    }

    pub fn ensure_running(&self, sozu_socket_path: SozuSocketPath<'_>) {
        match self {
            WorkloadGroup::Vms(configs) => deployments::ensure_running(configs, sozu_socket_path),
            WorkloadGroup::Containers(configs) => {
                deployments::ensure_running(configs, sozu_socket_path)
            }
        }
    }
}

enum RepoSource<'a> {
    Remote { url: &'a str, commit: &'a str },
    Local { path: &'a str },
}

impl<'a> RepoSource<'a> {
    fn resolve(&self, ssh_key_candidates: &[String]) -> Result<(String, String)> {
        match self {
            RepoSource::Local { path } => {
                let commit = git_head_commit(path)?;
                info!("Using local repo at {} (HEAD {})", path, commit);
                Ok((path.to_string(), commit))
            }
            RepoSource::Remote { url, commit } => {
                let dest_path = format!("{}/{}", BASE_REPO_PATH, commit);
                info!("Cloning {} at commit {} to {}", url, commit, dest_path);
                git_ensure_commit(url, &dest_path, commit, ssh_key_candidates)?;
                Ok((dest_path, commit.to_string()))
            }
        }
    }
}

pub fn run_pipeline(repo_url: &str, commit_hash: &str, app_config: &AppConfig) -> Result<()> {
    let source = match app_config.local_repo.as_deref() {
        Some(path) => RepoSource::Local { path },
        None => RepoSource::Remote { url: repo_url, commit: commit_hash },
    };
    run_from(source, app_config)
}

pub fn run_local(app_config: &AppConfig) -> Result<()> {
    match app_config.local_repo.as_deref() {
        Some(path) => run_from(RepoSource::Local { path }, app_config),
        None => Err(AppError::CmdError(
            "--deploy-once needs services.proxnix.local_repo to be set".to_string(),
        )),
    }
}

pub fn hold_service_addresses(
    groups: &[WorkloadGroup],
    backend_pool: Option<&BackendPool>,
) -> Result<()> {
    let bindings: Vec<ServiceBinding> =
        groups.iter().flat_map(|g| g.service_addresses()).collect();

    match check_uniqueness(&bindings) {
        Uniqueness::Clashing { duplicates } => {
            duplicates.iter().for_each(|address| {
                error!("service address {} is declared by more than one workload", address)
            });
            return match duplicates.first() {
                Some(first) => Err(AppError::DuplicateServiceAddress(*first)),
                None => Ok(()),
            };
        }
        Uniqueness::Unique => {}
    }

    if let Some(pool) = backend_pool {
        bindings
            .iter()
            .filter(|b| pool.contains(b.address))
            .for_each(|b| {
                error!(
                    "service address {} sits inside the backend pool {}-{}, so DHCP can hand the same address to a guest",
                    b.address, pool.start, pool.end
                )
            });
    }

    let prober = choose_prober();

    by_bridge(&bindings).iter().for_each(|b| {
        match ensure_service_addresses(prober, &b.bridge, &b.addresses) {
            Err(e) => warn!("could not inspect {}: {}", b.bridge, e),
            Ok(AddressesHeld { added: 0, conflicted: 0, failed: 0, .. }) => {}
            Ok(held) => info!(
                "{}: {} service addresses newly held, {} already held, {} refused because another host answers for them, {} otherwise unclaimed",
                b.bridge, held.added, held.already, held.conflicted, held.failed
            ),
        }
    });

    Ok(())
}

fn run_from(source: RepoSource<'_>, app_config: &AppConfig) -> Result<()> {
    let (dest_path, commit_hash) = source.resolve(&app_config.ssh_key_candidates)?;
    let commit_hash = commit_hash.as_str();
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

    hold_service_addresses(&groups, app_config.backend_pool.as_ref())?;

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

    let live: HashSet<NixHash> = image_hashes.values().cloned().collect();
    match reap_template_cache(app_config.template_cache_path.as_str(), &live) {
        Ok(reaped) if reaped.files > 0 => info!(
            "reaped {} stale container templates, freeing {} MB",
            reaped.files,
            reaped.bytes / 1_048_576
        ),
        Ok(_) => {}
        Err(e) => warn!("could not reap the template cache: {}", e),
    }
    Ok(())
}
