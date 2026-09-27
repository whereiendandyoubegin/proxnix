use crate::api::{self, Kind, Lxc, Qemu};
use crate::pve::Pve;
use crate::types::{AppConfig, AppError, BindMount, DesiredState, MountMode, Result};
use proxmox_api::nodes::node::{lxc, qemu};
use proxmox_api::types::bounded_integer::BoundedInteger;
use proxmox_api::access::permissions;
use proxnix_core::{
    Audited, Cores, DiskGib, Grant, GuestName, GuestPath, GuestStatus, HostPath, KindFacts, MemoryMb, Mount,
    MountMode as CoreMountMode, Observation, Permissions, Privilege, RawTags, Resources as CoreResources, Sighting,
    Sockets, VisibilityFault, Vmid,
};
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::net::Ipv4Addr;

pub fn parse_config(json: &str) -> Result<DesiredState> {
    let state: DesiredState = serde_json::from_str(json)?;
    Ok(state)
}

pub fn parse_appconfig(json: &str) -> Result<AppConfig> {
    let appconfig: AppConfig = serde_json::from_str(json)?;
    Ok(appconfig)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Listed<X> {
    pub id: Vmid,
    pub name: String,
    pub status: GuestStatus,
    pub tags: Option<String>,
    pub extra: X,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resources {
    pub memory_mb: u32,
    pub disk_gb: f64,
    pub cores: u16,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QemuListing {
    pub memory_mb: u32,
    pub disk_gb: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QemuExtra {
    pub sockets: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LxcExtra {
    pub privileged: bool,
    pub bind_mounts: Vec<BindMount>,
}

pub trait Observe: Kind {
    type Listing: Send;
    type Config: Send;
    type Extra: Send + Sync + Into<KindFacts>;

    fn list(pve: &Pve) -> Result<Vec<Listed<Self::Listing>>>;
    fn config(pve: &Pve, id: Vmid) -> Result<Self::Config>;
    fn tags_of(config: &Self::Config) -> Option<&str>;
    fn decode(id: Vmid, listing: Self::Listing, config: Self::Config) -> Result<(Resources, Self::Extra)>;
}

fn vmid_of(id: &impl BoundedInteger) -> Result<Vmid> {
    Ok(Vmid::new(u32::try_from(id.get())?))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bytes(i64);

impl Bytes {
    fn reported(bytes: Option<i64>) -> Bytes {
        Bytes(bytes.unwrap_or(0))
    }

    fn mebibytes(self) -> Result<u32> {
        Ok(u32::try_from(self.0 / (1_i64 << 20))?)
    }

    #[allow(clippy::cast_precision_loss)]
    fn gibibytes(self) -> f64 {
        self.0 as f64 / f64::from(1_u32 << 30)
    }
}

fn required<T>(value: Option<T>, id: Vmid, key: &str) -> Result<T> {
    value.ok_or_else(|| AppError::ProxmoxError(format!("config of {} is missing `{key}`", id.get())))
}

fn narrowed<T: TryFrom<i128, Error = std::num::TryFromIntError>>(value: &impl BoundedInteger) -> Result<T> {
    Ok(T::try_from(value.get())?)
}

fn listed_qemu(item: qemu::GetOutputItems) -> Result<Listed<QemuListing>> {
    Ok(Listed {
        id: vmid_of(&item.vmid)?,
        name: item.name.unwrap_or_default(),
        status: match item.status {
            qemu::Status::Running => GuestStatus::Running,
            qemu::Status::Stopped => GuestStatus::Stopped,
        },
        tags: item.tags,
        extra: QemuListing {
            memory_mb: Bytes::reported(item.maxmem).mebibytes()?,
            disk_gb: Bytes::reported(item.maxdisk).gibibytes(),
        },
    })
}

fn listed_lxc(item: lxc::GetOutputItems) -> Result<Listed<()>> {
    Ok(Listed {
        id: vmid_of(&item.vmid)?,
        name: item.name.unwrap_or_default(),
        status: match item.status {
            lxc::Status::Running => GuestStatus::Running,
            lxc::Status::Stopped => GuestStatus::Stopped,
        },
        tags: item.tags,
        extra: (),
    })
}

impl Observe for Qemu {
    type Listing = QemuListing;
    type Config = qemu::vmid::config::GetOutput;
    type Extra = QemuExtra;

    fn list(pve: &Pve) -> Result<Vec<Listed<QemuListing>>> {
        pve.call(pve.node().qemu().get(qemu::GetParams::default()))?
            .into_iter()
            .map(listed_qemu)
            .collect()
    }

    fn config(pve: &Pve, id: Vmid) -> Result<Self::Config> {
        pve.call(
            pve.node()
                .qemu()
                .vmid(api::vmid(id)?)
                .config()
                .get(qemu::vmid::config::GetParams::default()),
        )
    }

    fn tags_of(config: &Self::Config) -> Option<&str> {
        config.tags.as_deref()
    }

    fn decode(_id: Vmid, listing: QemuListing, config: Self::Config) -> Result<(Resources, QemuExtra)> {
        Ok((
            Resources {
                memory_mb: listing.memory_mb,
                disk_gb: listing.disk_gb,
                cores: config.cores.map_or(Ok(0), |n| u16::try_from(n.get()))?,
            },
            QemuExtra {
                sockets: config.sockets.map_or(Ok(1), |n| u8::try_from(n.get()))?,
            },
        ))
    }
}

impl Observe for Lxc {
    type Listing = ();
    type Config = lxc::vmid::config::GetOutput;
    type Extra = LxcExtra;

    fn list(pve: &Pve) -> Result<Vec<Listed<()>>> {
        pve.call(pve.node().lxc().get())?
            .into_iter()
            .map(listed_lxc)
            .collect()
    }

    fn config(pve: &Pve, id: Vmid) -> Result<Self::Config> {
        pve.call(
            pve.node()
                .lxc()
                .vmid(api::vmid(id)?)
                .config()
                .get(lxc::vmid::config::GetParams::default()),
        )
    }

    fn tags_of(config: &Self::Config) -> Option<&str> {
        config.tags.as_deref()
    }

    fn decode(id: Vmid, (): (), config: Self::Config) -> Result<(Resources, LxcExtra)> {
        Ok((
            Resources {
                memory_mb: narrowed(&required(config.memory, id, "memory")?)?,
                disk_gb: rootfs_size_gb(&required(config.rootfs, id, "rootfs")?)?,
                cores: narrowed(&required(config.cores, id, "cores")?)?,
            },
            LxcExtra {
                privileged: !config.unprivileged.unwrap_or(false),
                bind_mounts: config
                    .mps
                    .into_iter()
                    .collect::<BTreeMap<_, _>>()
                    .values()
                    .map(|mount| parse_mount(mount))
                    .collect::<Result<_>>()?,
            },
        ))
    }
}

pub fn audit(pve: &Pve) -> Result<Audited> {
    audited(
        &pve.call(pve.access().permissions().get(permissions::GetParams {
            path: Some(String::from("/vms")),
            ..permissions::GetParams::default()
        }))?
        .additional_properties,
    )
}

pub(crate) fn audited(granted: &HashMap<String, serde_json::Value>) -> Result<Audited> {
    let vm_audit = match granted
        .get("/vms")
        .and_then(|privileges| privileges.get("VM.Audit"))
        .and_then(serde_json::Value::as_u64)
    {
        Some(1) => Grant::Granted,
        _ => Grant::Denied,
    };
    Audited::try_from(Permissions { vm_audit }).map_err(|fault| match fault {
        VisibilityFault::NoVmAudit => AppError::ProxmoxError(String::from(
            "the api token lacks VM.Audit on /vms, so proxmox would hide every guest; refusing to observe",
        )),
    })
}

pub fn observe(pve: &Pve) -> Result<Observation> {
    let audited = audit(pve)?;
    let qemu = sightings::<Qemu>(Qemu::list(pve)?, |id| Qemu::config(pve, id))?;
    let lxc = sightings::<Lxc>(Lxc::list(pve)?, |id| Lxc::config(pve, id))?;
    Ok(Observation::new(audited, qemu.into_iter().chain(lxc).collect()))
}

fn sightings<K: Observe>(
    listed: Vec<Listed<K::Listing>>,
    config: impl Fn(Vmid) -> Result<K::Config> + Sync,
) -> Result<Vec<Sighting>> {
    listed
        .into_par_iter()
        .map(|entry| {
            let fetched = config(entry.id)?;
            sighting::<K>(entry, fetched)
        })
        .collect()
}

fn sighting<K: Observe>(entry: Listed<K::Listing>, config: K::Config) -> Result<Sighting> {
    let Listed { id, name, status, extra, .. } = entry;
    let tags = RawTags::from(K::tags_of(&config).map(str::to_string).unwrap_or_default());
    let (resources, extra) = K::decode(id, extra, config)?;
    Ok(Sighting {
        id,
        name: GuestName(name),
        status,
        tags,
        resources: observed_resources(&resources, id)?,
        facts: extra.into(),
    })
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn observed_resources(resources: &Resources, id: Vmid) -> Result<CoreResources> {
    let disk = resources.disk_gb.round();
    if disk.is_finite() && (0.0..=f64::from(u32::MAX)).contains(&disk) {
        Ok(CoreResources {
            memory: MemoryMb(resources.memory_mb),
            disk: DiskGib(disk as u32),
            cores: Cores(resources.cores),
        })
    } else {
        Err(AppError::ProxmoxError(format!(
            "guest {} reports an unusable disk size of {} GiB",
            id.get(),
            resources.disk_gb
        )))
    }
}

impl From<QemuExtra> for KindFacts {
    fn from(extra: QemuExtra) -> KindFacts {
        KindFacts::Qemu { sockets: Sockets(extra.sockets) }
    }
}

impl From<LxcExtra> for KindFacts {
    fn from(extra: LxcExtra) -> KindFacts {
        KindFacts::Lxc {
            privilege: if extra.privileged { Privilege::Privileged } else { Privilege::Unprivileged },
            mounts: extra
                .bind_mounts
                .into_iter()
                .map(|mount| Mount {
                    host: HostPath(mount.host_path),
                    guest: GuestPath(mount.container_path),
                    mode: match mount.mode {
                        MountMode::ReadOnly => CoreMountMode::ReadOnly,
                        MountMode::ReadWrite => CoreMountMode::ReadWrite,
                    },
                })
                .collect(),
        }
    }
}

pub(crate) fn agent_ipv4(result: &serde_json::Value) -> Option<Ipv4Addr> {
    result
        .as_array()?
        .iter()
        .filter(|iface| iface["name"] != "lo")
        .find_map(|iface| {
            iface["ip-addresses"]
                .as_array()?
                .iter()
                .find(|addr| addr["ip-address-type"] == "ipv4")
                .and_then(|addr| addr["ip-address"].as_str())
        })?
        .parse()
        .ok()
}

pub(crate) fn cidr_ipv4(inet: &str) -> Option<Ipv4Addr> {
    inet.split('/').next()?.parse().ok()
}

fn parse_mount(value: &str) -> Result<BindMount> {
    let (host_path, rest) = value
        .split_once(',')
        .ok_or_else(|| AppError::ProxmoxError(format!("mount point `{value}` has no options")))?;
    let opts: BTreeMap<&str, &str> = rest.split(',').filter_map(|s| s.split_once('=')).collect();

    let container_path = opts
        .get("mp")
        .ok_or_else(|| AppError::ProxmoxError(format!("mount point `{value}` has no mp=")))?;

    let mode = match opts.get("ro").copied() {
        Some("1") => MountMode::ReadOnly,
        None | Some("0") => MountMode::ReadWrite,
        Some(other) => {
            return Err(AppError::ProxmoxError(format!(
                "mount point `{value}` has unexpected ro={other}"
            )));
        }
    };

    Ok(BindMount {
        host_path: host_path.to_string(),
        container_path: container_path.to_string(),
        mode,
    })
}

fn rootfs_size_gb(rootfs: &str) -> Result<f64> {
    let size = rootfs
        .split(',')
        .find_map(|s| s.strip_prefix("size="))
        .ok_or_else(|| AppError::ProxmoxError(format!("rootfs `{rootfs}` has no size=")))?;
    let (n, unit) = size
        .char_indices()
        .last()
        .map(|(i, _)| size.split_at(i))
        .ok_or_else(|| AppError::ProxmoxError(format!("rootfs `{rootfs}` has an empty size")))?;

    match unit {
        "T" => Ok(n.parse::<f64>()? * 1024.0),
        "G" => Ok(n.parse::<f64>()?),
        "M" => Ok(n.parse::<f64>()? / 1024.0),
        _ => Err(AppError::ProxmoxError(format!(
            "rootfs `{rootfs}` has unknown size unit `{unit}`"
        ))),
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    pub(crate) const NIXOLOGY_APPCONFIG: &str = r#"{"backend_pool":null,"guest_check":{"command":"/run/current-system/sw/bin/proxnix-health-check","shell":"/run/current-system/sw/bin/bash"},"local_repo":null,"proxmox":{"ca_file":"/etc/pve/pve-root-ca.pem","node":"pve01","realm":"pve","token_file":"/run/secrets/proxnix/api_token","token_id":"proxnix","url":"https://localhost:8006","user":"proxnix"},"repo_cache":"/tmp/proxnix/repos","server_address":"0.0.0.0:6780","sozu":{"http_port":80,"listen_ip":"0.0.0.0","socket_path":"/run/sozu/command.sock"},"ssh_key_candidates":["/root/.ssh/id_ed25519","/root/.ssh/id_ecdsa","/root/.ssh/id_rsa"],"template_cache_path":"/var/lib/vz/template/cache/","timings_ms":{"arp_probe":2000,"guest_check_poll":3000,"guest_check_run":60000,"nix_build":3600000,"nix_eval":300000,"periodic_reconcile":120000,"provision_stagger":150,"sozu_tcp_idle":3600000,"address_poll":2000,"port_poll":2000,"task_poll":1000,"task_timeout":600000,"webhook_lock_wait":600000},"unprivileged_idmap":{"count":65536,"host_base":100000},"zfs_images":{"images":"ZFS/proxnix-images","pool":"ZFS","storage":"ZFS"}}"#;
}

#[cfg(test)]
mod tests {
    use super::tests_support::NIXOLOGY_APPCONFIG;
    use super::*;
    use serde_json::json;

    use proxnix_core::{Ownership, RawTags, Slot};
    use std::net::Ipv4Addr;

    const NIX_EVAL_SAMPLE: &str = r#"{
      "vms": {
        "test-website": {
          "name": "test-website", "hostname": "test-website",
          "blue_id": 823, "green_id": 923,
          "service_address": "192.168.1.23", "backend_port": 80,
          "dhcp_timeout_seconds": 240, "health_check_timeout_seconds": 180,
          "image_type": "build-qcow2-website",
          "cores": 2, "sockets": 1, "memory_mb": 2048, "disk_gb": 10,
          "storage_location": "local-lvm", "protected": false, "impure": false
        }
      },
      "containers": {
        "pihole": {
          "name": "pihole", "hostname": "pihole",
          "blue_id": 833, "green_id": 933,
          "dhcp_timeout_seconds": 240, "health_check_timeout_seconds": 180,
          "image_type": "build-lxc-pihole",
          "cores": 2, "memory_mb": 1024, "disk_gb": 8,
          "storage_location": "local-lvm", "protected": false,
          "privileged": true, "impure": false
        }
      }
    }"#;

    #[test]
    fn the_nix_schema_parses_into_the_config_types() {
        let parsed = parse_config(NIX_EVAL_SAMPLE).expect("nix eval output should parse");

        let vm = &parsed.vms["test-website"];
        assert_eq!(vm.blue_id, Vmid::new(823));
        assert_eq!(vm.green_id, Vmid::new(923));
        assert_eq!(vm.service_address, Some(Ipv4Addr::new(192, 168, 1, 23)));
        assert_eq!(vm.backend_port, 80);
        assert_eq!(vm.dhcp_timeout_seconds, 240);
        assert_eq!(vm.health_check_timeout_seconds, 180);
    }

    #[test]
    fn a_workload_without_a_service_address_is_unproxied() {
        let parsed = parse_config(NIX_EVAL_SAMPLE).expect("nix eval output should parse");
        assert_eq!(parsed.containers["pihole"].service_address, None);
    }

    const MANAGED: &str = "proxnix;nix-abc123;commit-x;slot-green;ip-10.0.0.7";

    fn decoded<T: serde::de::DeserializeOwned>(json: &serde_json::Value) -> T {
        serde_json::from_str(&json.to_string()).unwrap()
    }

    #[test]
    fn list_items_are_read_in_the_units_qm_list_reported() {
        let items: Vec<qemu::GetOutputItems> = decoded(&json!([{
            "vmid": 823, "name": "web", "status": "running", "tags": MANAGED,
            "maxmem": 2_147_483_648_i64, "maxdisk": 10_737_418_240_i64
        }]));
        assert_eq!(
            items.into_iter().map(listed_qemu).collect::<Result<Vec<_>>>().unwrap(),
            vec![Listed {
                id: Vmid::new(823),
                name: "web".to_string(),
                status: GuestStatus::Running,
                tags: Some(MANAGED.to_string()),
                extra: QemuListing { memory_mb: 2048, disk_gb: 10.0 },
            }]
        );
    }

    #[test]
    fn a_ballooned_vm_config_still_becomes_a_sighting() {
        let listing = Listed {
            id: Vmid::new(823),
            name: "web".to_string(),
            status: GuestStatus::Running,
            tags: Some(MANAGED.to_string()),
            extra: QemuListing { memory_mb: 2048, disk_gb: 10.0 },
        };
        let config: qemu::vmid::config::GetOutput =
            decoded(&json!({ "digest": "0123", "balloon": 1024, "cores": 2, "sockets": 1, "memory": "2048", "tags": MANAGED }));
        let seen = sighting::<Qemu>(listing, config).unwrap();
        assert_eq!(seen.resources, CoreResources { memory: MemoryMb(2048), disk: DiskGib(10), cores: Cores(2) });
        assert_eq!(seen.facts, KindFacts::Qemu { sockets: Sockets(1) });
    }

    #[test]
    fn a_container_config_missing_its_memory_is_an_error_not_a_guess() {
        let listing = Listed { id: Vmid::new(833), name: "pihole".to_string(), status: GuestStatus::Running, tags: Some(MANAGED.to_string()), extra: () };
        let config: lxc::vmid::config::GetOutput = decoded(&json!({ "digest": "0123", "cores": 2, "rootfs": "x:y,size=8G" }));
        assert!(sighting::<Lxc>(listing, config).is_err());
    }

    #[test]
    fn a_stopped_container_keeps_the_reported_status() {
        let items: Vec<lxc::GetOutputItems> =
            decoded(&json!([{ "vmid": 833, "name": "pihole", "status": "stopped" }]));
        assert_eq!(
            items.into_iter().map(listed_lxc).collect::<Result<Vec<_>>>().unwrap(),
            vec![Listed {
                id: Vmid::new(833),
                name: "pihole".to_string(),
                status: GuestStatus::Stopped,
                tags: None,
                extra: (),
            }]
        );
    }

    #[test]
    fn the_first_non_loopback_ipv4_is_the_vm_address() {
        let result = json!([
            {"name":"lo","ip-addresses":[{"ip-address-type":"ipv4","ip-address":"127.0.0.1"}]},
            {"name":"eth0","ip-addresses":[
                {"ip-address-type":"ipv6","ip-address":"fe80::1"},
                {"ip-address-type":"ipv4","ip-address":"10.0.0.9"}
            ]}
        ]);
        assert_eq!(agent_ipv4(&result), Some(Ipv4Addr::new(10, 0, 0, 9)));
        assert_eq!(agent_ipv4(&json!("not a list")), None);
    }

    #[test]
    fn a_container_address_drops_its_prefix_length() {
        assert_eq!(cidr_ipv4("10.0.0.12/24"), Some(Ipv4Addr::new(10, 0, 0, 12)));
        assert_eq!(cidr_ipv4("fe80::1/64"), None);
    }

    fn fixture<T: serde::de::DeserializeOwned>(path: &str) -> Option<T> {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/api")
                .join(path),
        )
        .ok()
        .map(|text| serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path} does not decode: {e}")))
    }

    fn managed_ids<X>(listed: &[Listed<X>]) -> Vec<Vmid> {
        listed
            .iter()
            .filter(|entry| matches!(Ownership::from(&RawTags::from(entry.tags.clone().unwrap_or_default())), Ownership::Managed(_)))
            .map(|entry| entry.id)
            .collect()
    }

    #[test]
    fn captured_qemu_responses_decode() {
        let listed: Vec<Listed<QemuListing>> = fixture::<Vec<qemu::GetOutputItems>>("qemu.json")
            .unwrap_or_default()
            .into_iter()
            .map(|item| listed_qemu(item).unwrap())
            .collect();
        for id in managed_ids(&listed) {
            let config: Option<qemu::vmid::config::GetOutput> = fixture(&format!("qemu/{}/config.json", id.get()));
            let listing = QemuListing { memory_mb: 0, disk_gb: 0.0 };
            if let Some(config) = config {
                assert!(Qemu::decode(id, listing, config).is_ok(), "config of {} is unusable", id.get());
            }
            let agent: Option<qemu::vmid::agent::network_get_interfaces::GetOutput> =
                fixture(&format!("qemu/{}/agent/network-get-interfaces.json", id.get()));
            if let Some(agent) = agent {
                assert!(agent.additional_properties.get("result").and_then(agent_ipv4).is_some());
            }
        }
    }

    #[test]
    fn captured_lxc_responses_decode() {
        let listed: Vec<Listed<()>> = fixture::<Vec<lxc::GetOutputItems>>("lxc.json")
            .unwrap_or_default()
            .into_iter()
            .map(|item| listed_lxc(item).unwrap())
            .collect();
        for id in managed_ids(&listed) {
            let config: Option<lxc::vmid::config::GetOutput> = fixture(&format!("lxc/{}/config.json", id.get()));
            if let Some(config) = config {
                assert!(Lxc::decode(id, (), config).is_ok(), "config of {} is unusable", id.get());
            }
            let interfaces: Option<Vec<lxc::vmid::interfaces::GetOutputItems>> =
                fixture(&format!("lxc/{}/interfaces.json", id.get()));
            if let Some(interfaces) = interfaces {
                assert!(
                    interfaces
                        .iter()
                        .filter(|iface| iface.name != "lo")
                        .any(|iface| iface.inet.as_deref().and_then(cidr_ipv4).is_some())
                );
            }
        }
    }


    #[test]
    fn nixology_appconfig_points_the_api_client_at_the_decrypted_token() {
        let config = parse_appconfig(NIXOLOGY_APPCONFIG).unwrap();
        assert_eq!(config.proxmox.node, "pve01");
        assert_eq!(config.proxmox.token_file, std::path::PathBuf::from("/run/secrets/proxnix/api_token"));
        assert_eq!(config.proxmox.ca_file, std::path::PathBuf::from("/etc/pve/pve-root-ca.pem"));
    }

    #[test]
    fn nixology_supplies_every_setting_in_its_own_units() {
        let config = parse_appconfig(NIXOLOGY_APPCONFIG).unwrap();
        assert_eq!(config.timings_ms.get(crate::types::Timing::ProvisionStagger), std::time::Duration::from_millis(150));
        assert_eq!(config.timings_ms.get(crate::types::Timing::NixBuild), std::time::Duration::from_secs(3600));
        assert_eq!(config.sozu.http_port, 80);
        assert_eq!(config.unprivileged_idmap, crate::types::IdRange { host_base: 100_000, count: 65_536 });
        assert_eq!(config.guest_check.command, "/run/current-system/sw/bin/proxnix-health-check");
    }

    #[test]
    fn a_config_missing_a_timing_is_rejected_when_read() {
        let partial = NIXOLOGY_APPCONFIG.replace("\"nix_build\":3600000,", "");
        assert_ne!(partial, NIXOLOGY_APPCONFIG);
        assert!(parse_appconfig(&partial).is_err());
    }

    fn raw_fixture(path: &str) -> serde_json::Value {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/api").join(path),
        )
        .unwrap_or_else(|e| panic!("fixture {path} is missing: {e}"));
        serde_json::from_str(&text).unwrap()
    }

    fn raw_tags(raw: &serde_json::Value) -> Vec<(u64, Option<String>)> {
        raw.as_array()
            .unwrap()
            .iter()
            .map(|item| (item["vmid"].as_u64().unwrap(), item["tags"].as_str().map(str::to_string)))
            .collect()
    }

    #[test]
    fn decoded_list_tags_match_what_proxmox_returned() {
        let lxc_raw = raw_fixture("lxc.json");
        let qemu_raw = raw_fixture("qemu.json");
        let lxc: Vec<(u64, Option<String>)> = decoded::<Vec<lxc::GetOutputItems>>(&lxc_raw)
            .into_iter()
            .map(|item| listed_lxc(item).unwrap())
            .map(|entry| (u64::from(entry.id.get()), entry.tags))
            .collect();
        let qemu: Vec<(u64, Option<String>)> = decoded::<Vec<qemu::GetOutputItems>>(&qemu_raw)
            .into_iter()
            .map(|item| listed_qemu(item).unwrap())
            .map(|entry| (u64::from(entry.id.get()), entry.tags))
            .collect();
        assert_eq!(lxc, raw_tags(&lxc_raw));
        assert_eq!(qemu, raw_tags(&qemu_raw));
    }

    #[test]
    fn every_captured_config_decodes_including_unmanaged_guests() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/api");
        let failures: Vec<String> = ["qemu", "lxc"]
            .into_iter()
            .flat_map(|kind| {
                std::fs::read_dir(dir.join(kind))
                    .unwrap()
                    .map(move |entry| (kind, entry.unwrap().file_name().to_string_lossy().to_string()))
            })
            .filter_map(|(kind, id)| {
                let path = format!("{kind}/{id}/config.json");
                let raw = raw_fixture(&path);
                let decoded = match kind {
                    "qemu" => serde_json::from_str::<qemu::vmid::config::GetOutput>(&raw.to_string()).map(|_| ()),
                    _ => serde_json::from_str::<lxc::vmid::config::GetOutput>(&raw.to_string()).map(|_| ()),
                };
                decoded.err().map(|e| format!("{path}: {e}"))
            })
            .collect();
        assert!(failures.is_empty(), "{failures:#?}");
    }

    fn captured_permissions() -> HashMap<String, serde_json::Value> {
        decoded::<permissions::GetOutput>(&raw_fixture("permissions.json")).additional_properties
    }

    fn observed_from_fixtures(granted: &HashMap<String, serde_json::Value>) -> Result<Observation> {
        let audited = audited(granted)?;
        let qemu = sightings::<Qemu>(
            decoded::<Vec<qemu::GetOutputItems>>(&raw_fixture("qemu.json"))
                .into_iter()
                .map(|item| listed_qemu(item).unwrap())
                .collect(),
            |id| Ok(decoded(&raw_fixture(&format!("qemu/{}/config.json", id.get())))),
        )?;
        let lxc = sightings::<Lxc>(
            decoded::<Vec<lxc::GetOutputItems>>(&raw_fixture("lxc.json"))
                .into_iter()
                .map(|item| listed_lxc(item).unwrap())
                .collect(),
            |id| Ok(decoded(&raw_fixture(&format!("lxc/{}/config.json", id.get())))),
        )?;
        Ok(Observation::new(audited, qemu.into_iter().chain(lxc).collect()))
    }

    fn without_vm_audit(granted: &HashMap<String, serde_json::Value>) -> HashMap<String, serde_json::Value> {
        granted
            .iter()
            .map(|(path, privileges)| {
                let kept: serde_json::Map<String, serde_json::Value> = privileges
                    .as_object()
                    .unwrap()
                    .iter()
                    .filter(|(privilege, _)| privilege.as_str() != "VM.Audit")
                    .map(|(privilege, value)| (privilege.clone(), value.clone()))
                    .collect();
                (path.clone(), serde_json::Value::Object(kept))
            })
            .collect()
    }

    #[test]
    fn the_captured_token_permissions_prove_the_guests_are_visible() {
        assert!(audited(&captured_permissions()).is_ok());
    }

    #[test]
    fn a_token_without_vm_audit_is_refused_instead_of_seeing_an_empty_cluster() {
        let stripped = without_vm_audit(&captured_permissions());
        assert_ne!(stripped, captured_permissions());
        assert!(audited(&stripped).is_err());
        assert!(audited(&HashMap::new()).is_err());
        assert!(audited(&[(String::from("/vms"), json!({ "VM.Audit": 0 }))].into()).is_err());
        assert!(observed_from_fixtures(&stripped).is_err());
    }

    #[test]
    fn the_observation_of_the_real_cluster_holds_every_managed_guest_and_nothing_else() {
        let observed = observed_from_fixtures(&captured_permissions()).unwrap();
        let managed: std::collections::BTreeSet<(u32, String)> = observed
            .managed()
            .iter()
            .map(|managed| (managed.id().get(), managed.guest().name().0.clone()))
            .collect();
        assert_eq!(
            managed,
            [
                (823, "test-website"),
                (841, "flake-updater"),
                (842, "postgres"),
                (843, "monitoring"),
                (844, "forgejo"),
                (845, "cloudflared"),
                (846, "hydra"),
                (930, "test-container"),
            ]
            .map(|(id, name)| (id, String::from(name)))
            .into()
        );
        assert!(observed.anomalies().is_empty(), "{:?}", observed.anomalies());
    }

    #[test]
    fn unmanaged_guests_occupy_their_ids_and_free_ids_are_vacant() {
        let observed = observed_from_fixtures(&captured_permissions()).unwrap();
        for id in [200, 201, 801, 900] {
            assert_eq!(
                observed.slot(Vmid::new(id)),
                proxnix_core::SlotState::Occupied(proxnix_core::Occupant::Unmanaged(Vmid::new(id)))
            );
        }
        for id in [944, 923, 941] {
            assert!(matches!(observed.slot(Vmid::new(id)), proxnix_core::SlotState::Vacant(v) if v.id() == Vmid::new(id)));
        }
    }

    #[test]
    fn a_managed_guest_carries_what_proxmox_reported_about_it() {
        let observed = observed_from_fixtures(&captured_permissions()).unwrap();
        let forgejo = match observed.slot(Vmid::new(844)) {
            proxnix_core::SlotState::Occupied(proxnix_core::Occupant::Managed(managed)) => managed,
            other => panic!("844 must be managed, got {other:?}"),
        };
        assert_eq!(forgejo.tags().slot, Slot::Blue);
        assert_eq!(forgejo.tags().nix.as_ref(), "78s0iadvjz6s48aqvx4rw78lwrzkjzlw");
        assert_eq!(forgejo.tags().service_ip, Some(Ipv4Addr::new(192, 168, 1, 214)));
        assert_eq!(
            forgejo.guest().resources(),
            CoreResources { memory: MemoryMb(2048), disk: DiskGib(20), cores: Cores(2) }
        );
        assert!(matches!(
            forgejo.guest().facts(),
            KindFacts::Lxc { privilege: Privilege::Unprivileged, mounts } if mounts.len() == 3
        ));
        let website = observed.managed_named(&GuestName(String::from("test-website")));
        assert_eq!(website.len(), 1);
        assert_eq!(
            website[0].guest().resources(),
            CoreResources { memory: MemoryMb(2048), disk: DiskGib(10), cores: Cores(2) }
        );
        assert_eq!(website[0].guest().facts(), &KindFacts::Qemu { sockets: Sockets(1) });
    }
}
