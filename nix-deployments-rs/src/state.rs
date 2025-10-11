use crate::types::{ AppError, DeployedState, DeployedVM, DesiredState, QMList, Result };
use std::fmt::DebugTuple;
use std::io::BufReader;
use std::path::Path;
use std::fs::{self, File};
use git2::CheckoutNotificationType;
use serde_json::Value;
use std::process::{Command, Output};



pub fn load_json(path: &str) -> Result<DesiredState> {
    let file = File::open(path)?;
    let file_read = BufReader::new(file);
    let state: DesiredState = serde_json::from_reader(file_read)?;

    Ok(state)
}

pub fn qm_list() -> Result<String> {
    let qm_list = Command::new("qm")
        .arg("list")
        .output()?;
    if !qm_list.status.success() {
        return Err(AppError::CmdError(format!("qm list has failed with exit code: {:?}", qm_list.status.code())));
    }
    let stdout_bytes = qm_list.stdout;
    let output_string = String::from_utf8(stdout_bytes)?;
    
    Ok(output_string)
}

pub fn parse_qm_list(output_string: &str) -> Result<Vec<QMList>> {
      let lines = output_string
        .lines()
        .skip(1)
        .map(|line| -> Result<QMList>{
            let parts: Vec<&str> = line.split_whitespace().collect();

            Ok(QMList {
                vm_id: parts[0].parse()?,
                name: parts[1].to_string(),
                status: parts[2].to_string(),
                mem_mb: parts[3].parse()?,
                bootdisk_gb: parts[4].parse()?,
                pid: parts[5].parse()?,
            })
        })
        .collect();
    
        lines
}

pub fn list_to_deployed_vm(qmlists: Vec<QMList>) -> DeployedState {
    let lists = qmlists
        .into_iter()
        .map(|qmlist| -> (String, DeployedVM) {
            (qmlist.name.clone(), DeployedVM {
                vm_id: qmlist.vm_id,
                vm_name: qmlist.name,
                commit_hash: None,
                template_id: None,
                mem_mb: qmlist.mem_mb,
                bootdisk_gb: qmlist.bootdisk_gb,
                status: qmlist.status,
                pid: qmlist.pid,
            })
       })
        .collect();

        DeployedState {
            vms: lists
        }
}

pub fn save_deployed_state
    
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
