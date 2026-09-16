use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;

use proxnix_core::{Slot, SlotId, Workload};
use rayon::prelude::*;

use crate::{
    context::{BackendId, NixHash, ReconcileContext, StorePath},
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

enum Phase {
    Initial,
    Provisioned { target: SlotId, new_backend_id: BackendId },
    Healthy { new_backend_id: BackendId, new_ip: Ipv4Addr },
    BackendRegistered,
}

pub struct DeployContext<'a, T: Deployments> {
    config: &'a T,
    new_slot: Slot,
    old_backend_id: Option<BackendId>,
    old_ip: Option<Ipv4Addr>,
    old_slot_id: Option<SlotId>,
    sozu: SozuClient,
    artifact: StorePath,
    commit_hash: &'a str,
    template_cache_path: &'a str,
    phase: Phase,
}

impl<'a, T: Deployments> DeployContext<'a, T> {
    fn from_create(
        config: &'a T,
        artifact: StorePath,
        commit_hash: &'a str,
        template_cache_path: &'a str,
        sozu_socket_path: &str,
    ) -> Result<Self> {
        let sozu = SozuClient::connect(sozu_socket_path)?;
        Ok(Self {
            config,
            new_slot: Slot::Blue,
            old_backend_id: None,
            old_ip: None,
            old_slot_id: None,
            sozu,
            artifact,
            commit_hash,
            template_cache_path,
            phase: Phase::Initial,
        })
    }

    fn from_rebuild(
        config: &'a T,
        deployed_slot: Slot,
        deployed_id: u32,
        old_nix_hash: Option<&NixHash>,
        artifact: StorePath,
        commit_hash: &'a str,
        template_cache_path: &'a str,
        sozu_socket_path: &str,
    ) -> Result<Self> {
        let new_slot = deployed_slot.switch_slot();
        let old_ip: Ipv4Addr = config.ip_for_slot(deployed_slot).parse()?;
        let old_backend_id = old_nix_hash.map(|h| BackendId::new(config.name(), h));
        let old_slot_id = match deployed_slot {
            Slot::Blue => SlotId::Blue(deployed_id),
            Slot::Green => SlotId::Green(deployed_id),
        };
        let sozu = SozuClient::connect(sozu_socket_path)?;
        Ok(Self {
            config,
            new_slot,
            old_backend_id,
            old_ip: Some(old_ip),
            old_slot_id: Some(old_slot_id),
            sozu,
            artifact,
            commit_hash,
            template_cache_path,
            phase: Phase::Initial,
        })
    }

    fn provision_inactive(self) -> Result<Self> {
        let target = self.config.id_for_slot(self.new_slot);
        let new_hash = self.artifact.nix_hash().ok_or_else(|| {
            AppError::CmdError(format!("could not extract nix hash from {}", self.artifact))
        })?;
        let new_backend_id = BackendId::new(self.config.name(), &new_hash);
        self.config.provision_inactive(&self.artifact, self.commit_hash, self.template_cache_path, target)?;
        Ok(Self {
            phase: Phase::Provisioned { target, new_backend_id },
            ..self
        })
    }

    fn start_and_check(self) -> Result<Self> {
        let Self { phase, config, new_slot, old_backend_id, old_ip, old_slot_id, sozu, artifact, commit_hash, template_cache_path } = self;
        let (target, new_backend_id) = match phase {
            Phase::Provisioned { target, new_backend_id } => (target, new_backend_id),
            _ => unreachable!("start_and_check called outside Provisioned phase"),
        };
        T::start(target.inner())?;
        let new_ip: Ipv4Addr = T::get_ip(target.inner())?.parse()?;
        config.post_check()?;
        config.health_check()?;
        Ok(Self {
            config,
            new_slot,
            old_backend_id,
            old_ip,
            old_slot_id,
            sozu,
            artifact,
            commit_hash,
            template_cache_path,
            phase: Phase::Healthy { new_backend_id, new_ip },
        })
    }

    fn register_and_switch(self) -> Result<Self> {
        let Self { phase, config, new_slot, old_backend_id, old_ip, old_slot_id, mut sozu, artifact, commit_hash, template_cache_path } = self;
        let (new_backend_id, new_ip) = match phase {
            Phase::Healthy { new_backend_id, new_ip, .. } => (new_backend_id, new_ip),
            _ => unreachable!("register_and_switch called outside Healthy phase"),
        };
        sozu.check_sozu_cluster(config)?
            .register_backend(&WithIp(config, new_ip), &new_backend_id)?;
        if let (Some(old_bid), Some(old_ip_val)) = (old_backend_id.as_ref(), old_ip) {
            sozu.remove_backend(&WithIp(config, old_ip_val), old_bid)?;
        }
        Ok(Self {
            config,
            new_slot,
            old_backend_id,
            old_ip,
            old_slot_id,
            sozu,
            artifact,
            commit_hash,
            template_cache_path,
            phase: Phase::BackendRegistered,
        })
    }

    fn maybe_destroy_old(self) -> Result<()> {
        let Phase::BackendRegistered = self.phase else {
            unreachable!("maybe_destroy_old called outside BackendRegistered phase")
        };
        match self.old_slot_id {
            Some(slot_id) => {
                T::stop(&slot_id.inner())?;
                T::destroy(slot_id.inner())
            }
            None => Ok(()),
        }
    }

    pub fn run(self) -> Result<()> {
        self.config.pre_check()?;
        self.provision_inactive()?
            .start_and_check()?
            .register_and_switch()?
            .maybe_destroy_old()
    }
}

pub trait DeployedState {
    fn id(&self) -> u32;
    fn name(&self) -> &str;
    fn nix_hash(&self) -> Option<&NixHash>;
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
    fn nix_hash(&self) -> Option<&NixHash> {
        self.nix_hash.as_ref()
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
    fn nix_hash(&self) -> Option<&NixHash> {
        self.nix_hash.as_ref()
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

impl Dangerous for VMConfig {}
impl Dangerous for ContainerConfig {}

pub trait Deployments: Dangerous + Materialise + Workload + Sized + Send + Sync + Proxied {
    type Deployed: DeployedState + Send + Sync;
    type FieldChange: PartialEq + Clone + Send + Sync;

    fn load_deployed() -> Result<HashMap<String, Self::Deployed>>;

    fn compute_changes(
        &self,
        deployed: &Self::Deployed,
        image_hashes: &HashMap<crate::context::ImageType, NixHash>,
    ) -> Vec<Self::FieldChange>;
    fn requires_rebuild(changes: &[Self::FieldChange]) -> bool;

    fn image_type(&self) -> &str;
    fn is_protected(&self) -> bool;

    fn get_ip(id: u32) -> Result<String>;
    fn stop(id: &u32) -> Result<()>;
    fn destroy(id: u32) -> Result<()>;
    fn start(id: u32) -> Result<bool>;
    fn apply_in_place(&self, deployed_id: u32, changes: &[Self::FieldChange]) -> Result<()>;
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
        &self,
        deployed: &Self::Deployed,
        image_hashes: &HashMap<crate::context::ImageType, NixHash>,
    ) -> Vec<Self::FieldChange> {
        let desired_nix_hash = image_hashes.get(self.image_type());
        let image_changed = desired_nix_hash
            .zip(deployed.nix_hash.as_ref())
            .map(|(desired, deployed)| desired != deployed)
            .unwrap_or(true);
        [
            (self.memory_mb != deployed.mem_mb, FieldChange::Memory),
            (self.disk_gb > deployed.bootdisk_gb.round() as u32, FieldChange::Disk),
            (self.cores != deployed.cores, FieldChange::Cores),
            (self.sockets != deployed.sockets, FieldChange::Sockets),
            (image_changed, FieldChange::Image),
        ]
        .into_iter()
        .filter_map(|(changed, field)| changed.then_some(field))
        .collect()
    }
    fn requires_rebuild(changes: &[Self::FieldChange]) -> bool {
        changes.iter().any(|s| matches!(s, FieldChange::Image | FieldChange::Disk))
    }
    fn image_type(&self) -> &str {
        self.image_type.as_str()
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
    fn apply_in_place(&self, deployed_id: u32, changes: &[Self::FieldChange]) -> Result<()> {
        qm_set_resources(deployed_id, self, changes)
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
        image_hashes: &HashMap<crate::context::ImageType, NixHash>,
    ) -> Vec<Self::FieldChange> {
        let desired_nix_hash = image_hashes.get(self.image_type());
        let image_changed = desired_nix_hash
            .zip(deployed.nix_hash.as_ref())
            .map(|(desired, deployed)| desired != deployed)
            .unwrap_or(true);
        [
            (self.memory_mb != deployed.mem_mb, ContainerFieldChange::Memory),
            (self.disk_gb > deployed.bootdisk_gb.round() as u32, ContainerFieldChange::Disk),
            (self.cores != deployed.cores, ContainerFieldChange::Cores),
            (image_changed, ContainerFieldChange::Image),
        ]
        .into_iter()
        .filter_map(|(changed, field)| changed.then_some(field))
        .collect()
    }
    fn requires_rebuild(changes: &[Self::FieldChange]) -> bool {
        changes.iter().any(|s| matches!(s, ContainerFieldChange::Image | ContainerFieldChange::Disk))
    }
    fn image_type(&self) -> &str {
        self.image_type.as_str()
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
    fn apply_in_place(&self, deployed_id: u32, changes: &[Self::FieldChange]) -> Result<()> {
        pct_set_resources(deployed_id, self, changes)
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
    },
    UpdateInPlace {
        config: &'a T,
        deployed_id: u32,
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
    ctx: &ReconcileContext<'_>,
) -> Result<Vec<Outcome>> {
    let deployed = T::load_deployed()?;
    let actions = plan(configs, &deployed, ctx.image_hashes);
    Ok(actions
        .into_par_iter()
        .map(|action| match action {
            Action::Create { config } => {
                let result = (|| -> Result<()> {
                    let artifact = get_artifact(config, ctx)?;
                    DeployContext::from_create(
                        config,
                        artifact,
                        ctx.commit_hash.as_str(),
                        ctx.template_cache_path.as_str(),
                        ctx.sozu_socket_path.as_str(),
                    )?.run()
                })();
                Outcome::new(config.name(), OutcomeKind::Created, result)
            }
            Action::Rebuild { config, deployed, deployed_id } => {
                let result = (|| -> Result<()> {
                    let artifact = get_artifact(config, ctx)?;
                    DeployContext::from_rebuild(
                        config,
                        deployed.active_slot(),
                        deployed_id,
                        deployed.nix_hash(),
                        artifact,
                        ctx.commit_hash.as_str(),
                        ctx.template_cache_path.as_str(),
                        ctx.sozu_socket_path.as_str(),
                    )?.run()
                })();
                Outcome::new(config.name(), OutcomeKind::Rebuilt, result)
            }
            Action::UpdateInPlace { config, deployed_id, changes } => {
                let result = config
                    .pre_check()
                    .and_then(|_| config.apply_in_place(deployed_id, &changes))
                    .and_then(|_| config.post_check())
                    .and_then(|_| config.health_check());
                Outcome::new(config.name(), OutcomeKind::Updated, result)
            }
            Action::Destroy { name, id } => {
                let result = (|| -> Result<()> {
                    let mut sozu = SozuClient::connect(ctx.sozu_socket_path.as_str())?;
                    sozu.remove_cluster(&name)?;
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
    ctx: &ReconcileContext<'_>,
) -> Result<StorePath> {
    match ctx.pre_built.get(config.image_type()) {
        Some(path) => Ok(path.clone()),
        None => match ctx.image_type_errors.get(config.image_type()) {
            Some(err) => Err(AppError::CmdError(format!(
                "image type '{}' failed to build: {}",
                config.image_type(),
                err
            ))),
            None => config.nix_build(ctx.repo_path.as_str()),
        },
    }
}

fn plan<'a, T: Deployments>(
    configs: &'a [T],
    deployed: &'a HashMap<String, T::Deployed>,
    image_hashes: &HashMap<crate::context::ImageType, NixHash>,
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
    image_hashes: &HashMap<crate::context::ImageType, NixHash>,
) -> Action<'a, T> {
    let Some(d) = deployed.get(config.name()) else {
        return Action::Create { config };
    };

    let changes = config.compute_changes(d, image_hashes);

    match (changes.is_empty(), T::requires_rebuild(&changes), config.is_protected()) {
        (true, _, _) => Action::NoOp { name: config.name().into() },
        (_, _, true) => Action::Skip { name: config.name().into(), reason: SkipReason::Protected },
        (_, true, _) => Action::Rebuild {
            config,
            deployed: d,
            deployed_id: d.id(),
        },
        (_, false, _) => Action::UpdateInPlace { config, deployed_id: d.id(), changes },
    }
}
