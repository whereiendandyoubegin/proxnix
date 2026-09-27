use proxmox_api::nodes::node::qemu::Scsihw;
use proxmox_api::types::bounded_integer::BoundedIntegerError;
use proxmox_api::types::bounded_string::BoundedStringError;
use proxnix_core::Vmid;
use std::collections::BTreeMap;
use std::fmt;
use std::net::Ipv4Addr;
use std::time::Duration;
use std::str::FromStr;
use std::{collections::HashMap, string::FromUtf8Error};

use crate::context::ImageType;

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
    #[error("ZFS error: {0}")]
    ZfsError(String),
    #[error("{0} is not a valid ZFS or storage name")]
    InvalidZfsName(String),
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
    #[error("Port out of range: {0}")]
    PortRangeError(#[from] std::num::TryFromIntError),
    #[error("Timed out waiting for an IP address on instance {}", .0.get())]
    IpTimeoutError(Vmid),
    #[error("Health check failed for {0}")]
    HealthCheckError(std::net::SocketAddr),
    #[error("{0} is not a MAC address")]
    MacParseError(String),
    #[error("Service address {address} is already answered on {bridge} by {responder}")]
    ServiceAddressConflict {
        address: std::net::Ipv4Addr,
        bridge: String,
        responder: String,
    },
    #[error("Service address {0} is declared by more than one workload")]
    DuplicateServiceAddress(std::net::Ipv4Addr),
    #[error("unable to deserialize: {0}")]
    DeserializationError(#[from] serde::de::value::Error),
    #[error("Proxmox rejected an integer value: {0}")]
    ProxmoxInteger(#[from] BoundedIntegerError),
    #[error("Proxmox rejected a string value: {0}")]
    ProxmoxString(#[from] BoundedStringError),
    #[error("{0} is not a disk slot")]
    InvalidDiskSlot(String),
    #[error("Proxmox API request failed: {0}")]
    ProxmoxApi(#[from] proxmox_api::ReqwestError),
    #[error("Proxmox task failed: {0:?}")]
    Api(crate::remote::ApiFault),
}

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct VMConfig {
    pub name: String,
    pub blue_id: Vmid,
    pub green_id: Vmid,
    pub hostname: String,
    #[serde(default)]
    pub service_address: Option<std::net::Ipv4Addr>,
    #[serde(default = "default_backend_port")]
    pub backend_port: u16,
    #[serde(default)]
    pub tcp_ports: Vec<u16>,
    pub dhcp_timeout_seconds: u64,
    pub health_check_timeout_seconds: u64,
    pub image_type: ImageType,
    pub cores: u16,
    pub sockets: u8,
    pub memory_mb: u32,
    pub storage_location: String,
    pub disk_gb: u32,
    pub protected: bool,
    #[serde(default = "default_network_bridge")]
    pub network_bridge: String,
    #[serde(default = "default_scsi_hw")]
    pub scsi_hw: Scsihw,
    #[serde(default = "default_disk_slot")]
    pub disk_slot: DiskSlot,
    pub impure: bool,
    #[serde(default)]
    pub cutover: Option<CutoverChoice>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CutoverChoice {
    Overlap,
    StopStart,
}


// Defaults for VMConfig
fn default_network_bridge() -> String {
    "vmbr0".to_string()
}

fn default_scsi_hw() -> Scsihw {
    Scsihw::VirtioScsiPci
}

fn default_disk_slot() -> DiskSlot {
    DiskSlot {
        bus: DiskBus::Scsi,
        index: 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskBus {
    Scsi,
    Virtio,
    Sata,
    Ide,
}

impl DiskBus {
    const ALL: [DiskBus; 4] = [DiskBus::Scsi, DiskBus::Virtio, DiskBus::Sata, DiskBus::Ide];

    fn prefix(self) -> &'static str {
        match self {
            DiskBus::Scsi => "scsi",
            DiskBus::Virtio => "virtio",
            DiskBus::Sata => "sata",
            DiskBus::Ide => "ide",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde_with::DeserializeFromStr, serde_with::SerializeDisplay)]
pub struct DiskSlot {
    pub bus: DiskBus,
    pub index: u32,
}

impl FromStr for DiskSlot {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self> {
        DiskBus::ALL
            .into_iter()
            .find_map(|bus| {
                s.strip_prefix(bus.prefix())
                    .and_then(|n| n.parse().ok())
                    .map(|index| DiskSlot { bus, index })
            })
            .ok_or_else(|| AppError::InvalidDiskSlot(s.to_string()))
    }
}

impl fmt::Display for DiskSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.bus.prefix(), self.index)
    }
}

fn default_backend_port() -> u16 {
    80
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ContainerConfig {
    pub name: String,
    pub hostname: String,
    #[serde(default)]
    pub service_address: Option<std::net::Ipv4Addr>,
    #[serde(default = "default_backend_port")]
    pub backend_port: u16,
    #[serde(default)]
    pub tcp_ports: Vec<u16>,
    pub dhcp_timeout_seconds: u64,
    pub health_check_timeout_seconds: u64,
    pub blue_id: Vmid,
    pub green_id: Vmid,
    pub image_type: ImageType,
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
    #[serde(default)]
    pub cutover: Option<CutoverChoice>,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum MountMode {
    #[default]
    ReadWrite,
    ReadOnly,
}


#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq)]
pub struct BindMount {
    pub host_path: String,
    pub container_path: String,
    #[serde(default)]
    pub mode: MountMode,
}


fn default_container_network_bridge() -> String {
    "vmbr0".to_string()
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AppConfig {
    pub sozu: SozuConfig,
    pub ssh_key_candidates: Vec<String>,
    pub template_cache_path: String,
    pub repo_cache: String,
    pub server_address: std::net::SocketAddr,
    pub backend_pool: Option<crate::context::BackendPool>,
    pub local_repo: Option<String>,
    pub zfs_images: Option<crate::zfs::ZfsImages>,
    pub timings_ms: Timings,
    pub unprivileged_idmap: IdRange,
    pub guest_check: GuestCheck,
    pub proxmox: crate::pve::PveConfig,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct SozuConfig {
    pub socket_path: String,
    pub listen_ip: Ipv4Addr,
    pub http_port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct IdRange {
    pub host_base: u32,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct GuestCheck {
    pub shell: String,
    pub command: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Timing {
    PeriodicReconcile,
    WebhookLockWait,
    NixBuild,
    NixEval,
    ProvisionStagger,
    GuestCheckPoll,
    GuestCheckRun,
    ArpProbe,
    SozuTcpIdle,
    AddressPoll,
    PortPoll,
    TaskPoll,
    TaskTimeout,
}

impl Timing {
    pub const ALL: [Timing; 13] = [
        Timing::PeriodicReconcile,
        Timing::WebhookLockWait,
        Timing::NixBuild,
        Timing::NixEval,
        Timing::ProvisionStagger,
        Timing::GuestCheckPoll,
        Timing::GuestCheckRun,
        Timing::ArpProbe,
        Timing::SozuTcpIdle,
        Timing::AddressPoll,
        Timing::PortPoll,
        Timing::TaskPoll,
        Timing::TaskTimeout,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(try_from = "BTreeMap<Timing, u64>")]
pub struct Timings(BTreeMap<Timing, u64>);

impl TryFrom<BTreeMap<Timing, u64>> for Timings {
    type Error = String;

    fn try_from(millis: BTreeMap<Timing, u64>) -> std::result::Result<Self, String> {
        match Timing::ALL.iter().find(|timing| !millis.contains_key(timing)) {
            Some(missing) => Err(format!("timings_ms has no value for {missing:?}")),
            None => Ok(Timings(millis)),
        }
    }
}

impl Timings {
    pub fn get(&self, timing: Timing) -> Duration {
        Duration::from_millis(self.0[&timing])
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct DesiredState {
    pub vms: HashMap<String, VMConfig>,
    #[serde(default)]
    pub containers: HashMap<String, ContainerConfig>,
}

#[derive(Debug)]
pub struct ParsedWebhook {
    pub repository: String,
    pub hash: String,
}
