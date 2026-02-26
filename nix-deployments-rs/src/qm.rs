use crate::types::{AppError, FieldChange, Result, VMConfig, VMUpdate};
use std::process::Command;

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
        let stderr = String::from_utf8_lossy(&qm_create.stderr);
        return Err(AppError::CmdError(format!(
            "qm create has failed with exit code: {:?}: {}",
            qm_create.status.code(),
            stderr
        )));
    }
    let stdout_bytes = qm_create.stdout;
    let output_string = String::from_utf8(stdout_bytes)?;

    Ok(output_string)
}
// Parses output like: "Successfully imported disk as 'unused0:local-lvm:vm-100-disk-1'"
// Returns the disk reference: "local-lvm:vm-100-disk-1"
fn parse_importdisk_output(output: &str) -> Result<String> {
    let disk_ref = output
        .lines()
        .find_map(|line| {
            if !line.contains("successfully imported disk") {
                return None;
            }
            let start = line.find('\'')?;
            let end = line.rfind('\'')?;
            if start < end {
                Some(line[start + 1..end].to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| {
            AppError::CmdError(format!(
                "could not find disk reference in qm importdisk output: {}",
                output
            ))
        })?;

    Ok(disk_ref)
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
        let stderr = String::from_utf8_lossy(&qm_importdisk.stderr);
        return Err(AppError::CmdError(format!(
            "qm importdisk has failed with exit code: {:?}: {}",
            qm_importdisk.status.code(),
            stderr
        )));
    }
    let output_string = String::from_utf8(qm_importdisk.stdout)?;
    let disk_ref = parse_importdisk_output(&output_string)?;

    Ok(disk_ref)
}

pub fn qm_set_disk(vm_id: u32, disk_ref: &str, disk_slot: &str) -> Result<String> {
    let qm_set_disk = Command::new("qm")
        .arg("set")
        .arg(vm_id.to_string())
        .arg(format!("--{}", disk_slot))
        .arg(disk_ref)
        .arg("--boot")
        .arg(format!("order={}", disk_slot))
        .output()?;
    if !qm_set_disk.status.success() {
        let stderr = String::from_utf8_lossy(&qm_set_disk.stderr);
        return Err(AppError::CmdError(format!(
            "qm set disk has failed with the exit code: {:?}: {}",
            qm_set_disk.status.code(),
            stderr
        )));
    }
    let stdout_bytes = qm_set_disk.stdout;
    let output_string = String::from_utf8(stdout_bytes)?;

    Ok(output_string)
}
//TODO MAYBE add something other than socket as the serial console, bit of a nitpick
pub fn qm_set_agent(vm_id: u32) -> Result<String> {
    let qm_set_agent = Command::new("qm")
        .arg("set")
        .arg(vm_id.to_string())
        .arg("--agent")
        .arg("1")
        .arg("--serial0")
        .arg("socket")
        .output()?;
    if !qm_set_agent.status.success() {
        let stderr = String::from_utf8_lossy(&qm_set_agent.stderr);
        return Err(AppError::CmdError(format!(
            "qm set agent has failed with the exit code: {:?}: {}",
            qm_set_agent.status.code(),
            stderr
        )));
    }
    let stdout_bytes = qm_set_agent.stdout;
    let output_string = String::from_utf8(stdout_bytes)?;

    Ok(output_string)
}

pub fn qm_template(vm_id: u32) -> Result<String> {
    let qm_template = Command::new("qm")
        .arg("template")
        .arg(vm_id.to_string())
        .output()?;
    if !qm_template.status.success() {
        let stderr = String::from_utf8_lossy(&qm_template.stderr);
        return Err(AppError::CmdError(format!(
            "qm template has failed with the exit code: {:?}: {}",
            qm_template.status.code(),
            stderr
        )));
    }
    let stdout_bytes = qm_template.stdout;
    let output_string = String::from_utf8(stdout_bytes)?;

    Ok(output_string)
}

pub fn qm_clone(source_vm_id: u32, dest_vm_id: u32, name: &str) -> Result<String> {
    let qm_clone = Command::new("qm")
        .arg("clone")
        .arg(source_vm_id.to_string())
        .arg(dest_vm_id.to_string())
        .arg("--name")
        .arg(name.to_string())
        .arg("--full")
        .arg("0")
        .output()?;
    if !qm_clone.status.success() {
        let stderr = String::from_utf8_lossy(&qm_clone.stderr);
        return Err(AppError::CmdError(format!(
            "qm clone has failed with the exit code: {:?}: {}",
            qm_clone.status.code(),
            stderr
        )));
    }
    let stdout_bytes = qm_clone.stdout;
    let output_string = String::from_utf8(stdout_bytes)?;

    Ok(output_string)
}

pub fn qm_start(vm_id: u32) -> Result<bool> {
    let output = Command::new("qm")
        .arg("start")
        .arg(vm_id.to_string())
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("already running") {
            return Ok(false);
        }
        return Err(AppError::CmdError(format!(
            "qm start {} failed (exit: {:?}): {}",
            vm_id,
            output.status.code(),
            stderr
        )));
    }
    Ok(true)
}

pub fn qm_destroy(vm_id: u32) -> Result<String> {
    let qm_destroy = Command::new("qm")
        .arg("destroy")
        .arg(vm_id.to_string())
        .output()?;
    if !qm_destroy.status.success() {
        let stderr = String::from_utf8_lossy(&qm_destroy.stderr);
        return Err(AppError::CmdError(format!(
            "qm destroy has failed with the exit code: {:?}: {}",
            qm_destroy.status.code(),
            stderr
        )));
    }
    let stdout_bytes = qm_destroy.stdout;
    let output_string = String::from_utf8(stdout_bytes)?;

    Ok(output_string)
}

pub fn qm_set_resources(vm_id: u32, update: &VMUpdate) -> Result<String> {
    let mut qm_set_resources = Command::new("qm");
    qm_set_resources.arg("set");
    qm_set_resources.arg(vm_id.to_string());

    for field in &update.changed_fields {
        match field {
            FieldChange::Memory => {
                qm_set_resources
                    .arg("--memory")
                    .arg(update.config.memory_mb.to_string());
            }
            FieldChange::Cores => {
                qm_set_resources
                    .arg("--cores")
                    .arg(update.config.cores.to_string());
            }
            FieldChange::Sockets => {
                qm_set_resources
                    .arg("--sockets")
                    .arg(update.config.sockets.to_string());
            }
            _ => {}
        }
    }
    let output = qm_set_resources.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::CmdError(format!(
            "qm set resources has failed with the exit code: {:?}: {}",
            output.status.code(),
            stderr
        )));
    }
    let output_string = String::from_utf8(output.stdout)?;

    Ok(output_string)
}
