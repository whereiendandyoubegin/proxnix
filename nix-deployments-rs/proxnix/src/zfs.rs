use std::collections::HashSet;
use std::fmt;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::process::Command;

use proxnix_core::SlotId;
use tracing::{info, warn};

use crate::context::NixHash;
use crate::types::{AppError, IdRange, Result};

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Dataset(String);

impl Dataset {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn child(&self, name: &impl fmt::Display) -> Dataset {
        Dataset(format!("{}/{}", self.0, name))
    }

    fn leaf(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }
}

impl TryFrom<String> for Dataset {
    type Error = AppError;
    fn try_from(s: String) -> Result<Self> {
        let valid = !s.is_empty()
            && !s.starts_with('/')
            && !s.ends_with('/')
            && !s.contains("//")
            && !s.contains('@')
            && !s.contains(char::is_whitespace);
        if valid { Ok(Dataset(s)) } else { Err(AppError::InvalidZfsName(s)) }
    }
}

impl TryFrom<&str> for Dataset {
    type Error = AppError;
    fn try_from(s: &str) -> Result<Self> {
        Dataset::try_from(s.to_string())
    }
}

impl From<Dataset> for String {
    fn from(d: Dataset) -> String {
        d.0
    }
}

impl fmt::Display for Dataset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct StorageId(String);

impl StorageId {
    pub fn is(&self, storage_location: &str) -> bool {
        self.0 == storage_location
    }
}

impl TryFrom<String> for StorageId {
    type Error = AppError;
    fn try_from(s: String) -> Result<Self> {
        if s.is_empty() || s.contains(':') || s.contains(char::is_whitespace) { Err(AppError::InvalidZfsName(s)) } else { Ok(StorageId(s)) }
    }
}

impl From<StorageId> for String {
    fn from(s: StorageId) -> String {
        s.0
    }
}

impl fmt::Display for StorageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ZfsImages {
    pub storage: StorageId,
    pub pool: Dataset,
    pub images: Dataset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotTag {
    Base,
}

impl fmt::Display for SnapshotTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SnapshotTag::Base => "base",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    dataset: Dataset,
    tag: SnapshotTag,
}

impl Snapshot {
    fn base(dataset: &Dataset) -> Snapshot {
        Snapshot { dataset: dataset.clone(), tag: SnapshotTag::Base }
    }
}

impl fmt::Display for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.dataset, self.tag)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mountpoint(PathBuf);

impl Mountpoint {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl TryFrom<&str> for Mountpoint {
    type Error = AppError;
    fn try_from(s: &str) -> Result<Self> {
        match s.trim() {
            path if path.starts_with('/') => Ok(Mountpoint(PathBuf::from(path))),
            other => Err(AppError::ZfsError(format!(
                "dataset has no usable mountpoint ({other}); give the images dataset a real mountpoint"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ownership {
    Unprivileged,
    Privileged,
}

impl Ownership {
    pub fn of(privileged: bool) -> Ownership {
        if privileged { Ownership::Privileged } else { Ownership::Unprivileged }
    }

    fn suffix(self) -> &'static str {
        match self {
            Ownership::Unprivileged => "",
            Ownership::Privileged => "-privileged",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdKind {
    User,
    Group,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IdMap {
    kind: IdKind,
    host_base: u32,
    count: u32,
}

impl IdMap {
    fn of(kind: IdKind, range: IdRange) -> IdMap {
        IdMap { kind, host_base: range.host_base, count: range.count }
    }
}

impl fmt::Display for IdMap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            IdKind::User => "u",
            IdKind::Group => "g",
        };
        write!(f, "{}:0:{}:{}", kind, self.host_base, self.count)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageKey {
    hash: NixHash,
    ownership: Ownership,
}

impl ImageKey {
    pub fn new(hash: NixHash, ownership: Ownership) -> ImageKey {
        ImageKey { hash, ownership }
    }
}

impl TryFrom<&str> for ImageKey {
    type Error = AppError;
    fn try_from(s: &str) -> Result<Self> {
        let (hash, ownership) = match s.strip_suffix(Ownership::Privileged.suffix()) {
            Some(hash) => (hash, Ownership::Privileged),
            None => (s, Ownership::Unprivileged),
        };
        if hash.len() == NixHash::STORE_LEN && hash.chars().all(|c| c.is_ascii_alphanumeric()) { Ok(ImageKey { hash: NixHash::try_from(hash)?, ownership }) } else { Err(AppError::InvalidZfsName(s.to_string())) }
    }
}

impl fmt::Display for ImageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.hash, self.ownership.suffix())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskSize(u32);

impl DiskSize {
    pub fn gib(gib: u32) -> DiskSize {
        DiskSize(gib)
    }
}

impl fmt::Display for DiskSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}G", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeName(String);

impl VolumeName {
    fn rootfs(target: SlotId) -> VolumeName {
        VolumeName(format!("subvol-{}-disk-0", target.inner().get()))
    }
}

impl fmt::Display for VolumeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootfsVolume {
    storage: StorageId,
    volume: VolumeName,
    size: DiskSize,
    owner: proxnix_core::Vmid,
}

impl RootfsVolume {
    pub fn for_slot(storage: StorageId, target: SlotId, size: DiskSize) -> RootfsVolume {
        RootfsVolume {
            storage,
            volume: VolumeName::rootfs(target),
            size,
            owner: target.inner(),
        }
    }

    fn volume_id(&self) -> String {
        format!("{}:{}", self.storage, self.volume)
    }

    fn alloc_args(&self) -> Vec<String> {
        vec![
            String::from("alloc"),
            self.storage.to_string(),
            self.owner.get().to_string(),
            self.volume.to_string(),
            self.size.to_string(),
            String::from("--format"),
            String::from("subvol"),
        ]
    }

    pub fn allocate(&self) -> Result<AllocatedRootfs> {
        run(Command::new("pvesm").args(self.alloc_args()), "pvesm alloc")?;
        let released = |error: AppError| {
            if let Err(cleanup) = run(Command::new("pvesm").args(["free", self.volume_id().as_str()]), "pvesm free") {
                warn!("could not free {} after {error}: {cleanup}", self.volume_id());
            }
            error
        };
        let path = run(Command::new("pvesm").args(["path", self.volume_id().as_str()]), "pvesm path").map_err(released)?;
        Ok(AllocatedRootfs { volume: self.clone(), path: PathBuf::from(path.trim()) })
    }
}

#[derive(Debug)]
pub struct AllocatedRootfs {
    volume: RootfsVolume,
    path: PathBuf,
}

impl AllocatedRootfs {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn release(self) -> Result<()> {
        run(Command::new("pvesm").args(["free", self.volume.volume_id().as_str()]), "pvesm free").map(|_| ())
    }
}

impl fmt::Display for RootfsVolume {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{},size={}", self.storage, self.volume, self.size)
    }
}

#[derive(Debug)]
pub struct Absent;
#[derive(Debug)]
pub struct Empty;
#[derive(Debug)]
pub struct Unpacked;
#[derive(Debug)]
pub struct Sealed;

#[derive(Debug)]
pub struct BaseImage<S> {
    dataset: Dataset,
    ownership: Ownership,
    state: PhantomData<S>,
}

pub enum Located {
    Sealed(BaseImage<Sealed>),
    Absent(BaseImage<Absent>),
}

impl<S> BaseImage<S> {
    fn into_state<T>(self) -> BaseImage<T> {
        BaseImage {
            dataset: self.dataset,
            ownership: self.ownership,
            state: PhantomData,
        }
    }
}

impl BaseImage<Absent> {
    pub fn locate(zfs: &ZfsImages, key: &ImageKey) -> Result<Located> {
        let image = BaseImage::<Absent> {
            dataset: zfs.images.child(key),
            ownership: key.ownership,
            state: PhantomData,
        };
        match (exists(&Snapshot::base(&image.dataset).to_string())?, exists(image.dataset.as_str())?) {
            (true, _) => Ok(Located::Sealed(image.into_state())),
            (false, true) => {
                warn!("discarding unsealed base image {}", image.dataset);
                destroy_recursive(&image.dataset)?;
                Ok(Located::Absent(image))
            }
            (false, false) => Ok(Located::Absent(image)),
        }
    }

    fn create(self) -> Result<BaseImage<Empty>> {
        zfs_cmd(&["create", "-p", self.dataset.as_str()])?;
        Ok(self.into_state())
    }
}

impl BaseImage<Empty> {
    fn unpack(self, tarball: &Tarball, idmap: IdRange) -> Result<BaseImage<Unpacked>> {
        info!("unpacking {} into {}", tarball.path().display(), self.dataset);
        let unpacked = mountpoint(&self.dataset).and_then(|into| extract(tarball, &into, self.ownership, idmap));
        match unpacked {
            Ok(()) => Ok(self.into_state()),
            Err(e) => {
                if let Err(cleanup) = destroy_recursive(&self.dataset) {
                    warn!("could not discard half-unpacked {}: {}", self.dataset, cleanup);
                }
                Err(e)
            }
        }
    }
}

impl BaseImage<Unpacked> {
    fn seal(self) -> Result<BaseImage<Sealed>> {
        zfs_cmd(&["snapshot", &Snapshot::base(&self.dataset).to_string()])?;
        Ok(self.into_state())
    }
}

impl BaseImage<Sealed> {
    pub fn ensure(zfs: &ZfsImages, key: &ImageKey, tarball: &Tarball, idmap: IdRange) -> Result<BaseImage<Sealed>> {
        match BaseImage::locate(zfs, key)? {
            Located::Sealed(image) => Ok(image),
            Located::Absent(image) => image.create()?.unpack(tarball, idmap)?.seal(),
        }
    }

    pub fn clone_rootfs(&self, zfs: &ZfsImages, volume: &RootfsVolume) -> Result<RootfsClone> {
        let dataset = zfs.pool.child(&volume.volume);
        if exists(dataset.as_str())? { Err(AppError::ZfsError(format!(
            "{dataset} already exists; destroy the leftover volume before provisioning into it"
        ))) } else {
            zfs_clone(&Snapshot::base(&self.dataset), &dataset, volume.size)?;
            Ok(RootfsClone { dataset })
        }
    }
}

#[derive(Debug)]
pub struct RootfsClone {
    dataset: Dataset,
}

impl RootfsClone {
    pub fn discard(self) -> Result<()> {
        zfs_cmd(&["destroy", self.dataset.as_str()]).map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tarball(PathBuf);

impl Tarball {
    pub fn find(result_path: &str) -> Result<Tarball> {
        let dir = Path::new(result_path).join("tarball");
        std::fs::read_dir(&dir)
            .map_err(|e| AppError::CmdError(format!("failed to read tarball dir {}: {}", dir.display(), e)))?
            .filter_map(std::result::Result::ok)
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|ext| ext == "xz"))
            .map(Tarball)
            .ok_or_else(|| AppError::CmdError(format!("no .tar.xz found in {}", dir.display())))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct ReapedImages(pub usize);

pub fn reap_images(zfs: &ZfsImages, keep: &HashSet<NixHash>) -> Result<ReapedImages> {
    if exists(zfs.images.as_str())? { Ok(image_datasets(zfs)?
    .into_iter()
    .filter(|(key, _)| !keep.contains(&key.hash))
    .fold(ReapedImages::default(), |acc, (_, dataset)| match reap_image(&dataset) {
        Ok(true) => ReapedImages(acc.0 + 1),
        Ok(false) => acc,
        Err(e) => {
            warn!("could not reap base image {}: {}", dataset, e);
            acc
        }
    })) } else { Ok(ReapedImages::default()) }
}

fn image_datasets(zfs: &ZfsImages) -> Result<Vec<(ImageKey, Dataset)>> {
    Ok(zfs_cmd(&["list", "-H", "-o", "name", "-t", "filesystem", "-d", "1", zfs.images.as_str()])?
        .lines()
        .filter_map(|line| Dataset::try_from(line.trim()).ok())
        .filter(|dataset| *dataset != zfs.images)
        .filter_map(|dataset| ImageKey::try_from(dataset.leaf()).ok().map(|key| (key, dataset)))
        .collect())
}

fn reap_image(dataset: &Dataset) -> Result<bool> {
    let clones = zfs_cmd(&["get", "-H", "-o", "value", "clones", &Snapshot::base(dataset).to_string()])?;
    if has_clones(&clones) { Ok(false) } else {
        info!("reaping base image {}", dataset);
        destroy_recursive(dataset).map(|()| true)
    }
}

fn has_clones(value: &str) -> bool {
    !matches!(value.trim(), "" | "-")
}

fn mountpoint(dataset: &Dataset) -> Result<Mountpoint> {
    Mountpoint::try_from(zfs_cmd(&["get", "-H", "-o", "value", "mountpoint", dataset.as_str()])?.as_str())
}

fn extract(tarball: &Tarball, into: &Mountpoint, ownership: Ownership, idmap: IdRange) -> Result<()> {
    let (uid_map, gid_map) = (IdMap::of(IdKind::User, idmap), IdMap::of(IdKind::Group, idmap));
    let tar = [
        "tar",
        "-x",
        "-p",
        "--numeric-owner",
        "-I",
        "xz -d -T0",
        "-f",
    ];
    let mut cmd = match ownership {
        Ownership::Privileged => {
            let mut cmd = Command::new(tar[0]);
            cmd.args(&tar[1..]);
            cmd
        }
        Ownership::Unprivileged => {
            std::os::unix::fs::chown(
                into.path(),
                Some(uid_map.host_base),
                Some(gid_map.host_base),
            )?;
            let mut cmd = Command::new("lxc-usernsexec");
            cmd.arg("-m")
                .arg(uid_map.to_string())
                .arg("-m")
                .arg(gid_map.to_string())
                .arg("--")
                .args(tar);
            cmd
        }
    };
    cmd.arg(tarball.path()).arg("-C").arg(into.path());
    run(&mut cmd, "unpack base image").map(|_| ())
}

fn zfs_clone(origin: &Snapshot, dataset: &Dataset, size: DiskSize) -> Result<()> {
    zfs_cmd(&[
        "clone",
        "-o",
        &format!("refquota={size}"),
        "-o",
        "acltype=posixacl",
        "-o",
        "xattr=sa",
        &origin.to_string(),
        dataset.as_str(),
    ])
    .map(|_| ())
}

fn destroy_recursive(dataset: &Dataset) -> Result<()> {
    zfs_cmd(&["destroy", "-r", dataset.as_str()]).map(|_| ())
}

fn exists(name: &str) -> Result<bool> {
    Ok(Command::new("zfs")
        .args(["list", "-H", "-o", "name", "-t", "all", name])
        .output()?
        .status
        .success())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Presence {
    Created,
    Existed,
}

pub(crate) fn ensure_dataset(name: &str) -> Result<(PathBuf, Presence)> {
    let presence = if exists(name)? {
        Presence::Existed
    } else {
        zfs_cmd(&["create", "-o", "acltype=posixacl", "-o", "xattr=sa", "-o", "atime=off", name])?;
        Presence::Created
    };
    Ok((PathBuf::from(zfs_cmd(&["get", "-H", "-o", "value", "mountpoint", name])?.trim()), presence))
}

fn zfs_cmd(args: &[&str]) -> Result<String> {
    run(Command::new("zfs").args(args), &format!("zfs {}", args.first().copied().unwrap_or("")))
}

fn run(cmd: &mut Command, what: &str) -> Result<String> {
    let output = cmd.output()?;
    if output.status.success() { Ok(String::from_utf8(output.stdout)?) } else { Err(AppError::ZfsError(format!(
        "{} failed (exit: {:?}): {}",
        what,
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).trim()
    ))) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_rootfs_is_allocated_through_proxmox_under_the_slot_it_belongs_to() {
        let volume = RootfsVolume::for_slot(StorageId::try_from(String::from("ZFS")).unwrap(), SlotId::Blue(proxnix_core::Vmid::new(830)), DiskSize::gib(10));
        assert_eq!(volume.alloc_args().join(" "), "alloc ZFS 830 subvol-830-disk-0 10G --format subvol");
        assert_eq!(volume.volume_id(), "ZFS:subvol-830-disk-0");
        assert_eq!(volume.to_string(), "ZFS:subvol-830-disk-0,size=10G");
    }
    use proxnix_core::Vmid;

    const HASH: &str = "0lmgpzmhq0d1yrpnl7fxpgnkqkgnxdq7";

    fn images() -> ZfsImages {
        ZfsImages {
            storage: StorageId::try_from("ZFS".to_string()).unwrap(),
            pool: Dataset::try_from("ZFS").unwrap(),
            images: Dataset::try_from("ZFS/proxnix-images").unwrap(),
        }
    }

    #[test]
    fn dataset_names_are_validated() {
        assert!(Dataset::try_from("ZFS/proxnix-images").is_ok());
        assert!(Dataset::try_from("").is_err());
        assert!(Dataset::try_from("/ZFS").is_err());
        assert!(Dataset::try_from("ZFS/").is_err());
        assert!(Dataset::try_from("ZFS//x").is_err());
        assert!(Dataset::try_from("ZFS@base").is_err());
        assert!(Dataset::try_from("ZFS x").is_err());
    }

    #[test]
    fn storage_ids_are_validated() {
        assert!(StorageId::try_from("ZFS".to_string()).is_ok());
        assert!(StorageId::try_from(String::new()).is_err());
        assert!(StorageId::try_from("ZFS:vol".to_string()).is_err());
    }

    #[test]
    fn storage_matches_only_its_own_location() {
        let zfs = images();
        assert!(zfs.storage.is("ZFS"));
        assert!(!zfs.storage.is("local-lvm"));
    }

    #[test]
    fn image_keys_round_trip_through_dataset_names() {
        let unprivileged = ImageKey::new(NixHash::try_from(HASH).unwrap(), Ownership::Unprivileged);
        let privileged = ImageKey::new(NixHash::try_from(HASH).unwrap(), Ownership::Privileged);
        assert_eq!(ImageKey::try_from(unprivileged.to_string().as_str()).unwrap(), unprivileged);
        assert_eq!(ImageKey::try_from(privileged.to_string().as_str()).unwrap(), privileged);
        assert_eq!(privileged.to_string(), format!("{}-privileged", HASH));
    }

    #[test]
    fn datasets_that_are_not_images_are_not_keys() {
        assert!(ImageKey::try_from("proxnix-images").is_err());
        assert!(ImageKey::try_from("short").is_err());
        assert!(ImageKey::try_from(&*format!("{}-other", HASH)).is_err());
    }

    #[test]
    fn a_base_image_lives_under_the_images_dataset() {
        let key = ImageKey::new(NixHash::try_from(HASH).unwrap(), Ownership::Unprivileged);
        let dataset = images().images.child(&key);
        assert_eq!(dataset.as_str(), format!("ZFS/proxnix-images/{}", HASH));
        assert_eq!(dataset.leaf(), HASH);
        assert_eq!(Snapshot::base(&dataset).to_string(), format!("ZFS/proxnix-images/{}@base", HASH));
    }

    #[test]
    fn a_rootfs_volume_renders_as_a_proxmox_volume() {
        let volume = RootfsVolume::for_slot(images().storage, SlotId::Blue(Vmid::new(842)), DiskSize::gib(10));
        assert_eq!(volume.to_string(), "ZFS:subvol-842-disk-0,size=10G");
    }

    #[test]
    fn mountpoints_must_be_paths() {
        assert_eq!(
            Mountpoint::try_from("/ZFS/proxnix-images/x\n").unwrap(),
            Mountpoint(PathBuf::from("/ZFS/proxnix-images/x"))
        );
        assert!(Mountpoint::try_from("none").is_err());
        assert!(Mountpoint::try_from("legacy").is_err());
        assert!(Mountpoint::try_from("-").is_err());
    }

    #[test]
    fn id_maps_render_for_lxc_usernsexec() {
        let range = IdRange { host_base: 100_000, count: 65_536 };
        assert_eq!(IdMap::of(IdKind::User, range).to_string(), "u:0:100000:65536");
        assert_eq!(IdMap::of(IdKind::Group, range).to_string(), "g:0:100000:65536");
    }

    #[test]
    fn a_snapshot_without_clones_reads_as_a_dash() {
        assert!(!has_clones("-\n"));
        assert!(!has_clones(""));
        assert!(has_clones("ZFS/subvol-842-disk-0"));
    }

    #[test]
    fn zfs_images_deserialise_with_validation() {
        let parsed: ZfsImages =
            serde_json::from_str(r#"{"storage":"ZFS","pool":"ZFS","images":"ZFS/proxnix-images"}"#).unwrap();
        assert_eq!(parsed, images());
        let invalid: std::result::Result<ZfsImages, _> =
            serde_json::from_str(r#"{"storage":"ZFS","pool":"/ZFS","images":"ZFS/proxnix-images"}"#);
        assert!(invalid.is_err());
    }
}
