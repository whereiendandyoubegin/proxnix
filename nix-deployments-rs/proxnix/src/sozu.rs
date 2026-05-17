use std::net::Ipv4Addr;

use sozu_command_lib::{
    channel::Channel,
    proto::command::{
        AddBackend, Cluster, IpAddress, RemoveBackend, Request, RequestHttpFrontend, Response,
        ResponseStatus, SocketAddress, request::RequestType,
    },
};

use crate::types::{AppError, ContainerConfig, Result, VMConfig};

pub trait Proxied {
    fn ip(&self) -> Result<IpAddress>;
    fn proxy_port(&self) -> u32;
    fn cluster_id(&self) -> &str;
    fn hostname(&self) -> &str;
    fn socket_address(&self) -> Result<SocketAddress>;
}

impl Proxied for VMConfig {
    fn ip(&self) -> Result<IpAddress> {
        let parsed: Ipv4Addr = self.ip.parse()?;
        Ok(IpAddress {
            inner: Some(sozu_command_lib::proto::command::ip_address::Inner::V4(
                u32::from(parsed),
            )),
        })
    }
    fn proxy_port(&self) -> u32 {
        self.proxy_port
    }
    fn cluster_id(&self) -> &str {
        &self.name
    }
    fn hostname(&self) -> &str {
        &self.hostname
    }
    fn socket_address(&self) -> Result<SocketAddress> {
        Ok(SocketAddress {
            ip: self.ip()?,
            port: self.proxy_port(),
        })
    }
}

impl Proxied for ContainerConfig {
    fn ip(&self) -> Result<IpAddress> {
        let parsed: Ipv4Addr = self.ip.parse()?;
        Ok(IpAddress {
            inner: Some(sozu_command_lib::proto::command::ip_address::Inner::V4(
                u32::from(parsed),
            )),
        })
    }
    fn proxy_port(&self) -> u32 {
        self.proxy_port
    }
    fn cluster_id(&self) -> &str {
        &self.name
    }
    fn hostname(&self) -> &str {
        &self.hostname
    }
    fn socket_address(&self) -> Result<SocketAddress> {
        Ok(SocketAddress {
            ip: self.ip()?,
            port: self.proxy_port(),
        })
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

    pub fn ensure_cluster<T: Proxied>(&mut self, config: &T) -> Result<&mut Self> {
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

        self.channel.write_message(
            &RequestType::AddHttpFrontend(RequestHttpFrontend {
                cluster_id: Some(config.cluster_id().to_string()),
                hostname: config.hostname().to_string(),
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
        backend_id: &str,
    ) -> Result<&mut Self> {
        self.channel.write_message(
            &RequestType::AddBackend(AddBackend {
                cluster_id: config.cluster_id().to_string(),
                backend_id: backend_id.to_string(),
                address: config.socket_address()?,
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
    pub fn remove_backend<T: Proxied>(&mut self, config: &T, backend_id: &str) -> Result<()> {
        self.channel.write_message(
            &RequestType::RemoveBackend(RemoveBackend {
                cluster_id: config.cluster_id().to_string(),
                backend_id: backend_id.to_string(),
                address: config.socket_address()?,
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
