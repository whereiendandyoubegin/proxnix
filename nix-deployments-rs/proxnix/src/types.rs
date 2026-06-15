use proxnix_core::Workload;
use std::{collections::HashMap, string::FromUtf8Error};

use crate::pipeline::WorkloadGroup;

#[allow(clippy::enum_variant_names)]
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
    #[error("Git2 error: {0}")]
    Git2Error(#[from] git2::Error),
    #[error("Parsing module error: {0}")]
    ParsingModuleError(String),
    #[error("Sozu error: {0}")]
    SozuError(String),
    #[error("Sozu channel error: {0}")]
    ChannelError(#[from] sozu_command_lib::channel::ChannelError),
    #[error("Error parsing addr: {0}")]
    AddrParseErr(#[from] std::net::AddrParseError),
}

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct VMConfig {
    pub name: String,
    pub vm_id: u32,
    pub ip: String,
    pub hostname: String,
    pub proxy_port: u32,
    pub image_type: String,
    pub cores: u16,
    pub sockets: u8,
    pub memory_mb: u32,
    pub storage_location: String,
    pub disk_gb: u32,
    pub protected: bool,
    #[serde(default = "default_network_bridge")]
    pub network_bridge: String,
    #[serde(default = "default_scsi_hw")]
    pub scsi_hw: String,
    #[serde(default = "default_disk_slot")]
    pub disk_slot: String,
    pub impure: bool,
    pub blue_ip: String,
    pub green_ip: String,
    pub active_slot: proxnix_core::Slot,
}

impl Workload for VMConfig {
    fn id(&self) -> u32 {
        self.vm_id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn memory_mb(&self) -> u32 {
        self.memory_mb
    }
    fn cores(&self) -> u16 {
        self.cores
    }
    fn ip_for_slot(&self, s: proxnix_core::Slot) -> &str {
        match s {
            proxnix_core::Slot::Blue => &self.blue_ip,
            proxnix_core::Slot::Green => &self.green_ip,
        }
    }
}

// Defaults for VMConfig
fn default_network_bridge() -> String {
    "vmbr0".to_string()
}

fn default_scsi_hw() -> String {
    "virtio-scsi-pci".to_string()
}

fn default_disk_slot() -> String {
    "scsi0".to_string()
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ContainerConfig {
    pub name: String,
    pub ip: String,
    pub proxy_port: u32,
    pub hostname: String,
    pub ct_id: u32,
    pub image_type: String,
    pub cores: u16,
    pub memory_mb: u32,
    pub storage_location: String,
    pub disk_gb: u32,
    pub protected: bool,
    #[serde(default)]
    pub privileged: bool,
    #[serde(default)]
    pub bind_mounts: Vec<BindMount>,
    #[serde(default = "default_container_network_bridge")]
    pub network_bridge: String,
    pub impure: bool,
    pub blue_ip: String,
    pub green_ip: String,
    pub active_slot: proxnix_core::Slot,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq)]
pub struct BindMount {
    pub host_path: String,
    pub container_path: String,
}

impl Workload for ContainerConfig {
    fn id(&self) -> u32 {
        self.ct_id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn memory_mb(&self) -> u32 {
        self.memory_mb
    }
    fn cores(&self) -> u16 {
        self.cores
    }
    fn ip_for_slot(&self, s: proxnix_core::Slot) -> &str {
        match s {
            proxnix_core::Slot::Blue => &self.blue_ip,
            proxnix_core::Slot::Green => &self.green_ip,
        }
    }
}

fn default_container_network_bridge() -> String {
    "vmbr0".to_string()
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

fn default_slot() -> proxnix_core::Slot {
    proxnix_core::Slot::Blue
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DeployedVM {
    pub vm_id: u32,
    pub vm_name: String,
    pub nix_hash: Option<String>,
    pub template_id: Option<u32>,
    pub mem_mb: u32,
    pub bootdisk_gb: f64,
    pub status: String,
    pub pid: u32,
    pub cores: u16,
    pub sockets: u8,
    #[serde(default = "default_slot")]
    pub active_slot: proxnix_core::Slot,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DeployedContainer {
    pub ct_id: u32,
    pub ct_name: String,
    pub nix_hash: Option<String>,
    pub mem_mb: u32,
    pub bootdisk_gb: f64,
    pub status: String,
    pub cores: u16,
    pub privileged: bool,
    pub bind_mounts: Vec<BindMount>,
    #[serde(default = "default_slot")]
    pub active_slot: proxnix_core::Slot,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct QMConfig {
    pub agent: String,
    pub balloon: u8,
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
    pub numa: u8,
    pub onboot: u8,
    pub protection: u8,
    pub serial: HashMap<String, String>,
    pub sockets: u8,
    pub sshkeys: Option<String>,
    pub tags: Option<String>,
    pub vga: String,
    pub vmgenid: String,
}

impl Default for QMConfig {
    fn default() -> Self {
        Self {
            sockets: 1,
            agent: Default::default(),
            balloon: Default::default(),
            boot: Default::default(),
            bootdisk: Default::default(),
            cipassword: Default::default(),
            ciuser: Default::default(),
            cores: Default::default(),
            cpu: Default::default(),
            cpuunits: Default::default(),
            disks: Default::default(),
            ipconfigs: Default::default(),
            memory: Default::default(),
            meta: Default::default(),
            name: Default::default(),
            networks: Default::default(),
            numa: Default::default(),
            onboot: Default::default(),
            protection: Default::default(),
            serial: Default::default(),
            sshkeys: Default::default(),
            tags: Default::default(),
            vga: Default::default(),
            vmgenid: Default::default(),
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AppConfig {
    #[serde(default = "default_sozu_socket_path")]
    pub sozu_socket_path: String,
    #[serde(default)]
    pub ssh_key_candidates: Vec<String>,
    #[serde(default = "default_template_cache_path")]
    pub template_cache_path: String,
    pub server_address: std::net::SocketAddr,
}

fn default_sozu_socket_path() -> String {
    "/run/sozu/command.sock".to_string()
}

fn default_template_cache_path() -> String {
    "/var/lib/vz/template/cache/".to_string()
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DesiredState {
    pub vms: HashMap<String, VMConfig>,
    #[serde(default)]
    pub containers: HashMap<String, ContainerConfig>,
}

impl DesiredState {
    pub fn into_workload_groups(self) -> Vec<WorkloadGroup> {
        vec![
            WorkloadGroup::new(self.vms.into_values().collect()),
            WorkloadGroup::new(self.containers.into_values().collect()),
        ]
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DeployedState {
    pub vms: HashMap<String, DeployedVM>,
    pub containers: HashMap<String, DeployedContainer>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq)]
pub enum ContainerFieldChange {
    Memory,
    Cores,
    Image,
    Privileged,
    BindMounts,
    Disk,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq)]
pub enum FieldChange {
    Memory,
    Cores,
    Sockets,
    Disk,
    Image,
}

#[derive(Debug)]
pub struct ParsedWebhook {
    pub repository: String,
    pub hash: String,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub enum SkipReason {
    Protected,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub enum OutcomeKind {
    Created,
    Rebuilt,
    Updated,
    Destroyed,
    Skipped(SkipReason),
    NoOp,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub enum RebuildStrategy {
    Rebuild,
    InPlace,
    Protected,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Outcome {
    pub name: String,
    pub kind: OutcomeKind,
    pub error: Option<String>,
}

impl Outcome {
    pub fn new(name: &str, kind: OutcomeKind, result: Result<()>) -> Self {
        Self {
            name: name.to_string(),
            kind,
            error: result.err().map(|e| e.to_string()),
        }
    }
}
