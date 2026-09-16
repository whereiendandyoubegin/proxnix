use crate::context::{StorePath, Tags};
use crate::nix::find_in_repo;
use proxnix_core::{SlotId, Workload};
use crate::pct::{copy_to_template_storage, pct_create};
use crate::qm::{qm_create, qm_importdisk, qm_resize, qm_set_agent, qm_set_disk};
use crate::types::{AppError, ContainerConfig, Result, VMConfig};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::info;

fn flake_installable(config_name: &str, build_attr: &str) -> String {
    format!(".#nixosConfigurations.{}.{}", config_name, build_attr)
}

fn qcow2_path(artifact_path: &str) -> String {
    format!("{}/nixos.qcow2", artifact_path)
}

// --- Side effects ---

fn find_flake_dir(repo_path: &str) -> Result<PathBuf> {
    let flake_path = find_in_repo(repo_path, "flake.nix")?;
    Path::new(&flake_path)
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| AppError::CmdError("flake.nix has no parent directory".to_string()))
}

fn run_nix_build(nix_dir: &Path, installable: &str, impure: bool) -> Result<String> {
    info!("Running nix build '{}' in {}", installable, nix_dir.display());
    let mut cmd = Command::new("nix");
    cmd.current_dir(nix_dir)
        .arg("build")
        .arg(installable)
        .arg("--no-link");
    if impure {
        cmd.arg("--impure");
    }
    let build_output = cmd
        .output()
        .map_err(|e| AppError::CmdError(format!("Failed to run nix build: {}", e)))?;

    if !build_output.status.success() {
        let stderr = String::from_utf8_lossy(&build_output.stderr);
        return Err(AppError::CmdError(format!(
            "Nix build failed for '{}' (exit: {:?}): {}",
            installable,
            build_output.status.code(),
            stderr
        )));
    }

    let mut path_cmd = Command::new("nix");
    path_cmd.current_dir(nix_dir).arg("path-info").arg(installable);
    if impure {
        path_cmd.arg("--impure");
    }
    let path_output = path_cmd
        .output()
        .map_err(|e| AppError::CmdError(format!("Failed to run nix path-info: {}", e)))?;
    if !path_output.status.success() {
        let stderr = String::from_utf8_lossy(&path_output.stderr);
        return Err(AppError::CmdError(format!(
            "nix path-info failed for '{}' (exit: {:?}): {}",
            installable,
            path_output.status.code(),
            stderr
        )));
    }
    let stdout = String::from_utf8(path_output.stdout)?;
    let store_path = stdout.lines().find(|l| !l.trim().is_empty())
        .ok_or_else(|| AppError::CmdError(format!("nix path-info produced no output for '{}'", installable)))?
        .trim()
        .to_string();
    Ok(store_path)
}

// --- Trait ---

pub trait Materialise: Workload {
    fn nix_build_attr(&self) -> &str;
    fn impure(&self) -> bool;
    fn provision_inactive(&self, artifact: &StorePath, tags: &Tags, template_cache_path: &str, target: SlotId) -> Result<()>;

    fn nix_build(&self, repo_path: &str) -> Result<StorePath> {
        let nix_dir = find_flake_dir(repo_path)?;
        let installable = flake_installable(self.name(), self.nix_build_attr());
        let raw = run_nix_build(&nix_dir, &installable, self.impure())?;
        StorePath::try_from(raw)
    }
}

impl Materialise for VMConfig {
    fn nix_build_attr(&self) -> &str {
        "config.system.build.qcow2"
    }
    fn impure(&self) -> bool {
        self.impure
    }
    fn provision_inactive(&self, artifact: &StorePath, tags: &Tags, _template_cache_path: &str, target: SlotId) -> Result<()> {
        let id = target.inner();
        qm_create(self, tags, target)?;
        let disk_ref = qm_importdisk(id, &qcow2_path(artifact.as_str()), &self.storage_location)?;
        qm_set_disk(id, &disk_ref, &self.disk_slot)?;
        qm_set_agent(id)?;
        qm_resize(id, &self.disk_slot, self.disk_gb)?;
        Ok(())
    }
}

impl Materialise for ContainerConfig {
    fn nix_build_attr(&self) -> &str {
        "config.system.build.tarball"
    }
    fn impure(&self) -> bool {
        self.impure
    }
    fn provision_inactive(&self, artifact: &StorePath, tags: &Tags, template_cache_path: &str, target: SlotId) -> Result<()> {
        let ostemplate = copy_to_template_storage(artifact.as_str(), template_cache_path)?;
        pct_create(self, &ostemplate, tags, target)?;
        Ok(())
    }
}
