use crate::types::{ DesiredState, DeployedState, Result, AppError, QMList };
use std::io::BufReader;
use std::path::Path;
use std::fs::{self, File};
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
    
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    
    pub fn test_parse_qm_list() {
        let sample = "      VMID NAME                 STATUS     MEM(MB)    BOOTDISK(GB) PID
                            100 master               stopped    8000              52.00 0";


        let result = parse_qm_list(sample);
        println!("{:#?}", result);
    }

}
