use crate::api::{self, Cli, Execute, GuestOp, Lxc, Qemu};
use crate::context::{ImageStore, StorePath, Tags};
use crate::host::{ConfPath, LxcConf, copy_to_template_storage, prepare_bind_mount, write_conf};
use crate::types::{AppError, BindMount, ContainerConfig, DiskBus, IdRange, MountMode, Result, VMConfig};
use crate::zfs::{BaseImage, DiskSize, ImageKey, Ownership, Sealed, Tarball, ZfsImages};
use proxmox_api::nodes::node::lxc::{self, PostParams as LxcCreate, vmid::config::PutParams as LxcSet};
use proxmox_api::nodes::node::qemu;
use proxnix_core::SlotId;
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
    ($params:ident, $config:expr, $tags:expr, $base:expr) => {
        $params {
            hostname: Some($config.name.clone().try_into()?),
            memory: Some(i128::from($config.memory_mb).try_into()?),
            cores: Some(i128::from($config.cores).try_into()?),
            nets: [(0, format!("name=eth0,bridge={}", $config.network_bridge))].into(),
            features: Some("nesting=1".to_string()),
            tags: Some($tags.render()),
            mps: mount_points(&$config.bind_mounts)?,
            ..$base
        }
    };
}

fn mount_points(mounts: &[BindMount]) -> Result<HashMap<u32, String>> {
    mounts
        .iter()
        .enumerate()
        .map(|(i, mount)| {
            let suffix = match mount.mode {
                MountMode::ReadOnly => ",ro=1",
                MountMode::ReadWrite => "",
            };
            Ok((
                u32::try_from(i)?,
                format!("{},mp={}{}", mount.host_path, mount.container_path, suffix),
            ))
        })
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

fn container_create(config: &ContainerConfig, ostemplate: String, tags: &Tags, target: SlotId) -> Result<LxcCreate> {
    Ok(lxc_settings!(
        LxcCreate,
        config,
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

fn container_settings(config: &ContainerConfig, tags: &Tags) -> Result<LxcSet> {
    Ok(lxc_settings!(LxcSet, config, tags, LxcSet::default()))
}

fn create_from_clone(
    config: &ContainerConfig,
    idmap: IdRange,
    zfs: &ZfsImages,
    image: &BaseImage<Sealed>,
    tags: &Tags,
    target: SlotId,
) -> Result<()> {
    let id = target.inner();
    let settings = container_settings(config, tags)?;
    config
        .bind_mounts
        .iter()
        .try_for_each(|mount| prepare_bind_mount(mount, config.privileged, idmap))?;

    let clone = image.clone_rootfs(zfs, target, DiskSize::gib(config.disk_gb))?;
    let conf = LxcConf {
        rootfs: clone.volume(),
        ownership: Ownership::of(config.privileged),
    };
    if let Err(e) = write_conf(&ConfPath::of(target), &conf) {
        if let Err(cleanup) = clone.discard() {
            warn!("could not discard rootfs clone for {}: {}", target.inner().get(), cleanup);
        }
        return Err(e);
    }

    let configured = Cli
        .run(&GuestOp::<Lxc>::Set(id, settings))
        .and_then(|_| Cli.run(&GuestOp::<Lxc>::protection(id, config.protected)));
    match configured {
        Ok(_) => Ok(()),
        Err(e) => {
            if let Err(cleanup) = Cli.run(&GuestOp::<Lxc>::destroy(id)) {
                warn!("could not remove half-configured container {}: {}", target.inner().get(), cleanup);
            }
            Err(e)
        }
    }
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

impl Materialise for ContainerConfig {
    fn provision_inactive(&self, artifact: &StorePath, tags: &Tags, image_store: ImageStore<'_>, target: SlotId) -> Result<()> {
        let tarball = Tarball::find(artifact.as_str())?;
        if let Some(zfs) = image_store.zfs.filter(|zfs| zfs.storage.is(&self.storage_location)) {
            let key = ImageKey::new(tags.nix_hash.clone(), Ownership::of(self.privileged));
            let image = BaseImage::ensure(zfs, &key, &tarball, image_store.idmap)?;
            create_from_clone(self, image_store.idmap, zfs, &image, tags, target)
        } else {
            let ostemplate = copy_to_template_storage(
                &tarball,
                image_store.template_cache_path.as_str(),
                &tags.nix_hash,
            )?;
            let create = container_create(self, ostemplate, tags, target)?;
            self.bind_mounts
                .iter()
                .try_for_each(|mount| prepare_bind_mount(mount, self.privileged, image_store.idmap))?;
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
            bind_mounts: vec![BindMount {
                host_path: "/var/lib/proxnix/web".to_string(),
                container_path: "/var/lib/web".to_string(),
                mode: MountMode::ReadOnly,
            }],
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
        let create = container_create(&container("web"), "local:vztmpl/web.tar.xz".to_string(), &tags(), SlotId::Blue(Vmid::new(946))).unwrap();
        assert_eq!(
            rendered(&GuestOp::<Lxc>::Create(create)),
            "pct create 946 local:vztmpl/web.tar.xz --cores 2 --features nesting=1 --hostname web \
             --memory 512 --mp0 /var/lib/proxnix/web,mp=/var/lib/web,ro=1 \
             --net0 name=eth0,bridge=vmbr0 --ostype unmanaged --protection 1 --rootfs ZFS:8 \
             --tags proxnix;nix-k8whj0lg7k95jn6h57k99kvikc0zrpp3;commit-abc123;slot-blue --unprivileged 1"
        );
    }

    #[test]
    fn a_cloned_container_gets_the_same_settings_without_protection() {
        assert_eq!(
            rendered(&GuestOp::<Lxc>::Set(Vmid::new(946), container_settings(&container("web"), &tags()).unwrap())),
            "pct set 946 --cores 2 --features nesting=1 --hostname web --memory 512 \
             --mp0 /var/lib/proxnix/web,mp=/var/lib/web,ro=1 --net0 name=eth0,bridge=vmbr0 \
             --tags proxnix;nix-k8whj0lg7k95jn6h57k99kvikc0zrpp3;commit-abc123;slot-blue"
        );
    }

    #[test]
    fn a_hostname_longer_than_proxmox_allows_is_rejected_before_anything_runs() {
        assert!(matches!(
            container_settings(&container(&"a".repeat(256)), &tags()),
            Err(AppError::ProxmoxString(_))
        ));
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
