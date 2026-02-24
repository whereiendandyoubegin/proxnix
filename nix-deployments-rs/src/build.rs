use crate::git::git_ensure_commit;
use crate::nix::{BASE_REPO_PATH, configure_dirs, list_nix_configs, nix_build};
use crate::qm::{
    qm_create, qm_destroy, qm_importdisk, qm_set_agent, qm_set_disk, qm_set_resources,
};
use crate::state::{full_diff, save_deployed_state, update_deployed_state_commit};
use crate::types::{AppError, Result, StateDiff, UpdateAction, VMConfig, VMUpdate};
use std::collections::HashMap;

pub fn provision_vm(config: &VMConfig, qcow2_path: &str) -> Result<()> {
    qm_create(config)?;
    let disk_ref = qm_importdisk(config.vm_id, qcow2_path, &config.storage_location)?;
    qm_set_disk(config.vm_id, &disk_ref, &config.disk_slot)?;
    qm_set_agent(config.vm_id)?;

    Ok(())
}

pub fn build_all_configs(repo_url: &str, commit_hash: &str) -> Result<HashMap<String, String>> {
    let dest_path = format!("{}/{}", BASE_REPO_PATH, commit_hash);
    git_ensure_commit(&repo_url, &dest_path, &commit_hash)?;
    let config_names = list_nix_configs(&dest_path)?;
    configure_dirs(&commit_hash, config_names.clone(), &dest_path)?;
    let builds = config_names
        .iter()
        .map(|config_name| -> Result<(String, String)> {
            let result_path = nix_build(config_name, &dest_path, commit_hash)?;
            Ok((config_name.clone(), format!("{}/nixos.qcow2", result_path)))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    Ok(builds)
}
