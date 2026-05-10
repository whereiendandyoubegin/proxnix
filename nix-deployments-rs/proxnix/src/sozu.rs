use std::net::Ipv4Addr;

use sozu_command_lib::{
    channel::{Channel, ChannelError},
    proto::command::{
        AddBackend, Cluster, IpAddress, RemoveBackend, Request, RequestHttpFrontend, Response,
        SocketAddress, request::RequestType,
    },
    response::Backend,
};

use crate::types::{ContainerConfig, Result, VMConfig};

pub trait Proxied {
    fn ip(&self) -> Result<IpAddress>;
    fn proxy_port(&self) -> u32;
    fn cluster_id(&self) -> &str;
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

    pub fn ensure_cluster<T: Proxied>(&mut self, config: T) -> Result<()> {
        self.channel.write_message(
            &RequestType::AddCluster(Cluster {
                cluster_id: config.cluster_id().to_string(),
                ..Default::default()
            })
            .into(),
        )?;

        self.channel.write_message(
            &RequestType::AddHttpFrontend(RequestHttpFrontend {
                cluster_id: Some(config.cluster_id().to_string()),
                ..Default::default()
            })
            .into(),
        )?;

        let response = self.channel.read_message()?;

        Ok(())
    }

    pub fn register_backend<T: Proxied>(&mut self, config: &T, backend_id: &str) -> Result<()> {
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
        Ok(())
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
        Ok(())
    }
}
