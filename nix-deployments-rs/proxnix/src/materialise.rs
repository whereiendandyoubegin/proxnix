use crate::api::{self, Cli, Execute, GuestOp, Lxc, Qemu};
use crate::context::{ImageStore, StorePath, Tags};
use crate::host::{LxcConf, PveFs, copy_to_template_storage, ensure, ensure_all, host_text, mount_spec, stub_rootfs, write_conf};
use crate::nixstore::{NixStore, store_text};
use crate::interpret::Placed;
use crate::types::{AppError, ContainerConfig, DiskBus, IdRange, Result, StoreChoice, VMConfig};
use crate::zfs::{BaseImage, DiskSize, ImageKey, Ownership, RootfsVolume, Sealed, Tarball, ZfsImages};
use proxmox_api::nodes::node::lxc::{self, PostParams as LxcCreate};
use proxmox_api::nodes::node::qemu;
use proxnix_core::{HostEffect, Mount, Owner, RootHolder, SlotId, Storage, Toplevel};
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::num::NonZeroU64;
use tracing::warn;

fn qcow2_path(artifact_path: &str) -> String {
    format!("{artifact_path}/nixos.qcow2")
}

pub trait Materialise {
    fn provision_inactive(&self, artifact: &StorePath, tags: &Tags, image_store: ImageStore<'_>, target: SlotId) -> Result<()>;
}

macro_rules! lxc_settings {
    ($params:ident, $config:expr, $mounts:expr, $tags:expr, $base:expr) => {
        $params {
            hostname: Some($config.name.clone().try_into()?),
            memory: Some(i128::from($config.memory_mb).try_into()?),
            cores: Some(i128::from($config.cores).try_into()?),
            nets: [(0, format!("name=eth0,bridge={}", $config.network_bridge))].into(),
            features: Some("nesting=1".to_string()),
            tags: Some($tags.render()),
            mps: mount_points($mounts)?,
            ..$base
        }
    };
}

fn mount_points(mounts: &[Mount]) -> Result<HashMap<u32, String>> {
    mounts
        .iter()
        .enumerate()
        .map(|(i, mount)| Ok((u32::try_from(i)?, mount_spec(mount))))
        .collect()
}

fn vm_create(config: &VMConfig, artifact: &StorePath, tags: &Tags, target: SlotId) -> Result<qemu::PostParams> {
    let disk = HashMap::from([(
        config.disk_slot.index,
        format!(
            "{}:0,import-from={},format=raw",
            config.storage_location,
            qcow2_path(artifact.as_str())
        ),
    )]);
    let base = qemu::PostParams {
        name: Some(config.name.clone()),
        memory: Some(config.memory_mb.to_string()),
        cores: NonZeroU64::new(u64::from(config.cores)),
        nets: [(0, format!("virtio,bridge={}", config.network_bridge))].into(),
        scsihw: Some(config.scsi_hw.clone()),
        tags: Some(tags.render()),
        agent: Some("1".to_string()),
        serials: [(0, "socket".to_string().try_into()?)].into(),
        boot: Some(format!("order={}", config.disk_slot)),
        ..qemu::PostParams::new(api::vmid(target.inner())?)
    };
    Ok(match config.disk_slot.bus {
        DiskBus::Scsi => qemu::PostParams { scsis: disk, ..base },
        DiskBus::Virtio => qemu::PostParams { virtios: disk, ..base },
        DiskBus::Sata => qemu::PostParams { satas: disk, ..base },
        DiskBus::Ide => qemu::PostParams { ides: disk, ..base },
    })
}

fn vm_resize(config: &VMConfig) -> Result<qemu::vmid::resize::PutParams> {
    Ok(qemu::vmid::resize::PutParams::new(
        config
            .disk_slot
            .to_string()
            .as_str()
            .try_into()
            .map_err(AppError::InvalidDiskSlot)?,
        format!("{}G", config.disk_gb).try_into()?,
    ))
}

fn container_create(config: &ContainerConfig, mounts: &[Mount], ostemplate: String, tags: &Tags, target: SlotId) -> Result<LxcCreate> {
    Ok(lxc_settings!(
        LxcCreate,
        config,
        mounts,
        tags,
        LxcCreate {
            rootfs: Some(format!("{}:{}", config.storage_location, config.disk_gb)),
            ostype: Some(lxc::Ostype::Unmanaged),
            unprivileged: Some(!config.privileged),
            protection: Some(config.protected),
            ..LxcCreate::new(ostemplate.try_into()?, api::vmid(target.inner())?)
        }
    ))
}

fn create_from_clone(
    config: &ContainerConfig,
    storage: &Storage,
    idmap: IdRange,
    zfs: &ZfsImages,
    image: &BaseImage<Sealed>,
    tags: &Tags,
    target: SlotId,
) -> Result<()> {
    let volume = RootfsVolume::for_slot(zfs.storage.clone(), target, DiskSize::gib(config.disk_gb));
    let conf = LxcConf::of(config, &storage.mounts, tags, &volume, target)?;
    ensure_all(&storage.prepare, idmap)?;
    let clone = image.clone_rootfs(zfs, &volume)?;
    let written = write_conf(&PveFs::live(), target, &conf);
    if let Err(e) = &written
        && let Err(cleanup) = clone.discard()
    {
        warn!("could not discard rootfs clone for {} after {}: {}", target.inner().get(), e, cleanup);
    }
    written
}

impl Materialise for VMConfig {
    fn provision_inactive(&self, artifact: &StorePath, tags: &Tags, _image_store: ImageStore<'_>, target: SlotId) -> Result<()> {
        let id = target.inner();
        let create = vm_create(self, artifact, tags, target)?;
        let resize = vm_resize(self)?;
        Cli.run(&GuestOp::<Qemu>::Create(create))?;
        Cli.run(&GuestOp::<Qemu>::Resize(id, resize))?;
        Ok(())
    }
}

fn store_root(config: &ContainerConfig, storage: &Storage, shared: &proxnix_core::Dataset) -> Result<PathBuf> {
    match config.store {
        StoreChoice::Private => storage
            .mounts
            .iter()
            .find(|mount| mount.guest.0 == "/nix")
            .and_then(|mount| PathBuf::from(host_text(&mount.host)).parent().map(Path::to_path_buf))
            .ok_or_else(|| AppError::CmdError(format!("{} has a private store but no /nix mount", config.name))),
        StoreChoice::Shared | StoreChoice::Image => Ok(PathBuf::from(host_text(&shared.mountpoint()))),
    }
}

fn create_on_store(
    config: &ContainerConfig,
    storage: &Storage,
    artifact: &StorePath,
    tags: &Tags,
    image_store: ImageStore<'_>,
    target: SlotId,
) -> Result<()> {
    let refuse = |why: &str| AppError::CmdError(format!("{} boots from the store, but {why}", config.name));
    let zfs = image_store.zfs.filter(|zfs| zfs.storage.is(&config.storage_location)).ok_or_else(|| refuse("its rootfs is not on the zfs storage"))?;
    let shared = image_store.store.ok_or_else(|| refuse("there is no zfs pool to hold the store"))?;
    let toplevel = artifact
        .as_str()
        .parse::<proxnix_core::StorePath>()
        .map(Toplevel::from)
        .map_err(|fault| refuse(&format!("{} is not a store path ({fault:?})", artifact.as_str())))?;
    let volume = RootfsVolume::for_slot(zfs.storage.clone(), target, DiskSize::gib(config.disk_gb));
    let conf = LxcConf::of(config, &storage.mounts, tags, &volume, target)?;
    let root = store_root(config, storage, shared)?;
    ensure(&HostEffect::EnsureDataset { dataset: shared.clone(), owner: Owner::HostRoot }, image_store.idmap)?;
    ensure_all(&storage.prepare, image_store.idmap)?;
    let store = NixStore::seeding(root, image_store.seed_timeout);
    store.seed(&toplevel)?;
    store.root(&RootHolder::Guest(target.inner()), &toplevel)?;
    let rootfs = volume.allocate()?;
    let owner = (!config.privileged).then_some(image_store.idmap.host_base);
    let written = stub_rootfs(rootfs.path(), Path::new(&store_text(toplevel.path())), owner).and_then(|()| write_conf(&PveFs::live(), target, &conf));
    match written {
        Ok(()) => Ok(()),
        Err(error) => {
            if let Err(cleanup) = rootfs.release() {
                warn!("could not free the rootfs of {} after {error}: {cleanup}", target.inner().get());
            }
            Err(error)
        }
    }
}

impl Materialise for Placed {
    fn provision_inactive(&self, artifact: &StorePath, tags: &Tags, image_store: ImageStore<'_>, target: SlotId) -> Result<()> {
        let config = &self.config;
        let storage = self
            .storage
            .as_ref()
            .map_err(|fault| AppError::CmdError(format!("{} has no usable storage: {fault:?}", config.name)))?;
        if config.store != StoreChoice::Image {
            return create_on_store(config, storage, artifact, tags, image_store, target);
        }
        let tarball = Tarball::find(artifact.as_str())?;
        if let Some(zfs) = image_store.zfs.filter(|zfs| zfs.storage.is(&config.storage_location)) {
            let key = ImageKey::new(tags.nix_hash.clone(), Ownership::of(config.privileged));
            let image = BaseImage::ensure(zfs, &key, &tarball, image_store.idmap)?;
            create_from_clone(config, storage, image_store.idmap, zfs, &image, tags, target)
        } else {
            let ostemplate = copy_to_template_storage(&tarball, image_store.template_cache_path.as_str(), &tags.nix_hash)?;
            let create = container_create(config, &storage.mounts, ostemplate, tags, target)?;
            ensure_all(&storage.prepare, image_store.idmap)?;
            Cli.run(&GuestOp::<Lxc>::Create(create)).map(|_| ())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Kind;
    use crate::context::{ImageType, NixHash};
    use proxnix_core::{Slot, Vmid};

    fn tags() -> Tags {
        Tags::new(NixHash::try_from("k8whj0lg7k95jn6h57k99kvikc0zrpp3").unwrap(), "abc123", Slot::Blue)
    }

    fn vm() -> VMConfig {
        serde_json::from_value(serde_json::json!({
            "name": "website",
            "blue_id": 823,
            "green_id": 824,
            "hostname": "website",
            "dhcp_timeout_seconds": 240,
            "health_check_timeout_seconds": 180,
            "image_type": "build-qcow2-website",
            "cores": 2,
            "sockets": 1,
            "memory_mb": 2048,
            "storage_location": "local-lvm",
            "disk_gb": 20,
            "protected": false,
            "impure": false
        }))
        .unwrap()
    }

    fn container(name: &str) -> ContainerConfig {
        ContainerConfig {
            name: name.to_string(),
            hostname: name.to_string(),
            service_address: None,
            backend_port: 80,
            tcp_ports: vec![],
            dhcp_timeout_seconds: 240,
            health_check_timeout_seconds: 180,
            blue_id: Vmid::new(946),
            green_id: Vmid::new(947),
            image_type: ImageType::from("build-lxc-web"),
            cores: 2,
            memory_mb: 512,
            storage_location: "ZFS".to_string(),
            disk_gb: 8,
            protected: true,
            privileged: false,
            state: vec![],
            mounts: vec![],
            secrets: false,
            store: crate::types::StoreChoice::Image,
            network_bridge: "vmbr0".to_string(),
            impure: false,
            cutover: None,
        }
    }

    fn rendered<K: Kind>(op: &GuestOp<K>) -> String {
        op.invocation().unwrap().to_string()
    }

    #[test]
    fn a_vm_is_created_with_its_disk_imported_in_one_call() {
        let artifact = StorePath::try_from("/nix/store/k8whj0lg7k95jn6h57k99kvikc0zrpp3-website".to_string()).unwrap();
        let create = vm_create(&vm(), &artifact, &tags(), SlotId::Blue(Vmid::new(823))).unwrap();
        assert_eq!(
            rendered(&GuestOp::<Qemu>::Create(create)),
            "qm create 823 --agent 1 --boot order=scsi0 --cores 2 --memory 2048 --name website \
             --net0 virtio,bridge=vmbr0 \
             --scsi0 local-lvm:0,import-from=/nix/store/k8whj0lg7k95jn6h57k99kvikc0zrpp3-website/nixos.qcow2,format=raw \
             --scsihw virtio-scsi-pci --serial0 socket \
             --tags proxnix;nix-k8whj0lg7k95jn6h57k99kvikc0zrpp3;commit-abc123;slot-blue"
        );
    }

    #[test]
    fn the_disk_slot_picks_the_bus_it_is_imported_on() {
        let config = VMConfig { disk_slot: "virtio1".parse().unwrap(), ..vm() };
        let artifact = StorePath::try_from("/nix/store/x-website".to_string()).unwrap();
        let create = vm_create(&config, &artifact, &tags(), SlotId::Blue(Vmid::new(823))).unwrap();
        assert_eq!(create.virtios.keys().collect::<Vec<_>>(), [&1]);
        assert!(create.scsis.is_empty());
        assert_eq!(create.boot.as_deref(), Some("order=virtio1"));
    }

    #[test]
    fn a_vm_disk_is_resized_after_import() {
        assert_eq!(
            rendered(&GuestOp::<Qemu>::Resize(Vmid::new(823), vm_resize(&vm()).unwrap())),
            "qm disk resize 823 scsi0 20G"
        );
    }

    #[test]
    fn a_container_is_created_from_its_template() {
        let mounts = [Mount {
            host: proxnix_core::HostPath::try_from("/var/lib/proxnix/web").unwrap(),
            guest: proxnix_core::GuestPath(String::from("/var/lib/web")),
            mode: proxnix_core::MountMode::ReadOnly,
        }];
        let create = container_create(&container("web"), &mounts, "local:vztmpl/web.tar.xz".to_string(), &tags(), SlotId::Blue(Vmid::new(946))).unwrap();
        assert_eq!(
            rendered(&GuestOp::<Lxc>::Create(create)),
            "pct create 946 local:vztmpl/web.tar.xz --cores 2 --features nesting=1 --hostname web \
             --memory 512 --mp0 /var/lib/proxnix/web,mp=/var/lib/web,ro=1 \
             --net0 name=eth0,bridge=vmbr0 --ostype unmanaged --protection 1 --rootfs ZFS:8 \
             --tags proxnix;nix-k8whj0lg7k95jn6h57k99kvikc0zrpp3;commit-abc123;slot-blue --unprivileged 1"
        );
    }

    #[test]
    fn a_shared_container_seeds_the_shared_store_and_a_private_one_its_own_nix() {
        let settings = crate::state::parse_appconfig(crate::state::tests_support::NIXOLOGY_APPCONFIG).unwrap();
        let layout = crate::engine::layout(&settings).unwrap();
        let spec = |store: StoreChoice| {
            let config = ContainerConfig { name: String::from("hydra"), store, cutover: Some(crate::types::CutoverChoice::StopStart), ..container("hydra") };
            let wanted = proxnix_core::StorageSpec {
                state: vec![],
                mounts: vec![],
                secrets: false,
                privilege: proxnix_core::Privilege::Unprivileged,
                store: match store {
                    StoreChoice::Image => proxnix_core::StoreMode::Image,
                    StoreChoice::Shared => proxnix_core::StoreMode::Shared,
                    StoreChoice::Private => proxnix_core::StoreMode::Private,
                },
            };
            (layout.storage(&proxnix_core::GuestName(String::from("hydra")), &wanted).unwrap(), config)
        };
        let (shared, shared_config) = spec(StoreChoice::Shared);
        assert_eq!(store_root(&shared_config, &shared, &layout.store()).unwrap(), PathBuf::from("/ZFS/proxnix/store"));
        let (private, private_config) = spec(StoreChoice::Private);
        assert_eq!(store_root(&private_config, &private, &layout.store()).unwrap(), PathBuf::from("/ZFS/proxnix/state/hydra/nix"));
    }

    fn vm_with(key: &str, value: &str) -> serde_json::Result<VMConfig> {
        let serde_json::Value::Object(fields) = serde_json::to_value(vm()).unwrap() else {
            unreachable!()
        };
        serde_json::from_value(
            fields
                .into_iter()
                .map(|(k, v)| if k == key { (k, value.into()) } else { (k, v) })
                .collect::<serde_json::Map<_, _>>()
                .into(),
        )
    }

    #[test]
    fn config_round_trips_through_the_typed_fields() {
        let config = vm_with("disk_slot", "sata2").unwrap();
        assert_eq!(config.disk_slot.to_string(), "sata2");
    }

    #[test]
    fn an_unknown_disk_slot_is_rejected_when_the_config_is_read() {
        assert!(vm_with("disk_slot", "floppy0").is_err());
    }

    #[test]
    fn an_unknown_scsi_controller_is_rejected_when_the_config_is_read() {
        assert!(vm_with("scsi_hw", "not-a-controller").is_err());
    }
}
