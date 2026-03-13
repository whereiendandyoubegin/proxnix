use crate::{
    state::{enrich_cpu_info, list_to_deployed_vm, parse_qm_list, qm_list},
    types::{DeployedVM, FieldChange, VMConfig},
};
use std::collections::HashMap;

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

    fn stop(id: u32) -> Result<()>;
    fn destroy(id: u32) -> Result<()>;
    fn start(id: u32) -> Result<()>;
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
    fn deployed_id(d: &Self::Deployed) -> u32 {
        d.vm_id
    }
    fn deployed_name(d: &Self::Deployed) -> &str {
        &d.vm_name
    }
    fn deployed_nix_hash(d: &Self::Deployed) -> Option<&str> {
        d.&nix_hash
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
            (self.disk_gb > deployed.bootdisk_gb.round() as u32, FieldChange::Disk)
            (self.cores != deployed.cores, FieldChange::Cores)
            (self.sockets != deployed.sockets, FieldChange::Sockets)
            (image_changed, FieldChange::Image)
        ]
        .into_iter()
        .filter_map(|changed, field| changed.then_some(field))
        .collect()
    }
}
