#[pure_only]
use crate::guest::{GuestPath, HostPath, Mount, MountMode, PathFault, Privilege};
use crate::ids::Slot;
#[pure_only]
use crate::spec::{Cutover, GuestName};
use proxnix_pure::pure_only;

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Segment(String);

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadSegment;

#[pure_only]
impl TryFrom<&str> for Segment {
    type Error = BadSegment;

    fn try_from(text: &str) -> Result<Segment, BadSegment> {
        let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':');
        match text {
            "" | "." | ".." => Err(BadSegment),
            name if name.chars().all(allowed) => Ok(Segment(String::from(name))),
            _ => Err(BadSegment),
        }
    }
}

#[pure_only]
impl AsRef<str> for Segment {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fixed {
    Proxnix,
    Store,
    State,
    Logs,
    Nix,
    Blue { rehearsal: Option<Rehearsal> },
    Green { rehearsal: Option<Rehearsal> },
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlotColour {
    Blue,
    Green,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rehearsal {}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DataTag {
    Start,
    Final,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DataSnapshot {
    pub dataset: Dataset,
    pub tag: DataTag,
}

#[pure_only]
impl From<Fixed> for Segment {
    fn from(fixed: Fixed) -> Segment {
        Segment(String::from(match fixed {
            Fixed::Proxnix => "proxnix",
            Fixed::Store => "store",
            Fixed::State => "state",
            Fixed::Logs => "logs",
            Fixed::Nix => "nix",
            Fixed::Blue {
                rehearsal: Some(_rehearsal),
            } => "blue-rehearsal",
            Fixed::Blue { rehearsal: None } => "blue",
            Fixed::Green {
                rehearsal: Some(_rehearsal),
            } => "green-rehearsal",
            Fixed::Green { rehearsal: None } => "green",
        }))
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dataset(Vec<Segment>);

#[pure_only]
impl TryFrom<&str> for Dataset {
    type Error = BadSegment;

    fn try_from(text: &str) -> Result<Dataset, BadSegment> {
        text.split('/')
            .map(Segment::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map(Dataset)
    }
}

#[pure_only]
impl Dataset {
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.0
    }

    #[must_use]
    pub fn mountpoint(&self) -> HostPath {
        HostPath::within(self, &[])
    }

    #[must_use]
    pub fn parent(&self) -> Option<Dataset> {
        match self.0.split_last() {
            Some((_, rest)) if !rest.is_empty() => Some(Dataset(rest.to_vec())),
            _ => None,
        }
    }

    fn child(&self, segment: Segment) -> Dataset {
        Dataset(self.0.iter().cloned().chain([segment]).collect())
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Owner {
    HostRoot,
    GuestRoot,
}

#[pure_only]
impl Owner {
    #[must_use]
    pub fn writer(privilege: Privilege) -> Owner {
        match privilege {
            Privilege::Privileged => Owner::HostRoot,
            Privilege::Unprivileged => Owner::GuestRoot,
        }
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostEffect {
    EnsureDataset { dataset: Dataset, owner: Owner },
    EnsureDirectory { path: HostPath, owner: Owner },
    EnsureHostPath { path: HostPath, owner: Owner },
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StateLabel(Segment);

#[pure_only]
impl StateLabel {
    #[must_use]
    pub fn of(at: &GuestPath) -> Option<StateLabel> {
        at.0.rsplit('/')
            .next()
            .map(|last| last.trim_start_matches('.'))
            .and_then(|last| Segment::try_from(last).ok())
            .map(StateLabel)
    }
}

#[pure_only]
impl AsRef<str> for StateLabel {
    fn as_ref(&self) -> &str {
        self.0.as_ref()
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageFault {
    NoLayout,
    BadHostPath(GuestPath, PathFault),
    UnnamableWorkload(GuestName),
    UnlabelledState(GuestPath),
    SameLabel(StateLabel),
    MountedTwice(GuestPath),
    PrivateStoreNeedsStopStart,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageSpec {
    pub state: Vec<GuestPath>,
    pub mounts: Vec<Mount>,
    pub secrets: bool,
    pub privilege: Privilege,
    pub store: StoreMode,
}

#[pure_only]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StoreMode {
    #[default]
    Image,
    Shared,
    Private,
}

#[pure_only]
pub fn store_cutover(store: StoreMode, cutover: Cutover) -> Result<(), StorageFault> {
    match (store, cutover) {
        (StoreMode::Private, Cutover::Overlap) => Err(StorageFault::PrivateStoreNeedsStopStart),
        _ => Ok(()),
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Shared,
    PerSlot(SlotColour),
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Storage {
    pub mounts: Vec<Mount>,
    pub prepare: Vec<HostEffect>,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: Dataset,
    secrets: HostPath,
}

#[pure_only]
fn journal() -> GuestPath {
    GuestPath(String::from("/var/log/journal"))
}

#[pure_only]
fn nix_store() -> GuestPath {
    GuestPath(String::from("/nix/store"))
}

#[pure_only]
fn nix() -> GuestPath {
    GuestPath(String::from("/nix"))
}

#[pure_only]
fn sops_key() -> GuestPath {
    GuestPath(String::from("/var/lib/sops-key"))
}

#[pure_only]
fn repeated<T: PartialEq + Clone>(items: &[T]) -> Option<T> {
    items
        .iter()
        .enumerate()
        .find(|(index, item)| items[..*index].contains(item))
        .map(|(_, item)| item.clone())
}

#[pure_only]
fn determine_placement(plan: (Cutover, SlotColour)) -> Placement {
    match plan {
        (Cutover::FenceTransfer, _) => Placement::PerSlot(plan.1),
        _ => Placement::Shared,
    }
}

#[pure_only]
fn labelled(state: &[GuestPath]) -> Result<Vec<(GuestPath, StateLabel)>, StorageFault> {
    let found: Vec<(GuestPath, StateLabel)> = state
        .iter()
        .map(|at| {
            StateLabel::of(at)
                .map(|label| (at.clone(), label))
                .ok_or_else(|| StorageFault::UnlabelledState(at.clone()))
        })
        .collect::<Result<_, _>>()?;
    match repeated(
        &found
            .iter()
            .map(|(_, label)| label.clone())
            .collect::<Vec<_>>(),
    ) {
        Some(label) => Err(StorageFault::SameLabel(label)),
        None => Ok(found),
    }
}

#[pure_only]
fn distinct(mounts: Vec<Mount>) -> Result<Vec<Mount>, StorageFault> {
    match repeated(
        &mounts
            .iter()
            .map(|mount| mount.guest.clone())
            .collect::<Vec<_>>(),
    ) {
        Some(twice) => Err(StorageFault::MountedTwice(twice)),
        None => Ok(mounts),
    }
}

#[pure_only]
fn workload(name: &GuestName) -> Result<Segment, StorageFault> {
    Segment::try_from(name.0.as_str()).map_err(|_| StorageFault::UnnamableWorkload(name.clone()))
}

#[pure_only]
impl Layout {
    #[must_use]
    pub fn under(pool: &Dataset, secrets: HostPath) -> Layout {
        Layout {
            root: pool.child(Segment::from(Fixed::Proxnix)),
            secrets,
        }
    }

    #[must_use]
    pub fn root(&self) -> &Dataset {
        &self.root
    }

    #[must_use]
    pub fn store(&self) -> Dataset {
        self.root.child(Segment::from(Fixed::Store))
    }

    #[must_use]
    pub fn state(&self) -> Dataset {
        self.root.child(Segment::from(Fixed::State))
    }

    pub fn slot_home(&self, name: &GuestName, slot: SlotColour) -> Result<Dataset, StorageFault> {
        let side = match slot {
            SlotColour::Blue => Fixed::Blue { rehearsal: None },
            SlotColour::Green => Fixed::Green { rehearsal: None },
        };
        Ok(self
            .state()
            .child(workload(name)?)
            .child(Segment::from(side)))
    }

    #[must_use]
    pub fn logs(&self) -> Dataset {
        self.root.child(Segment::from(Fixed::Logs))
    }

    pub fn storage(
        &self,
        name: &GuestName,
        wanted: &StorageSpec,
        placement: Placement,
    ) -> Result<Storage, StorageFault> {
        let workload = Segment::try_from(name.0.as_str())
            .map_err(|_| StorageFault::UnnamableWorkload(name.clone()))?;
        let labelled = labelled(&wanted.state)?;
        let owner = Owner::writer(wanted.privilege);
        let home = self.state().child(workload.clone());
        #[pure_only]
        enum StateHome {
            Datasets(Dataset),
            Directories(Dataset),
        }
        let state_home = match placement {
            Placement::Shared => StateHome::Datasets(home.clone()),
            Placement::PerSlot(colour) => StateHome::Directories(self.slot_home(name, colour)?),
        };
        let logs = Mount {
            host: HostPath::within(&self.logs(), &[workload]),
            guest: journal(),
            mode: MountMode::ReadWrite,
        };
        let private = home.child(Segment::from(Fixed::Nix));
        let store = match wanted.store {
            StoreMode::Image => None,
            StoreMode::Shared => Some(Mount {
                host: HostPath::within(
                    &self.store(),
                    &[Segment::from(Fixed::Nix), Segment::from(Fixed::Store)],
                ),
                guest: nix_store(),
                mode: MountMode::ReadOnly,
            }),
            StoreMode::Private => Some(Mount {
                host: HostPath::within(&private, &[Segment::from(Fixed::Nix)]),
                guest: nix(),
                mode: MountMode::ReadWrite,
            }),
        };
        let owns_home = !labelled.is_empty() || wanted.store == StoreMode::Private;
        let mounts = distinct(
            labelled
                .iter()
                .map(|(at, label)| Mount {
                    // host: home.child(label.0.clone()).mountpoint(),
                    host: match &state_home {
                        StateHome::Datasets(home) => home.child(label.0.clone()).mountpoint(),
                        StateHome::Directories(slot_home) => {
                            HostPath::within(slot_home, &[label.0.clone()])
                        }
                    },
                    guest: at.clone(),
                    mode: MountMode::ReadWrite,
                })
                .chain(wanted.mounts.iter().cloned())
                .chain([logs.clone()])
                .chain(wanted.secrets.then(|| Mount {
                    host: self.secrets.clone(),
                    guest: sops_key(),
                    mode: MountMode::ReadOnly,
                }))
                .chain(store)
                .collect(),
        )?;
        let prepare = [self.root.clone(), self.state()]
            .into_iter()
            .chain(owns_home.then(|| home.clone()))
            .map(|dataset| HostEffect::EnsureDataset {
                dataset,
                owner: Owner::HostRoot,
            })
            .chain(labelled.iter().map(|(_, label)| HostEffect::EnsureDataset {
                dataset: home.child(label.0.clone()),
                owner,
            }))
            .chain(
                (wanted.store == StoreMode::Private).then(|| HostEffect::EnsureDataset {
                    dataset: private.clone(),
                    owner,
                }),
            )
            .chain([
                HostEffect::EnsureDataset {
                    dataset: self.logs(),
                    owner: Owner::HostRoot,
                },
                HostEffect::EnsureDirectory {
                    path: logs.host,
                    owner,
                },
            ])
            .chain(
                wanted
                    .mounts
                    .iter()
                    .filter(|mount| mount.mode == MountMode::ReadWrite)
                    .map(|mount| HostEffect::EnsureHostPath {
                        path: mount.host.clone(),
                        owner,
                    }),
            )
            .collect();
        Ok(Storage { mounts, prepare })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> HostPath {
        HostPath::try_from(text).unwrap()
    }

    fn layout() -> Layout {
        Layout::under(
            &Dataset::try_from("ZFS").unwrap(),
            path("/var/lib/proxnix/sops"),
        )
    }

    fn named(dataset: &Dataset) -> String {
        dataset
            .segments()
            .iter()
            .map(AsRef::as_ref)
            .collect::<Vec<&str>>()
            .join("/")
    }

    fn shown(host: &HostPath) -> String {
        host.parts()
            .iter()
            .fold(String::new(), |shown, part| shown + "/" + part.as_ref())
    }

    fn at(text: &str) -> GuestPath {
        GuestPath(String::from(text))
    }

    fn wanted(state: &[&str], secrets: bool) -> StorageSpec {
        StorageSpec {
            state: state.iter().map(|text| at(text)).collect(),
            mounts: vec![],
            secrets,
            privilege: Privilege::Unprivileged,
            store: StoreMode::Image,
        }
    }

    fn hosts(storage: &Storage) -> Vec<(String, String)> {
        storage
            .mounts
            .iter()
            .map(|mount| (shown(&mount.host), mount.guest.0.clone()))
            .collect()
    }

    fn workload(name: &str) -> GuestName {
        GuestName(String::from(name))
    }

    #[test]
    fn the_layout_lives_under_the_pool_in_three_fixed_datasets() {
        assert_eq!(named(&layout().store()), "ZFS/proxnix/store");
        assert_eq!(named(&layout().state()), "ZFS/proxnix/state");
        assert_eq!(named(&layout().logs()), "ZFS/proxnix/logs");
        assert_eq!(shown(&layout().logs().mountpoint()), "/ZFS/proxnix/logs");
        assert_eq!(layout().logs().parent().as_ref(), Some(layout().root()));
        assert!(Dataset::try_from("ZFS/../etc").is_err());
    }

    #[test]
    fn a_layout_path_and_the_same_path_reported_by_proxmox_are_equal() {
        let storage = layout()
            .storage(&workload("forgejo"), &wanted(&["/var/lib/forgejo"], false))
            .unwrap();
        assert_eq!(
            storage.mounts[0].host,
            path("/ZFS/proxnix/state/forgejo/forgejo")
        );
        assert_eq!(path("/ZFS//proxnix/"), layout().root().mountpoint());
    }

    #[test]
    fn host_paths_must_be_absolute_and_never_climb_or_enter_the_nix_store() {
        assert_eq!(
            HostPath::try_from("var/lib"),
            Err(crate::guest::PathFault::Relative)
        );
        assert_eq!(
            HostPath::try_from("/var/../etc"),
            Err(crate::guest::PathFault::Climbs)
        );
        assert_eq!(
            HostPath::try_from("/nix/store/abc-x"),
            Err(crate::guest::PathFault::InStore)
        );
        assert!(HostPath::try_from("/ZFS/nixflix").is_ok());
    }

    #[test]
    fn each_state_path_gets_its_own_dataset_named_by_its_last_component() {
        let storage = layout()
            .storage(
                &workload("hydra"),
                &wanted(&["/var/lib/hydra", "/var/lib/nix-cache-key"], false),
            )
            .unwrap();
        assert_eq!(
            hosts(&storage),
            vec![
                (
                    String::from("/ZFS/proxnix/state/hydra/hydra"),
                    String::from("/var/lib/hydra")
                ),
                (
                    String::from("/ZFS/proxnix/state/hydra/nix-cache-key"),
                    String::from("/var/lib/nix-cache-key")
                ),
                (
                    String::from("/ZFS/proxnix/logs/hydra"),
                    String::from("/var/log/journal")
                ),
            ]
        );
    }

    #[test]
    fn a_hidden_state_directory_drops_its_dot() {
        assert_eq!(
            StateLabel::of(&at("/data/.state")).map(|label| String::from(label.as_ref())),
            Some(String::from("state"))
        );
        assert_eq!(StateLabel::of(&at("/")), None);
        assert_eq!(StateLabel::of(&at("/data/..")), None);
    }

    #[test]
    fn two_state_paths_with_one_name_or_one_guest_path_are_refused() {
        assert_eq!(
            layout().storage(&workload("x"), &wanted(&["/a/data", "/b/data"], false)),
            Err(StorageFault::SameLabel(
                StateLabel::of(&at("/a/data")).unwrap()
            ))
        );
        assert_eq!(
            layout().storage(&workload("x"), &wanted(&["/var/log/journal"], false)),
            Err(StorageFault::MountedTwice(journal()))
        );
        assert_eq!(
            layout().storage(&workload("no/slash"), &wanted(&[], false)),
            Err(StorageFault::UnnamableWorkload(workload("no/slash")))
        );
    }

    #[test]
    fn a_workload_without_state_gets_no_state_dataset() {
        let storage = layout()
            .storage(&workload("cloudflared"), &wanted(&[], true))
            .unwrap();
        let home = layout()
            .state()
            .child(Segment::try_from("cloudflared").unwrap());
        assert!(!storage.prepare.iter().any(
            |effect| matches!(effect, HostEffect::EnsureDataset { dataset, .. } if *dataset == home)
        ));
    }

    #[test]
    fn the_secrets_key_is_mounted_read_only_only_when_asked_for() {
        let without = layout()
            .storage(&workload("updater"), &wanted(&[], false))
            .unwrap();
        assert!(without.mounts.iter().all(|mount| mount.guest != sops_key()));
        let with = layout()
            .storage(&workload("forgejo"), &wanted(&[], true))
            .unwrap();
        assert!(with.mounts.contains(&Mount {
            host: path("/var/lib/proxnix/sops"),
            guest: sops_key(),
            mode: MountMode::ReadOnly
        }));
        assert!(
            !with
                .prepare
                .iter()
                .any(|effect| matches!(effect, HostEffect::EnsureHostPath { .. })),
            "the key is never created or chowned"
        );
    }

    #[test]
    fn datasets_are_ensured_parents_first_and_owned_by_whoever_writes_them() {
        let storage = layout()
            .storage(&workload("forgejo"), &wanted(&["/var/lib/forgejo"], true))
            .unwrap();
        let ensured: Vec<(String, Owner)> = storage
            .prepare
            .iter()
            .map(|effect| match effect {
                HostEffect::EnsureDataset { dataset, owner } => (named(dataset), *owner),
                HostEffect::EnsureDirectory { path, owner }
                | HostEffect::EnsureHostPath { path, owner } => (shown(path), *owner),
            })
            .collect();
        assert_eq!(
            ensured,
            vec![
                (String::from("ZFS/proxnix"), Owner::HostRoot),
                (String::from("ZFS/proxnix/state"), Owner::HostRoot),
                (String::from("ZFS/proxnix/state/forgejo"), Owner::HostRoot),
                (
                    String::from("ZFS/proxnix/state/forgejo/forgejo"),
                    Owner::GuestRoot
                ),
                (String::from("ZFS/proxnix/logs"), Owner::HostRoot),
                (String::from("/ZFS/proxnix/logs/forgejo"), Owner::GuestRoot),
            ]
        );
    }

    #[test]
    fn explicit_host_data_is_mounted_as_given_and_only_created_if_missing() {
        let media = Mount {
            host: path("/ZFS/nixflix"),
            guest: at("/data/media"),
            mode: MountMode::ReadWrite,
        };
        let storage = layout()
            .storage(
                &workload("nixflix"),
                &StorageSpec {
                    mounts: vec![media.clone()],
                    ..wanted(&["/data/.state"], true)
                },
            )
            .unwrap();
        assert!(storage.mounts.contains(&media));
        assert!(storage.prepare.contains(&HostEffect::EnsureHostPath {
            path: media.host,
            owner: Owner::GuestRoot
        }));
    }

    #[test]
    fn an_image_container_gets_no_store_mount_at_all() {
        let storage = layout()
            .storage(&workload("forgejo"), &wanted(&["/var/lib/forgejo"], true))
            .unwrap();
        assert!(
            storage
                .mounts
                .iter()
                .all(|mount| mount.guest != nix_store() && mount.guest != nix())
        );
    }

    #[test]
    fn a_shared_store_container_mounts_the_synced_store_read_only_and_owns_nothing_new() {
        let image = layout()
            .storage(&workload("test-container"), &wanted(&[], true))
            .unwrap();
        let shared = layout()
            .storage(
                &workload("test-container"),
                &StorageSpec {
                    store: StoreMode::Shared,
                    ..wanted(&[], true)
                },
            )
            .unwrap();
        assert!(shared.mounts.contains(&Mount {
            host: path("/ZFS/proxnix/store/nix/store"),
            guest: nix_store(),
            mode: MountMode::ReadOnly
        }));
        assert_eq!(
            shared.prepare, image.prepare,
            "the store dataset belongs to the sync, not to any workload"
        );
    }

    #[test]
    fn a_private_store_container_gets_its_own_writable_nix_dataset() {
        let storage = layout()
            .storage(
                &workload("hydra"),
                &StorageSpec {
                    store: StoreMode::Private,
                    ..wanted(&["/var/lib/hydra"], true)
                },
            )
            .unwrap();
        assert!(storage.mounts.contains(&Mount {
            host: path("/ZFS/proxnix/state/hydra/nix/nix"),
            guest: nix(),
            mode: MountMode::ReadWrite
        }));
        let nix_dataset = layout()
            .state()
            .child(Segment::try_from("hydra").unwrap())
            .child(Segment::from(Fixed::Nix));
        assert!(storage.prepare.contains(&HostEffect::EnsureDataset {
            dataset: nix_dataset,
            owner: Owner::GuestRoot
        }));
        assert!(
            !storage
                .mounts
                .iter()
                .any(|mount| mount.guest == nix_store())
        );
    }

    #[test]
    fn a_private_store_cannot_overlap_because_two_nix_daemons_cannot_share_one_database() {
        assert_eq!(
            store_cutover(StoreMode::Private, Cutover::Overlap),
            Err(StorageFault::PrivateStoreNeedsStopStart)
        );
        assert_eq!(
            store_cutover(StoreMode::Private, Cutover::StopStart),
            Ok(())
        );
        assert_eq!(
            store_cutover(StoreMode::Private, Cutover::Protected),
            Ok(())
        );
        assert_eq!(store_cutover(StoreMode::Shared, Cutover::Overlap), Ok(()));
        assert_eq!(store_cutover(StoreMode::Image, Cutover::Overlap), Ok(()));
    }

    #[test]
    fn a_private_store_mounted_over_a_state_path_is_refused() {
        assert_eq!(
            layout().storage(
                &workload("x"),
                &StorageSpec {
                    store: StoreMode::Private,
                    ..wanted(&["/nix"], false)
                }
            ),
            Err(StorageFault::MountedTwice(nix()))
        );
    }

    #[test]
    fn a_privileged_container_leaves_its_state_owned_by_host_root() {
        let storage = layout()
            .storage(
                &workload("pihole"),
                &StorageSpec {
                    privilege: Privilege::Privileged,
                    ..wanted(&["/etc/pihole"], false)
                },
            )
            .unwrap();
        assert!(storage.prepare.iter().all(|effect| match effect {
            HostEffect::EnsureDataset { owner, .. }
            | HostEffect::EnsureDirectory { owner, .. }
            | HostEffect::EnsureHostPath { owner, .. } => *owner == Owner::HostRoot,
        }));
    }
}
