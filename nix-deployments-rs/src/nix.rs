use crate::types::{AppError, Result};
use axum::serve::IncomingStream;
use serde_json;
use std::fs;
use std::process::Command;

pub const BASE_REPO_PATH: &str = "/tmp/proxnix/repos";

pub fn list_nix_configs(repo_path: &str) -> Result<Vec<String>> {
    let nix_eval = Command::new("nix")
        .current_dir(repo_path)
        .arg("eval")
        .arg(".#nixosConfigurations")
        .arg("--apply")
        .arg("builtins.attrNames")
        .arg("--json")
        .output()?;
    if !nix_eval.status.success() {
        return Err(AppError::CmdError(format!(
            "Nix eval has failed with the exit code: {:?}",
            nix_eval.status.code()
        )));
    }
    let stdout_bytes = nix_eval.stdout;
    let output_string = String::from_utf8(stdout_bytes)?;
    let parsed: Vec<String> = serde_json::from_str(&output_string)?;

    Ok(parsed)
}

pub fn nix_build(config_name: &str, repo_path: &str, commit_hash: &str) -> Result<String> {
    let nix_build = Command::new("nix")
        .current_dir(repo_path)
        .arg("build")
        .arg(format!(
            ".#nixosConfigurations.{}.config.system.build.qcow2",
            config_name
        ))
        .arg("--out-link")
        .arg(format!(
            "{}/{}/{}/result",
            repo_path, commit_hash, config_name
        ))
        .output()?;
    if !nix_build.status.success() {
        return Err(AppError::CmdError(format!(
            "Nix build has failed with the exit code: {:?}",
            nix_build.status.code()
        )));
    }
    let result_path = format!("{}/{}/{}/result", repo_path, commit_hash, config_name);

    Ok(result_path)
}

// TODO I need to finish up some utils to initialise this dir on setup. I will probably do a utils module.
// I probably wil want to init the user there as well rather than in this module
pub fn configure_dirs(commit_hash: &str, configs: Vec<String>, repo_path: &str) -> Result<()> {
    let repo_base = std::path::Path::new(repo_path);
    std::fs::create_dir_all(repo_base)?;

    let base = repo_base.join(commit_hash);

    std::fs::create_dir(&base)?;
    for config in configs {
        std::fs::create_dir(base.join(config))?;
    }

    Ok(())
}
