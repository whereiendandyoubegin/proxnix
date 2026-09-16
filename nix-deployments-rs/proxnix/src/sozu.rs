use std::net::Ipv4Addr;
use crate::context::BackendId;

use sozu_command_lib::{
    channel::Channel,
    proto::command::{
        ActivateListener, AddBackend, Cluster, HttpListenerConfig, IpAddress, ListenerType,
        RemoveBackend, Request, RequestHttpFrontend, Response, ResponseStatus, SocketAddress,
        request::RequestType,
    },
};
use tracing::info;

use crate::types::{AppError, ContainerConfig, Result, VMConfig};

fn socket_address(ip: Ipv4Addr, port: u16) -> SocketAddress {
    SocketAddress {
        ip: IpAddress {
            inner: Some(sozu_command_lib::proto::command::ip_address::Inner::V4(
                u32::from(ip),
            )),
        },
        port: u32::from(port),
    }
}

pub trait Proxied {
    fn backend_port(&self) -> u16;
    fn service_address(&self) -> Option<Ipv4Addr>;
    fn cluster_id(&self) -> &str;
    fn hostname(&self) -> &str;

    fn backend_address(&self, ip: Ipv4Addr) -> SocketAddress {
        socket_address(ip, self.backend_port())
    }

    fn frontend_address(&self) -> Option<SocketAddress> {
        self.service_address()
            .map(|ip| socket_address(ip, FRONTEND_PORT))
    }
}

pub const FRONTEND_PORT: u16 = 80;

impl Proxied for VMConfig {
    fn backend_port(&self) -> u16 {
        self.backend_port
    }
    fn service_address(&self) -> Option<Ipv4Addr> {
        self.service_address
    }
    fn cluster_id(&self) -> &str {
        &self.name
    }
    fn hostname(&self) -> &str {
        &self.hostname
    }
}

impl Proxied for ContainerConfig {
    fn backend_port(&self) -> u16 {
        self.backend_port
    }
    fn service_address(&self) -> Option<Ipv4Addr> {
        self.service_address
    }
    fn cluster_id(&self) -> &str {
        &self.name
    }
    fn hostname(&self) -> &str {
        &self.hostname
    }
}

pub struct SozuClient {
    pub channel: Channel<Request, Response>,
}

impl SozuClient {
    pub fn connect(socket_path: &str) -> Result<Self> {
        let mut channel = Channel::from_path(socket_path, 16384, 163840)?;
        channel.blocking()?;
        Ok(Self { channel })
    }

    fn ensure_listener(&mut self, address: SocketAddress) -> Result<()> {
        info!("sozu: ensuring http listener on port {}", address.port);
        self.channel.write_message(
            &RequestType::AddHttpListener(HttpListenerConfig {
                address: address.clone(),
                sticky_name: "SOZUBALANCEID".to_string(),
                front_timeout: 60,
                back_timeout: 30,
                connect_timeout: 3,
                request_timeout: 10,
                active: false,
                ..Default::default()
            })
            .into(),
        )?;
        let added = self.channel.read_message()?;
        match ResponseStatus::from_i32(added.status) {
            Some(ResponseStatus::Ok) => {}
            _ => info!(
                "sozu: listener not added, assuming it already exists: {}",
                added.message
            ),
        }

        self.channel.write_message(
            &RequestType::ActivateListener(ActivateListener {
                address,
                proxy: ListenerType::Http.into(),
                from_scm: false,
            })
            .into(),
        )?;
        let activated = self.channel.read_message()?;
        match ResponseStatus::from_i32(activated.status) {
            Some(ResponseStatus::Ok) => {}
            _ => info!(
                "sozu: listener not activated, assuming it is already active: {}",
                activated.message
            ),
        }
        Ok(())
    }

    pub fn ensure_cluster<T: Proxied>(&mut self, config: &T) -> Result<&mut Self> {
        info!("sozu: adding cluster '{}'", config.cluster_id());
        self.channel.write_message(
            &RequestType::AddCluster(Cluster {
                cluster_id: config.cluster_id().to_string(),
                ..Default::default()
            })
            .into(),
        )?;

        let response_cluster = self.channel.read_message()?;
        let parsed = ResponseStatus::from_i32(response_cluster.status)
            .ok_or(AppError::SozuError("invalid status".to_string()))?;
        match parsed {
            ResponseStatus::Ok => {}
            ResponseStatus::Failure => return Err(AppError::SozuError(response_cluster.message)),
            _ => return Err(AppError::SozuError("invalid status".to_string())),
        }

        let frontend = config.frontend_address().ok_or_else(|| {
            AppError::SozuError(format!(
                "{} has no service address, it cannot be proxied",
                config.cluster_id()
            ))
        })?;

        self.ensure_listener(frontend.clone())?;

        info!(
            "sozu: adding http frontend for '{}' on {}:{} -> hostname '{}'",
            config.cluster_id(),
            config.service_address().map(|i| i.to_string()).unwrap_or_default(),
            FRONTEND_PORT,
            config.hostname()
        );
        self.channel.write_message(
            &RequestType::AddHttpFrontend(RequestHttpFrontend {
                cluster_id: Some(config.cluster_id().to_string()),
                hostname: config.hostname().to_string(),
                address: frontend,
                ..Default::default()
            })
            .into(),
        )?;

        let response_frontend = self.channel.read_message()?;
        let parsed = ResponseStatus::from_i32(response_frontend.status)
            .ok_or(AppError::SozuError("invalid status".to_string()))?;
        match parsed {
            ResponseStatus::Ok => Ok(self),
            ResponseStatus::Failure => Err(AppError::SozuError(response_frontend.message)),
            _ => Err(AppError::SozuError("invalid status".to_string())),
        }
    }

    pub fn register_backend<T: Proxied>(
        &mut self,
        config: &T,
        backend_id: &BackendId,
        ip: Ipv4Addr,
    ) -> Result<&mut Self> {
        info!(
            "sozu: registering backend '{}' at {} for cluster '{}'",
            backend_id,
            ip,
            config.cluster_id()
        );
        self.channel.write_message(
            &RequestType::AddBackend(AddBackend {
                cluster_id: config.cluster_id().to_string(),
                backend_id: backend_id.as_str().to_string(),
                address: config.backend_address(ip),
                ..Default::default()
            })
            .into(),
        )?;
        let response = self.channel.read_message()?;
        let parsed = ResponseStatus::from_i32(response.status)
            .ok_or(AppError::SozuError("invalid status".to_string()))?;
        match parsed {
            ResponseStatus::Ok => Ok(self),
            ResponseStatus::Failure => Err(AppError::SozuError(response.message)),
            _ => Err(AppError::SozuError("invalid status".to_string())),
        }
    }
    pub fn remove_backend<T: Proxied>(
        &mut self,
        config: &T,
        backend_id: &BackendId,
        ip: Ipv4Addr,
    ) -> Result<()> {
        info!(
            "sozu: removing backend '{}' at {} from cluster '{}'",
            backend_id,
            ip,
            config.cluster_id()
        );
        self.channel.write_message(
            &RequestType::RemoveBackend(RemoveBackend {
                cluster_id: config.cluster_id().to_string(),
                backend_id: backend_id.as_str().to_string(),
                address: config.backend_address(ip),
                ..Default::default()
            })
            .into(),
        )?;
        let response = self.channel.read_message()?;
        let parsed = ResponseStatus::from_i32(response.status)
            .ok_or(AppError::SozuError("invalid status".to_string()))?;
        match parsed {
            ResponseStatus::Ok => Ok(()),
            ResponseStatus::Failure => Err(AppError::SozuError(response.message)),
            _ => Err(AppError::SozuError("invalid status".to_string())),
        }
    }
    pub fn check_sozu_cluster<T: Proxied>(&mut self, config: &T) -> Result<&mut Self> {
        info!("sozu: checking cluster '{}'", config.cluster_id());
        self.channel.write_message(
            &RequestType::QueryClusterById(config.cluster_id().to_string()).into(),
        )?;
        let response = self.channel.read_message()?;
        let parsed = ResponseStatus::from_i32(response.status)
            .ok_or(AppError::SozuError("invalid status".to_string()))?;
        match parsed {
            ResponseStatus::Ok => Ok(self),
            ResponseStatus::Failure => self.ensure_cluster(config),
            _ => Err(AppError::SozuError("invalid status".to_string())),
        }
    }

    pub fn remove_cluster(&mut self, cluster_id: &str) -> Result<&mut Self> {
        info!("sozu: removing cluster '{}'", cluster_id);
        self.channel
            .write_message(&RequestType::RemoveCluster(cluster_id.to_string()).into())?;
        let response = self.channel.read_message()?;
        let parsed = ResponseStatus::from_i32(response.status)
            .ok_or(AppError::SozuError("invalid status".to_string()))?;
        match parsed {
            ResponseStatus::Ok => Ok(self),
            ResponseStatus::Failure => Err(AppError::SozuError(response.message)),
            _ => Err(AppError::SozuError("invalid status".to_string())),
        }
    }
}
