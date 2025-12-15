use crate::types::{ AppError, DeployedState, DeployedVM, DesiredState, FieldChange, QMConfig, QMList, Result, StateDiff, UpdateAction, VMConfig, VMUpdate };
use std::process::Command;
use std::path::Path;

// TODO Parse the output from this and pattern match to see if it has failed and add some cases to retry
pub fn qm_create(config: &VMConfig) -> Result<String> {
    let qm_create = Command::new("qm")
        .arg("create")
        .arg(config.vm_id.to_string())
        .arg("--name")
        .arg(&config.name)
        .arg("--memory")
        .arg(config.memory_mb.to_string())
        .arg("--cores")
        .arg(config.cores.to_string())
        .arg("--net0")
        .arg(format!("virtio,bridge={}", config.network_bridge))
        .arg("--scsihw")
        .arg(config.scsi_hw.to_string())
        .output()?;
    if !qm_create.status.success() {
        return Err(AppError::CmdError(format!("qm create has failed with exit code: {:?}", qm_create.status.code())));
    }
    let stdout_bytes = qm_create.stdout;
    let output_string = String::from_utf8(stdout_bytes)?;

    Ok(output_string)
}

pub fn qm_importdisk(vm_id: u32, qcow_path: &str, storage: &str) -> Result<String> {
    let qm_importdisk = Command::new("qm")
        .arg("importdisk")
        .arg(vm_id.to_string())
        .arg(qcow_path.to_string())
        .arg(storage.to_string())
        .arg("--format=qcow2")
        .output()?;
    if !qm_importdisk.status.success() {
        return Err(AppError::CmdError(format!("qm importdisk has failed with exit code: {:?}", qm_importdisk.status.code())));
    }
    let stdout_bytes = qm_importdisk.stdout;
    let output_string = String::from_utf8(stdout_bytes)?;

    Ok(output_string)
}
