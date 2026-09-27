use std::net::Ipv4Addr;
use crate::context::BackendId;

use sozu_command_lib::{
    channel::Channel,
    proto::command::{
        ActivateListener, AddBackend, Cluster, IpAddress, ListListeners, ListenerType,
        QueryClustersHashes, RemoveBackend, Request, RequestHttpFrontend, RequestTcpFrontend,
        Response, ResponseContent, ResponseStatus, SocketAddress, TcpListenerConfig,
        request::RequestType, response_content::ContentType,
    },
};
use tracing::{debug, info, warn};

use crate::types::{AppConfig, AppError, ContainerConfig, Result, Timing, VMConfig};

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
    fn tcp_ports(&self) -> &[u16];
    fn cluster_id(&self) -> &str;
    fn hostname(&self) -> &str;

    fn backend_address(&self, ip: Ipv4Addr) -> SocketAddress {
        socket_address(ip, self.backend_port())
    }
}

pub fn tcp_cluster_id(name: &str, port: u16) -> String {
    format!("{name}-tcp-{port}")
}

fn is_tcp_cluster_of(cluster_id: &str, name: &str) -> bool {
    cluster_id
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix("-tcp-"))
        .is_some_and(|port| port.parse::<u16>().is_ok())
}

pub fn tcp_clusters_in(content: &ResponseContent, name: &str) -> Vec<String> {
    let mut ids: Vec<String> = match &content.content_type {
        Some(ContentType::ClusterHashes(hashes)) => hashes
            .map
            .keys()
            .filter(|id| is_tcp_cluster_of(id, name))
            .cloned()
            .collect(),
        Some(ContentType::WorkerResponses(responses)) => responses
            .map
            .values()
            .flat_map(|r| tcp_clusters_in(r, name))
            .collect(),
        _ => Vec::new(),
    };
    ids.sort();
    ids.dedup();
    ids
}

pub fn tcp_listener_in(content: &ResponseContent, address: &SocketAddress) -> Option<bool> {
    match &content.content_type {
        Some(ContentType::ListenersList(list)) => list
            .tcp_listeners
            .values()
            .find(|l| l.address == *address)
            .map(|l| l.active),
        Some(ContentType::WorkerResponses(responses)) => responses
            .map
            .values()
            .filter_map(|r| tcp_listener_in(r, address))
            .reduce(|a, b| a && b),
        _ => None,
    }
}

impl Proxied for VMConfig {
    fn backend_port(&self) -> u16 {
        self.backend_port
    }
    fn tcp_ports(&self) -> &[u16] {
        &self.tcp_ports
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
    fn tcp_ports(&self) -> &[u16] {
        &self.tcp_ports
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlreadyApplied {
    NoChange,
    AlreadyExists,
}

impl AlreadyApplied {
    const ALL: [AlreadyApplied; 2] = [AlreadyApplied::NoChange, AlreadyApplied::AlreadyExists];

    fn phrase(self) -> &'static str {
        match self {
            AlreadyApplied::NoChange => "did not bring any change",
            AlreadyApplied::AlreadyExists => "already exists",
        }
    }

    fn in_message(message: &str) -> Option<AlreadyApplied> {
        AlreadyApplied::ALL
            .into_iter()
            .find(|refusal| message.contains(refusal.phrase()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
    Changed,
    AlreadyApplied,
}

impl Settled {
    fn and(self, other: Settled) -> Settled {
        match (self, other) {
            (Settled::AlreadyApplied, Settled::AlreadyApplied) => Settled::AlreadyApplied,
            _ => Settled::Changed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleBackend {
    pub backend_id: String,
    pub address: SocketAddress,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    pub removed: usize,
    pub failed: usize,
}

impl Pruned {
    fn removed(self) -> Self {
        Self { removed: self.removed + 1, ..self }
    }
    fn failed(self) -> Self {
        Self { failed: self.failed + 1, ..self }
    }
}

pub fn backends_in(content: &ResponseContent) -> Vec<AddBackend> {
    match &content.content_type {
        Some(ContentType::Clusters(clusters)) => clusters
            .vec
            .iter()
            .flat_map(|info| info.backends.iter().cloned())
            .collect(),
        Some(ContentType::WorkerResponses(responses)) => responses
            .map
            .values()
            .flat_map(backends_in)
            .collect(),
        _ => Vec::new(),
    }
}

pub fn stale_backends(
    existing: &[AddBackend],
    cluster_id: &str,
    keep: &SocketAddress,
) -> Vec<StaleBackend> {
    existing
        .iter()
        .filter(|b| b.cluster_id == cluster_id && b.address != *keep)
        .map(|b| StaleBackend {
            backend_id: b.backend_id.clone(),
            address: b.address,
        })
        .fold(Vec::new(), |acc, stale| if acc.contains(&stale) { acc } else { [acc, vec![stale]].concat() })
}

pub struct SozuClient {
    pub channel: Channel<Request, Response>,
    listen_ip: Ipv4Addr,
    http_port: u16,
    tcp_idle_secs: u32,
}

impl SozuClient {
    pub fn connect(settings: &AppConfig) -> Result<Self> {
        let mut channel = Channel::from_path(&settings.sozu.socket_path, 16384, 163840)?;
        channel.blocking()?;
        Ok(Self {
            channel,
            listen_ip: settings.sozu.listen_ip,
            http_port: settings.sozu.http_port,
            tcp_idle_secs: u32::try_from(settings.timings_ms.get(Timing::SozuTcpIdle).as_secs())?,
        })
    }

    fn frontend_address<T: Proxied>(&self, config: &T) -> Option<SocketAddress> {
        config
            .service_address()
            .map(|_| socket_address(self.listen_ip, self.http_port))
    }

    fn settled_response(&mut self) -> Result<Response> {
        for _ in 0..SOZU_MAX_PROCESSING {
            let response = self.channel.read_message()?;
            match ResponseStatus::try_from(response.status) {
                Ok(ResponseStatus::Processing) => continue,
                Ok(_) => return Ok(response),
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

    fn settled(&mut self) -> Result<(ResponseStatus, String)> {
        let response = self.settled_response()?;
        match ResponseStatus::try_from(response.status) {
            Ok(status) => Ok((status, response.message)),
            Err(_) => Err(AppError::SozuError(format!(
                "unrecognised sozu status {}",
                response.status
            ))),
        }
    }

    fn expect_applied(&mut self, what: &str) -> Result<Settled> {
        match self.settled()? {
            (ResponseStatus::Ok, _) => Ok(Settled::Changed),
            (_, message) if AlreadyApplied::in_message(&message).is_some() => {
                Ok(Settled::AlreadyApplied)
            }
            (_, message) => Err(AppError::SozuError(format!("{what}: {message}"))),
        }
    }

    pub fn ensure_cluster<T: Proxied>(&mut self, config: &T) -> Result<Settled> {
        debug!("sozu: adding cluster '{}'", config.cluster_id());
        self.channel.write_message(
            &RequestType::AddCluster(Cluster {
                cluster_id: config.cluster_id().to_string(),
                ..Default::default()
            })
            .into(),
        )?;

        self.expect_applied("add cluster")?;

        let frontend = self.frontend_address(config).ok_or_else(|| {
            AppError::SozuError(format!(
                "{} has no service address, it cannot be proxied",
                config.cluster_id()
            ))
        })?;

        debug!(
            "sozu: adding http frontend for '{}' on {}:{} matching hostname '{}'",
            config.cluster_id(),
            self.listen_ip,
            self.http_port,
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

        self.expect_applied("add http frontend")
    }

    pub fn register_backend<T: Proxied>(
        &mut self,
        config: &T,
        backend_id: &BackendId,
        ip: Ipv4Addr,
    ) -> Result<Settled> {
        debug!(
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
        self.expect_applied("add backend")
    }
    fn ensure_tcp_listener(&mut self, port: u16) -> Result<Settled> {
        let address = socket_address(self.listen_ip, port);
        self.channel
            .write_message(&RequestType::ListListeners(ListListeners {}).into())?;
        let response = self.settled_response()?;
        let existing = match ResponseStatus::try_from(response.status) {
            Ok(ResponseStatus::Ok) => response
                .content
                .as_ref()
                .and_then(|c| tcp_listener_in(c, &address)),
            _ => {
                return Err(AppError::SozuError(format!(
                    "could not list listeners: {}",
                    response.message
                )));
            }
        };

        let added = if existing.is_some() { Settled::AlreadyApplied } else {
            info!("sozu: adding tcp listener on {}:{}", self.listen_ip, port);
            self.channel.write_message(
                &RequestType::AddTcpListener(TcpListenerConfig {
                    address,
                    front_timeout: self.tcp_idle_secs,
                    back_timeout: self.tcp_idle_secs,
                    ..Default::default()
                })
                .into(),
            )?;
            self.expect_applied("add tcp listener")?
        };

        if let Some(true) = existing { Ok(added) } else {
            info!("sozu: activating tcp listener on {}:{}", self.listen_ip, port);
            self.channel.write_message(
                &RequestType::ActivateListener(ActivateListener {
                    address,
                    proxy: ListenerType::Tcp as i32,
                    from_scm: false,
                })
                .into(),
            )?;
            self.expect_applied("activate tcp listener")
                .map(|activated| added.and(activated))
        }
    }

    pub fn register_tcp_backends<T: Proxied>(
        &mut self,
        config: &T,
        backend_id: &BackendId,
        ip: Ipv4Addr,
    ) -> Result<Settled> {
        config.tcp_ports().iter().try_fold(Settled::AlreadyApplied, |acc, &port| {
            let cluster_id = tcp_cluster_id(config.cluster_id(), port);
            let listener = self.ensure_tcp_listener(port)?;

            debug!("sozu: adding tcp cluster '{}'", cluster_id);
            self.channel.write_message(
                &RequestType::AddCluster(Cluster {
                    cluster_id: cluster_id.clone(),
                    ..Default::default()
                })
                .into(),
            )?;
            self.expect_applied("add tcp cluster")?;

            self.channel.write_message(
                &RequestType::AddTcpFrontend(RequestTcpFrontend {
                    cluster_id: cluster_id.clone(),
                    address: socket_address(self.listen_ip, port),
                    ..Default::default()
                })
                .into(),
            )?;
            let frontend = self.expect_applied("add tcp frontend")?;

            debug!(
                "sozu: registering tcp backend '{}' at {}:{} for cluster '{}'",
                backend_id, ip, port, cluster_id
            );
            self.channel.write_message(
                &RequestType::AddBackend(AddBackend {
                    cluster_id: cluster_id.clone(),
                    backend_id: backend_id.as_str().to_string(),
                    address: socket_address(ip, port),
                    ..Default::default()
                })
                .into(),
            )?;
            self.expect_applied("add tcp backend")?;

            Ok(acc.and(listener).and(frontend))
        })
    }

    pub fn prune_tcp_backends<T: Proxied>(&mut self, config: &T, keep: Ipv4Addr) -> Result<Pruned> {
        config.tcp_ports().iter().try_fold(Pruned::default(), |acc, &port| {
            let cluster_id = tcp_cluster_id(config.cluster_id(), port);
            let wanted = socket_address(keep, port);
            let existing = self.cluster_backends(&cluster_id)?;
            Ok(stale_backends(&existing, &cluster_id, &wanted)
                .iter()
                .fold(acc, |acc, backend| {
                    info!(
                        "sozu: dropping stale tcp backend '{}' at {:?} from cluster '{}'",
                        backend.backend_id, backend.address, cluster_id
                    );
                    match self.remove_backend_at(&cluster_id, &backend.backend_id, &backend.address) {
                        Ok(()) => acc.removed(),
                        Err(e) => {
                            warn!(
                                "sozu: could not drop stale tcp backend '{}' from cluster '{}': {}",
                                backend.backend_id, cluster_id, e
                            );
                            acc.failed()
                        }
                    }
                }))
        })
    }

    pub fn remove_tcp_backends<T: Proxied>(&mut self, config: &T, backend_id: &BackendId, ip: Ipv4Addr) {
        config.tcp_ports().iter().for_each(|&port| {
            let cluster_id = tcp_cluster_id(config.cluster_id(), port);
            match self.remove_backend_at(&cluster_id, backend_id.as_str(), &socket_address(ip, port)) {
                Ok(()) => {}
                Err(e) => warn!(
                    "sozu: could not deregister tcp backend '{}' from cluster '{}': {}",
                    backend_id, cluster_id, e
                ),
            }
        });
    }

    pub fn remove_tcp_clusters(&mut self, name: &str) -> Result<usize> {
        self.channel
            .write_message(&RequestType::QueryClustersHashes(QueryClustersHashes {}).into())?;
        let response = self.settled_response()?;
        let ids = match ResponseStatus::try_from(response.status) {
            Ok(ResponseStatus::Ok) => response
                .content
                .as_ref()
                .map(|c| tcp_clusters_in(c, name))
                .unwrap_or_default(),
            _ => {
                return Err(AppError::SozuError(format!(
                    "could not list clusters: {}",
                    response.message
                )));
            }
        };
        ids.iter().try_for_each(|id| self.remove_cluster(id).map(|_| ()))?;
        Ok(ids.len())
    }

    fn cluster_backends(&mut self, cluster_id: &str) -> Result<Vec<AddBackend>> {
        self.channel.write_message(
            &RequestType::QueryClusterById(cluster_id.to_string()).into(),
        )?;
        let response = self.settled_response()?;
        match ResponseStatus::try_from(response.status) {
            Ok(ResponseStatus::Ok) => Ok(response
                .content
                .as_ref()
                .map(backends_in)
                .unwrap_or_default()),
            _ => Err(AppError::SozuError(format!(
                "could not query cluster {}: {}",
                cluster_id, response.message
            ))),
        }
    }

    pub fn prune_backends<T: Proxied>(&mut self, config: &T, keep: Ipv4Addr) -> Result<Pruned> {
        let wanted = config.backend_address(keep);
        let existing = self.cluster_backends(config.cluster_id())?;
        let stale = stale_backends(&existing, config.cluster_id(), &wanted);

        Ok(stale.iter().fold(Pruned::default(), |acc, backend| {
            info!(
                "sozu: dropping stale backend '{}' at {:?} from cluster '{}'",
                backend.backend_id,
                backend.address,
                config.cluster_id()
            );
            match self.remove_backend_at(config.cluster_id(), &backend.backend_id, &backend.address)
            {
                Ok(()) => acc.removed(),
                Err(e) => {
                    warn!(
                        "sozu: could not drop stale backend '{}' from cluster '{}': {}",
                        backend.backend_id,
                        config.cluster_id(),
                        e
                    );
                    acc.failed()
                }
            }
        }))
    }

    fn remove_backend_at(
        &mut self,
        cluster_id: &str,
        backend_id: &str,
        address: &SocketAddress,
    ) -> Result<()> {
        self.channel.write_message(
            &RequestType::RemoveBackend(RemoveBackend {
                cluster_id: cluster_id.to_string(),
                backend_id: backend_id.to_string(),
                address: *address,
            })
            .into(),
        )?;
        self.expect_applied("remove backend").map(|_| ())
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
        match self.expect_applied("remove backend")? {
            Settled::Changed => Ok(()),
            Settled::AlreadyApplied => {
                info!("sozu: backend '{}' was already absent", backend_id);
                Ok(())
            }
        }
    }
    pub fn remove_cluster(&mut self, cluster_id: &str) -> Result<&mut Self> {
        info!("sozu: removing cluster '{}'", cluster_id);
        self.channel
            .write_message(&RequestType::RemoveCluster(cluster_id.to_string()).into())?;
        self.expect_applied("remove cluster")?;
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sozu_command_lib::proto::command::{ClusterInformation, ClusterInformations, WorkerResponses};
    use std::collections::BTreeMap;

    fn addr(last: u8, port: u16) -> SocketAddress {
        socket_address(Ipv4Addr::new(192, 168, 1, last), port)
    }

    fn backend(cluster: &str, id: &str, last: u8, port: u16) -> AddBackend {
        AddBackend {
            cluster_id: cluster.to_string(),
            backend_id: id.to_string(),
            address: addr(last, port),
            sticky_id: None,
            load_balancing_parameters: None,
            backup: None,
        }
    }

    fn clusters(backends: Vec<AddBackend>) -> ResponseContent {
        ResponseContent {
            content_type: Some(ContentType::Clusters(ClusterInformations {
                vec: vec![ClusterInformation {
                    configuration: None,
                    http_frontends: vec![],
                    https_frontends: vec![],
                    tcp_frontends: vec![],
                    backends,
                }],
            })),
        }
    }

    #[test]
    fn the_live_backend_is_never_stale() {
        let existing = vec![backend("test-website", "new", 178, 3000)];
        assert_eq!(
            stale_backends(&existing, "test-website", &addr(178, 3000)),
            vec![]
        );
    }

    #[test]
    fn a_backend_at_a_retired_address_is_stale() {
        let existing = vec![
            backend("test-website", "new", 178, 3000),
            backend("test-website", "old", 113, 3000),
        ];
        assert_eq!(
            stale_backends(&existing, "test-website", &addr(178, 3000)),
            vec![StaleBackend { backend_id: "old".to_string(), address: addr(113, 3000) }]
        );
    }

    #[test]
    fn the_same_address_on_a_different_port_is_stale() {
        let existing = vec![backend("test-website", "port80", 178, 80)];
        assert_eq!(
            stale_backends(&existing, "test-website", &addr(178, 3000)),
            vec![StaleBackend { backend_id: "port80".to_string(), address: addr(178, 80) }]
        );
    }

    #[test]
    fn backends_of_other_clusters_are_left_alone() {
        let existing = vec![
            backend("monitoring", "other", 232, 3000),
            backend("test-website", "old", 113, 3000),
        ];
        assert_eq!(
            stale_backends(&existing, "test-website", &addr(178, 3000)),
            vec![StaleBackend { backend_id: "old".to_string(), address: addr(113, 3000) }]
        );
    }

    #[test]
    fn a_cluster_with_nothing_registered_has_nothing_to_prune() {
        assert_eq!(stale_backends(&[], "test-website", &addr(178, 3000)), vec![]);
    }

    #[test]
    fn a_backend_reported_by_several_workers_is_removed_once() {
        let existing = vec![
            backend("test-website", "old", 113, 3000),
            backend("test-website", "old", 113, 3000),
        ];
        assert_eq!(
            stale_backends(&existing, "test-website", &addr(178, 3000)).len(),
            1
        );
    }

    #[test]
    fn backends_are_read_out_of_a_direct_cluster_response() {
        let content = clusters(vec![backend("test-website", "old", 113, 3000)]);
        assert_eq!(backends_in(&content), vec![backend("test-website", "old", 113, 3000)]);
    }

    #[test]
    fn backends_are_read_out_of_every_worker_response() {
        let content = ResponseContent {
            content_type: Some(ContentType::WorkerResponses(WorkerResponses {
                map: BTreeMap::from([
                    ("0".to_string(), clusters(vec![backend("test-website", "old", 113, 3000)])),
                    ("1".to_string(), clusters(vec![backend("test-website", "new", 178, 3000)])),
                ]),
            })),
        };
        assert_eq!(backends_in(&content).len(), 2);
    }

    #[test]
    fn a_response_carrying_something_else_yields_no_backends() {
        let content = ResponseContent { content_type: None };
        assert_eq!(backends_in(&content), vec![]);
    }

    fn hashes(ids: &[&str]) -> ResponseContent {
        ResponseContent {
            content_type: Some(ContentType::ClusterHashes(
                sozu_command_lib::proto::command::ClusterHashes {
                    map: ids.iter().map(|id| (id.to_string(), 0)).collect(),
                },
            )),
        }
    }

    #[test]
    fn tcp_clusters_are_named_after_workload_and_port() {
        assert_eq!(tcp_cluster_id("forgejo", 2222), "forgejo-tcp-2222");
    }

    #[test]
    fn only_this_workloads_tcp_clusters_are_found() {
        let content = hashes(&[
            "forgejo",
            "forgejo-tcp-2222",
            "forgejo-tcp-22",
            "forgejo-staging-tcp-2222",
            "forgejo-tcp-notaport",
            "monitoring-tcp-2222",
        ]);
        assert_eq!(
            tcp_clusters_in(&content, "forgejo"),
            vec!["forgejo-tcp-22".to_string(), "forgejo-tcp-2222".to_string()]
        );
    }

    #[test]
    fn tcp_clusters_reported_by_several_workers_are_found_once() {
        let content = ResponseContent {
            content_type: Some(ContentType::WorkerResponses(WorkerResponses {
                map: BTreeMap::from([
                    ("0".to_string(), hashes(&["forgejo-tcp-2222"])),
                    ("1".to_string(), hashes(&["forgejo-tcp-2222"])),
                ]),
            })),
        };
        assert_eq!(tcp_clusters_in(&content, "forgejo"), vec!["forgejo-tcp-2222".to_string()]);
    }

    fn listeners(port: u16, active: bool) -> ResponseContent {
        ResponseContent {
            content_type: Some(ContentType::ListenersList(
                sozu_command_lib::proto::command::ListenersList {
                    tcp_listeners: BTreeMap::from([(
                        format!("0.0.0.0:{}", port),
                        TcpListenerConfig {
                            address: socket_address(Ipv4Addr::UNSPECIFIED, port),
                            active,
                            ..Default::default()
                        },
                    )])
                    .into_iter()
                    .collect(),
                    ..Default::default()
                },
            )),
        }
    }

    #[test]
    fn a_tcp_listener_is_found_by_address() {
        let wanted = socket_address(Ipv4Addr::UNSPECIFIED, 2222);
        assert_eq!(tcp_listener_in(&listeners(2222, true), &wanted), Some(true));
        assert_eq!(tcp_listener_in(&listeners(2222, false), &wanted), Some(false));
        assert_eq!(tcp_listener_in(&listeners(2223, true), &wanted), None);
    }

    #[test]
    fn prune_outcomes_are_counted_separately() {
        assert_eq!(
            Pruned::default().removed().removed().failed(),
            Pruned { removed: 2, failed: 1 }
        );
    }
}
