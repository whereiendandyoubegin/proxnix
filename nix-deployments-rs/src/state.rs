use crate::types::{ DesiredState, DeployedState, Result, AppError };
use std::io::BufReader;
use std::path::Path;
use std::fs::{self, File};
use serde_json::Value;


pub fn load_json(path: &str) -> Result<DesiredState> {
    let file = File::open(path)?;
    let file_read = BufReader::new(file);
    let state: DesiredState = serde_json::from_reader(file_read)?;

    Ok(state)
}
