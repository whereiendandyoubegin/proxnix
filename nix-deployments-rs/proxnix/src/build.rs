use crate::nix::{eval_config, nix_build};
use crate::pct::pct_start;
use crate::qm::qm_start;
use crate::state::{get_container_statuses, get_vm_statuses, parse_config};
use crate::types::Result;
use rayon::prelude::*;
use std::collections::HashMap;
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
    image_type_attrs
        .par_iter()
        .map(|(image_type, build_attr)| -> Result<(String, String)> {
            info!("Building image type '{}' ({})", image_type, build_attr);
            let store_path = nix_build(image_type, build_attr, repo_path)?;
            info!("Built '{}' -> {}", image_type, store_path);
            Ok((image_type.clone(), store_path))
        })
        .collect::<Result<HashMap<_, _>>>()
}

pub fn ensure_vms_running(repo_path: &str) {
    let raw = match eval_config(repo_path) {
        Ok(r) => r,
        Err(e) => {
            warn!("Periodic reconcile: failed to eval vm config: {:?}", e);
            return;
        }
    };
    let desired = match parse_config(&raw) {
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
