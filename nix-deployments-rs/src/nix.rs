use crate::types::{AppError, Result};
use std::process::Command;
use serde_json;

pub const BASE_REPO_PATH: &str = "/tmp/proxnix/repos"

pub fn list_nix_configs(repo_path: &str) -> Result<Vec<String>> {
    let nix_eval = Command::new("nix")
        .arg("eval")
        .arg(".#nixosConfigurations")
        .arg("--apply")
        .arg("builtins.attrNames")
        .arg("--json")
        .output()?;
    if !nix_eval.status.success() {
        return Err(AppError::CmdError(format!("Nix eval has failed with the exit code: {:?}", nix_eval.status.code())))
    }
    let stdout_bytes = nix_eval.stdout;
    let output_string = String::from_utf8(stdout_bytes)?;

    Ok(output_string)

}
