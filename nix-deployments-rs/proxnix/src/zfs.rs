use std::collections::HashSet;
use std::fmt;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::process::Command;

use proxnix_core::{DataSnapshot, DataTag, Dataset as CoreDataset, SlotId};
use tracing::{info, warn};

use crate::context::NixHash;
use crate::types::{AppError, IdRange, Result};

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Dataset(pub (String));

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

impl From<&CoreDataset> for Dataset {
    fn from(core: &CoreDataset) -> Dataset {
        Dataset(
            core.segments()
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<&str>>()
                .join("/"),
        )
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
        if valid {
            Ok(Dataset(s))
        } else {
            Err(AppError::InvalidZfsName(s))
        }
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
        if s.is_empty() || s.contains(':') || s.contains(char::is_whitespace) {
            Err(AppError::InvalidZfsName(s))
        } else {
            Ok(StorageId(s))
        }
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
    Start,
    Final,
}

impl From<DataTag> for SnapshotTag {
    fn from(value: DataTag) -> Self {
        match value {
            DataTag::Start => SnapshotTag::Start,
            DataTag::Final => SnapshotTag::Final,
        }
    }
}

impl TryFrom<&str> for SnapshotTag {
    type Error = AppError;
    fn try_from(s: &str) -> Result<Self> {
        match s {
            "base" => Ok(SnapshotTag::Base),
            "start" => Ok(SnapshotTag::Start),
            "final" => Ok(SnapshotTag::Final),
            other => Err(AppError::InvalidZfsName(other.to_string())),
        }
    }
}

impl fmt::Display for SnapshotTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SnapshotTag::Base => "base",
            SnapshotTag::Start => "start",
            SnapshotTag::Final => "final",
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
        Snapshot {
            dataset: dataset.clone(),
            tag: SnapshotTag::Base,
        }
    }
}

impl From<&DataSnapshot> for Snapshot {
    fn from(value: &DataSnapshot) -> Self {
        Snapshot {
            dataset: Dataset::from(&value.dataset),
            tag: SnapshotTag::from(value.tag),
        }
    }
}

impl TryFrom<&str> for Snapshot {
    type Error = AppError;
    fn try_from(s: &str) -> Result<Self> {
        let (dataset, tag) = s
            .split_once('@')
            .ok_or_else(|| AppError::InvalidZfsName(s.to_string()))?;
        Ok(Snapshot {
            dataset: Dataset::try_from(dataset)?,
            tag: SnapshotTag::try_from(tag)?,
        })
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
        if privileged {
            Ownership::Privileged
        } else {
            Ownership::Unprivileged
        }
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
        IdMap {
            kind,
            host_base: range.host_base,
            count: range.count,
        }
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
        if hash.len() == NixHash::STORE_LEN && hash.chars().all(|c| c.is_ascii_alphanumeric()) {
            Ok(ImageKey {
                hash: NixHash::try_from(hash)?,
                ownership,
            })
        } else {
            Err(AppError::InvalidZfsName(s.to_string()))
        }
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
            if let Err(cleanup) = run(
                Command::new("pvesm").args(["free", self.volume_id().as_str()]),
                "pvesm free",
            ) {
                warn!(
                    "could not free {} after {error}: {cleanup}",
                    self.volume_id()
                );
            }
            error
        };
        let path = run(
            Command::new("pvesm").args(["path", self.volume_id().as_str()]),
            "pvesm path",
        )
        .map_err(released)?;
        Ok(AllocatedRootfs {
            volume: self.clone(),
            path: PathBuf::from(path.trim()),
        })
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
        run(
            Command::new("pvesm").args(["free", self.volume.volume_id().as_str()]),
            "pvesm free",
        )
        .map(|_| ())
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
        match (
            exists(&Snapshot::base(&image.dataset).to_string())?,
            exists(image.dataset.as_str())?,
        ) {
            (true, _) => Ok(Located::Sealed(image.into_state())),
            (false, true) => {
                warn!("discarding unsealed base image {}", image.dataset);
                Destroy::Recursive(image.dataset.clone()).run_cmd()?;
                Ok(Located::Absent(image))
            }
            (false, false) => Ok(Located::Absent(image)),
        }
    }

    fn create(self) -> Result<BaseImage<Empty>> {
        Create::Parents(self.dataset.clone()).run_cmd()?;
        Ok(self.into_state())
    }
}

impl BaseImage<Empty> {
    fn unpack(self, tarball: &Tarball, idmap: IdRange) -> Result<BaseImage<Unpacked>> {
        info!(
            "unpacking {} into {}",
            tarball.path().display(),
            self.dataset
        );
        let unpacked = get::<MountpointProp, _>(&self.dataset)
            .run_cmd()
            .and_then(|into| extract(tarball, &into, self.ownership, idmap));
        match unpacked {
            Ok(()) => Ok(self.into_state()),
            Err(e) => {
                if let Err(cleanup) = Destroy::Recursive(self.dataset.clone()).run_cmd() {
                    warn!(
                        "could not discard half-unpacked {}: {}",
                        self.dataset, cleanup
                    );
                }
                Err(e)
            }
        }
    }
}

impl BaseImage<Unpacked> {
    fn seal(self) -> Result<BaseImage<Sealed>> {
        TakeSnapshot {
            snapshot: Snapshot::base(&self.dataset),
        }
        .run_cmd()?;
        Ok(self.into_state())
    }
}

impl BaseImage<Sealed> {
    pub fn ensure(
        zfs: &ZfsImages,
        key: &ImageKey,
        tarball: &Tarball,
        idmap: IdRange,
    ) -> Result<BaseImage<Sealed>> {
        match BaseImage::locate(zfs, key)? {
            Located::Sealed(image) => Ok(image),
            Located::Absent(image) => image.create()?.unpack(tarball, idmap)?.seal(),
        }
    }

    pub fn clone_rootfs(&self, zfs: &ZfsImages, volume: &RootfsVolume) -> Result<RootfsClone> {
        let dataset = zfs.pool.child(&volume.volume);
        if exists(dataset.as_str())? {
            Err(AppError::ZfsError(format!(
                "{dataset} already exists; destroy the leftover volume before provisioning into it"
            )))
        } else {
            CloneSnapshot {
                origin: Snapshot::base(&self.dataset),
                into: dataset.clone(),
                props: Props::Rootfs(volume.size),
            }
            .run_cmd()?;
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
        Destroy::Single(self.dataset).run_cmd()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tarball(PathBuf);

impl Tarball {
    pub fn find(result_path: &str) -> Result<Tarball> {
        let dir = Path::new(result_path).join("tarball");
        std::fs::read_dir(&dir)
            .map_err(|e| {
                AppError::CmdError(format!(
                    "failed to read tarball dir {}: {}",
                    dir.display(),
                    e
                ))
            })?
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
    if exists(zfs.images.as_str())? {
        Ok(image_datasets(zfs)?
            .into_iter()
            .filter(|(key, _)| !keep.contains(&key.hash))
            .fold(
                ReapedImages::default(),
                |acc, (_, dataset)| match reap_image(&dataset) {
                    Ok(true) => ReapedImages(acc.0 + 1),
                    Ok(false) => acc,
                    Err(e) => {
                        warn!("could not reap base image {}: {}", dataset, e);
                        acc
                    }
                },
            ))
    } else {
        Ok(ReapedImages::default())
    }
}

fn image_datasets(zfs: &ZfsImages) -> Result<Vec<(ImageKey, Dataset)>> {
    Ok(ListChildren {
        parent: zfs.images.clone(),
    }
    .run_cmd()?
    .into_iter()
    .filter(|dataset| *dataset != zfs.images)
    .filter_map(|dataset| {
        ImageKey::try_from(dataset.leaf())
            .ok()
            .map(|key| (key, dataset))
    })
    .collect())
}

fn reap_image(dataset: &Dataset) -> Result<bool> {
    match get::<ClonesProp, _>(Snapshot::base(dataset)).run_cmd()? {
        HasClones(true) => Ok(false),
        HasClones(false) => {
            info!("reaping base image {}", dataset);
            Destroy::Recursive(dataset.clone()).run_cmd().map(|()| true)
        }
    }
}

fn has_clones(value: &str) -> bool {
    !matches!(value.trim(), "" | "-")
}

fn extract(
    tarball: &Tarball,
    into: &Mountpoint,
    ownership: Ownership,
    idmap: IdRange,
) -> Result<()> {
    let (uid_map, gid_map) = (
        IdMap::of(IdKind::User, idmap),
        IdMap::of(IdKind::Group, idmap),
    );
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
    let dataset = Dataset::try_from(name)?;
    let presence = if exists(name)? {
        Presence::Existed
    } else {
        Create::Data(dataset.clone()).run_cmd()?;
        Presence::Created
    };
    Ok((get::<RawMountpointProp, _>(&dataset).run_cmd()?, presence))
}

fn zfs(args: &[String]) -> Result<String> {
    run(
        Command::new("zfs").args(args),
        &format!("zfs {}", args.first().map_or("", String::as_str)),
    )
}

fn argv(head: &[&str], tail: impl IntoIterator<Item = String>) -> Vec<String> {
    head.iter().copied().map(String::from).chain(tail).collect()
}

pub trait FromStdout: Sized {
    fn from_stdout(stdout: &str) -> Result<Self>;
}

impl FromStdout for () {
    fn from_stdout(_: &str) -> Result<()> {
        Ok(())
    }
}

impl FromStdout for PathBuf {
    fn from_stdout(stdout: &str) -> Result<PathBuf> {
        Ok(PathBuf::from(stdout.trim()))
    }
}

impl FromStdout for Mountpoint {
    fn from_stdout(stdout: &str) -> Result<Mountpoint> {
        Mountpoint::try_from(stdout)
    }
}

impl FromStdout for Vec<Dataset> {
    fn from_stdout(stdout: &str) -> Result<Vec<Dataset>> {
        Ok(stdout
            .lines()
            .filter_map(|line| Dataset::try_from(line.trim()).ok())
            .collect())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HasClones(bool);

impl FromStdout for HasClones {
    fn from_stdout(stdout: &str) -> Result<HasClones> {
        Ok(HasClones(has_clones(stdout)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginOut {
    NotCloned,
    Cloned { snapshot: Snapshot },
}

impl FromStdout for OriginOut {
    fn from_stdout(stdout: &str) -> Result<OriginOut> {
        match stdout.trim() {
            "-" => Ok(OriginOut::NotCloned),
            other => Snapshot::try_from(other).map(|snapshot| OriginOut::Cloned { snapshot }),
        }
    }
}

impl TryFrom<&Snapshot> for DataSnapshot {
    type Error = crate::zfs::AppError;
    fn try_from(shell: &Snapshot) -> Result<DataSnapshot> {
        Ok(DataSnapshot {
            dataset: CoreDataset::try_from(&shell.dataset)?,
            tag: DataTag::try_from(shell.tag)?,
        })
    }
}

impl TryFrom<SnapshotTag> for DataTag {
    type Error = crate::zfs::AppError;
    fn try_from(shell: SnapshotTag) -> Result<DataTag> {
        match shell {
            SnapshotTag::Start => Ok(DataTag::Start),
            SnapshotTag::Final => Ok(DataTag::Final),
            SnapshotTag::Base  => Err(AppError::ConversionError("Error parsing SnapshotTag::Base to DataTag::Base - variant not present in core, base is not slot slot data".to_string())),
        }
    }
}

impl TryFrom<&Dataset> for CoreDataset {
    type Error = crate::zfs::AppError;
    fn try_from(shell: &Dataset) -> Result<CoreDataset> {
        CoreDataset::try_from(shell.as_str()).map_err(|_| {
            AppError::ConversionError("Error parsing Dataset to CoreDataset".to_string())
        })
    }
}

pub trait Property {
    const NAME: &'static str;
    type Value: FromStdout;
}

pub enum MountpointProp {}
pub enum RawMountpointProp {}
pub enum ClonesProp {}
pub enum OriginProp {}

impl Property for MountpointProp {
    const NAME: &'static str = "mountpoint";
    type Value = Mountpoint;
}

impl Property for RawMountpointProp {
    const NAME: &'static str = "mountpoint";
    type Value = PathBuf;
}

impl Property for ClonesProp {
    const NAME: &'static str = "clones";
    type Value = HasClones;
}

impl Property for OriginProp {
    const NAME: &'static str = "origin";
    type Value = OriginOut;
}

pub struct Get<P, T> {
    target: T,
    property: PhantomData<P>,
}

fn get<P: Property, T: fmt::Display>(target: T) -> Get<P, T> {
    Get {
        target,
        property: PhantomData,
    }
}

pub enum Props {
    Data,
    Rootfs(DiskSize),
}

impl Props {
    fn options(&self) -> Vec<String> {
        match self {
            Props::Data => vec![
                String::from("acltype=posixacl"),
                String::from("xattr=sa"),
                String::from("atime=off"),
            ],
            Props::Rootfs(size) => vec![
                format!("refquota={size}"),
                String::from("acltype=posixacl"),
                String::from("xattr=sa"),
            ],
        }
        .into_iter()
        .flat_map(|prop| [String::from("-o"), prop])
        .collect()
    }
}

pub struct Promote {
    dataset: Dataset,
}

pub enum Destroy {
    Single(Dataset),
    Recursive(Dataset),
    Snapshot(Snapshot),
}

pub enum Create {
    Parents(Dataset),
    Data(Dataset),
}

pub struct TakeSnapshot {
    snapshot: Snapshot,
}

pub struct CloneSnapshot {
    origin: Snapshot,
    into: Dataset,
    props: Props,
}

pub struct ListChildren {
    parent: Dataset,
}

pub struct Rename {
    old: Dataset,
    new: Dataset,
}

pub trait ZfsCmd {
    type Output: FromStdout;
    fn construct_args(&self) -> Vec<String>;
    fn run_cmd(&self) -> Result<Self::Output> {
        Self::Output::from_stdout(&zfs(&self.construct_args())?)
    }
}

impl ZfsCmd for Promote {
    type Output = ();
    fn construct_args(&self) -> Vec<String> {
        argv(&["promote"], [self.dataset.to_string()])
    }
}

impl ZfsCmd for Destroy {
    type Output = ();
    fn construct_args(&self) -> Vec<String> {
        match self {
            Destroy::Single(dataset) => argv(&["destroy"], [dataset.to_string()]),
            Destroy::Recursive(dataset) => argv(&["destroy", "-r"], [dataset.to_string()]),
            Destroy::Snapshot(snapshot) => argv(&["destroy"], [snapshot.to_string()]),
        }
    }
}

impl ZfsCmd for Create {
    type Output = ();
    fn construct_args(&self) -> Vec<String> {
        match self {
            Create::Parents(dataset) => argv(&["create", "-p"], [dataset.to_string()]),
            Create::Data(dataset) => argv(
                &["create"],
                Props::Data
                    .options()
                    .into_iter()
                    .chain([dataset.to_string()]),
            ),
        }
    }
}

impl ZfsCmd for TakeSnapshot {
    type Output = ();
    fn construct_args(&self) -> Vec<String> {
        argv(&["snapshot"], [self.snapshot.to_string()])
    }
}

impl ZfsCmd for CloneSnapshot {
    type Output = ();
    fn construct_args(&self) -> Vec<String> {
        argv(
            &["clone"],
            self.props
                .options()
                .into_iter()
                .chain([self.origin.to_string(), self.into.to_string()]),
        )
    }
}

impl ZfsCmd for ListChildren {
    type Output = Vec<Dataset>;
    fn construct_args(&self) -> Vec<String> {
        argv(
            &["list", "-H", "-o", "name", "-t", "filesystem", "-d", "1"],
            [self.parent.to_string()],
        )
    }
}

impl ZfsCmd for Rename {
    type Output = ();
    fn construct_args(&self) -> Vec<String> {
        argv(&["rename"], [self.old.to_string(), self.new.to_string()])
    }
}

impl<P: Property, T: fmt::Display> ZfsCmd for Get<P, T> {
    type Output = P::Value;
    fn construct_args(&self) -> Vec<String> {
        argv(
            &["get", "-H", "-o", "value", P::NAME],
            [self.target.to_string()],
        )
    }
}

fn run(cmd: &mut Command, what: &str) -> Result<String> {
    let output = cmd.output()?;
    if output.status.success() {
        Ok(String::from_utf8(output.stdout)?)
    } else {
        Err(AppError::ZfsError(format!(
            "{} failed (exit: {:?}): {}",
            what,
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_rootfs_is_allocated_through_proxmox_under_the_slot_it_belongs_to() {
        let volume = RootfsVolume::for_slot(
            StorageId::try_from(String::from("ZFS")).unwrap(),
            SlotId::Blue(proxnix_core::Vmid::new(830)),
            DiskSize::gib(10),
        );
        assert_eq!(
            volume.alloc_args().join(" "),
            "alloc ZFS 830 subvol-830-disk-0 10G --format subvol"
        );
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
        assert_eq!(
            ImageKey::try_from(unprivileged.to_string().as_str()).unwrap(),
            unprivileged
        );
        assert_eq!(
            ImageKey::try_from(privileged.to_string().as_str()).unwrap(),
            privileged
        );
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
        assert_eq!(
            Snapshot::base(&dataset).to_string(),
            format!("ZFS/proxnix-images/{}@base", HASH)
        );
    }

    #[test]
    fn a_rootfs_volume_renders_as_a_proxmox_volume() {
        let volume = RootfsVolume::for_slot(
            images().storage,
            SlotId::Blue(Vmid::new(842)),
            DiskSize::gib(10),
        );
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
        let range = IdRange {
            host_base: 100_000,
            count: 65_536,
        };
        assert_eq!(
            IdMap::of(IdKind::User, range).to_string(),
            "u:0:100000:65536"
        );
        assert_eq!(
            IdMap::of(IdKind::Group, range).to_string(),
            "g:0:100000:65536"
        );
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
            serde_json::from_str(r#"{"storage":"ZFS","pool":"ZFS","images":"ZFS/proxnix-images"}"#)
                .unwrap();
        assert_eq!(parsed, images());
        let invalid: std::result::Result<ZfsImages, _> = serde_json::from_str(
            r#"{"storage":"ZFS","pool":"/ZFS","images":"ZFS/proxnix-images"}"#,
        );
        assert!(invalid.is_err());
    }

    #[test]
    fn a_data_snapshot_is_taken_by_its_full_name() {
        let data = DataSnapshot {
            dataset: CoreDataset::try_from("ZFS/proxnix/state/monitoring/blue").unwrap(),
            tag: DataTag::Final,
        };
        assert_eq!(
            TakeSnapshot {
                snapshot: Snapshot::from(&data)
            }
            .construct_args()
            .join(" "),
            "snapshot ZFS/proxnix/state/monitoring/blue@final"
        );
    }

    #[test]
    fn a_data_clone_carries_the_dataset_properties_and_names_origin_and_target() {
        let data = DataSnapshot {
            dataset: CoreDataset::try_from("ZFS/proxnix/state/monitoring/blue").unwrap(),
            tag: DataTag::Start,
        };
        let green = Dataset::try_from("ZFS/proxnix/state/monitoring/green").unwrap();
        assert_eq!(
            CloneSnapshot {
                origin: Snapshot::from(&data),
                into: green,
                props: Props::Data,
            }
            .construct_args()
            .join(" "),
            "clone -o acltype=posixacl -o xattr=sa -o atime=off ZFS/proxnix/state/monitoring/blue@start ZFS/proxnix/state/monitoring/green"
        );
    }

    fn image() -> Dataset {
        images().images.child(&ImageKey::new(
            NixHash::try_from(HASH).unwrap(),
            Ownership::Unprivileged,
        ))
    }

    fn joined(cmd: &impl ZfsCmd) -> String {
        cmd.construct_args().join(" ")
    }

    #[test]
    fn destroying_names_its_target_and_recurses_only_when_asked() {
        assert_eq!(
            joined(&Destroy::Recursive(image())),
            format!("destroy -r ZFS/proxnix-images/{HASH}")
        );
        assert_eq!(
            joined(&Destroy::Single(
                Dataset::try_from("ZFS/subvol-842-disk-0").unwrap()
            )),
            "destroy ZFS/subvol-842-disk-0"
        );
    }

    #[test]
    fn a_base_image_is_created_with_parents_and_no_options() {
        assert_eq!(
            joined(&Create::Parents(image())),
            format!("create -p ZFS/proxnix-images/{HASH}")
        );
    }

    #[test]
    fn a_host_dataset_is_created_with_the_data_properties() {
        assert_eq!(
            joined(&Create::Data(
                Dataset::try_from("ZFS/proxnix/state").unwrap()
            )),
            "create -o acltype=posixacl -o xattr=sa -o atime=off ZFS/proxnix/state"
        );
    }

    #[test]
    fn a_base_image_is_sealed_by_its_base_snapshot() {
        assert_eq!(
            joined(&TakeSnapshot {
                snapshot: Snapshot::base(&image())
            }),
            format!("snapshot ZFS/proxnix-images/{HASH}@base")
        );
    }

    #[test]
    fn a_rootfs_clone_carries_its_quota_first_and_no_atime() {
        assert_eq!(
            joined(&CloneSnapshot {
                origin: Snapshot::base(&image()),
                into: Dataset::try_from("ZFS/subvol-842-disk-0").unwrap(),
                props: Props::Rootfs(DiskSize::gib(10)),
            }),
            format!(
                "clone -o refquota=10G -o acltype=posixacl -o xattr=sa ZFS/proxnix-images/{HASH}@base ZFS/subvol-842-disk-0"
            )
        );
    }

    #[test]
    fn image_datasets_are_listed_one_level_deep() {
        assert_eq!(
            joined(&ListChildren {
                parent: images().images
            }),
            "list -H -o name -t filesystem -d 1 ZFS/proxnix-images"
        );
    }

    #[test]
    fn properties_are_read_as_bare_values_from_their_target() {
        assert_eq!(
            joined(&get::<MountpointProp, _>(&image())),
            format!("get -H -o value mountpoint ZFS/proxnix-images/{HASH}")
        );
        assert_eq!(
            joined(&get::<RawMountpointProp, _>("ZFS/proxnix/state")),
            "get -H -o value mountpoint ZFS/proxnix/state"
        );
        assert_eq!(
            joined(&get::<ClonesProp, _>(Snapshot::base(&image()))),
            format!("get -H -o value clones ZFS/proxnix-images/{HASH}@base")
        );
        assert_eq!(
            joined(&get::<OriginProp, _>(&image())),
            format!("get -H -o value origin ZFS/proxnix-images/{HASH}")
        );
    }

    #[test]
    fn an_origin_parses_into_its_snapshot() {
        assert_eq!(
            OriginOut::from_stdout("ZFS/proxnix/state/monitoring/blue@final\n").unwrap(),
            OriginOut::Cloned {
                snapshot: Snapshot {
                    dataset: Dataset::try_from("ZFS/proxnix/state/monitoring/blue").unwrap(),
                    tag: SnapshotTag::Final,
                }
            }
        );
        assert_eq!(OriginOut::from_stdout("-\n").unwrap(), OriginOut::NotCloned);
        assert!(OriginOut::from_stdout("ZFS/x@other").is_err());
    }

    #[test]
    fn a_listing_parses_into_datasets() {
        assert_eq!(
            Vec::<Dataset>::from_stdout("ZFS/proxnix-images\nZFS/proxnix-images/a\n").unwrap(),
            vec![
                Dataset::try_from("ZFS/proxnix-images").unwrap(),
                Dataset::try_from("ZFS/proxnix-images/a").unwrap(),
            ]
        );
    }

    #[test]
    fn a_rename_moves_the_old_name_to_the_new_one() {
        assert_eq!(
            joined(&Rename {
                old: Dataset::try_from("ZFS/proxnix/state/monitoring/green").unwrap(),
                new: Dataset::try_from("ZFS/proxnix/state/monitoring/green-rehearsal").unwrap(),
            }),
            "rename ZFS/proxnix/state/monitoring/green ZFS/proxnix/state/monitoring/green-rehearsal"
        );
    }
    #[test]
    fn a_single_snapshot_is_destroyed_by_name_without_recursing() {
        let start = DataSnapshot {
            dataset: CoreDataset::try_from("ZFS/proxnix/state/monitoring/green").unwrap(),
            tag: DataTag::Start,
        };
        assert_eq!(
            joined(&Destroy::Snapshot(Snapshot::from(&start))),
            "destroy ZFS/proxnix/state/monitoring/green@start"
        );
    }

    #[test]
    fn a_slot_snapshot_converts_into_the_core_and_back() {
        let shell = Snapshot::try_from("ZFS/proxnix/state/monitoring/blue@start").unwrap();
        let core = DataSnapshot::try_from(&shell).unwrap();
        assert_eq!(
            core,
            DataSnapshot {
                dataset: CoreDataset::try_from("ZFS/proxnix/state/monitoring/blue").unwrap(),
                tag: DataTag::Start,
            }
        );
        assert_eq!(Snapshot::from(&core), shell);
    }

    #[test]
    fn a_base_image_snapshot_is_not_slot_data() {
        let shell =
            Snapshot::try_from("ZFS/proxnix-images/ygpwy38d8s1hgh4m35lapbn8ifwyp8ls@base").unwrap();
        assert!(DataSnapshot::try_from(&shell).is_err());
    }

    #[test]
    fn a_shell_dataset_the_core_cannot_name_does_not_convert() {
        let shell = Dataset::try_from("ZFS/foo+bar").unwrap();
        assert!(CoreDataset::try_from(&shell).is_err());
    }
}
