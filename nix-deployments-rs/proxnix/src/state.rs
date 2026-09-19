use crate::context::NixHash;
use crate::pct::{pct_config, pct_list};
use crate::types::{
    AppConfig, AppError, BindMount, DeployedContainer, DeployedState, DeployedVM, DesiredState,
    MountMode, QMConfig, QMList, Result,
};
use proxnix_core::Slot;
use rayon::prelude::*;
use std::collections::HashMap;
use std::process::Command;

pub(crate) fn slot_from_tags(tags: Option<&str>) -> Slot {
    tags.and_then(|t| {
        t.split(';')
            .find(|tag| tag.trim().starts_with("slot-"))
            .and_then(|tag| Slot::try_from(tag.trim()).ok())
    })
    .unwrap_or(Slot::Blue)
}

pub(crate) fn nix_hash_from_tags(tags: Option<&str>) -> Option<NixHash> {
    tags.and_then(|t| {
        t.split(';')
            .find(|tag| tag.trim().starts_with("nix-"))
            .and_then(|tag| NixHash::try_from(tag.trim().trim_start_matches("nix-")).ok())
    })
}

pub(crate) fn service_ip_from_tags(tags: Option<&str>) -> Option<std::net::Ipv4Addr> {
    tags.and_then(|t| {
        t.split(';')
            .find(|tag| tag.trim().starts_with("ip-"))
            .and_then(|tag| tag.trim().trim_start_matches("ip-").parse().ok())
    })
}

pub(crate) fn is_proxnix_managed(tags: Option<&str>) -> bool {
    tags.map(|t| t.split(';').any(|tag| tag.trim() == "proxnix"))
        .unwrap_or(false)
}

pub(crate) fn vm_tags(vm_id: u32) -> Result<Option<String>> {
    Ok(parse_qm_config(&qm_config(vm_id)?)?.tags)
}

pub(crate) fn container_tags(ct_id: u32) -> Result<Option<String>> {
    Ok(parse_pct_config(&pct_config(ct_id)?)?.tags)
}

pub fn parse_config(json: &str) -> Result<DesiredState> {
    let state: DesiredState = serde_json::from_str(json)?;
    Ok(state)
}

pub fn parse_appconfig(json: &str) -> Result<AppConfig> {
    let appconfig: AppConfig = serde_json::from_str(json)?;
    Ok(appconfig)
}

pub fn qm_list() -> Result<String> {
    let qm_list = Command::new("qm").arg("list").output()?;
    if !qm_list.status.success() {
        return Err(AppError::CmdError(format!(
            "qm list has failed with exit code: {:?}",
            qm_list.status.code()
        )));
    }
    let stdout_bytes = qm_list.stdout;
    let output_string = String::from_utf8(stdout_bytes)?;

    Ok(output_string)
}

pub(crate) fn vm_exists(vm_id: u32) -> Result<bool> {
    qm_list().and_then(|raw| {
        parse_qm_list(&raw).map(|vms| vms.into_iter().any(|vm| vm.vm_id == vm_id))
    })
}

pub fn qm_config(vm_id: u32) -> Result<String> {
    let qm_config = Command::new("qm")
        .arg("config")
        .arg(vm_id.to_string())
        .output()?;
    if !qm_config.status.success() {
        return Err(AppError::CmdError(format!(
            "qm config has failed with exit code: {:?}",
            qm_config.status.code()
        )));
    }

    let stdout_bytes = qm_config.stdout;
    let output_string = String::from_utf8(stdout_bytes)?;

    Ok(output_string)
}

pub fn parse_qm_config(output_string: &str) -> Result<QMConfig> {
    let qmconfig = output_string
        .lines()
        .fold(QMConfig::default(), |mut accumulator, line| {
            let (key, value) = line.split_once(':').unwrap(); // TODO Maybe make a function to validate qm config output in the future
            let key = key.trim();
            let value = value.trim();

            match key {
                "agent" => accumulator.agent = value.parse().unwrap(),
                "balloon" => accumulator.balloon = value.parse().unwrap(),
                "boot" => accumulator.boot = value.parse().unwrap(),
                "bootdisk" => accumulator.bootdisk = value.parse().unwrap(),
                "cipassword" => accumulator.cipassword = Some(value.to_string()),
                "ciuser" => accumulator.ciuser = Some(value.to_string()),
                "cores" => accumulator.cores = value.parse().unwrap(),
                "cpu" => accumulator.cpu = value.parse().unwrap(),
                "cpuunits" => accumulator.cpuunits = value.parse().unwrap(),
                "memory" => accumulator.memory = value.parse().unwrap(),
                "meta" => accumulator.meta = value.parse().unwrap(),
                "name" => accumulator.name = value.parse().unwrap(),
                "numa" => accumulator.numa = value.parse().unwrap(),
                "onboot" => accumulator.onboot = value.parse().unwrap(),
                "protection" => accumulator.protection = value.parse().unwrap(),
                "sockets" => accumulator.sockets = value.parse().unwrap(),
                "sshkeys" => accumulator.sshkeys = Some(value.to_string()),
                "tags" => accumulator.tags = Some(value.to_string()),
                "vga" => accumulator.vga = value.parse().unwrap(),
                "vmgenid" => accumulator.vmgenid = value.parse().unwrap(),
                key if key.starts_with("scsi")
                    || key.starts_with("sata")
                    || key.starts_with("ide")
                    || key.starts_with("virtio") =>
                {
                    accumulator.disks.insert(key.to_string(), value.to_string());
                }
                key if key.starts_with("ipconfig") => {
                    accumulator
                        .ipconfigs
                        .insert(key.to_string(), value.to_string());
                }
                key if key.starts_with("net") => {
                    accumulator
                        .networks
                        .insert(key.to_string(), value.to_string());
                }
                key if key.starts_with("serial") => {
                    accumulator
                        .serial
                        .insert(key.to_string(), value.to_string());
                }
                _ => {}
            }
            accumulator
        });
    Ok(qmconfig)
}

pub fn parse_qm_list(output_string: &str) -> Result<Vec<QMList>> {
    output_string
        .lines()
        .skip(1)
        .map(|line| -> Result<QMList> {
            let parts: Vec<&str> = line.split_whitespace().collect();

            let col = |n: usize| -> crate::types::Result<&str> {
                parts.get(n).copied().ok_or_else(|| {
                    AppError::ParsingModuleError(format!(
                        "qm list line has fewer columns than expected: '{}'",
                        line
                    ))
                })
            };

            Ok(QMList {
                vm_id: col(0)?.parse()?,
                name: col(1)?.to_string(),
                status: col(2)?.to_string(),
                mem_mb: col(3)?.parse()?,
                bootdisk_gb: col(4)?.parse()?,
                pid: col(5)?.parse()?,
            })
        })
        .collect()
}

pub fn enrich_cpu_info(deployed: DeployedState) -> Result<DeployedState> {
    let DeployedState { vms, containers } = deployed;
    let deployedvms = vms
        .into_par_iter()
        .map(|(_name, vm)| -> Result<Option<(String, DeployedVM)>> {
            let config = qm_config(vm.vm_id)?;
            let parsed = parse_qm_config(&config)?;
            if !is_proxnix_managed(parsed.tags.as_deref()) {
                return Ok(None);
            }
            let nix_hash = nix_hash_from_tags(parsed.tags.as_deref());
            let active_slot = slot_from_tags(parsed.tags.as_deref());
            let service_ip = service_ip_from_tags(parsed.tags.as_deref());
            Ok(Some((
                vm.vm_name.clone(),
                DeployedVM {
                    vm_id: vm.vm_id,
                    vm_name: vm.vm_name,
                    nix_hash,
                    template_id: vm.template_id,
                    mem_mb: vm.mem_mb,
                    bootdisk_gb: vm.bootdisk_gb,
                    status: vm.status,
                    pid: vm.pid,
                    cores: parsed.cores as u16,
                    sockets: parsed.sockets,
                    active_slot,
                    service_ip,
                },
            )))
        })
        .collect::<Result<Vec<Option<_>>>>()?
        .into_iter()
        .flatten()
        .collect();
    Ok(DeployedState {
        vms: deployedvms,
        containers,
    })
}

pub fn list_to_deployed_vm(qmlists: Vec<QMList>) -> DeployedState {
    let lists = qmlists
        .into_iter()
        .map(|qmlist| -> (String, DeployedVM) {
            (
                qmlist.name.clone(),
                DeployedVM {
                    vm_id: qmlist.vm_id,
                    vm_name: qmlist.name.clone(),
                    nix_hash: None,
                    template_id: None,
                    mem_mb: qmlist.mem_mb,
                    bootdisk_gb: qmlist.bootdisk_gb,
                    status: qmlist.status,
                    pid: qmlist.pid,
                    cores: 0,   //placeholder
                    sockets: 0, //placeholder
                    active_slot: Slot::Blue,
                    service_ip: None,
                },
            )
        })
        .collect();

    DeployedState {
        vms: lists,
        containers: HashMap::new(),
    }
}

pub(crate) struct PctListEntry {
    ct_id: u32,
    status: String,
    ct_name: String,
}

struct PctConfigData {
    hostname: String,
    memory_mb: u32,
    cores: u16,
    rootfs_gb: f64,
    tags: Option<String>,
    unprivileged: bool,
    bind_mounts: Vec<BindMount>,
}

pub fn parse_pct_list(output: &str) -> Result<Vec<PctListEntry>> {
    output
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 {
                return Err(AppError::ParsingModuleError(format!(
                    "pct list line has fewer columns than expected: '{}'",
                    line
                )));
            }
            Ok(PctListEntry {
                ct_id: parts[0].parse()?,
                status: parts[1].to_string(),
                ct_name: parts.last().unwrap().to_string(),
            })
        })
        .collect()
}

fn parse_pct_config(output: &str) -> Result<PctConfigData> {
    let mut hostname = String::new();
    let mut memory_mb = 0u32;
    let mut cores = 0u16;
    let mut rootfs_gb = 0.0f64;
    let mut tags: Option<String> = None;
    let mut unprivileged = false;
    let mut bind_mounts: Vec<BindMount> = Vec::new();

    for line in output.lines() {
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "hostname" => hostname = value.to_string(),
                "memory" => memory_mb = value.parse()?,
                "cores" => cores = value.parse()?,
                "unprivileged" => unprivileged = value.trim() == "1",
                "rootfs" => {
                    // Format: "local-lvm:vm-200-disk-0,size=8G"
                    if let Some(size_part) =
                        value.split(',').find(|s| s.trim().starts_with("size="))
                    {
                        let size_str = size_part.trim().trim_start_matches("size=");
                        if let Some(gb) = size_str.strip_suffix('G') {
                            rootfs_gb = gb.parse().unwrap_or(0.0);
                        } else if let Some(mb) = size_str.strip_suffix('M') {
                            rootfs_gb = mb.parse::<f64>().unwrap_or(0.0) / 1024.0;
                        }
                    }
                }
                "tags" => tags = Some(value.to_string()),
                k if k.starts_with("mp") && k[2..].parse::<u32>().is_ok() => {
                    // Format: "/host/path,mp=/container/path"
                    let parts: Vec<&str> = value.split(',').collect();
                    if let (Some(host_path), Some(mp_part)) = (
                        parts.first(),
                        parts.iter().find(|p| p.trim().starts_with("mp=")),
                    ) {
                        let container_path = mp_part.trim().trim_start_matches("mp=");
                        bind_mounts.push(BindMount {
                            host_path: host_path.to_string(),
                            container_path: container_path.to_string(),
                            mode: match value.contains("ro=1") {
                                true => MountMode::ReadOnly,
                                false => MountMode::ReadWrite,
                            },
                        });
                    }
                }
                _ => {}
            }
        }
    }

    Ok(PctConfigData {
        hostname,
        memory_mb,
        cores,
        rootfs_gb,
        tags,
        unprivileged,
        bind_mounts,
    })
}

pub fn enrich_container_info(
    entries: Vec<PctListEntry>,
) -> Result<HashMap<String, DeployedContainer>> {
    let result = entries
        .into_par_iter()
        .map(|entry| -> Result<Option<(String, DeployedContainer)>> {
            let config_raw = pct_config(entry.ct_id)?;
            let config = parse_pct_config(&config_raw)?;
            if !is_proxnix_managed(config.tags.as_deref()) {
                return Ok(None);
            }
            let nix_hash = nix_hash_from_tags(config.tags.as_deref());
            let active_slot = slot_from_tags(config.tags.as_deref());
            let service_ip = service_ip_from_tags(config.tags.as_deref());
            Ok(Some((
                entry.ct_name.clone(),
                DeployedContainer {
                    ct_id: entry.ct_id,
                    ct_name: entry.ct_name,
                    nix_hash,
                    mem_mb: config.memory_mb,
                    bootdisk_gb: config.rootfs_gb,
                    status: entry.status,
                    cores: config.cores,
                    bind_mounts: config.bind_mounts,
                    privileged: !config.unprivileged,
                    active_slot,
                    service_ip,
                },
            )))
        })
        .collect::<Result<Vec<Option<_>>>>()?
        .into_iter()
        .flatten()
        .collect();
    Ok(result)
}

pub(crate) fn container_exists(ct_id: u32) -> Result<bool> {
    pct_list().and_then(|raw| {
        parse_pct_list(&raw).map(|containers| {
            containers
                .into_iter()
                .any(|container| container.ct_id == ct_id)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]

    pub fn test_parse_qm_list() {
        let sample = "      VMID NAME                 STATUS     MEM(MB)    BOOTDISK(GB) PID
       100 master               stopped    8000              52.00 0
       101 plextemp             running    12000             52.00 1476084
       102 master               stopped    1200              60.00 0
       103 k3s-warm             stopped    1200              60.00 0
       104 controltemp          stopped    1200              60.00 0
       105 eos                  stopped    4000              50.00 0
       106 proxmox-staging      running    4000             100.00 160968
       201 k3s-cp-01            running    10240             60.00 1811557
       202 k3s-cp-02            running    10240             60.00 29387
       203 k3s-cp-03            running    10240             60.00 29688
       204 k3s-wrk-fat-01       running    32768             64.00 29513
       205 k3s-wrk-fat-02       running    32768             64.00 1163752
       206 k3s-wrk-01           running    15360             60.00 29816
       207 k3s-wrk-02           running    15360             60.00 29727
       300 discord-bot-guest    stopped    4000               4.00 0
       700 nixos-test           running    4048              24.41 87587
       802 k3s-init             running    4096               3.91 1206131
       810 nix-worker           stopped    4096               3.91 0
       811 nix-control          stopped    4096               3.91 0
       900 Copy-of-VM-k3s-warm  running    6000              60.00 89806
      9000 ubuntu-template      stopped    1024              20.00 0
      9005 nixos-template       stopped    4096               3.91 0
      9006 nixos-template       stopped    4096               3.91 0
      9010 clean-ubuntu         stopped    1024               2.20 0";

        let result = parse_qm_list(sample);
        println!("{:#?}", result)
    }

    use crate::context::Tags;
    use std::net::Ipv4Addr;

    fn tags_for(nix: &str, commit: &str, slot: Slot) -> String {
        Tags::new(NixHash::try_from(nix).unwrap(), commit, slot).render()
    }

    const NIX_EVAL_SAMPLE: &str = r#"{
      "vms": {
        "test-website": {
          "name": "test-website", "hostname": "test-website",
          "blue_id": 823, "green_id": 923,
          "service_address": "192.168.1.23", "backend_port": 80,
          "image_type": "build-qcow2-website",
          "cores": 2, "sockets": 1, "memory_mb": 2048, "disk_gb": 10,
          "storage_location": "local-lvm", "protected": false, "impure": false
        }
      },
      "containers": {
        "pihole": {
          "name": "pihole", "hostname": "pihole",
          "blue_id": 833, "green_id": 933,
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
        assert_eq!(vm.blue_id, 823);
        assert_eq!(vm.green_id, 923);
        assert_eq!(vm.service_address, Some(Ipv4Addr::new(192, 168, 1, 23)));
        assert_eq!(vm.backend_port, 80);
    }

    #[test]
    fn a_workload_without_a_service_address_is_unproxied() {
        let parsed = parse_config(NIX_EVAL_SAMPLE).expect("nix eval output should parse");
        assert_eq!(parsed.containers["pihole"].service_address, None);
    }

    #[test]
    fn a_registered_service_ip_survives_a_tag_round_trip() {
        let ip = Ipv4Addr::new(10, 42, 0, 7);
        let rendered = Tags::new(NixHash::try_from("abc123").unwrap(), "deadbeef", Slot::Green)
            .with_service_ip(ip)
            .render();

        assert_eq!(service_ip_from_tags(Some(&rendered)), Some(ip));
        assert_eq!(slot_from_tags(Some(&rendered)), Slot::Green);
        assert_eq!(
            nix_hash_from_tags(Some(&rendered)).unwrap().as_str(),
            "abc123"
        );
        assert!(is_proxnix_managed(Some(&rendered)));
    }

    #[test]
    fn an_instance_with_no_observed_ip_yet_has_none() {
        let rendered = tags_for("abc123", "deadbeef", Slot::Blue);
        assert_eq!(service_ip_from_tags(Some(&rendered)), None);
    }

    #[test]
    fn the_service_ip_tag_is_not_confused_with_the_nix_tag() {
        let rendered = Tags::new(NixHash::try_from("abc123").unwrap(), "x", Slot::Blue)
            .with_service_ip(Ipv4Addr::new(192, 168, 1, 50))
            .render();
        assert_eq!(
            service_ip_from_tags(Some(&rendered)),
            Some(Ipv4Addr::new(192, 168, 1, 50))
        );
        assert_eq!(nix_hash_from_tags(Some(&rendered)).unwrap().as_str(), "abc123");
    }

    #[test]
    fn slot_round_trips_through_proxmox_tags() {
        for slot in [Slot::Blue, Slot::Green] {
            let tags = tags_for("abc123", "deadbeef", slot);
            assert_eq!(slot_from_tags(Some(&tags)), slot);
        }
    }

    #[test]
    fn nix_hash_round_trips_through_proxmox_tags() {
        let tags = tags_for("0lmgpzmhq0d1yrpnl7fxpgnkqkgnxdq7", "deadbeef", Slot::Green);
        assert_eq!(
            nix_hash_from_tags(Some(&tags)).unwrap().as_str(),
            "0lmgpzmhq0d1yrpnl7fxpgnkqkgnxdq7"
        );
    }

    #[test]
    fn untagged_vm_defaults_to_blue() {
        assert_eq!(slot_from_tags(None), Slot::Blue);
        assert_eq!(slot_from_tags(Some("proxnix;nix-abc")), Slot::Blue);
    }

    #[test]
    fn slot_tag_is_not_confused_with_other_tags() {
        let tags = "proxnix;nix-slot-green-looking-hash;commit-abc;slot-blue";
        assert_eq!(slot_from_tags(Some(tags)), Slot::Blue);
    }

    #[test]
    fn only_proxnix_tagged_resources_are_managed() {
        assert!(is_proxnix_managed(Some("proxnix;nix-abc;slot-blue")));
        assert!(!is_proxnix_managed(Some("nix-abc;slot-blue")));
        assert!(!is_proxnix_managed(None));
    }

    #[test]
    fn ownership_depends_on_the_proxnix_tag_not_the_nix_hash() {
        assert!(is_proxnix_managed(Some(
            "proxnix;nix-abc123;commit-x;slot-green"
        )));
        assert!(is_proxnix_managed(Some(
            "proxnix;nix-somethingelse;commit-x;slot-green"
        )));
        assert!(!is_proxnix_managed(Some(
            "nix-abc123;commit-x;slot-green"
        )));
        assert!(!is_proxnix_managed(Some("a-hand-made-vm")));
        assert!(!is_proxnix_managed(None));
    }

    #[test]
    fn tags_tolerate_surrounding_whitespace() {
        assert_eq!(slot_from_tags(Some("proxnix; slot-green ")), Slot::Green);
        assert_eq!(
            nix_hash_from_tags(Some("proxnix; nix-abc123 ")).unwrap().as_str(),
            "abc123"
        );
    }
}
