use crate::context::{NixHash, Tags};
use crate::types::{AppError, ContainerConfig, IdRange, Result};
use crate::zfs::{Presence, RootfsVolume, Tarball};
use proxmox_api::nodes::node::lxc::vmid::config::HostnameStr;
use proxmox_api::types::bounded_string::BoundedString;
use proxnix_core::{Dataset, HostEffect, HostPath, Mount, MountMode, Owner, SlotId, Vmid};
use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
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

pub(crate) fn host_text(path: &HostPath) -> String {
    match path.parts() {
        [] => String::from("/"),
        parts => parts.iter().map(|part| format!("/{}", part.as_ref())).collect(),
    }
}

pub(crate) fn dataset_text(dataset: &Dataset) -> String {
    dataset.segments().iter().map(AsRef::as_ref).collect::<Vec<&str>>().join("/")
}

pub(crate) fn host_effect_text(effect: &HostEffect) -> String {
    match effect {
        HostEffect::EnsureDataset { dataset, owner } => format!("dataset {} (owned by {owner:?} when created)", dataset_text(dataset)),
        HostEffect::EnsureDirectory { path, owner } => format!("directory {} (owned by {owner:?} when created)", host_text(path)),
        HostEffect::EnsureHostPath { path, owner } => format!("host path {} (owned by {owner:?} if proxnix creates it)", host_text(path)),
    }
}

fn own(path: &Path, owner: Owner, idmap: IdRange) -> Result<()> {
    let id = match owner {
        Owner::HostRoot => 0,
        Owner::GuestRoot => idmap.host_base,
    };
    std::os::unix::fs::chown(path, Some(id), Some(id))
        .map_err(|e| AppError::CmdError(format!("could not give {} to {id}: {e}", path.display())))
}

pub(crate) fn ensure(effect: &HostEffect, idmap: IdRange) -> Result<()> {
    match effect {
        HostEffect::EnsureDataset { dataset, owner } => {
            let name = dataset_text(dataset);
            let expected = PathBuf::from(host_text(&dataset.mountpoint()));
            match crate::zfs::ensure_dataset(&name)? {
                (mounted, Presence::Created) if mounted == expected => own(&expected, *owner, idmap),
                (mounted, Presence::Existed) if mounted == expected => Ok(()),
                (mounted, _) => {
                    Err(AppError::ZfsError(format!(
                        "{name} is mounted at {}, but the layout expects {}; proxnix never moves a mountpoint",
                        mounted.display(),
                        expected.display()
                    )))
                }
            }
        }
        HostEffect::EnsureDirectory { path, owner } | HostEffect::EnsureHostPath { path, owner } => {
            let path = PathBuf::from(host_text(path));
            if path.exists() {
                Ok(())
            } else {
                info!("creating host path {}", path.display());
                std::fs::create_dir_all(&path)?;
                own(&path, *owner, idmap)
            }
        }
    }
}

pub(crate) fn ensure_all(effects: &[HostEffect], idmap: IdRange) -> Result<()> {
    effects.iter().try_for_each(|effect| ensure(effect, idmap))
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

pub(crate) fn mount_spec(mount: &Mount) -> String {
    let suffix = match mount.mode {
        MountMode::ReadOnly => ",ro=1",
        MountMode::ReadWrite => "",
    };
    format!("{},mp={}{}", host_text(&mount.host), mount.guest.0, suffix)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MacAddress([u8; 6]);

impl MacAddress {
    pub(crate) fn for_guest(id: Vmid) -> Result<MacAddress> {
        match id.get().to_be_bytes() {
            [0, high, middle, low] => Ok(MacAddress([0x02, 0x70, 0x78, high, middle, low])),
            _ => Err(AppError::CmdError(format!("vmid {} does not fit in a guest mac address", id.get()))),
        }
    }
}

impl fmt::Display for MacAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02X}:{b:02X}:{c:02X}:{d:02X}:{e:02X}:{g:02X}")
    }
}

pub(crate) struct LxcConf<'a> {
    config: &'a ContainerConfig,
    mounts: &'a [Mount],
    hostname: HostnameStr,
    tags: &'a Tags,
    rootfs: &'a RootfsVolume,
    hwaddr: MacAddress,
}

impl<'a> LxcConf<'a> {
    pub(crate) fn of(
        config: &'a ContainerConfig,
        mounts: &'a [Mount],
        tags: &'a Tags,
        rootfs: &'a RootfsVolume,
        target: SlotId,
    ) -> Result<LxcConf<'a>> {
        Ok(LxcConf {
            config,
            mounts,
            hostname: HostnameStr::try_from(config.name.clone())?,
            tags,
            rootfs,
            hwaddr: MacAddress::for_guest(target.inner())?,
        })
    }
}

impl fmt::Display for LxcConf<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tags: BTreeSet<String> = self.tags.render().split(';').map(str::to_string).collect();
        writeln!(f, "arch: amd64")?;
        writeln!(f, "cores: {}", self.config.cores)?;
        writeln!(f, "features: nesting=1")?;
        writeln!(f, "hostname: {}", self.hostname.get_value())?;
        writeln!(f, "memory: {}", self.config.memory_mb)?;
        self.mounts
            .iter()
            .enumerate()
            .try_for_each(|(index, mount)| writeln!(f, "mp{index}: {}", mount_spec(mount)))?;
        writeln!(f, "net0: name=eth0,bridge={},hwaddr={},type=veth", self.config.network_bridge, self.hwaddr)?;
        writeln!(f, "ostype: unmanaged")?;
        writeln!(f, "protection: {}", u8::from(self.config.protected))?;
        writeln!(f, "rootfs: {}", self.rootfs)?;
        writeln!(f, "tags: {}", tags.into_iter().collect::<Vec<_>>().join(";"))?;
        writeln!(f, "unprivileged: {}", u8::from(!self.config.privileged))
    }
}

pub(crate) struct PveFs {
    conf_dir: PathBuf,
    lock_dir: PathBuf,
    vmlist: PathBuf,
    attempts: u32,
    pause: Duration,
}

impl PveFs {
    pub(crate) fn live() -> PveFs {
        PveFs {
            conf_dir: PathBuf::from("/etc/pve/lxc"),
            lock_dir: PathBuf::from("/run/lock/lxc"),
            vmlist: PathBuf::from("/etc/pve/.vmlist"),
            attempts: 100,
            pause: Duration::from_millis(100),
        }
    }

    fn conf(&self, id: Vmid) -> PathBuf {
        self.conf_dir.join(format!("{}.conf", id.get()))
    }

    fn staged(&self, id: Vmid) -> PathBuf {
        self.conf_dir.join(format!("{}.conf.tmp.{}", id.get(), std::process::id()))
    }

    fn lock(&self, id: Vmid) -> PathBuf {
        self.lock_dir.join(format!("pve-config-{}.lock", id.get()))
    }

    fn hold(&self, id: Vmid) -> Result<File> {
        std::fs::create_dir_all(&self.lock_dir)?;
        let file = OpenOptions::new().create(true).append(true).open(self.lock(id))?;
        self.acquire(id, file, self.attempts)
    }

    fn acquire(&self, id: Vmid, file: File, left: u32) -> Result<File> {
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(TryLockError::WouldBlock) if left > 1 => {
                std::thread::sleep(self.pause);
                self.acquire(id, file, left - 1)
            }
            Err(TryLockError::WouldBlock) => Err(AppError::CmdError(format!("timed out waiting for the config lock of {}", id.get()))),
            Err(TryLockError::Error(error)) => Err(AppError::from(error)),
        }
    }
}

fn unclaimed(vmlist: &str, id: Vmid) -> Result<()> {
    let listed: serde_json::Value = serde_json::from_str(vmlist)?;
    match listed.get("ids").map(|ids| ids.get(id.get().to_string())) {
        Some(None) => Ok(()),
        Some(Some(_)) => Err(AppError::CmdError(format!("vmid {} was claimed before its config could be written", id.get()))),
        None => Err(AppError::ProxmoxError(String::from("the cluster vmlist has no ids"))),
    }
}

pub(crate) fn write_conf(fs: &PveFs, target: SlotId, conf: &LxcConf<'_>) -> Result<()> {
    let id = target.inner();
    let _held = fs.hold(id)?;
    unclaimed(&std::fs::read_to_string(&fs.vmlist)?, id)?;
    let staged = fs.staged(id);
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)
        .and_then(|file| (&file).write_all(conf.to_string().as_bytes()))
        .and_then(|()| std::fs::rename(&staged, fs.conf(id)))
        .map_err(|error| {
            discard(&staged);
            AppError::from(error)
        })
}

fn discard(staged: &Path) {
    match std::fs::remove_file(staged) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => warn!("could not remove staged config {}: {}", staged.display(), error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxnix_core::Vmid;
    use crate::zfs::DiskSize;

    fn hash(s: &str) -> NixHash {
        NixHash::try_from(s).unwrap()
    }

    const NIXFLIX_947: &str = "arch: amd64
cores: 4
features: nesting=1
hostname: nixflix
memory: 4096
mp0: /var/lib/proxnix/nixflix,mp=/data/.state
mp1: /ZFS/nixflix,mp=/data/media
mp2: /var/lib/proxnix/nixflix-sabnzbd,mp=/var/lib/sabnzbd
mp3: /var/lib/proxnix/sops,mp=/var/lib/sops-key,ro=1
mp4: /var/lib/proxnix/logs/nixflix,mp=/var/log/journal
net0: name=eth0,bridge=vmbr0,hwaddr=02:70:78:00:03:B3,type=veth
ostype: unmanaged
protection: 0
rootfs: ZFS:subvol-947-disk-0,size=16G
tags: commit-43710d511fca5c72b71e154c8c1d22c0524b2816;nix-bv7mmn7x00swyhdrlaa0qs7dgqp9gv1n;pending;proxnix;slot-green
unprivileged: 1
";

    fn mount(host: &str, at: &str, mode: MountMode) -> Mount {
        Mount { host: HostPath::try_from(host).unwrap(), guest: proxnix_core::GuestPath(at.to_string()), mode }
    }

    fn nixflix_mounts() -> Vec<Mount> {
        vec![
            mount("/var/lib/proxnix/nixflix", "/data/.state", MountMode::ReadWrite),
            mount("/ZFS/nixflix", "/data/media", MountMode::ReadWrite),
            mount("/var/lib/proxnix/nixflix-sabnzbd", "/var/lib/sabnzbd", MountMode::ReadWrite),
            mount("/var/lib/proxnix/sops", "/var/lib/sops-key", MountMode::ReadOnly),
            mount("/var/lib/proxnix/logs/nixflix", "/var/log/journal", MountMode::ReadWrite),
        ]
    }

    fn nixflix() -> ContainerConfig {
        ContainerConfig {
            name: "nixflix".to_string(),
            hostname: "media.thesta.rs".to_string(),
            service_address: None,
            backend_port: 8096,
            tcp_ports: vec![],
            dhcp_timeout_seconds: 240,
            health_check_timeout_seconds: 900,
            blue_id: Vmid::new(847),
            green_id: Vmid::new(947),
            image_type: crate::context::ImageType::from("build-lxc-nixflix"),
            cores: 4,
            memory_mb: 4096,
            storage_location: "ZFS".to_string(),
            disk_gb: 16,
            protected: false,
            privileged: false,
            state: vec![],
            mounts: vec![],
            secrets: false,
            network_bridge: "vmbr0".to_string(),
            impure: false,
            cutover: None,
        }
    }

    fn fresh_tags() -> Tags {
        Tags {
            pending: true,
            ..Tags::new(hash("bv7mmn7x00swyhdrlaa0qs7dgqp9gv1n"), "43710d511fca5c72b71e154c8c1d22c0524b2816", proxnix_core::Slot::Green)
        }
    }

    fn green_rootfs() -> RootfsVolume {
        RootfsVolume::for_slot(
            crate::zfs::StorageId::try_from("ZFS".to_string()).unwrap(),
            SlotId::Green(Vmid::new(947)),
            DiskSize::gib(16),
        )
    }

    #[test]
    fn a_fresh_container_conf_is_what_pct_set_left_on_pve01_apart_from_its_mac() {
        let (config, tags, rootfs, mounts) = (nixflix(), fresh_tags(), green_rootfs(), nixflix_mounts());
        let conf = LxcConf::of(&config, &mounts, &tags, &rootfs, SlotId::Green(Vmid::new(947))).unwrap();
        assert_eq!(conf.to_string(), NIXFLIX_947);
    }

    #[test]
    fn a_hostname_longer_than_proxmox_allows_is_rejected_before_anything_runs() {
        let config = ContainerConfig { name: "a".repeat(256), ..nixflix() };
        let (tags, rootfs) = (fresh_tags(), green_rootfs());
        assert!(matches!(LxcConf::of(&config, &nixflix_mounts(), &tags, &rootfs, SlotId::Green(Vmid::new(947))), Err(AppError::ProxmoxString(_))));
    }

    #[test]
    fn a_guest_mac_is_local_unicast_and_fixed_by_its_vmid() {
        let mac = MacAddress::for_guest(Vmid::new(947)).unwrap();
        assert_eq!(mac.to_string(), "02:70:78:00:03:B3");
        assert_eq!(mac.0[0] & 0b11, 0b10, "locally administered, not multicast");
        assert_ne!(MacAddress::for_guest(Vmid::new(847)).unwrap(), mac);
        assert!(MacAddress::for_guest(Vmid::new(1 << 24)).is_err());
    }

    fn scratch(name: &str, vmlist: &str) -> PveFs {
        let root = std::env::temp_dir().join(format!("proxnix-pvefs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("lxc")).unwrap();
        std::fs::write(root.join("vmlist"), vmlist).unwrap();
        PveFs {
            conf_dir: root.join("lxc"),
            lock_dir: root.join("lock"),
            vmlist: root.join("vmlist"),
            attempts: 3,
            pause: Duration::from_millis(1),
        }
    }

    fn written(fs: &PveFs) -> Vec<String> {
        std::fs::read_dir(&fs.conf_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    const VMLIST: &str = r#"{ "version": 1, "ids": { "847": { "node": "pve01", "type": "lxc", "version": 1 } } }"#;

    #[test]
    fn a_conf_appears_whole_under_its_vmid_and_leaves_nothing_staged() {
        let fs = scratch("whole", VMLIST);
        let (config, tags, rootfs, mounts) = (nixflix(), fresh_tags(), green_rootfs(), nixflix_mounts());
        let conf = LxcConf::of(&config, &mounts, &tags, &rootfs, SlotId::Green(Vmid::new(947))).unwrap();
        write_conf(&fs, SlotId::Green(Vmid::new(947)), &conf).unwrap();
        assert_eq!(written(&fs), vec![String::from("947.conf")]);
        assert_eq!(std::fs::read_to_string(fs.conf(Vmid::new(947))).unwrap(), NIXFLIX_947);
    }

    #[test]
    fn a_vmid_the_cluster_already_lists_is_never_written_over() {
        let fs = scratch("claimed", VMLIST);
        let (config, tags, rootfs, mounts) = (nixflix(), fresh_tags(), green_rootfs(), nixflix_mounts());
        let conf = LxcConf::of(&config, &mounts, &tags, &rootfs, SlotId::Blue(Vmid::new(847))).unwrap();
        assert!(write_conf(&fs, SlotId::Blue(Vmid::new(847)), &conf).is_err());
        assert!(written(&fs).is_empty());
    }

    #[test]
    fn a_config_lock_someone_else_holds_is_waited_on_then_refused() {
        let fs = scratch("locked", VMLIST);
        let held = fs.hold(Vmid::new(947)).unwrap();
        let (config, tags, rootfs, mounts) = (nixflix(), fresh_tags(), green_rootfs(), nixflix_mounts());
        let conf = LxcConf::of(&config, &mounts, &tags, &rootfs, SlotId::Green(Vmid::new(947))).unwrap();
        assert!(write_conf(&fs, SlotId::Green(Vmid::new(947)), &conf).is_err());
        assert!(written(&fs).is_empty());
        drop(held);
        assert!(write_conf(&fs, SlotId::Green(Vmid::new(947)), &conf).is_ok());
    }

    #[test]
    fn an_existing_directory_keeps_whatever_owner_the_guest_gave_it() {
        let existing = std::env::temp_dir().join(format!("proxnix-owned-{}", std::process::id()));
        std::fs::create_dir_all(&existing).unwrap();
        let path = HostPath::try_from(existing.to_str().unwrap()).unwrap();
        let idmap = IdRange { host_base: 100_000, count: 65_536 };
        assert!(ensure(&HostEffect::EnsureDirectory { path: path.clone(), owner: Owner::HostRoot }, idmap).is_ok());
        assert!(ensure(&HostEffect::EnsureHostPath { path, owner: Owner::GuestRoot }, idmap).is_ok());
    }

    #[test]
    fn a_vmlist_without_ids_is_refused() {
        assert!(unclaimed("{}", Vmid::new(947)).is_err());
        assert!(unclaimed("not json", Vmid::new(947)).is_err());
        assert!(unclaimed(VMLIST, Vmid::new(947)).is_ok());
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
