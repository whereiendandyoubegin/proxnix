use crate::git::git_ensure_commit;
use crate::nix::{BASE_REPO_PATH, configure_dirs, eval_vm_config, list_nix_configs, nix_build};
use crate::qm::{
    qm_create, qm_destroy, qm_importdisk, qm_set_agent, qm_set_disk, qm_set_resources, qm_start,
};
use crate::state::{full_diff, get_vm_statuses, parse_vm_config};
use crate::types::{AppError, FieldChange, Result, StateDiff, UpdateAction, VMConfig};
use std::collections::HashMap;
use tracing::{info, warn};

pub fn provision_vm(config: &VMConfig, qcow2_path: &str) -> Result<()> {
    info!("Provisioning VM {} (id: {})", config.name, config.vm_id);
    qm_create(config)?;
    let disk_ref = qm_importdisk(config.vm_id, qcow2_path, &config.storage_location)?;
    qm_set_disk(config.vm_id, &disk_ref, &config.disk_slot)?;
    qm_set_agent(config.vm_id)?;
    info!("VM {} provisioned successfully, starting", config.name);
    qm_start(config.vm_id)?;
    info!("VM {} started", config.name);

    Ok(())
}

pub fn build_all_configs(repo_url: &str, commit_hash: &str) -> Result<HashMap<String, String>> {
    let dest_path = format!("{}/{}", BASE_REPO_PATH, commit_hash);
    info!(
        "Cloning {} at commit {} to {}",
        repo_url, commit_hash, dest_path
    );
    git_ensure_commit(&repo_url, &dest_path, &commit_hash)?;
    let config_names = list_nix_configs(&dest_path)?;
    info!(
        "Found {} nix configs: {:?}",
        config_names.len(),
        config_names
    );
    configure_dirs(config_names.clone(), &dest_path)?;
    let builds = config_names
        .iter()
        .map(|config_name| -> Result<(String, String)> {
            info!("Building nix config: {}", config_name);
            let result_path = nix_build(config_name, &dest_path)?;
            info!("Built {} -> {}", config_name, result_path);
            Ok((config_name.clone(), format!("{}/nixos.qcow2", result_path)))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    Ok(builds)
}

pub fn run_pipeline(repo_url: &str, commit_hash: &str) -> Result<()> {
    let dest_path = format!("{}/{}", BASE_REPO_PATH, commit_hash);
    info!("Building all configs for commit {}", commit_hash);
    let built_configs = build_all_configs(repo_url, commit_hash)?;
    let eval = eval_vm_config(&dest_path)?;
    let parsed = parse_vm_config(&eval)?;
    let diff = full_diff(&parsed)?;
    info!(
        "Diff: {} to create, {} to update, {} to delete",
        diff.to_create.len(),
        diff.to_update.len(),
        diff.to_delete.len()
    );
    for config in &diff.to_create {
        info!("{}: does not exist -> will be created", config.name);
    }
    for vm in &diff.to_delete {
        info!("{}: no longer in config -> will be destroyed", vm.vm_name);
    }
    for update in &diff.to_update {
        let changes: Vec<String> = update
            .changed_fields
            .iter()
            .map(|f| match f {
                FieldChange::Memory => format!("memory"),
                FieldChange::Cores => format!("cores"),
                FieldChange::Sockets => format!("sockets"),
                FieldChange::Disk => format!("disk"),
            })
            .collect();
        match &update.required_action {
            UpdateAction::InPlace => {
                info!("{}: {} changed -> in-place update", update.name, changes.join(", "));
            }
            UpdateAction::Rebuild => {
                info!("{}: {} changed -> full rebuild", update.name, changes.join(", "));
            }
            UpdateAction::Protected => {
                warn!("{}: {} changed but vm is protected -> no action", update.name, changes.join(", "));
            }
        }
    }

    reconcile(diff, built_configs)?;
    info!("Pipeline complete for commit {}", commit_hash);

    Ok(())
}

pub fn ensure_vms_running(repo_path: &str) {
    let raw = match eval_vm_config(repo_path) {
        Ok(r) => r,
        Err(e) => {
            warn!("Periodic reconcile: failed to eval vm config: {:?}", e);
            return;
        }
    };
    let desired = match parse_vm_config(&raw) {
        Ok(d) => d,
        Err(e) => {
            warn!("Periodic reconcile: failed to parse vm config: {:?}", e);
            return;
        }
    };
    if desired.vms.is_empty() {
        info!("Periodic reconcile: no VMs in config");
        return;
    }
    let actual = match get_vm_statuses() {
        Ok(s) => s,
        Err(e) => {
            warn!("Periodic reconcile: failed to get VM statuses: {:?}", e);
            return;
        }
    };
    info!(
        "Periodic reconcile: checking {} managed VMs",
        desired.vms.len()
    );
    for (name, vm) in &desired.vms {
        match actual.get(&vm.vm_id).map(|s| s.as_str()) {
            Some("running") => {
                info!("Periodic reconcile: {} (id: {}) is running", name, vm.vm_id);
            }
            Some(status) => {
                info!(
                    "Periodic reconcile: {} (id: {}) is {} -> starting",
                    name, vm.vm_id, status
                );
                match qm_start(vm.vm_id) {
                    Ok(true) => {
                        info!("Periodic reconcile: started VM {}", name);
                    }
                    Ok(false) => {
                        info!("Periodic reconcile: {} already running", name);
                    }
                    Err(e) => {
                        warn!("Periodic reconcile: failed to start VM {}: {:?}", name, e);
                    }
                }
            }
            None => {
                warn!(
                    "Periodic reconcile: {} (id: {}) does not exist in Proxmox, will be recreated on next push",
                    name, vm.vm_id
                );
            }
        }
    }
}

pub fn reconcile(diff: StateDiff, built_configs: HashMap<String, String>) -> Result<()> {
    for config in diff.to_create {
        let qcow_path = built_configs
            .get(&config.image_type)
            .ok_or(AppError::CmdError(format!(
                "No built image for type '{}' (vm: {})",
                config.image_type, config.name
            )))?;
        provision_vm(&config, qcow_path)?;
    }
    for vm in diff.to_delete {
        info!("Deleting VM {} (id: {})", vm.vm_name, vm.vm_id);
        qm_destroy(vm.vm_id)?;
        info!("Deleted VM {}", vm.vm_name);
    }
    for actions in diff.to_update {
        match &actions.required_action {
            UpdateAction::InPlace => {
                info!("Updating VM {} in place", actions.name);
                qm_set_resources(actions.config.vm_id, &actions)?;
                info!("Updated VM {}", actions.name);
            }
            UpdateAction::Rebuild => {
                info!("Rebuilding VM {} (destroy + provision)", actions.name);
                let qcow_path =
                    built_configs
                        .get(&actions.config.image_type)
                        .ok_or(AppError::CmdError(format!(
                            "No built image for type '{}' (vm: {})",
                            actions.config.image_type, actions.name
                        )))?;
                qm_destroy(actions.config.vm_id)?;
                provision_vm(&actions.config, qcow_path)?;
            }
            UpdateAction::Protected => {
                warn!("{} is protected, no action taken", actions.name);
            }
        }
    }
    Ok(())
}
