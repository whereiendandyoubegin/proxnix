use std::{
    net::{AddrParseError, Ipv4Addr},
    ops::DerefMut,
};

use sozu_command_lib::{
    channel::Channel,
    proto::command::{
        AddBackend, Cluster, IpAddress, Request, RequestHttpFrontend, Response, SocketAddress,
        request::RequestType,
    },
    response::Backend,
};

use crate::types::{ContainerConfig, VMConfig};

pub trait Proxied {
    fn ip(&self) -> Result<IpAddress, AddrParseError>;
    fn proxy_port(&self) -> u32;
    fn cluster_id(&self) -> &str;
    fn hostname(&self) -> &str;
    fn socket_address(&self) -> Result<SocketAddress, AddrParseError>;
}

impl Proxied for VMConfig {
    fn ip(&self) -> Result<IpAddress, AddrParseError> {
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
    fn socket_address(&self) -> Result<SocketAddress, AddrParseError> {
        Ok(SocketAddress {
            ip: self.ip()?,
            port: self.proxy_port(),
        })
    }
}

impl Proxied for ContainerConfig {
    fn ip(&self) -> Result<IpAddress, AddrParseError> {
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
    fn socket_address(&self) -> Result<SocketAddress, AddrParseError> {
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
        let mut channel = Channel::from_path("/run/sozu/command.sock", 16384, 163840)?;
        channel.blocking()?;
        Ok(Self { channel })
    }

    pub fn ensure_cluster<T: Proxied>(&mut self, config: T) -> Result<()> {
        let response = self.channel.read_message()?;

        self.channel.write_message(
            &RequestType::AddCluster(Cluster {
                cluster_id: config.cluster_id().to_string(),
                ..Default::default()
            })
            .into(),
        )?;

        self.channel.write_message(
            &RequestType::AddHttpFrontend(RequestHttpFrontend {
                cluster_id: config.cluster_id()?.to_string(),
                ..Default::default()
            })
            .into(),
        )?;
        Ok(())
    }
    pub fn register_backend<T: Proxied>(&mut self, config: &T, backend_id: &str) -> Result<()> {
        let response = self.channel.read_message()?;

        self.channel.write_message(
            &RequestType::AddBackend(AddBackend {
                cluster_id: config.cluster_id().to_string(),
                backend_id: backend_id.to_string(),
                address: config.socket_address(),
                ..Default::default()
            })
            .into(),
        )?;
        Ok(())
    }
}
