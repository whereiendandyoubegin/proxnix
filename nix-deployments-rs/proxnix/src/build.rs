use crate::git::git_ensure_commit;
use crate::materialise::Materialise;
use crate::nix::{BASE_REPO_PATH, configure_dirs, eval_vm_config, nix_build};
use crate::pct::{pct_destroy, pct_start, pct_stop};
use crate::qm::{qm_destroy, qm_set_resources, qm_start, qm_stop};
use crate::state::{full_diff, get_container_statuses, get_vm_statuses, parse_vm_config};
use crate::types::{AppError, FieldChange, Result, StateDiff, UpdateAction};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use tracing::{info, warn};

pub fn nix_store_hash(store_path: &str) -> Option<&str> {
    store_path
        .strip_prefix("/nix/store/")
        .and_then(|s| s.split('-').next())
}

pub fn build_image_types(
    image_type_attrs: &HashMap<String, String>,
    repo_path: &str,
) -> Result<HashMap<String, String>> {
    configure_dirs(image_type_attrs.keys().cloned().collect(), repo_path)?;
    image_type_attrs
        .par_iter()
        .map(|(image_type, build_attr)| -> Result<(String, String)> {
            info!("Building image type '{}' ({})", image_type, build_attr);
            let result_path = nix_build(image_type, build_attr, repo_path)?;
            let canonical = fs::canonicalize(&result_path)?;
            let store_path = canonical.to_string_lossy().to_string();
            info!("Built '{}' -> {}", image_type, store_path);
            Ok((image_type.clone(), store_path))
        })
        .collect::<Result<HashMap<_, _>>>()
}

pub fn run_pipeline(repo_url: &str, commit_hash: &str) -> Result<()> {
    let dest_path = format!("{}/{}", BASE_REPO_PATH, commit_hash);
    info!("Cloning {} at commit {} to {}", repo_url, commit_hash, dest_path);
    git_ensure_commit(repo_url, &dest_path, commit_hash)?;

    let eval = eval_vm_config(&dest_path)?;
    let desired = parse_vm_config(&eval)?;

    let mut image_type_attrs: HashMap<String, String> = HashMap::new();
    for config in desired.vms.values() {
        image_type_attrs
            .entry(config.image_type.clone())
            .or_insert_with(|| config.nix_build_attr().to_string());
    }
    for config in desired.containers.values() {
        image_type_attrs
            .entry(config.image_type.clone())
            .or_insert_with(|| config.nix_build_attr().to_string());
    }

    info!(
        "Building {} image type(s): {:?}",
        image_type_attrs.len(),
        image_type_attrs.keys().collect::<Vec<_>>()
    );
    let built = build_image_types(&image_type_attrs, &dest_path)?;

    let image_hashes: HashMap<String, String> = built
        .iter()
        .filter_map(|(image_type, store_path)| {
            nix_store_hash(store_path).map(|h| (image_type.clone(), h.to_string()))
        })
        .collect();

    let diff = full_diff(&desired, &image_hashes)?;
    info!(
        "Diff: {} VMs to create, {} to update, {} to delete, {} containers to create, {} to delete",
        diff.to_create.len(),
        diff.to_update.len(),
        diff.to_delete.len(),
        diff.to_create_containers.len(),
        diff.to_delete_containers.len(),
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
                FieldChange::Image => format!("image"),
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

    reconcile(diff, built, commit_hash)?;
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

    if !desired.vms.is_empty() {
        let vm_statuses = match get_vm_statuses() {
            Ok(s) => s,
            Err(e) => {
                warn!("Periodic reconcile: failed to get VM statuses: {:?}", e);
                return;
            }
        };
        info!("Periodic reconcile: checking {} managed VMs", desired.vms.len());
        for (name, vm) in &desired.vms {
            match vm_statuses.get(&vm.vm_id).map(|s| s.as_str()) {
                Some("running") => {
                    info!("Periodic reconcile: {} (id: {}) is running", name, vm.vm_id);
                }
                Some(status) => {
                    info!(
                        "Periodic reconcile: {} (id: {}) is {} -> starting",
                        name, vm.vm_id, status
                    );
                    match qm_start(vm.vm_id) {
                        Ok(true) => info!("Periodic reconcile: started VM {}", name),
                        Ok(false) => info!("Periodic reconcile: {} already running", name),
                        Err(e) => warn!("Periodic reconcile: failed to start VM {}: {:?}", name, e),
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

    if !desired.containers.is_empty() {
        let ct_statuses = match get_container_statuses() {
            Ok(s) => s,
            Err(e) => {
                warn!("Periodic reconcile: failed to get container statuses: {:?}", e);
                return;
            }
        };
        info!("Periodic reconcile: checking {} managed containers", desired.containers.len());
        for (name, ct) in &desired.containers {
            match ct_statuses.get(&ct.ct_id).map(|s| s.as_str()) {
                Some("running") => {
                    info!("Periodic reconcile: container {} (id: {}) is running", name, ct.ct_id);
                }
                Some(status) => {
                    info!(
                        "Periodic reconcile: container {} (id: {}) is {} -> starting",
                        name, ct.ct_id, status
                    );
                    match pct_start(ct.ct_id) {
                        Ok(true) => info!("Periodic reconcile: started container {}", name),
                        Ok(false) => info!("Periodic reconcile: container {} already running", name),
                        Err(e) => warn!("Periodic reconcile: failed to start container {}: {:?}", name, e),
                    }
                }
                None => {
                    warn!(
                        "Periodic reconcile: container {} (id: {}) does not exist, will be recreated on next push",
                        name, ct.ct_id
                    );
                }
            }
        }
    }
}

pub fn reconcile(
    diff: StateDiff,
    built: HashMap<String, String>,
    commit_hash: &str,
) -> Result<()> {
    for config in diff.to_create {
        let store_path = built
            .get(&config.image_type)
            .ok_or_else(|| AppError::CmdError(format!(
                "No built image for type '{}' (vm: {})",
                config.image_type, config.name
            )))?;
        config.provision(store_path, commit_hash)?;
    }
    for vm in diff.to_delete {
        info!("Deleting VM {} (id: {})", vm.vm_name, vm.vm_id);
        qm_stop(&vm.vm_id)?;
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
                let store_path = built
                    .get(&actions.config.image_type)
                    .ok_or_else(|| AppError::CmdError(format!(
                        "No built image for type '{}' (vm: {})",
                        actions.config.image_type, actions.name
                    )))?;
                qm_stop(&actions.config.vm_id)?;
                qm_destroy(actions.config.vm_id)?;
                actions.config.provision(store_path, commit_hash)?;
            }
            UpdateAction::Protected => {
                warn!("{} is protected, no action taken", actions.name);
            }
        }
    }
    for container in diff.to_delete_containers {
        info!("Deleting container {} (id: {})", container.ct_name, container.ct_id);
        pct_stop(container.ct_id)?;
        pct_destroy(container.ct_id)?;
        info!("Deleted container {}", container.ct_name);
    }
    for config in diff.to_create_containers {
        let store_path = built
            .get(&config.image_type)
            .ok_or_else(|| AppError::CmdError(format!(
                "No built image for type '{}' (container: {})",
                config.image_type, config.name
            )))?;
        config.provision(store_path, commit_hash)?;
    }
    Ok(())
}
