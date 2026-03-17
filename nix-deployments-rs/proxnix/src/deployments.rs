use crate::{
    materialise::Materialise,
    pct::{pct_destroy, pct_list, pct_set_resources, pct_start, pct_stop},
    qm::{qm_destroy, qm_set_resources, qm_start, qm_stop},
    state::{
        enrich_container_info, enrich_cpu_info, list_to_deployed_vm, parse_pct_list, parse_qm_list,
        qm_list,
    },
    types::{
        ContainerConfig, ContainerFieldChange, DeployedContainer, DeployedVM, FieldChange,
        OutcomeKind, Result, SkipReason, VMConfig,
    },
};
use std::collections::{HashMap, HashSet};

pub trait Deployments: Materialise + Sized + Send + Sync {
    type Deployed: Send + Sync;
    type FieldChange: PartialEq + Clone + Send + Sync;

    fn load_deployed() -> Result<HashMap<String, Self::Deployed>>;

    fn deployed_id(d: &Self::Deployed) -> u32;
    fn deployed_name(d: &Self::Deployed) -> &str;
    fn deployed_nix_hash(d: &Self::Deployed) -> Option<&str>;
    fn deployed_status(d: &Self::Deployed) -> &str;

    fn compute_changes(
        &self,
        deployed: &Self::Deployed,
        image_hashes: &HashMap<String, String>,
    ) -> Vec<Self::FieldChange>;
    fn requires_rebuild(changes: &[Self::FieldChange]) -> bool;

    fn id(&self) -> u32;
    fn name(&self) -> &str;
    fn image_type(&self) -> &str;
    fn is_protected(&self) -> bool;

    fn stop(id: &u32) -> Result<String>;
    fn destroy(id: u32) -> Result<String>;
    fn start(id: u32) -> Result<bool>;
    fn apply_in_place(&self, changes: &[Self::FieldChange]) -> Result<String>;
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
    fn deployed_id(d: &Self::Deployed) -> u32 {
        d.vm_id
    }
    fn deployed_name(d: &Self::Deployed) -> &str {
        &d.vm_name
    }
    fn deployed_nix_hash(d: &Self::Deployed) -> Option<&str> {
        d.nix_hash.as_deref()
    }
    fn deployed_status(d: &Self::Deployed) -> &str {
        &d.status
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
    fn name(&self) -> &str {
        &self.name
    }
    fn image_type(&self) -> &str {
        &self.image_type
    }
    fn is_protected(&self) -> bool {
        self.protected
    }
    fn stop(id: &u32) -> Result<String> {
        qm_stop(id)
    }
    fn destroy(id: u32) -> Result<String> {
        qm_destroy(id)
    }
    fn start(id: u32) -> Result<bool> {
        qm_start(id)
    }
    fn apply_in_place(&self, changes: &[Self::FieldChange]) -> Result<String> {
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
    fn deployed_id(d: &Self::Deployed) -> u32 {
        d.ct_id
    }
    fn deployed_name(d: &Self::Deployed) -> &str {
        &d.ct_name
    }
    fn deployed_nix_hash(d: &Self::Deployed) -> Option<&str> {
        d.nix_hash.as_deref()
    }
    fn deployed_status(d: &Self::Deployed) -> &str {
        &d.status
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
    fn name(&self) -> &str {
        &self.name
    }
    fn image_type(&self) -> &str {
        &self.image_type
    }
    fn is_protected(&self) -> bool {
        self.protected
    }
    fn stop(id: &u32) -> Result<String> {
        pct_stop(id)
    }
    fn destroy(id: u32) -> Result<String> {
        pct_destroy(id)
    }
    fn start(id: u32) -> Result<bool> {
        pct_start(id)
    }
    fn apply_in_place(&self, changes: &[Self::FieldChange]) -> Result<String> {
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
) -> Result<Vec<Outcome>> {
    T::load_deployed()
        .map(|deployed| plan(configs, &deployed, image_hashes))
        .map(execute)
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
                    id: T::deployed_id(d),
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
            name: config.name.into(),
            reason: SkipReason::Protected,
        },
        (_, true, _) => Action::Rebuild {
            config,
            deployed_id: T::deployed_id(d),
            changes,
        },
        (_, false, _) => Action::UpdateInPlace { config, changes },
    }
}

fn execute<T: Deployments>(actions: Vec<Action<'_, T>>) -> Vec<Outcome> {
    actions.into_iter().map(|action| match action {
        Action::Create { config } => {
            Outcome::new(config.name(), OutcomeKind::Created, do_create(config))
        }
        Action::Rebuild {
            config,
            deployed_id,
            changes,
        } => Outcome::new(config.name(), OutcomeKind::Rebuilt, do_rebui),
    })
}
