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

const SOZU_MAX_PROCESSING: u32 = 32;

pub struct SozuClient {
    pub channel: Channel<Request, Response>,
}

impl SozuClient {
    pub fn connect(socket_path: &str) -> Result<Self> {
        let mut channel = Channel::from_path(socket_path, 16384, 163840)?;
        channel.blocking()?;
        Ok(Self { channel })
    }

    fn settled(&mut self) -> Result<(ResponseStatus, String)> {
        for _ in 0..SOZU_MAX_PROCESSING {
            let response = self.channel.read_message()?;
            match ResponseStatus::try_from(response.status) {
                Ok(ResponseStatus::Processing) => continue,
                Ok(status) => return Ok((status, response.message)),
                Err(_) => {
                    return Err(AppError::SozuError(format!(
                        "unrecognised sozu status {}",
                        response.status
                    )));
                }
            }
        }
        Err(AppError::SozuError(
            "sozu kept reporting Processing without settling".to_string(),
        ))
    }

    fn expect_ok(&mut self, what: &str) -> Result<()> {
        match self.settled()? {
            (ResponseStatus::Ok, _) => Ok(()),
            (_, message) => Err(AppError::SozuError(format!("{}: {}", what, message))),
        }
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
        match self.settled()? {
            (ResponseStatus::Ok, _) => {}
            (_, message) => info!(
                "sozu: listener not added, assuming it already exists: {}",
                message
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
        match self.settled()? {
            (ResponseStatus::Ok, _) => {}
            (_, message) => info!(
                "sozu: listener not activated, assuming it is already active: {}",
                message
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

        self.expect_ok("add cluster")?;

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

        self.expect_ok("add http frontend")?;
        Ok(self)
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
        self.expect_ok("add backend")?;
        Ok(self)
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
        self.expect_ok("remove backend")
    }
    pub fn check_sozu_cluster<T: Proxied>(&mut self, config: &T) -> Result<&mut Self> {
        info!("sozu: checking cluster '{}'", config.cluster_id());
        self.channel.write_message(
            &RequestType::QueryClusterById(config.cluster_id().to_string()).into(),
        )?;
        match self.settled()? {
            (ResponseStatus::Ok, _) => Ok(self),
            (_, message) => {
                info!(
                    "sozu: cluster '{}' not present ({}), creating it",
                    config.cluster_id(),
                    message
                );
                self.ensure_cluster(config)
            }
        }
    }

    pub fn remove_cluster(&mut self, cluster_id: &str) -> Result<&mut Self> {
        info!("sozu: removing cluster '{}'", cluster_id);
        self.channel
            .write_message(&RequestType::RemoveCluster(cluster_id.to_string()).into())?;
        self.expect_ok("add backend")?;
        Ok(self)
    }
}
