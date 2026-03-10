use crate::types::{AppError, ContainerConfig, Result};
use std::process::Command;

// Finds the .tar.xz inside the nix build result tarball directory,
// copies it to Proxmox template storage, and returns the storage reference
// for use with pct create (e.g. "local:vztmpl/nixos-image-lxc-....tar.xz")
pub fn copy_to_template_storage(result_path: &str) -> Result<String> {
    let tarball_dir = std::path::Path::new(result_path).join("tarball");
    let entry = std::fs::read_dir(&tarball_dir)?
        .filter_map(|e| e.ok())
        .find(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "xz")
                .unwrap_or(false)
        })
        .ok_or_else(|| {
            AppError::CmdError(format!(
                "no .tar.xz found in {}",
                tarball_dir.display()
            ))
        })?;

    let src = entry.path();
    let filename = src
        .file_name()
        .ok_or_else(|| AppError::CmdError("tarball has no filename".to_string()))?
        .to_string_lossy()
        .to_string();

    let dest = format!("/var/lib/vz/template/cache/{}", filename);
    std::fs::copy(&src, &dest)?;

    Ok(format!("local:vztmpl/{}", filename))
}

pub fn pct_create(
    config: &ContainerConfig,
    ostemplate: &str,
    nix_hash: &str,
    commit_hash: &str,
) -> Result<String> {
    let output = Command::new("pct")
        .arg("create")
        .arg(config.ct_id.to_string())
        .arg(ostemplate)
        .arg("--hostname")
        .arg(&config.name)
        .arg("--memory")
        .arg(config.memory_mb.to_string())
        .arg("--cores")
        .arg(config.cores.to_string())
        .arg("--rootfs")
        .arg(format!("{}:{}", config.storage_location, config.disk_gb))
        .arg("--net0")
        .arg(format!("name=eth0,bridge={}", config.network_bridge))
        .arg("--ostype")
        .arg("unmanaged")
        .arg("--unprivileged")
        .arg("1")
        .arg("--protection")
        .arg(if config.protected { "1" } else { "0" })
        .arg("--tags")
        .arg(format!("proxnix;nix-{};commit-{}", nix_hash, commit_hash))
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::CmdError(format!(
            "pct create failed (exit: {:?}): {}",
            output.status.code(),
            stderr
        )));
    }
    Ok(String::from_utf8(output.stdout)?)
}

pub fn pct_start(ct_id: u32) -> Result<bool> {
    let output = Command::new("pct")
        .arg("start")
        .arg(ct_id.to_string())
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("already running") {
            return Ok(false);
        }
        return Err(AppError::CmdError(format!(
            "pct start {} failed (exit: {:?}): {}",
            ct_id,
            output.status.code(),
            stderr
        )));
    }
    Ok(true)
}

pub fn pct_stop(ct_id: u32) -> Result<String> {
    let output = Command::new("pct")
        .arg("stop")
        .arg(ct_id.to_string())
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not running") {
            return Ok(String::new());
        }
        return Err(AppError::CmdError(format!(
            "pct stop {} failed (exit: {:?}): {}",
            ct_id,
            output.status.code(),
            stderr
        )));
    }
    Ok(String::from_utf8(output.stdout)?)
}

pub fn pct_destroy(ct_id: u32) -> Result<String> {
    let output = Command::new("pct")
        .arg("destroy")
        .arg(ct_id.to_string())
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::CmdError(format!(
            "pct destroy {} failed (exit: {:?}): {}",
            ct_id,
            output.status.code(),
            stderr
        )));
    }
    Ok(String::from_utf8(output.stdout)?)
}

pub fn pct_list() -> Result<String> {
    let output = Command::new("pct").arg("list").output()?;
    if !output.status.success() {
        return Err(AppError::CmdError(format!(
            "pct list failed (exit: {:?})",
            output.status.code()
        )));
    }
    Ok(String::from_utf8(output.stdout)?)
}

pub fn pct_config(ct_id: u32) -> Result<String> {
    let output = Command::new("pct")
        .arg("config")
        .arg(ct_id.to_string())
        .output()?;
    if !output.status.success() {
        return Err(AppError::CmdError(format!(
            "pct config {} failed (exit: {:?})",
            ct_id,
            output.status.code()
        )));
    }
    Ok(String::from_utf8(output.stdout)?)
}
