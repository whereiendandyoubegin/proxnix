use crate::types::{
    AppError, DeployedState, DeployedVM, DesiredState, FieldChange, QMConfig, QMList, Result,
    StateDiff, UpdateAction, VMConfig, VMUpdate,
};
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::process::Command;

pub const DEPLOYED_STATE_PATH: &str = "/var/lib/proxnix/deployed_state.json";

pub fn load_json(path: &str) -> Result<DesiredState> {
    let file = File::open(path)
        .map_err(|e| AppError::CmdError(format!("Failed to open config {}: {}", path, e)))?;
    let file_read = BufReader::new(file);
    let state: DesiredState = serde_json::from_reader(file_read)?;

    Ok(state)
}

pub fn parse_vm_config(json: &str) -> Result<DesiredState> {
    let state: DesiredState = serde_json::from_str(&json)?;
    Ok(state)
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
    let lines = output_string
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
        .collect();

    lines
}

pub fn enrich_cpu_info(deployed: DeployedState) -> Result<DeployedState> {
    let deployedvms = deployed
        .vms
        .into_iter()
        .map(|(_name, vm)| -> Result<(String, DeployedVM)> {
            let config = qm_config(vm.vm_id)?;
            let parsed = parse_qm_config(&config)?;
            Ok((
                vm.vm_name.clone(),
                DeployedVM {
                    vm_id: vm.vm_id,
                    vm_name: vm.vm_name,
                    commit_hash: vm.commit_hash,
                    template_id: vm.template_id,
                    mem_mb: vm.mem_mb,
                    bootdisk_gb: vm.bootdisk_gb,
                    status: vm.status,
                    pid: vm.pid,
                    cores: parsed.cores as u16,
                    sockets: parsed.sockets,
                },
            ))
        })
        .collect::<Result<HashMap<_, _>>>()?;
    Ok(DeployedState { vms: deployedvms })
}

pub fn list_to_deployed_vm(qmlists: Vec<QMList>) -> DeployedState {
    let lists = qmlists
        .into_iter()
        .map(|qmlist| -> (String, DeployedVM) {
            (
                qmlist.name.clone(),
                DeployedVM {
                    vm_id: qmlist.vm_id,
                    vm_name: qmlist.name,
                    commit_hash: None,
                    template_id: None,
                    mem_mb: qmlist.mem_mb,
                    bootdisk_gb: qmlist.bootdisk_gb,
                    status: qmlist.status,
                    pid: qmlist.pid,
                    cores: 0,   //placeholder
                    sockets: 0, //placeholder
                },
            )
        })
        .collect();

    DeployedState { vms: lists }
}

pub fn save_deployed_state(state: &DeployedState, path: &str) -> Result<()> {
    let file = File::create(path)?;
    serde_json::to_writer_pretty(file, state)?;

    Ok(())
}

pub fn diff_state(deployed: &DeployedState, desired: &DesiredState) -> StateDiff {
    let mut to_create: Vec<VMConfig> = Vec::new();
    let mut to_update: Vec<VMUpdate> = Vec::new();
    let mut to_delete: Vec<DeployedVM> = Vec::new();

    for (name, vmconfig) in &desired.vms {
        let mut changes = Vec::new();

        if deployed.vms.contains_key(name) {
            let deployed_vm = deployed.vms.get(name).unwrap();
            if vmconfig.memory_mb != deployed_vm.mem_mb {
                changes.push(FieldChange::Memory);
            }
            if vmconfig.disk_gb as f64 != deployed_vm.bootdisk_gb {
                changes.push(FieldChange::Disk);
            }
            if vmconfig.cores != deployed_vm.cores {
                changes.push(FieldChange::Cores);
            }
            if vmconfig.sockets != deployed_vm.sockets {
                changes.push(FieldChange::Sockets);
            }
            if !changes.is_empty() {
                let action = if vmconfig.protected {
                    UpdateAction::Protected
                } else if changes.contains(&FieldChange::Disk) {
                    UpdateAction::Rebuild
                } else {
                    UpdateAction::InPlace
                };

                to_update.push(VMUpdate {
                    name: name.clone(),
                    config: vmconfig.clone(),
                    changed_fields: changes,
                    required_action: action,
                });
            }
        } else {
            to_create.push(vmconfig.clone())
        }
    }

    for (name, deployed_vm) in &deployed.vms {
        if !desired.vms.contains_key(name) {
            to_delete.push(deployed_vm.clone())
        }
    }

    StateDiff {
        to_create,
        to_update,
        to_delete,
    }
}

pub fn update_deployed_state_commit(deployed: &mut DeployedState, name: &str, commit: &str) -> () {
    if let Some(vm) = deployed.vms.get_mut(name) {
        vm.commit_hash = Some(String::from(commit))
    }
}

pub fn get_vm_statuses() -> Result<HashMap<u32, String>> {
    let raw = qm_list()?;
    let parsed = parse_qm_list(&raw)?;
    Ok(parsed.into_iter().map(|q| (q.vm_id, q.status)).collect())
}

pub fn load_state() -> Result<DeployedState> {
    let qm_list = qm_list()?;
    let parsed_qm_list = parse_qm_list(&qm_list)?;
    let deployed_vm = list_to_deployed_vm(parsed_qm_list);
    let enriched = enrich_cpu_info(deployed_vm)?;

    Ok(enriched)
}

pub fn load_deployed_state(path: &str) -> Result<DeployedState> {
    let p = Path::new(path);
    if !p.exists() {
        return Ok(DeployedState {
            vms: HashMap::new(),
        });
    }
    let file = File::open(p)?;
    let reader = BufReader::new(file);
    let state: DeployedState = serde_json::from_reader(reader)?;
    Ok(state)
}

pub fn full_diff(desired: &DesiredState) -> Result<StateDiff> {
    let deployed = load_state()?;
    let diff = diff_state(&deployed, &desired);

    Ok(diff)
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
}
