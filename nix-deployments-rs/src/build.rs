use crate::git::git_ensure_commit;
use crate::nix::{BASE_REPO_PATH, configure_dirs, list_nix_configs, nix_build};
use crate::qm::{
    qm_create, qm_destroy, qm_importdisk, qm_set_agent, qm_set_disk, qm_set_resources,
};
use crate::state::{full_diff, save_deployed_state, update_deployed_state_commit};
use crate::types::{
    AppError, RebuildStrategy, Result, StateDiff, UpdateAction, VMConfig, VMUpdate,
};
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

pub fn reconcile(diff: StateDiff, built_configs: HashMap<String, String>) -> Result<()> {
    for config in diff.to_create {
        let qcow_path = built_configs
            .get(&config.name)
            .ok_or(AppError::CmdError("Could not create new VM".to_string()))?;
        provision_vm(&config, qcow_path)?;
    }
    for vm in diff.to_delete {
        qm_destroy(vm.vm_id)?;
    }
    for actions in diff.to_update {
        match &actions.required_action {
            UpdateAction::InPlace => {
                qm_set_resources(actions.config.vm_id, &actions)?;
            }
            UpdateAction::Rebuild => {
                let qcow_path = built_configs
                    .get(&actions.config.name)
                    .ok_or(AppError::CmdError("Could not create new VM".to_string()))?;
                qm_destroy(actions.config.vm_id)?;
                provision_vm(&actions.config, qcow_path)?;
            }
            UpdateAction::Protected => {
                // TODO I need to log this properly
                let message = format!("{} is protected! No action can be taken", actions.name);
                println!("{}", message);
            }
        }
    }
    Ok(())
}
