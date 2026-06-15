use crate::{
    build::nix_store_hash,
    materialise::Materialise,
    pct::{pct_destroy, pct_list, pct_set_resources, pct_start, pct_stop},
    qm::{qm_destroy, qm_get_running_ip, qm_set_resources, qm_start, qm_stop},
    sozu::{Proxied, SozuClient, WithIp},
    state::{
        enrich_container_info, enrich_cpu_info, list_to_deployed_vm, parse_pct_list, parse_qm_list,
        qm_list,
    },
    types::{
        AppError, ContainerConfig, ContainerFieldChange, DeployedContainer, DeployedVM,
        FieldChange, Outcome, OutcomeKind, Result, SkipReason, VMConfig,
    },
};
use proxnix_core::{Slot, Workload};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

enum Phase {
    Initial,
    Provisioned { new_id: u32, new_backend_id: String },
    Healthy { new_id: u32, new_backend_id: String, new_ip: String },
    Switched { old_id: u32 },
}

pub struct DeployContext<'a, T: Deployments> {
    config: &'a T,
    deployed_id: u32,
    active_slot: Slot,
    old_backend_id: String,
    old_ip: String,
    sozu_socket_path: &'a str,
    artifact: String,
    commit_hash: &'a str,
    template_cache_path: &'a str,
    phase: Phase,
}

impl<'a, T: Deployments> DeployContext<'a, T>{
    // Provision the new VM without starting it. Old VM still running.
    fn provision_inactive(self, new_backend_id: String) -> Result<Self> {
        self.config.provision_inactive(&self.artifact, self.commit_hash, self.template_cache_path)?;
        Ok(Self {
            phase: Phase::Provisioned { new_id: self.config.id(), new_backend_id },
            ..self
        })
    }    

    fn start_and_check(self) -> Result<Self> {
        let Phase::Provisioned { new_id, new_backend_id } = self.phase else {
            unreachable!("start_and_check called outside of Provisioned phase")
        };
        T::start(new_id)?;
        let new_ip = T::get_ip(new_id)?;
        self.config.post_check()?;
        self.config.health_check()?;
        Ok(Self {
            phase: Phase::Healthy { new_id, new_backend_id, new_ip },
            ..self
        })
    }

    fn switch_sozu(self) -> Result<Self> {
        let Phase::Healthy { ref new_backend_id, ref new_ip, .. } = self.phase else {
            unreachable!("switch_sozu called outside Healthy phase");
        };
        let mut sozu = SozuClient::connect(self.sozu_socket_path)?;
        sozu.register_backend(&WithIp(self.config, new_ip), new_backend_id)?;
        sozu.remove_backend(&WithIp(self.config, &self.old_ip), &self.old_backend_id)?;
        Ok(Self {
            phase: Phase::Switched { old_id: self.deployed_id },
            ..self
        })
    }

    fn destroy_old(self) -> Result<()> {
        let Phase::Switched { old_id } = self.phase else {
            unreachable!("destroy_old called outside Switched phase");
        };
        T::stop(&old_id)?;
        T::destroy(old_id)
    }

    pub fn run(self) -> Result<()> {
        let new_hash = nix_store_hash(&self.artifact)
            .ok_or_else(|| AppError::CmdError("could not get nix hash".to_string()))?;
        let new_backend_id = format!("{}-{}", self.config.name(), new_hash);

        self.provision_inactive(new_backend_id)?
            .start_and_check()?
            .switch_sozu()?
            .destroy_old()
    }



}

pub trait DeployedState {
    fn id(&self) -> u32;
    fn name(&self) -> &str;
    fn nix_hash(&self) -> Option<&str>;
    fn status(&self) -> &str;
    fn active_slot(&self) -> Slot;
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
    fn active_slot(&self) -> Slot {
        self.active_slot
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
    fn active_slot(&self) -> Slot {
        self.active_slot
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

pub trait Deployments: Dangerous + Materialise + Workload + Sized + Send + Sync + Proxied {
    type Deployed: DeployedState + Send + Sync;
    type FieldChange: PartialEq + Clone + Send + Sync;

    fn load_deployed() -> Result<HashMap<String, Self::Deployed>>;

    fn compute_changes(
        &self,
        deployed: &Self::Deployed,
        image_hashes: &HashMap<String, String>,
    ) -> Vec<Self::FieldChange>;
    fn requires_rebuild(changes: &[Self::FieldChange]) -> bool;

    fn image_type(&self) -> &str;
    fn is_protected(&self) -> bool;

    fn get_ip(id: u32) -> Result<String>;
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
    fn image_type(&self) -> &str {
        &self.image_type
    }
    fn is_protected(&self) -> bool {
        self.protected
    }
    fn get_ip(id: u32) -> Result<String> {
        qm_get_running_ip(&id)
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
    fn image_type(&self) -> &str {
        &self.image_type
    }
    fn is_protected(&self) -> bool {
        self.protected
    }
    fn get_ip(id: u32) -> Result<String> {
        let output = std::process::Command::new("pct")
            .arg("exec")
            .arg(id.to_string())
            .arg("--")
            .arg("ip").arg("-4").arg("-o").arg("addr").arg("show").arg("eth0")
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AppError::CmdError(format!(
                "pct exec ip addr failed for container {}: {}",
                id, stderr
            )));
        }

        String::from_utf8(output.stdout)?
            .split_whitespace()
            .skip_while(|s| *s != "inet")
            .nth(1)
            .and_then(|s| s.split('/').next())
            .map(|s| s.to_string())
            .ok_or_else(|| AppError::CmdError(format!("no IPv4 found for container {}", id)))
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
        deployed: &'a T::Deployed,
        deployed_id: u32,
        changes: Vec<T::FieldChange>,
        old_nix_hash: Option<&'a str>,
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

pub fn reconcile<T: Deployments>(
    configs: &[T],
    image_hashes: &HashMap<String, String>,
    pre_built: &HashMap<String, String>,
    image_type_errors: &HashMap<String, String>,
    repo_path: &str,
    commit_hash: &str,
    template_cache_path: &str,
    sozu_socket_path: &str,
) -> Result<Vec<Outcome>> {
    let deployed = T::load_deployed()?;
    let actions = plan(configs, &deployed, image_hashes);
    Ok(actions
        .into_par_iter()
        .map(|action| match action {
            Action::Create { config } => {
                let result = (|| -> Result<()> {
                    config.pre_check()?;
                    let p = get_artifact(config, pre_built, image_type_errors, repo_path)?;
                    let nix_hash = nix_store_hash(&p)
                        .ok_or_else(|| AppError::CmdError("could not get nix hash".to_string()))?;
                    let backend_id = format!("{}-{}", config.name(), nix_hash);
                    config.provision(&p, commit_hash, template_cache_path)?;
                    config.post_check()?;
                    config.health_check()?;
                    let mut sozu = SozuClient::connect(sozu_socket_path)?;
                    sozu.check_sozu_cluster(config)?
                        .register_backend(config, &backend_id)?;
                    Ok(())
                })();
                Outcome::new(config.name(), OutcomeKind::Created, result)
            }
            Action::Rebuild {
                config,
                deployed_id,
                old_nix_hash,
                ..
            } => {
                let result = (|| -> Result<()> {
                    config.pre_check()?;
                    let p = get_artifact(config, pre_built, image_type_errors, repo_path)?;
                    let new_hash = nix_store_hash(&p)
                        .ok_or_else(|| AppError::CmdError("could not get nix hash".to_string()))?;
                    let new_backend_id = format!("{}-{}", config.name(), new_hash);
                    do_rebuild::<T>(config, deployed_id, &p, commit_hash, template_cache_path)?;
                    config.post_check()?;
                    config.health_check()?;
                    let mut sozu = SozuClient::connect(sozu_socket_path)?;
                    sozu.register_backend(config, &new_backend_id)?;
                    if let Some(old_hash) = old_nix_hash {
                        let old_backend_id = format!("{}-{}", config.name(), old_hash);
                        sozu.remove_backend(config, &old_backend_id)?;
                    }
                    Ok(())
                })();
                Outcome::new(config.name(), OutcomeKind::Rebuilt, result)
            }
            Action::UpdateInPlace { config, changes } => {
                let result = config
                    .pre_check()
                    .and_then(|_| config.apply_in_place(&changes))
                    .and_then(|_| config.post_check())
                    .and_then(|_| config.health_check());
                Outcome::new(config.name(), OutcomeKind::Updated, result)
            }
            Action::Destroy { name, id } => {
                let result = (|| -> Result<()> {
                    SozuClient::connect(sozu_socket_path)?.remove_cluster(&name)?;
                    T::stop(&id)?;
                    T::destroy(id)
                })();
                Outcome::new(&name, OutcomeKind::Destroyed, result)
            }
            Action::Skip { name, reason } => {
                Outcome::new(&name, OutcomeKind::Skipped(reason), Ok(()))
            }
            Action::NoOp { name } => Outcome::new(&name, OutcomeKind::NoOp, Ok(())),
        })
        .collect())
}

fn get_artifact<T: Deployments>(
    config: &T,
    pre_built: &HashMap<String, String>,
    image_type_errors: &HashMap<String, String>,
    repo_path: &str,
) -> Result<String> {
    match pre_built.get(config.image_type()) {
        Some(path) => Ok(path.clone()),
        None => match image_type_errors.get(config.image_type()) {
            Some(err) => Err(AppError::CmdError(format!(
                "image type '{}' failed to build: {}",
                config.image_type(),
                err
            ))),
            None => config.nix_build(repo_path),
        },
    }
}

fn plan<'a, T: Deployments>(
    configs: &'a [T],
    deployed: &'a HashMap<String, T::Deployed>,
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
    deployed: &'a HashMap<String, T::Deployed>,
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
            deployed: d,
            deployed_id: d.id(),
            changes,
            old_nix_hash: d.nix_hash(),
        },
        (_, false, _) => Action::UpdateInPlace { config, changes },
    }
}

fn do_rebuild<T: Deployments>(
    config: &T,
    deployed_id: u32,
    artifact_path: &str,
    commit_hash: &str,
    template_cache_path: &str,
) -> Result<()> {
    T::stop(&deployed_id)?;
    T::destroy(deployed_id)?;
    config.provision(artifact_path, commit_hash, template_cache_path)
}
