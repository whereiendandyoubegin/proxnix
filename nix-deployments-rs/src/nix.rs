use crate::types::{AppError, Result};
use serde_json;
use std::process::Command;
use tracing::info;

pub const BASE_REPO_PATH: &str = "/tmp/proxnix/repos";

pub fn list_nix_configs(repo_path: &str) -> Result<Vec<String>> {
    let nix_eval = Command::new("nix")
        .current_dir(repo_path)
        .arg("eval")
        .arg(".#nixosConfigurations")
        .arg("--apply")
        .arg("builtins.attrNames")
        .arg("--json")
        .output()
        .map_err(|e| AppError::CmdError(format!("Failed to run nix eval: {}", e)))?;
    if !nix_eval.status.success() {
        let stderr = String::from_utf8_lossy(&nix_eval.stderr);
        return Err(AppError::CmdError(format!(
            "Nix eval failed (exit: {:?}): {}",
            nix_eval.status.code(),
            stderr
        )));
    }
    let stdout_bytes = nix_eval.stdout;
    let output_string = String::from_utf8(stdout_bytes)?;
    let parsed: Vec<String> = serde_json::from_str(&output_string)?;

    Ok(parsed)
}

pub fn nix_build(config_name: &str, repo_path: &str) -> Result<String> {
    info!("Running nix build for config '{}' in {}", config_name, repo_path);
    let result_path = format!("{}/{}/result", repo_path, config_name);
    let nix_build = Command::new("nix")
        .current_dir(repo_path)
        .arg("build")
        .arg(format!(
            ".#nixosConfigurations.{}.config.system.build.qcow2",
            config_name
        ))
        .arg("--out-link")
        .arg(&result_path)
        .output()
        .map_err(|e| AppError::CmdError(format!("Failed to run nix build: {}", e)))?;
    if !nix_build.status.success() {
        let stderr = String::from_utf8_lossy(&nix_build.stderr);
        return Err(AppError::CmdError(format!(
            "Nix build failed for '{}' (exit: {:?}): {}",
            config_name,
            nix_build.status.code(),
            stderr
        )));
    }
    info!("Nix build succeeded for '{}': {}", config_name, result_path);

    Ok(result_path)
}

// TODO I need to finish up some utils to initialise this dir on setup. I will probably do a utils module.
// I probably wil want to init the user there as well rather than in this module
pub fn configure_dirs(configs: Vec<String>, repo_path: &str) -> Result<()> {
    let repo_base = std::path::Path::new(repo_path);
    std::fs::create_dir_all(repo_base)?;

    for config in configs {
        std::fs::create_dir_all(repo_base.join(config))?;
    }

    Ok(())
}
