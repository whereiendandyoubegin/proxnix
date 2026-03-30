use crate::{
    materialise::Materialise,
    pct::{pct_destroy, pct_list, pct_set_resources, pct_start, pct_stop},
    qm::{qm_destroy, qm_set_resources, qm_start, qm_stop},
    state::{
        enrich_container_info, enrich_cpu_info, list_to_deployed_vm, parse_pct_list, parse_qm_list,
        qm_list,
    },
    types::{
        AppError, ContainerConfig, ContainerFieldChange, DeployedContainer, DeployedVM,
        FieldChange, Outcome, OutcomeKind, Result, SkipReason, VMConfig,
    },
};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

pub trait DeployedState {
    fn id(&self) -> u32;
    fn name(&self) -> &str;
    fn nix_hash(&self) -> Option<&str>;
    fn status(&self) -> &str;
}

impl DeployedState for DeployedVM {
    fn id(&self) -> u32 {
        self.vm_id
    }
    fn name(&self) -> &str {
        &self.vm_name
    }
    fn nix_hash(&self) -> Option<&str> {
        self.nix_hash.as_deref()
    }
    fn status(&self) -> &str {
        &self.status
    }
}

impl DeployedState for DeployedContainer {
    fn id(&self) -> u32 {
        self.ct_id
    }
    fn name(&self) -> &str {
        &self.ct_name
    }
    fn nix_hash(&self) -> Option<&str> {
        self.nix_hash.as_deref()
    }
    fn status(&self) -> &str {
        &self.status
    }
}

pub trait Dangerous {
    fn pre_check(&self) -> Result<()> {
        Ok(())
    }
    fn post_check(&self) -> Result<()> {
        Ok(())
    }
    fn health_check(&self) -> Result<()> {
        Ok(())
    }
}

// VMConfig and ContainerConfig don't need this, this is for when I add storage and network support
impl Dangerous for VMConfig {}

impl Dangerous for ContainerConfig {}

pub trait Deployments: Dangerous + Materialise + Sized + Send + Sync {
    type Deployed: DeployedState + Send + Sync;
    type FieldChange: PartialEq + Clone + Send + Sync;

    fn load_deployed() -> Result<HashMap<String, Self::Deployed>>;

    fn compute_changes(
        &self,
        deployed: &Self::Deployed,
        image_hashes: &HashMap<String, String>,
    ) -> Vec<Self::FieldChange>;
    fn requires_rebuild(changes: &[Self::FieldChange]) -> bool;

    fn id(&self) -> u32;
    fn image_type(&self) -> &str;
    fn is_protected(&self) -> bool;

    fn stop(id: &u32) -> Result<()>;
    fn destroy(id: u32) -> Result<()>;
    fn start(id: u32) -> Result<bool>;
    fn apply_in_place(&self, changes: &[Self::FieldChange]) -> Result<()>;
}

impl Deployments for VMConfig {
    type Deployed = DeployedVM;
    type FieldChange = FieldChange;
    fn load_deployed() -> Result<HashMap<String, Self::Deployed>> {
        let qm_raw = qm_list()?;
        let parsed_qm_list = parse_qm_list(&qm_raw)?;
        let deployed_vms = list_to_deployed_vm(parsed_qm_list);
        let enriched = enrich_cpu_info(deployed_vms)?;

        Ok(enriched.vms)
    }
    fn compute_changes(
        // The logic from what was diff_state
        &self,
        deployed: &Self::Deployed,
        image_hashes: &HashMap<String, String>,
    ) -> Vec<Self::FieldChange> {
        let desired_nix_hash = image_hashes.get(&self.image_type).map(|s| s.as_str());
        let image_changed = desired_nix_hash
            .zip(deployed.nix_hash.as_deref())
            .map(|(desired, deployed)| desired != deployed)
            .unwrap_or(true);
        // This evaluates everything to a tuple like (true, FieldChange::xx)
        // Then the filter_map collects everything
        [
            (self.memory_mb != deployed.mem_mb, FieldChange::Memory),
            (
                self.disk_gb > deployed.bootdisk_gb.round() as u32,
                FieldChange::Disk,
            ),
            (self.cores != deployed.cores, FieldChange::Cores),
            (self.sockets != deployed.sockets, FieldChange::Sockets),
            (image_changed, FieldChange::Image),
        ]
        .into_iter()
        .filter_map(|(changed, field)| changed.then_some(field))
        .collect()
    }
    fn requires_rebuild(changes: &[Self::FieldChange]) -> bool {
        changes
            .iter()
            .any(|s| matches!(s, FieldChange::Image | FieldChange::Disk))
    }
    fn id(&self) -> u32 {
        self.vm_id
    }
    fn image_type(&self) -> &str {
        &self.image_type
    }
    fn is_protected(&self) -> bool {
        self.protected
    }
    fn stop(id: &u32) -> Result<()> {
        qm_stop(id)
    }
    fn destroy(id: u32) -> Result<()> {
        qm_destroy(id)
    }
    fn start(id: u32) -> Result<bool> {
        qm_start(id)
    }
    fn apply_in_place(&self, changes: &[Self::FieldChange]) -> Result<()> {
        qm_set_resources(self.vm_id, self, changes)
    }
}

impl Deployments for ContainerConfig {
    type Deployed = DeployedContainer;
    type FieldChange = ContainerFieldChange;
    fn load_deployed() -> Result<HashMap<String, Self::Deployed>> {
        let pct_raw = pct_list()?;
        let pct_entries = parse_pct_list(&pct_raw)?;
        enrich_container_info(pct_entries)
    }
    fn compute_changes(
        &self,
        deployed: &Self::Deployed,
        image_hashes: &HashMap<String, String>,
    ) -> Vec<Self::FieldChange> {
        let desired_nix_hash = image_hashes.get(&self.image_type).map(|s| s.as_str());
        let image_changed = desired_nix_hash
            .zip(deployed.nix_hash.as_deref())
            .map(|(desired, deployed)| desired != deployed)
            .unwrap_or(true);
        [
            (
                self.memory_mb != deployed.mem_mb,
                ContainerFieldChange::Memory,
            ),
            (
                self.disk_gb > deployed.bootdisk_gb.round() as u32,
                ContainerFieldChange::Disk,
            ),
            (self.cores != deployed.cores, ContainerFieldChange::Cores),
            (image_changed, ContainerFieldChange::Image),
        ]
        .into_iter()
        .filter_map(|(changed, field)| changed.then_some(field))
        .collect()
    }
    fn requires_rebuild(changes: &[Self::FieldChange]) -> bool {
        changes
            .iter()
            .any(|s| matches!(s, ContainerFieldChange::Image | ContainerFieldChange::Disk))
    }
    fn id(&self) -> u32 {
        self.ct_id
    }
    fn image_type(&self) -> &str {
        &self.image_type
    }
    fn is_protected(&self) -> bool {
        self.protected
    }
    fn stop(id: &u32) -> Result<()> {
        pct_stop(id)
    }
    fn destroy(id: u32) -> Result<()> {
        pct_destroy(id)
    }
    fn start(id: u32) -> Result<bool> {
        pct_start(id)
    }
    fn apply_in_place(&self, changes: &[Self::FieldChange]) -> Result<()> {
        pct_set_resources(self.ct_id, self, changes)
    }
}

enum Action<'a, T: Deployments> {
    Create {
        config: &'a T,
    },
    Rebuild {
        config: &'a T,
        deployed_id: u32,
        changes: Vec<T::FieldChange>,
    },
    UpdateInPlace {
        config: &'a T,
        changes: Vec<T::FieldChange>,
    },
    Destroy {
        name: String,
        id: u32,
    },
    Skip {
        name: String,
        reason: SkipReason,
    },
    NoOp {
        name: String,
    },
}

// State refresh and reconcile pipeline
pub fn reconcile<T: Deployments>(
    configs: &[T],
    image_hashes: &HashMap<String, String>,
    repo_path: &str,
    commit_hash: &str,
) -> Result<Vec<Outcome>> {
    let deployed = T::load_deployed()?;
    let actions = plan(configs, &deployed, image_hashes);
    let artifacts = build(&actions, repo_path)?;
    Ok(execute(actions, &artifacts, commit_hash))
}

fn plan<'a, T: Deployments>(
    configs: &'a [T],
    deployed: &HashMap<String, T::Deployed>,
    image_hashes: &HashMap<String, String>,
) -> Vec<Action<'a, T>> {
    let desired: HashSet<&str> = configs.iter().map(|c| c.name()).collect();

    configs
        .iter()
        .map(|c| classify(c, deployed, image_hashes))
        .chain(
            deployed
                .iter()
                .filter(|(n, _)| !desired.contains(n.as_str()))
                .map(|(n, d)| Action::Destroy {
                    name: n.clone(),
                    id: d.id(),
                }),
        )
        .collect()
}

fn classify<'a, T: Deployments>(
    config: &'a T,
    deployed: &HashMap<String, T::Deployed>,
    image_hashes: &HashMap<String, String>,
) -> Action<'a, T> {
    let Some(d) = deployed.get(config.name()) else {
        return Action::Create { config };
    };

    let changes = config.compute_changes(d, image_hashes);

    match (
        changes.is_empty(),
        T::requires_rebuild(&changes),
        config.is_protected(),
    ) {
        (true, _, _) => Action::NoOp {
            name: config.name().into(),
        },
        (_, _, true) => Action::Skip {
            name: config.name().into(),
            reason: SkipReason::Protected,
        },
        (_, true, _) => Action::Rebuild {
            config,
            deployed_id: d.id(),
            changes,
        },
        (_, false, _) => Action::UpdateInPlace { config, changes },
    }
}

fn build<T: Deployments>(
    actions: &[Action<'_, T>],
    repo_path: &str,
) -> Result<HashMap<String, String>> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Create { config } | Action::Rebuild { config, .. } => Some(config),
            _ => None,
        })
        .map(|config| {
            let out_link = Path::new(repo_path).join(config.name()).join("result");
            let image_type_link = Path::new(repo_path).join(config.image_type()).join("result");
            let resolve = |p: &Path| {
                std::fs::canonicalize(p).map_err(|e| {
                    AppError::CmdError(format!(
                        "failed to resolve store path for {}: {}",
                        config.name(),
                        e
                    ))
                })
            };
            let artifact_path = [&out_link, &image_type_link]
                .into_iter()
                .find(|p| p.exists())
                .map(|p| resolve(p))
                .unwrap_or_else(|| {
                    config
                        .nix_build(repo_path, &out_link)
                        .and_then(|_| resolve(&out_link))
                })?;
            Ok((
                config.name().to_string(),
                artifact_path.to_string_lossy().to_string(),
            ))
        })
        .collect()
}

fn do_rebuild<T: Deployments>(
    config: &T,
    deployed_id: u32,
    artifact_path: &str,
    commit_hash: &str,
) -> Result<()> {
    T::stop(&deployed_id)?;
    T::destroy(deployed_id)?;
    config.provision(artifact_path, commit_hash)
}

fn execute<T: Deployments>(
    actions: Vec<Action<'_, T>>,
    artifacts: &HashMap<String, String>,
    commit_hash: &str,
) -> Vec<Outcome> {
    actions
        .into_iter()
        .map(|action| match action {
            Action::Create { config } => {
                let result = config.pre_check()
                    .and_then(|_| {
                        artifacts
                            .get(config.name())
                            .map(|p| config.provision(p, commit_hash))
                            .unwrap_or_else(|| {
                                Err(AppError::CmdError(format!(
                                    "no artifact built for {}",
                                    config.name()
                                )))
                            })
                    })
                    .and_then(|_| config.post_check())
                    .and_then(|_| config.health_check());
                Outcome::new(config.name(), OutcomeKind::Created, result)
            }
            Action::Rebuild {
                config,
                deployed_id,
                changes: _,
            } => {
                let result = config.pre_check()
                    .and_then(|_| {
                        artifacts
                            .get(config.name())
                            .map(|p| do_rebuild::<T>(config, deployed_id, p, commit_hash))
                            .unwrap_or_else(|| {
                                Err(AppError::CmdError(format!(
                                    "no artifact built for {}",
                                    config.name()
                                )))
                            })
                    })
                    .and_then(|_| config.post_check())
                    .and_then(|_| config.health_check());
                Outcome::new(config.name(), OutcomeKind::Rebuilt, result)
            }
            Action::UpdateInPlace { config, changes } => {
                let result = config.pre_check()
                    .and_then(|_| config.apply_in_place(&changes))
                    .and_then(|_| config.post_check())
                    .and_then(|_| config.health_check());
                Outcome::new(config.name(), OutcomeKind::Updated, result)
            }
            Action::Destroy { name, id } => {
                Outcome::new(&name, OutcomeKind::Destroyed, T::destroy(id))
            }
            Action::Skip { name, reason } => {
                Outcome::new(&name, OutcomeKind::Skipped(reason), Ok(()))
            }
            Action::NoOp { name } => Outcome::new(&name, OutcomeKind::NoOp, Ok(())),
        })
        .collect()
}
