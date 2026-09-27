use crate::types::{AppError, Result};
use proxmox_api::access::AccessClient;
use proxmox_api::nodes::NodesClient;
use proxmox_api::nodes::node::NodeClient;
use std::future::Future;
use std::path::{Path, PathBuf};
use tokio::runtime::Handle;

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct PveConfig {
    pub url: String,
    pub node: String,
    pub user: String,
    pub realm: String,
    pub token_id: String,
    pub token_file: PathBuf,
    pub ca_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Pve {
    client: proxmox_api::ReqwestClient,
    node: String,
    runtime: Handle,
}

fn read(path: &Path, what: &str) -> Result<Vec<u8>> {
    std::fs::read(path)
        .map_err(|e| AppError::ProxmoxError(format!("could not read {what} {}: {e}", path.display())))
}

fn secret(path: &Path) -> Result<String> {
    String::from_utf8(read(path, "api token")?)
        .map(|token| token.trim().to_string())
        .map_err(AppError::from)
}

fn pinned_client(ca_file: &Path) -> Result<reqwest::Client> {
    reqwest::Certificate::from_pem(&read(ca_file, "ca certificate")?)
        .and_then(|ca| {
            reqwest::ClientBuilder::new()
                .tls_certs_only([ca])
                .build()
        })
        .map_err(|e| AppError::ProxmoxError(format!("could not build the api client: {e}")))
}

impl Pve {
    pub fn connect(config: &PveConfig, runtime: Handle) -> Result<Self> {
        Ok(Pve {
            client: proxmox_api::ReqwestClient::new(
                &config.url,
                &config.user,
                &config.realm,
                Some(pinned_client(&config.ca_file)?),
            )
            .with_api_token(&config.token_id, &secret(&config.token_file)?),
            node: config.node.clone(),
            runtime,
        })
    }

    pub fn node(&self) -> NodeClient<&proxmox_api::ReqwestClient> {
        NodesClient::new(&self.client).node(&self.node)
    }

    pub fn access(&self) -> AccessClient<&proxmox_api::ReqwestClient> {
        AccessClient::new(&self.client)
    }

    pub fn call<T>(&self, request: impl Future<Output = std::result::Result<T, proxmox_api::ReqwestError>>) -> Result<T> {
        self.runtime.block_on(request).map_err(AppError::from)
    }
}
