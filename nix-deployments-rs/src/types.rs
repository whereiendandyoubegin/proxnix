use std::{collections::HashMap, str::Utf8Error, string::FromUtf8Error};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Git has failed, error: {0}")]
    GitError(String),
    #[error("Nix failed to build, output: {0}")]
    NixError(String),
    #[error("Proxmox API error: {0}")]
    ProxmoxError(String),
    #[error("QM error: {0}")]
    QMError(String),
    #[error("File IO error {0}")]
    FileIOError(#[from] std::io::Error),
    #[error("Serialisation error at some point {0}")]
    SerialisationError(#[from] serde_json::Error),
    #[error("Command error: {0}")]
    CmdError(String),
    #[error("Type conversion error {0}")]
    ConversionError(#[from] FromUtf8Error), 
}

pub type Result<T> = std::result::Result<T, AppError>; 

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct VMConfig {
    pub name: String,
    pub vm_id: u32,
    pub image_type: String,
    pub cores: u16,
    pub sockets: u8,
    pub memory_mb: u32,
    pub storage_location: String,
    pub disk_gb: u32,
    pub cloud_init: CloudInit,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub enum CloudInit {
    None,
    StorageReference(String),
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DeployedVM {
    pub vm_id: u32,
    pub vm_name: String,
    pub template_id: u32,
    pub commit_hash: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DesiredState {
    pub vms: HashMap<String, VMConfig>
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DeployedState {
    pub vms: HashMap<String, DeployedVM>
}
