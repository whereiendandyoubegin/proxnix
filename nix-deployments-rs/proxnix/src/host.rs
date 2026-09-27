use crate::context::NixHash;
use crate::types::{AppError, IdRange, MountMode, Result};
use crate::zfs::{Ownership, RootfsVolume, Tarball};
use proxnix_core::SlotId;
use std::collections::HashSet;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

mod template_name {
    use crate::context::NixHash;

    pub fn of(nix_hash: &NixHash, tarball: &str) -> String {
        format!("{nix_hash}-{tarball}")
    }

    pub fn nix_hash(file_name: &str) -> Option<NixHash> {
        match (
            file_name.ends_with(".tar.xz"),
            file_name.contains("-nixos-image-"),
            file_name.split_once('-'),
        ) {
            (true, true, Some((prefix, _))) if prefix.len() == NixHash::STORE_LEN => NixHash::try_from(prefix).ok(),
            _ => None,
        }
    }
}

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
    template_name::nix_hash(file_name).map(|nix_hash| CachedTemplate { path, nix_hash, bytes })
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

pub(crate) fn prepare_bind_mount(mount: &crate::types::BindMount, privileged: bool, idmap: IdRange) -> Result<()> {
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
                    mount.host_path, idmap.host_base
                );
                std::os::unix::fs::chown(
                    path,
                    Some(idmap.host_base),
                    Some(idmap.host_base),
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

    let unique = template_name::of(nix_hash, &filename);
    let dest = format!("{template_cache_path}{unique}");
    std::fs::copy(src, &dest)
        .map_err(|e| AppError::CmdError(format!("failed to copy {} to {}: {}", src.display(), dest, e)))?;

    Ok(format!("local:vztmpl/{unique}"))
}

pub(crate) struct ConfPath(PathBuf);

impl ConfPath {
    pub(crate) fn of(target: SlotId) -> ConfPath {
        ConfPath(PathBuf::from(format!("/etc/pve/lxc/{}.conf", target.inner())))
    }
}

pub(crate) struct LxcConf<'a> {
    pub(crate) rootfs: &'a RootfsVolume,
    pub(crate) ownership: Ownership,
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

pub(crate) fn write_conf(path: &ConfPath, conf: &LxcConf<'_>) -> Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path.0)?
        .write_all(conf.to_string().as_bytes())
        .map_err(AppError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxnix_core::Vmid;
    use crate::zfs::DiskSize;

    fn hash(s: &str) -> NixHash {
        NixHash::try_from(s).unwrap()
    }

    #[test]
    fn a_cloned_rootfs_is_described_by_a_minimal_lxc_conf() {
        let rootfs = RootfsVolume::for_slot(
            crate::zfs::StorageId::try_from("ZFS".to_string()).unwrap(),
            SlotId::Green(Vmid::new(946)),
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
        assert_eq!(ConfPath::of(SlotId::Green(Vmid::new(946))).0, PathBuf::from("/etc/pve/lxc/946.conf"));
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
