use crate::context::{ImageType, StorePath};
use crate::nix::{eval_config, nix_build};
use crate::state::parse_config;
use crate::types::Result;
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
                .and_then(StorePath::try_from);
            match &result {
                Ok(path) => info!("Built '{}' -> {}", image_type, path),
                Err(e) => warn!("Failed to build image type '{}': {}", image_type, e),
            }
            (image_type.clone(), result)
        })
        .collect();

    let built = results
        .iter()
        .filter_map(|(k, v)| v.as_ref().ok().map(|p| (k.clone(), p.clone())))
        .collect();
    let errors = results
        .iter()
        .filter_map(|(k, v)| v.as_ref().err().map(|e| (k.clone(), e.to_string())))
        .collect();

    (built, errors)
}

pub fn ensure_vms_running(repo_path: &str) {
    let raw = match eval_config(repo_path) {
        Ok(r) => r,
        Err(e) => {
            warn!("periodic reconcile: failed to eval config: {:?}", e);
            return;
        }
    };
    match parse_config(&raw) {
        Ok(desired) => desired
            .into_workload_groups()
            .iter()
            .for_each(|g| g.ensure_running()),
        Err(e) => warn!("periodic reconcile: failed to parse config: {:?}", e),
    }
}
