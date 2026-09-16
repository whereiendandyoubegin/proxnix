use crate::context::{ImageType, StorePath};
use crate::nix::{eval_config, nix_build};
use crate::pct::pct_start;
use crate::qm::qm_start;
use crate::state::{get_container_statuses, get_vm_statuses, parse_config};
use crate::types::Result;
use proxnix_core::{Slot, Workload};
use rayon::prelude::*;
use std::collections::HashMap;
use tracing::{info, warn};

pub fn build_image_types(
    image_type_attrs: &HashMap<ImageType, String>,
    repo_path: &str,
) -> (HashMap<ImageType, StorePath>, HashMap<ImageType, String>) {
    let results: Vec<(ImageType, Result<StorePath>)> = image_type_attrs
        .par_iter()
        .map(|(image_type, build_attr)| {
            info!("Building image type '{}' ({})", image_type, build_attr);
            let result = nix_build(image_type.as_str(), build_attr, repo_path)
                .and_then(|raw| StorePath::try_from(raw));
            match &result {
                Ok(path) => info!("Built '{}' -> {}", image_type, path),
                Err(e) => warn!("Failed to build image type '{}': {}", image_type, e),
            }
            (image_type.clone(), result)
        })
        .collect();

    let built = results.iter()
        .filter_map(|(k, v)| v.as_ref().ok().map(|p| (k.clone(), p.clone())))
        .collect();
    let errors = results.iter()
        .filter_map(|(k, v)| v.as_ref().err().map(|e| (k.clone(), e.to_string())))
        .collect();

    (built, errors)
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
            let id = vm.id_for_slot(Slot::Blue).inner();
            match vm_statuses.get(&id).map(|s| s.as_str()) {
                Some("running") => {
                    info!("Periodic reconcile: {} (id: {}) is running", name, id);
                }
                Some(status) => {
                    info!(
                        "Periodic reconcile: {} (id: {}) is {} -> starting",
                        name, id, status
                    );
                    match qm_start(id) {
                        Ok(true) => info!("Periodic reconcile: started VM {}", name),
                        Ok(false) => info!("Periodic reconcile: {} already running", name),
                        Err(e) => warn!("Periodic reconcile: failed to start VM {}: {:?}", name, e),
                    }
                }
                None => {
                    warn!(
                        "Periodic reconcile: {} (id: {}) does not exist in Proxmox",
                        name, id
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
            let id = ct.id_for_slot(Slot::Blue).inner();
            match ct_statuses.get(&id).map(|s| s.as_str()) {
                Some("running") => {
                    info!(
                        "Periodic reconcile: container {} (id: {}) is running",
                        name, id
                    );
                }
                Some(status) => {
                    info!(
                        "Periodic reconcile: container {} (id: {}) is {} -> starting",
                        name, id, status
                    );
                    match pct_start(id) {
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
                        "Periodic reconcile: container {} (id: {}) does not exist",
                        name, id
                    );
                }
            }
        }
    }
}
