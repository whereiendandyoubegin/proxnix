use crate::deployments;
use crate::git::git_ensure_commit;
use crate::materialise::Materialise;
use crate::nix::{BASE_REPO_PATH, configure_dirs, eval_vm_config, nix_build};
use crate::pct::pct_start;
use crate::qm::qm_start;
use crate::state::{get_container_statuses, get_vm_statuses, parse_vm_config};
use crate::types::{AppError, ContainerConfig, Result, VMConfig};
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
    info!(
        "Cloning {} at commit {} to {}",
        repo_url, commit_hash, dest_path
    );
    git_ensure_commit(repo_url, &dest_path, commit_hash)?;

    let eval = eval_config(&dest_path)?;
    let desired = parse_config(&eval)?;

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

    let vms: Vec<VMConfig> = desired.vms.into_values().collect();
    let containers: Vec<ContainerConfig> = desired.containers.into_values().collect();

    let vm_result = deployments::reconcile(&vms, &image_hashes, &dest_path, commit_hash);
    let ct_result = deployments::reconcile(&containers, &image_hashes, &dest_path, commit_hash);

    let vm_outcomes = match vm_result {
        Ok(outcomes) => outcomes,
        Err(e) => {
            warn!("VM reconcile failed: {}", e);
            vec![]
        }
    };
    let ct_outcomes = match ct_result {
        Ok(outcomes) => outcomes,
        Err(e) => {
            warn!("Container reconcile failed: {}", e);
            vec![]
        }
    };

    for outcome in vm_outcomes.iter().chain(ct_outcomes.iter()) {
        match &outcome.error {
            Some(e) => warn!("{}: {:?} failed: {}", outcome.name, outcome.kind, e),
            None => info!("{}: {:?}", outcome.name, outcome.kind),
        }
    }

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
        info!(
            "Periodic reconcile: checking {} managed VMs",
            desired.vms.len()
        );
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
                warn!(
                    "Periodic reconcile: failed to get container statuses: {:?}",
                    e
                );
                return;
            }
        };
        info!(
            "Periodic reconcile: checking {} managed containers",
            desired.containers.len()
        );
        for (name, ct) in &desired.containers {
            match ct_statuses.get(&ct.ct_id).map(|s| s.as_str()) {
                Some("running") => {
                    info!(
                        "Periodic reconcile: container {} (id: {}) is running",
                        name, ct.ct_id
                    );
                }
                Some(status) => {
                    info!(
                        "Periodic reconcile: container {} (id: {}) is {} -> starting",
                        name, ct.ct_id, status
                    );
                    match pct_start(ct.ct_id) {
                        Ok(true) => info!("Periodic reconcile: started container {}", name),
                        Ok(false) => {
                            info!("Periodic reconcile: container {} already running", name)
                        }
                        Err(e) => warn!(
                            "Periodic reconcile: failed to start container {}: {:?}",
                            name, e
                        ),
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
