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
    let lines = output_string.lines();
    
}
    
