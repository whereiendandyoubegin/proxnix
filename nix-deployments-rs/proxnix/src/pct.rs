use crate::context::{NixHash, Tags};
use crate::types::{AppError, ContainerConfig, ContainerFieldChange, MountMode, Result};
use crate::zfs::{BaseImage, DiskSize, Ownership, RootfsVolume, Sealed, Tarball, ZfsImages};
use proxnix_core::SlotId;
use std::collections::HashSet;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tracing::{info, warn};

const UNPRIVILEGED_ROOT: u32 = 100000;
const PCT_EXEC_POLL: Duration = Duration::from_millis(250);
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
            "could not read template cache {template_cache_path}: {e}"
        ))
    })?;

    Ok(entries
        .filter_map(std::result::Result::ok)
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
    if path.exists() { Ok(()) } else {
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

pub fn copy_to_template_storage(
    tarball: &Tarball,
    template_cache_path: &str,
    nix_hash: &NixHash,
) -> Result<String> {
    let src = tarball.path();
    let filename = src
        .file_name()
        .ok_or_else(|| AppError::CmdError("tarball has no filename".to_string()))?
        .to_string_lossy()
        .to_string();

    let unique = format!("{nix_hash}-{filename}");
    let dest = format!("{template_cache_path}{unique}");
    std::fs::copy(src, &dest)
        .map_err(|e| AppError::CmdError(format!("failed to copy {} to {}: {}", src.display(), dest, e)))?;

    Ok(format!("local:vztmpl/{unique}"))
}

struct PctArgs(Vec<String>);

impl PctArgs {
    fn settings(config: &ContainerConfig, tags: &Tags) -> PctArgs {
        let base = [
            "--hostname".to_string(),
            config.name.clone(),
            "--memory".to_string(),
            config.memory_mb.to_string(),
            "--cores".to_string(),
            config.cores.to_string(),
            "--net0".to_string(),
            format!("name=eth0,bridge={}", config.network_bridge),
            "--features".to_string(),
            "nesting=1".to_string(),
            "--tags".to_string(),
            tags.render(),
        ];
        let mounts = config.bind_mounts.iter().enumerate().flat_map(|(i, mount)| {
            let suffix = match mount.mode {
                MountMode::ReadOnly => ",ro=1",
                MountMode::ReadWrite => "",
            };
            [
                format!("--mp{i}"),
                format!("{},mp={}{}", mount.host_path, mount.container_path, suffix),
            ]
        });
        PctArgs(base.into_iter().chain(mounts).collect())
    }
}

struct ConfPath(PathBuf);

impl ConfPath {
    fn of(target: SlotId) -> ConfPath {
        ConfPath(PathBuf::from(format!("/etc/pve/lxc/{}.conf", target.inner())))
    }
}

struct LxcConf<'a> {
    rootfs: &'a RootfsVolume,
    ownership: Ownership,
}

impl fmt::Display for LxcConf<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let unprivileged = match self.ownership {
            Ownership::Unprivileged => 1,
            Ownership::Privileged => 0,
        };
        write!(
            f,
            "arch: amd64\nostype: unmanaged\nrootfs: {}\nunprivileged: {}\n",
            self.rootfs, unprivileged
        )
    }
}

fn write_conf(path: &ConfPath, conf: &LxcConf<'_>) -> Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path.0)?
        .write_all(conf.to_string().as_bytes())
        .map_err(AppError::from)
}

fn pct(cmd: &mut Command, what: &str) -> Result<String> {
    let output = cmd.output()?;
    if output.status.success() { Ok(String::from_utf8(output.stdout)?) } else { Err(AppError::CmdError(format!(
        "{} failed (exit: {:?}): {}",
        what,
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    ))) }
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

    pct(
        Command::new("pct")
            .arg("create")
            .arg(target.inner().to_string())
            .arg(ostemplate)
            .arg("--rootfs")
            .arg(format!("{}:{}", config.storage_location, config.disk_gb))
            .arg("--ostype")
            .arg("unmanaged")
            .arg("--unprivileged")
            .arg(if config.privileged { "0" } else { "1" })
            .arg("--protection")
            .arg(if config.protected { "1" } else { "0" })
            .args(PctArgs::settings(config, tags).0),
        "pct create",
    )
}

pub fn pct_create_from_clone(
    config: &ContainerConfig,
    zfs: &ZfsImages,
    image: &BaseImage<Sealed>,
    tags: &Tags,
    target: SlotId,
) -> Result<()> {
    config
        .bind_mounts
        .iter()
        .try_for_each(|mount| prepare_bind_mount(mount, config.privileged))?;

    let clone = image.clone_rootfs(zfs, target, DiskSize::gib(config.disk_gb))?;
    let conf = LxcConf {
        rootfs: clone.volume(),
        ownership: Ownership::of(config.privileged),
    };
    if let Err(e) = write_conf(&ConfPath::of(target), &conf) {
        if let Err(cleanup) = clone.discard() {
            warn!("could not discard rootfs clone for {}: {}", target.inner(), cleanup);
        }
        return Err(e);
    }

    let configured = pct(
        Command::new("pct")
            .arg("set")
            .arg(target.inner().to_string())
            .args(PctArgs::settings(config, tags).0),
        "pct set",
    )
    .and_then(|_| pct_set_protection(target.inner(), config.protected));
    match configured {
        Ok(()) => Ok(()),
        Err(e) => {
            if let Err(cleanup) = pct_destroy(target.inner()) {
                warn!("could not remove half-configured container {}: {}", target.inner(), cleanup);
            }
            Err(e)
        }
    }
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

#[derive(Debug, PartialEq)]
pub enum ExecOutcome {
    Succeeded { stdout: String },
    Failed { code: Option<i32>, output: String },
}

pub fn pct_exec(ct_id: u32, argv: &[&str], timeout: Duration) -> Result<ExecOutcome> {
    let mut child = Command::new("pct")
        .arg("exec")
        .arg(ct_id.to_string())
        .arg("--")
        .args(argv)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let started = Instant::now();
    loop {
        match child.try_wait()? {
            Some(_) => {
                let out = child.wait_with_output()?;
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                return if out.status.success() { Ok(ExecOutcome::Succeeded { stdout }) } else { Ok(ExecOutcome::Failed {
                    code: out.status.code(),
                    output: format!(
                        "{}{}",
                        stdout,
                        String::from_utf8_lossy(&out.stderr)
                    )
                    .trim()
                    .to_string(),
                }) };
            }
            None => if started.elapsed() >= timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AppError::CmdError(format!(
                    "pct exec {} {:?} did not return within {}s",
                    ct_id,
                    argv,
                    timeout.as_secs()
                )));
            } else { std::thread::sleep(PCT_EXEC_POLL) },
        }
    }
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

    #[test]
    fn a_cloned_rootfs_is_described_by_a_minimal_lxc_conf() {
        let rootfs = RootfsVolume::for_slot(
            crate::zfs::StorageId::try_from("ZFS".to_string()).unwrap(),
            SlotId::Green(946),
            DiskSize::gib(150),
        );
        let conf = LxcConf {
            rootfs: &rootfs,
            ownership: Ownership::Unprivileged,
        };
        assert_eq!(
            conf.to_string(),
            "arch: amd64\nostype: unmanaged\nrootfs: ZFS:subvol-946-disk-0,size=150G\nunprivileged: 1\n"
        );
        assert_eq!(ConfPath::of(SlotId::Green(946)).0, PathBuf::from("/etc/pve/lxc/946.conf"));
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
