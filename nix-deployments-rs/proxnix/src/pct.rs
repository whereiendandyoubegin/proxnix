use crate::context::{NixHash, Tags};
use crate::types::{AppError, ContainerConfig, ContainerFieldChange, MountMode, Result};
use proxnix_core::SlotId;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{info, warn};

const UNPRIVILEGED_ROOT: u32 = 100000;
const NIX_STORE_HASH_LEN: usize = 32;
const TEMPLATE_MARKER: &str = "-nixos-image-";
const TEMPLATE_SUFFIX: &str = ".tar.xz";

struct CachedTemplate {
    path: PathBuf,
    nix_hash: NixHash,
    bytes: u64,
}

#[derive(Debug, Default, PartialEq)]
pub struct Reaped {
    pub files: usize,
    pub bytes: u64,
}

impl Reaped {
    fn plus(self, bytes: u64) -> Self {
        Self {
            files: self.files + 1,
            bytes: self.bytes + bytes,
        }
    }
}

fn cached_template(path: PathBuf, file_name: &str, bytes: u64) -> Option<CachedTemplate> {
    match (
        file_name.ends_with(TEMPLATE_SUFFIX),
        file_name.contains(TEMPLATE_MARKER),
        file_name.split_once('-'),
    ) {
        (true, true, Some((prefix, _))) if prefix.len() == NIX_STORE_HASH_LEN => {
            NixHash::try_from(prefix).ok().map(|nix_hash| CachedTemplate {
                path,
                nix_hash,
                bytes,
            })
        }
        _ => None,
    }
}

pub fn reap_template_cache(template_cache_path: &str, keep: &HashSet<NixHash>) -> Result<Reaped> {
    let entries = std::fs::read_dir(template_cache_path).map_err(|e| {
        AppError::CmdError(format!(
            "could not read template cache {}: {}",
            template_cache_path, e
        ))
    })?;

    Ok(entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let bytes = entry.metadata().ok()?.len();
            let name = entry.file_name().to_string_lossy().to_string();
            cached_template(entry.path(), &name, bytes)
        })
        .filter(|template| !keep.contains(&template.nix_hash))
        .fold(Reaped::default(), |acc, template| {
            match std::fs::remove_file(&template.path) {
                Ok(()) => {
                    info!("reaped stale template {}", template.path.display());
                    acc.plus(template.bytes)
                }
                Err(e) => {
                    warn!(
                        "could not reap stale template {}: {}",
                        template.path.display(),
                        e
                    );
                    acc
                }
            }
        }))
}

fn prepare_bind_mount(mount: &crate::types::BindMount, privileged: bool) -> Result<()> {
    let path = Path::new(&mount.host_path);
    match path.exists() {
        true => Ok(()),
        false => {
            info!("creating bind mount host directory {}", mount.host_path);
            std::fs::create_dir_all(path).map_err(|e| {
                AppError::CmdError(format!(
                    "could not create bind mount directory {}: {}",
                    mount.host_path, e
                ))
            })?;
            match (privileged, mount.mode) {
                (false, MountMode::ReadWrite) => {
                    info!(
                        "chowning {} to {} for unprivileged container access",
                        mount.host_path, UNPRIVILEGED_ROOT
                    );
                    std::os::unix::fs::chown(
                        path,
                        Some(UNPRIVILEGED_ROOT),
                        Some(UNPRIVILEGED_ROOT),
                    )
                    .map_err(|e| {
                        AppError::CmdError(format!(
                            "could not chown bind mount directory {}: {}",
                            mount.host_path, e
                        ))
                    })
                }
                _ => Ok(()),
            }
        }
    }
}

pub fn pct_set_tags(ct_id: u32, tags: &Tags) -> Result<()> {
    let output = Command::new("pct")
        .arg("set")
        .arg(ct_id.to_string())
        .arg("--tags")
        .arg(tags.render())
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::CmdError(format!(
            "pct set tags failed for {} (exit: {:?}): {}",
            ct_id,
            output.status.code(),
            stderr
        )));
    }
    Ok(())
}

// Finds the .tar.xz inside the nix build result tarball directory,
// copies it to Proxmox template storage, and returns the storage reference
// for use with pct create (e.g. "local:vztmpl/nixos-image-lxc-....tar.xz")
pub fn copy_to_template_storage(
    result_path: &str,
    template_cache_path: &str,
    nix_hash: &NixHash,
) -> Result<String> {
    let tarball_dir = std::path::Path::new(result_path).join("tarball");
    let entry = std::fs::read_dir(&tarball_dir)
        .map_err(|e| AppError::CmdError(format!("failed to read tarball dir {}: {}", tarball_dir.display(), e)))?
        .filter_map(|e| e.ok())
        .find(|e| e.path().extension().map(|ext| ext == "xz").unwrap_or(false))
        .ok_or_else(|| {
            AppError::CmdError(format!("no .tar.xz found in {}", tarball_dir.display()))
        })?;

    let src = entry.path();
    let filename = src
        .file_name()
        .ok_or_else(|| AppError::CmdError("tarball has no filename".to_string()))?
        .to_string_lossy()
        .to_string();

    let unique = format!("{}-{}", nix_hash, filename);
    let dest = format!("{}{}", template_cache_path, unique);
    std::fs::copy(&src, &dest)
        .map_err(|e| AppError::CmdError(format!("failed to copy {} to {}: {}", src.display(), dest, e)))?;

    Ok(format!("local:vztmpl/{}", unique))
}

pub fn pct_create(
    config: &ContainerConfig,
    ostemplate: &str,
    tags: &Tags,
    target: SlotId,
) -> Result<String> {
    config
        .bind_mounts
        .iter()
        .try_for_each(|mount| prepare_bind_mount(mount, config.privileged))?;

    let mut cmd = Command::new("pct");
    cmd.arg("create")
        .arg(target.inner().to_string())
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
        .arg(if config.privileged { "0" } else { "1" })
        .arg("--features")
        .arg("nesting=1")
        .arg("--protection")
        .arg(if config.protected { "1" } else { "0" })
        .arg("--tags")
        .arg(tags.render());

    for (i, mount) in config.bind_mounts.iter().enumerate() {
        let suffix = match mount.mode {
            MountMode::ReadOnly => ",ro=1",
            MountMode::ReadWrite => "",
        };
        cmd.arg(format!("--mp{}", i)).arg(format!(
            "{},mp={}{}",
            mount.host_path, mount.container_path, suffix
        ));
    }

    let output = cmd.output()?;
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

pub fn pct_set_protection(ct_id: u32, protected: bool) -> Result<()> {
    let output = Command::new("pct")
        .arg("set")
        .arg(ct_id.to_string())
        .arg("--protection")
        .arg(if protected { "1" } else { "0" })
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::CmdError(format!(
            "pct set protection {} failed (exit: {:?}): {}",
            ct_id,
            output.status.code(),
            stderr
        )));
    }
    Ok(())
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

pub fn pct_stop(ct_id: &u32) -> Result<()> {
    let output = Command::new("pct")
        .arg("stop")
        .arg(ct_id.to_string())
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not running") {
            return Ok(());
        }
        return Err(AppError::CmdError(format!(
            "pct stop {} failed (exit: {:?}): {}",
            ct_id,
            output.status.code(),
            stderr
        )));
    }
    Ok(())
}

pub fn pct_destroy(ct_id: u32) -> Result<()> {
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
    Ok(())
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

pub fn pct_set_resources(
    ct_id: u32,
    config: &ContainerConfig,
    changes: &[ContainerFieldChange],
) -> Result<()> {
    let output = Command::new("pct")
        .arg("set")
        .arg(ct_id.to_string())
        .args(
            changes
                .iter()
                .filter_map(|field| match field {
                    ContainerFieldChange::Memory => {
                        Some(["--memory".to_string(), config.memory_mb.to_string()])
                    }
                    ContainerFieldChange::Cores => {
                        Some(["--cores".to_string(), config.cores.to_string()])
                    }
                    _ => None,
                })
                .flatten(),
        )
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::CmdError(format!(
            "pct set {} failed (exit: {:?}): {}",
            ct_id,
            output.status.code(),
            stderr
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(s: &str) -> NixHash {
        NixHash::try_from(s).unwrap()
    }

    const LIVE: &str = "k8whj0lg7k95jn6h57k99kvikc0zrpp3";
    const STALE: &str = "qd7rhsd030gx35sqx1bfh9kbq5na28ir";

    fn template_name(prefix: &str) -> String {
        format!("{}-nixos-image-lxc-26.05.20260505.549bd84-x86_64-linux.tar.xz", prefix)
    }

    #[test]
    fn a_template_we_wrote_is_recognised_by_its_nix_hash() {
        let name = template_name(STALE);
        match cached_template(PathBuf::from(&name), &name, 42) {
            Some(t) => {
                assert_eq!(t.nix_hash, hash(STALE));
                assert_eq!(t.bytes, 42);
            }
            None => panic!("our own template should be recognised"),
        }
    }

    #[test]
    fn templates_the_user_downloaded_are_never_reaped() {
        let foreign = [
            "debian-12-standard_12.7-1_amd64.tar.zst",
            "ubuntu-24.04-standard_24.04-2_amd64.tar.zst",
            "nixos-image-lxc-26.05-x86_64-linux.tar.xz",
        ];
        foreign.iter().for_each(|name| {
            assert!(
                cached_template(PathBuf::from(*name), name, 1).is_none(),
                "{} is not ours and must not be reaped",
                name
            )
        });
    }

    #[test]
    fn a_prefix_that_is_not_a_store_hash_is_left_alone() {
        let name = template_name("tooshort");
        assert!(cached_template(PathBuf::from(&name), &name, 1).is_none());
    }

    #[test]
    fn reaping_counts_files_and_bytes() {
        let empty = Reaped::default();
        assert_eq!(empty, Reaped { files: 0, bytes: 0 });
        assert_eq!(empty.plus(100).plus(50), Reaped { files: 2, bytes: 150 });
    }

    #[test]
    fn the_live_hash_is_kept_and_the_stale_one_is_not() {
        let keep: HashSet<NixHash> = HashSet::from([hash(LIVE)]);
        let live_name = template_name(LIVE);
        let stale_name = template_name(STALE);
        let live = cached_template(PathBuf::from(&live_name), &live_name, 1).unwrap();
        let stale = cached_template(PathBuf::from(&stale_name), &stale_name, 1).unwrap();
        assert!(keep.contains(&live.nix_hash), "the current build must survive");
        assert!(!keep.contains(&stale.nix_hash), "an older build must be reaped");
    }
}
