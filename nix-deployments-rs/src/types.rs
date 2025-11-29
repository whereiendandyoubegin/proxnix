use std::{collections::HashMap, string::FromUtf8Error};

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
    #[error("Error during UTF8 conversion {0}")]
    UTF8Error(#[from] FromUtf8Error),
    #[error("Command error: {0}")]
    CmdError(String),
    #[error("Parsing int error: {0}")]
    ParseIntError(#[from] std::num::ParseIntError),
    #[error("Parsing float error: {0}")]
    ParseFloatError(#[from] std::num::ParseFloatError),
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
    pub protected: bool,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub enum CloudInit {
    None,
    StorageReference(String),
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct QMList {
    pub vm_id: u32,
    pub name: String,
    pub status: String,
    pub mem_mb: u32,
    pub bootdisk_gb: f64,
    pub pid: u32,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DeployedVM {
    pub vm_id: u32,
    pub vm_name: String,
    pub commit_hash: Option<String>,
    pub template_id: Option<u32>,
    pub mem_mb: u32,
    pub bootdisk_gb: f64,
    pub status: String,
    pub pid: u32,
    pub cores: u16,
    pub sockets: u8,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default)]
pub struct QMConfig {
    pub agent: String,
    pub balloon: bool,
    pub boot: String,
    pub bootdisk: String,
    pub cipassword: Option<String>,
    pub ciuser: Option<String>,
    pub cores: u8,
    pub cpu: String,
    pub cpuunits: u16,
    pub disks: HashMap<String, String>,
    pub ipconfigs: HashMap<String, String>,
    pub memory: u32,
    pub meta: String,
    pub name: String,
    pub networks: HashMap<String, String>,
    pub numa: bool,
    pub onboot: bool,
    pub protection: bool,
    pub serial: HashMap<String, String>,
    pub sockets: u8,
    pub sshkeys: Option<String>,
    pub vga: String,
    pub vmgenid: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DesiredState {
    pub vms: HashMap<String, VMConfig>
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DeployedState {
    pub vms: HashMap<String, DeployedVM>
}


#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct StateDiff {
    pub to_create: Vec<VMConfig>,
    pub to_update: Vec<VMUpdate>,
    pub to_delete: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct VMUpdate {
    pub name: String,
    pub config: VMConfig,
    pub changed_fields: Vec<FieldChange>,
    pub required_action: UpdateAction, 
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub enum UpdateAction{
    InPlace,
    Rebuild,
    Protected,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq)]
pub enum FieldChange {
    Memory,
    Cores, 
    Sockets,
    Disk,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub enum RebuildStrategy {
    Rebuild,
    InPlace,
    Protected,
}
