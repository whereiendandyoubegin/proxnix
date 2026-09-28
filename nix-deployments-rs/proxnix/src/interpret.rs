use crate::api::{GuestOp, Lxc, Qemu};
use crate::context::{BackendId, NixHash, StorePath, Tags, render_managed};
use crate::remote::{Api, ApiError, ApiFault, Presence, Remote};
use crate::sozu::{Proxied, Pruned, Settled};
use crate::types::{AppError, ContainerConfig, Result, VMConfig};
use proxmox_api::client::Client;
use proxmox_api::nodes::node::{lxc, qemu};
use proxnix_core::{
    Artifact, Backend, Detail, DurationMs, Effect, EffectError, Endpoint, Event, GuestEffect, GuestKind, GuestName,
    ManagedTags, Outcome, Ownership, Planned, ProbeEffect, ProxySpec, ResourceChange, RouteEffect, SlotId, Vmid, WorkloadSpec,
};
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::num::NonZeroU64;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct Placed {
    pub config: ContainerConfig,
    pub storage: std::result::Result<proxnix_core::Storage, proxnix_core::StorageFault>,
}

#[derive(Debug, Clone)]
pub enum Declared {
    Vm(VMConfig),
    Container(Box<Placed>),
}

pub struct Routed {
    name: String,
    hostname: String,
    service_address: Option<Ipv4Addr>,
    backend_port: u16,
    tcp_ports: Vec<u16>,
}

impl Routed {
    fn of(name: &GuestName, proxy: &ProxySpec) -> Routed {
        Routed {
            name: name.0.clone(),
            hostname: proxy.hostname.0.clone(),
            service_address: proxy.service_address,
            backend_port: proxy.backend_port.0,
            tcp_ports: proxy.tcp_ports.iter().map(|port| port.0).collect(),
        }
    }
}

impl Proxied for Routed {
    fn backend_port(&self) -> u16 {
        self.backend_port
    }
    fn service_address(&self) -> Option<Ipv4Addr> {
        self.service_address
    }
    fn tcp_ports(&self) -> &[u16] {
        &self.tcp_ports
    }
    fn cluster_id(&self) -> &str {
        &self.name
    }
    fn hostname(&self) -> &str {
        &self.hostname
    }
}

pub trait Routes {
    fn ensure_cluster(&mut self, target: &Routed) -> Result<Settled>;
    fn register_backend(&mut self, target: &Routed, id: &BackendId, ip: Ipv4Addr) -> Result<Settled>;
    fn prune_backends(&mut self, target: &Routed, keep: Ipv4Addr) -> Result<Pruned>;
    fn remove_backend(&mut self, target: &Routed, id: &BackendId, ip: Ipv4Addr) -> Result<()>;
    fn register_tcp_backends(&mut self, target: &Routed, id: &BackendId, ip: Ipv4Addr) -> Result<Settled>;
    fn prune_tcp_backends(&mut self, target: &Routed, keep: Ipv4Addr) -> Result<Pruned>;
    fn remove_tcp_backends(&mut self, target: &Routed, id: &BackendId, ip: Ipv4Addr);
    fn remove_cluster(&mut self, name: &GuestName) -> Result<()>;
    fn remove_tcp_clusters(&mut self, name: &GuestName) -> Result<usize>;
}

impl Routes for crate::sozu::SozuClient {
    fn ensure_cluster(&mut self, target: &Routed) -> Result<Settled> {
        crate::sozu::SozuClient::ensure_cluster(self, target)
    }
    fn register_backend(&mut self, target: &Routed, id: &BackendId, ip: Ipv4Addr) -> Result<Settled> {
        crate::sozu::SozuClient::register_backend(self, target, id, ip)
    }
    fn prune_backends(&mut self, target: &Routed, keep: Ipv4Addr) -> Result<Pruned> {
        crate::sozu::SozuClient::prune_backends(self, target, keep)
    }
    fn remove_backend(&mut self, target: &Routed, id: &BackendId, ip: Ipv4Addr) -> Result<()> {
        crate::sozu::SozuClient::remove_backend(self, target, id, ip)
    }
    fn register_tcp_backends(&mut self, target: &Routed, id: &BackendId, ip: Ipv4Addr) -> Result<Settled> {
        crate::sozu::SozuClient::register_tcp_backends(self, target, id, ip)
    }
    fn prune_tcp_backends(&mut self, target: &Routed, keep: Ipv4Addr) -> Result<Pruned> {
        crate::sozu::SozuClient::prune_tcp_backends(self, target, keep)
    }
    fn remove_tcp_backends(&mut self, target: &Routed, id: &BackendId, ip: Ipv4Addr) {
        crate::sozu::SozuClient::remove_tcp_backends(self, target, id, ip);
    }
    fn remove_cluster(&mut self, name: &GuestName) -> Result<()> {
        crate::sozu::SozuClient::remove_cluster(self, &name.0).map(|_| ())
    }
    fn remove_tcp_clusters(&mut self, name: &GuestName) -> Result<usize> {
        crate::sozu::SozuClient::remove_tcp_clusters(self, &name.0)
    }
}

pub trait Probes {
    fn port_open(&self, address: SocketAddr) -> bool;
    fn guest_check(&self, id: Vmid, kind: GuestKind) -> Result<bool>;
}

pub trait Provision {
    fn create(&self, declared: &Declared, artifact: &StorePath, tags: &Tags, target: SlotId) -> Result<()>;
}

pub struct Interpreter<'a, C, R, P, M> {
    pub api: &'a Api<C>,
    pub routes: R,
    pub probes: P,
    pub provision: M,
    pub declared: &'a BTreeMap<GuestName, Declared>,
}

fn effect_error(error: &AppError) -> EffectError {
    match error {
        AppError::Api(ApiFault::Task { exit: proxnix_core::TaskExit::Failed(detail), .. }) => EffectError::TaskFailed(detail.clone()),
        AppError::Api(ApiFault::TimedOut { after, .. }) => {
            EffectError::TimedOut(DurationMs(u64::try_from(after.as_millis()).unwrap_or(u64::MAX)))
        }
        AppError::ProxmoxApi(_) | AppError::ChannelError(_) | AppError::FileIOError(_) => EffectError::Unreachable(Detail(error.to_string())),
        _ => EffectError::Refused(Detail(error.to_string())),
    }
}

fn settled(result: Result<Settled>) -> Result<Outcome> {
    result.map(|settled| match settled {
        Settled::Changed => Outcome::Done,
        Settled::AlreadyApplied => Outcome::AlreadyApplied,
    })
}

fn store_path(artifact: &Artifact) -> Result<StorePath> {
    StorePath::try_from(format!("/nix/store/{}-{}", artifact.path.hash().as_ref(), artifact.path.name().as_ref()))
}

fn backend_id(name: &GuestName, backend: &Backend) -> Result<BackendId> {
    match backend.endpoint() {
        Endpoint::Primary => Ok(BackendId::new(&name.0, &NixHash::try_from(backend.nix().as_ref())?)),
        Endpoint::Replicas => Err(AppError::SozuError(format!(
            "{} has no replicas cluster yet; refusing to route replicas",
            name.0
        ))),
    }
}

fn qemu_changes(changes: &[ResourceChange]) -> qemu::vmid::config::PutParams {
    changes.iter().fold(qemu::vmid::config::PutParams::default(), |params, change| match change {
        ResourceChange::Memory(memory) => qemu::vmid::config::PutParams { memory: Some(memory.0.to_string()), ..params },
        ResourceChange::Cores(cores) => qemu::vmid::config::PutParams { cores: NonZeroU64::new(u64::from(cores.0)), ..params },
        ResourceChange::Sockets(sockets) => qemu::vmid::config::PutParams { sockets: NonZeroU64::new(u64::from(sockets.0)), ..params },
    })
}

fn lxc_changes(changes: &[ResourceChange]) -> Result<lxc::vmid::config::PutParams> {
    changes.iter().try_fold(lxc::vmid::config::PutParams::default(), |params, change| match change {
        ResourceChange::Memory(memory) => Ok(lxc::vmid::config::PutParams { memory: Some(i128::from(memory.0).try_into()?), ..params }),
        ResourceChange::Cores(cores) => Ok(lxc::vmid::config::PutParams { cores: Some(i128::from(cores.0).try_into()?), ..params }),
        ResourceChange::Sockets(_) => Ok(params),
    })
}

impl<C: Client, R: Routes, P: Probes, M: Provision> Interpreter<'_, C, R, P, M>
where
    C::Error: ApiError,
{
    pub fn execute(&mut self, planned: &[Planned]) -> Vec<Event> {
        planned
            .iter()
            .map(|planned| Event {
                effect: planned.id,
                outcome: self.outcome(&planned.effect).unwrap_or_else(|error| {
                    warn!("[{}] effect failed: {}", planned.workload.0, error);
                    Outcome::Failed(effect_error(&error))
                }),
            })
            .collect()
    }

    fn outcome(&mut self, effect: &Effect) -> Result<Outcome> {
        match effect {
            Effect::Guest(guest) => self.guest(guest),
            Effect::Probe(probe) => self.probe(probe),
            Effect::Route(route) => self.route(route),
        }
    }

    fn apply(&self, kind: GuestKind, qemu: impl FnOnce(Vmid) -> Vec<GuestOp<Qemu>>, lxc: impl FnOnce(Vmid) -> Vec<GuestOp<Lxc>>, id: Vmid) -> Result<Outcome> {
        match kind {
            GuestKind::Qemu => settle_all(self.api, &qemu(id)),
            GuestKind::Lxc => settle_all(self.api, &lxc(id)),
        }
    }

    fn retag(&self, kind: GuestKind, id: Vmid, tags: &ManagedTags) -> Result<Outcome> {
        let rendered = render_managed(tags);
        self.apply(
            kind,
            |id| vec![GuestOp::Set(id, qemu::vmid::config::PutParams { tags: Some(rendered.clone()), ..Default::default() })],
            |id| vec![GuestOp::Set(id, lxc::vmid::config::PutParams { tags: Some(rendered.clone()), ..Default::default() })],
            id,
        )
    }

    fn create(&self, target: Vmid, artifact: &Artifact, spec: &WorkloadSpec, fresh: &proxnix_core::Fresh) -> Result<Outcome> {
        let declared = self
            .declared
            .get(&spec.name)
            .ok_or_else(|| AppError::CmdError(format!("{} is not declared in this run's config", spec.name.0)))?;
        let slot = match fresh.slot() {
            proxnix_core::Slot::Blue => SlotId::Blue(target),
            proxnix_core::Slot::Green => SlotId::Green(target),
        };
        let created = store_path(artifact)
            .and_then(|path| Tags::fresh(fresh).map(|tags| (path, tags)))
            .and_then(|(path, tags)| self.provision.create(declared, &path, &tags, slot));
        match created {
            Ok(()) => Ok(Outcome::Done),
            Err(error) => {
                let cleaned = match spec.kind() {
                    GuestKind::Qemu => self.clean::<Qemu>(target, fresh),
                    GuestKind::Lxc => self.clean::<Lxc>(target, fresh),
                };
                match cleaned {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(AppError::CmdError(format!(
                        "create of {} failed ({error}) and the half-created guest could not be removed ({cleanup})",
                        target.get()
                    ))),
                }
            }
        }
    }

    fn clean<K: Remote>(&self, target: Vmid, fresh: &proxnix_core::Fresh) -> Result<()> {
        match self.api.presence::<K>(target)? {
            Presence::Absent => Ok(()),
            Presence::Present { ownership: Ownership::Managed(tags), .. } if made_by(&tags, fresh) => {
                warn!("create of {} failed part way, removing what it left behind", target.get());
                self.api.apply_all(&GuestOp::<K>::reclaim(target))
            }
            Presence::Present { ownership, .. } => Err(AppError::CmdError(format!(
                "{} is occupied by a guest this create did not make ({ownership:?}); leaving it untouched",
                target.get()
            ))),
        }
    }

    fn guest(&self, effect: &GuestEffect) -> Result<Outcome> {
        match effect {
            GuestEffect::Create { target, artifact, spec, fresh } => self.create(target.id(), artifact, spec, fresh),
            GuestEffect::Start(member) => self.apply(member.guest().kind(), |id| vec![GuestOp::start(id)], |id| vec![GuestOp::start(id)], member.id()),
            GuestEffect::Stop(member) => self.apply(member.guest().kind(), |id| vec![GuestOp::stop(id)], |id| vec![GuestOp::stop(id)], member.id()),
            GuestEffect::Record { guest, address } => {
                self.retag(guest.guest().kind(), guest.id(), &ManagedTags { service_ip: Some(*address), ..guest.tags().clone() })
            }
            GuestEffect::Role { guest, role } => {
                self.retag(guest.guest().kind(), guest.id(), &ManagedTags { role: Some(role.clone()), ..guest.tags().clone() })
            }
            GuestEffect::Commit(promotion) => {
                let guest = promotion.guest();
                self.retag(
                    guest.guest().kind(),
                    guest.id(),
                    &ManagedTags { generation: Some(promotion.generation()), pending: false, ..guest.tags().clone() },
                )
            }
            GuestEffect::Update { guest, changes } => match guest.guest().kind() {
                GuestKind::Qemu => settle_all(self.api, &[GuestOp::<Qemu>::Set(guest.id(), qemu_changes(changes))]),
                GuestKind::Lxc => settle_all(self.api, &[GuestOp::<Lxc>::Set(guest.id(), lxc_changes(changes)?)]),
            },
            GuestEffect::Undo(provisioned) => self.apply(
                provisioned.kind(),
                |id| GuestOp::reclaim(id).into(),
                |id| GuestOp::reclaim(id).into(),
                provisioned.id(),
            ),
            GuestEffect::Reclaim(doomed) => {
                self.apply(doomed.guest().kind(), |id| GuestOp::reclaim(id).into(), |id| GuestOp::reclaim(id).into(), doomed.id())
            }
            GuestEffect::Retire(doomed) => {
                self.apply(doomed.guest().kind(), |id| GuestOp::retire(id).into(), |id| GuestOp::retire(id).into(), doomed.id())
            }
        }
    }

    fn probe(&self, probe: &ProbeEffect) -> Result<Outcome> {
        let member = probe.guest();
        let (id, kind) = (member.id(), member.guest().kind());
        match probe {
            ProbeEffect::ReadAddress(_) => {
                let address = match kind {
                    GuestKind::Qemu => self.api.address::<Qemu>(id)?,
                    GuestKind::Lxc => self.api.address::<Lxc>(id)?,
                };
                Ok(address.map_or_else(
                    || Outcome::Failed(EffectError::Unreachable(Detail(format!("{} has no address yet", id.get())))),
                    Outcome::Address,
                ))
            }
            ProbeEffect::PortOpen { address, port, .. } => Ok(if self.probes.port_open(SocketAddr::from((*address, port.0))) {
                Outcome::Done
            } else {
                Outcome::Failed(EffectError::Unreachable(Detail(format!("{address}:{} is not accepting connections", port.0))))
            }),
            ProbeEffect::GuestCheck(_) => Ok(if self.probes.guest_check(id, kind)? {
                Outcome::Done
            } else {
                Outcome::Failed(EffectError::TaskFailed(Detail(format!("{}'s guest health check did not pass", id.get()))))
            }),
        }
    }

    fn route(&mut self, route: &RouteEffect) -> Result<Outcome> {
        match route {
            RouteEffect::Point { name, proxy, to, from } => self.point(name, proxy, to, from.as_ref()),
            RouteEffect::Restore { name, proxy, to } => self.restore(name, proxy, to),
            RouteEffect::RemoveCluster(name) => {
                if let Err(error) = self.routes.remove_cluster(name) {
                    info!("[{}] sozu had no cluster to remove ({}), continuing with teardown", name.0, error);
                }
                match self.routes.remove_tcp_clusters(name) {
                    Ok(0) => {}
                    Ok(count) => info!("[{}] removed {} tcp clusters", name.0, count),
                    Err(error) => warn!("[{}] could not remove tcp clusters ({}), continuing with teardown", name.0, error),
                }
                Ok(Outcome::Done)
            }
        }
    }

    fn point(&mut self, name: &GuestName, proxy: &ProxySpec, to: &Backend, from: Option<&Backend>) -> Result<Outcome> {
        let target = Routed::of(name, proxy);
        let incoming = backend_id(name, to)?;
        let address = to.address();
        if !target.tcp_ports.is_empty() {
            self.routes.register_tcp_backends(&target, &incoming, address)?;
        }
        if let Err(error) = self.cut_over(&target, &incoming, address, from) {
            self.routes.remove_tcp_backends(&target, &incoming, address);
            return Err(error);
        }
        report_prune(name, "tcp backends", self.routes.prune_tcp_backends(&target, address));
        Ok(Outcome::Done)
    }

    fn cut_over(&mut self, target: &Routed, incoming: &BackendId, address: Ipv4Addr, from: Option<&Backend>) -> Result<()> {
        if target.service_address.is_none() {
            return Ok(());
        }
        self.routes.ensure_cluster(target)?;
        self.routes.register_backend(target, incoming, address)?;
        report_prune(&GuestName(target.name.clone()), "backends", self.routes.prune_backends(target, address));
        match from {
            None => Ok(()),
            Some(old) => {
                let outgoing = backend_id(&GuestName(target.name.clone()), old)?;
                match self.routes.remove_backend(target, &outgoing, old.address()) {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        warn!("failed to deregister old backend {}, rolling back new registration: {}", outgoing, error);
                        if let Err(undo) = self.routes.remove_backend(target, incoming, address) {
                            warn!("could not deregister new backend {}: {}", incoming, undo);
                        }
                        Err(error)
                    }
                }
            }
        }
    }

    fn restore(&mut self, name: &GuestName, proxy: &ProxySpec, to: &Backend) -> Result<Outcome> {
        let target = Routed::of(name, proxy);
        let id = backend_id(name, to)?;
        let ip = to.address();
        if !target.tcp_ports.is_empty() {
            self.routes.register_tcp_backends(&target, &id, ip)?;
            report_prune(name, "tcp backends", self.routes.prune_tcp_backends(&target, ip));
        }
        if target.service_address.is_some() {
            self.routes.ensure_cluster(&target)?;
            self.routes.register_backend(&target, &id, ip)?;
            report_prune(name, "backends", self.routes.prune_backends(&target, ip));
        }
        Ok(Outcome::Done)
    }
}

fn made_by(tags: &ManagedTags, fresh: &proxnix_core::Fresh) -> bool {
    tags.nix == *fresh.nix() && tags.commit == *fresh.commit() && tags.slot == fresh.slot() && tags.generation.is_none()
}

fn settle_all<C: Client, K: Remote>(api: &Api<C>, ops: &[GuestOp<K>]) -> Result<Outcome>
where
    C::Error: ApiError,
{
    ops.iter().try_fold(Outcome::AlreadyApplied, |outcome, op| {
        settled(api.apply(op)).map(|next| match (outcome, next) {
            (Outcome::AlreadyApplied, Outcome::AlreadyApplied) => Outcome::AlreadyApplied,
            _ => Outcome::Done,
        })
    })
}

fn report_prune(name: &GuestName, what: &str, pruned: Result<Pruned>) {
    match pruned {
        Ok(Pruned { removed: 0, failed: 0 }) => {}
        Ok(pruned) => info!("[{}] dropped {} stale {} ({} could not be dropped)", name.0, pruned.removed, what, pruned.failed),
        Err(error) => warn!("[{}] could not check for stale {}, traffic may still reach a retired instance: {}", name.0, what, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::fake::Proxmox;
    use proxmox_api::client::Method;
    use proxnix_core::{
        Audited, Cohort, Cores, Cutover, DiskGib, EffectId, Grant, GuestStatus, Hostname, ImageType, KindFacts, KindSpec,
        Member, MemoryMb, Observation, Ownership, Permissions, Port, Privilege, Purity, RawTags, Resources, Sighting, SlotPair,
        Timeouts,
    };
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::rc::Rc;
    use std::time::Duration;

    const OLD: &str = "78s0iadvjz6s48aqvx4rw78lwrzkjzlw";
    const NEW: &str = "i3d00236fdkfw1v9cmasajkjhzl8zi5j";
    const COMMIT: &str = "66d0ba6b605de2703e0fb7bbf58b922d5b36597e";

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        RegisterTcp(String, Ipv4Addr),
        Ensure,
        Register(String, Ipv4Addr),
        Prune(Ipv4Addr),
        Remove(String, Ipv4Addr),
        PruneTcp(Ipv4Addr),
        RemoveTcp(String, Ipv4Addr),
        RemoveCluster,
        RemoveTcpClusters,
    }

    #[derive(Clone, Default)]
    struct Sozu {
        calls: Rc<RefCell<Vec<Call>>>,
        refuse: BTreeSet<&'static str>,
    }

    impl Sozu {
        fn record(&self, call: Call, name: &'static str) -> Result<()> {
            self.calls.borrow_mut().push(call);
            if self.refuse.contains(name) { Err(AppError::SozuError(String::from(name))) } else { Ok(()) }
        }
    }

    impl Routes for Sozu {
        fn ensure_cluster(&mut self, _: &Routed) -> Result<Settled> {
            self.record(Call::Ensure, "ensure").map(|()| Settled::Changed)
        }
        fn register_backend(&mut self, _: &Routed, id: &BackendId, ip: Ipv4Addr) -> Result<Settled> {
            self.record(Call::Register(id.as_str().to_string(), ip), "register").map(|()| Settled::Changed)
        }
        fn prune_backends(&mut self, _: &Routed, keep: Ipv4Addr) -> Result<Pruned> {
            self.record(Call::Prune(keep), "prune").map(|()| Pruned::default())
        }
        fn remove_backend(&mut self, _: &Routed, id: &BackendId, ip: Ipv4Addr) -> Result<()> {
            self.record(Call::Remove(id.as_str().to_string(), ip), "remove")
        }
        fn register_tcp_backends(&mut self, _: &Routed, id: &BackendId, ip: Ipv4Addr) -> Result<Settled> {
            self.record(Call::RegisterTcp(id.as_str().to_string(), ip), "register_tcp").map(|()| Settled::Changed)
        }
        fn prune_tcp_backends(&mut self, _: &Routed, keep: Ipv4Addr) -> Result<Pruned> {
            self.record(Call::PruneTcp(keep), "prune_tcp").map(|()| Pruned::default())
        }
        fn remove_tcp_backends(&mut self, _: &Routed, id: &BackendId, ip: Ipv4Addr) {
            let _ = self.record(Call::RemoveTcp(id.as_str().to_string(), ip), "remove_tcp");
        }
        fn remove_cluster(&mut self, _: &GuestName) -> Result<()> {
            self.record(Call::RemoveCluster, "remove_cluster")
        }
        fn remove_tcp_clusters(&mut self, _: &GuestName) -> Result<usize> {
            self.record(Call::RemoveTcpClusters, "remove_tcp_clusters").map(|()| 0)
        }
    }

    struct Healthy;

    impl Probes for Healthy {
        fn port_open(&self, _: SocketAddr) -> bool {
            true
        }
        fn guest_check(&self, _: Vmid, _: GuestKind) -> Result<bool> {
            Ok(true)
        }
    }

    #[derive(Default)]
    struct Provisioner {
        fail: bool,
        calls: RefCell<Vec<(GuestName, SlotId, String)>>,
    }

    impl Provision for Provisioner {
        fn create(&self, declared: &Declared, _: &StorePath, tags: &Tags, target: SlotId) -> Result<()> {
            let name = match declared {
                Declared::Vm(config) => config.name.clone(),
                Declared::Container(placed) => placed.config.name.clone(),
            };
            self.calls.borrow_mut().push((GuestName(name), target, tags.render()));
            if self.fail { Err(AppError::CmdError(String::from("pct create exited 255"))) } else { Ok(()) }
        }
    }

    fn spec(tcp: bool) -> WorkloadSpec {
        WorkloadSpec {
            name: GuestName(String::from("forgejo")),
            slots: SlotPair::new(Vmid::new(844), Vmid::new(944)).unwrap(),
            image: ImageType(String::from("build-lxc-forgejo")),
            resources: Resources { memory: MemoryMb(2048), disk: DiskGib(20), cores: Cores(2) },
            cutover: Cutover::Overlap,
            purity: Purity::Pure,
            proxy: ProxySpec {
                hostname: Hostname(String::from("git.thesta.rs")),
                service_address: Some(Ipv4Addr::new(192, 168, 1, 44)),
                backend_port: Port(3000),
                tcp_ports: if tcp { vec![Port(22)] } else { vec![] },
                bridge: proxnix_core::BridgeName(String::from("vmbr0")),
            },
            timeouts: Timeouts { dhcp: DurationMs(1), health_check: DurationMs(1) },
            kind: KindSpec::Lxc { privilege: Privilege::Unprivileged, mounts: vec![] },
        }
    }

    fn members() -> (Member, Member) {
        let sighting = |id: u32, tags: String| {
            Sighting::Settled(proxnix_core::Settled {
            id: Vmid::new(id),
            name: GuestName(String::from("forgejo")),
            status: GuestStatus::Running,
            tags: RawTags::from(tags),
            resources: Resources { memory: MemoryMb(2048), disk: DiskGib(20), cores: Cores(2) },
            facts: KindFacts::Lxc { privilege: Privilege::Unprivileged, mounts: vec![] },
            })
        };
        let observed = Observation::new(
            Audited::try_from(Permissions { vm_audit: Grant::Granted }).unwrap(),
            vec![
                sighting(844, format!("proxnix;nix-{OLD};commit-{COMMIT};slot-blue;ip-10.0.0.44;gen-1")),
                sighting(944, format!("proxnix;nix-{NEW};commit-{COMMIT};slot-green;ip-10.0.0.94;gen-2")),
            ],
        );
        let cohort = Cohort::gather(&observed, &spec(false)).unwrap();
        let find = |id: u32| cohort.members().iter().find(|member| member.id() == Vmid::new(id)).unwrap().clone();
        (find(944), find(844))
    }

    fn id(name: &str, nix: &str) -> String {
        BackendId::new(name, &NixHash::try_from(nix).unwrap()).as_str().to_string()
    }

    fn run(effects: Vec<Effect>, sozu: &Sozu, fake: &Proxmox, provision: Provisioner, declared: &BTreeMap<GuestName, Declared>) -> Vec<Event> {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let api = Api::new(fake.clone(), String::from("pve01"), runtime.handle().clone(), Duration::ZERO, Duration::from_secs(5));
        let mut interpreter = Interpreter { api: &api, routes: sozu.clone(), probes: Healthy, provision, declared };
        let planned: Vec<Planned> = effects
            .into_iter()
            .enumerate()
            .map(|(index, effect)| Planned { id: EffectId(index as u64), workload: GuestName(String::from("forgejo")), effect })
            .collect();
        interpreter.execute(&planned)
    }

    fn point(tcp: bool) -> Effect {
        let (serving, loser) = members();
        Effect::Route(RouteEffect::Point {
            name: GuestName(String::from("forgejo")),
            proxy: spec(tcp).proxy,
            to: Backend::of(&serving, Endpoint::Primary).unwrap(),
            from: Backend::of(&loser, Endpoint::Primary),
        })
    }

    #[test]
    fn a_cutover_makes_exactly_the_sozu_calls_the_old_deploy_made() {
        let sozu = Sozu::default();
        let events = run(vec![point(true)], &sozu, &Proxmox::default(), Provisioner::default(), &BTreeMap::new());
        assert_eq!(events[0].outcome, Outcome::Done);
        let (new, old) = (Ipv4Addr::new(10, 0, 0, 94), Ipv4Addr::new(10, 0, 0, 44));
        assert_eq!(
            *sozu.calls.borrow(),
            vec![
                Call::RegisterTcp(id("forgejo", NEW), new),
                Call::Ensure,
                Call::Register(id("forgejo", NEW), new),
                Call::Prune(new),
                Call::Remove(id("forgejo", OLD), old),
                Call::PruneTcp(new),
            ]
        );
    }

    #[test]
    fn a_failed_deregistration_rolls_the_new_backend_back_like_the_old_deploy() {
        let sozu = Sozu { refuse: ["remove"].into(), ..Sozu::default() };
        let events = run(vec![point(true)], &sozu, &Proxmox::default(), Provisioner::default(), &BTreeMap::new());
        assert!(matches!(events[0].outcome, Outcome::Failed(EffectError::Refused(_))));
        let (new, old) = (Ipv4Addr::new(10, 0, 0, 94), Ipv4Addr::new(10, 0, 0, 44));
        assert_eq!(
            *sozu.calls.borrow(),
            vec![
                Call::RegisterTcp(id("forgejo", NEW), new),
                Call::Ensure,
                Call::Register(id("forgejo", NEW), new),
                Call::Prune(new),
                Call::Remove(id("forgejo", OLD), old),
                Call::Remove(id("forgejo", NEW), new),
                Call::RemoveTcp(id("forgejo", NEW), new),
            ]
        );
    }

    #[test]
    fn a_restore_makes_exactly_the_sozu_calls_the_periodic_loop_made() {
        let (serving, _) = members();
        let restore = Effect::Route(RouteEffect::Restore {
            name: GuestName(String::from("forgejo")),
            proxy: spec(true).proxy,
            to: Backend::of(&serving, Endpoint::Primary).unwrap(),
        });
        let sozu = Sozu::default();
        run(vec![restore], &sozu, &Proxmox::default(), Provisioner::default(), &BTreeMap::new());
        let new = Ipv4Addr::new(10, 0, 0, 94);
        assert_eq!(
            *sozu.calls.borrow(),
            vec![Call::RegisterTcp(id("forgejo", NEW), new), Call::PruneTcp(new), Call::Ensure, Call::Register(id("forgejo", NEW), new), Call::Prune(new)]
        );
    }

    #[test]
    fn tearing_down_an_orphan_continues_past_a_missing_cluster() {
        let sozu = Sozu { refuse: ["remove_cluster"].into(), ..Sozu::default() };
        let events = run(
            vec![Effect::Route(RouteEffect::RemoveCluster(GuestName(String::from("forgejo"))))],
            &sozu,
            &Proxmox::default(),
            Provisioner::default(),
            &BTreeMap::new(),
        );
        assert_eq!(events[0].outcome, Outcome::Done);
        assert_eq!(*sozu.calls.borrow(), vec![Call::RemoveCluster, Call::RemoveTcpClusters]);
    }

    #[test]
    fn a_replicas_route_is_refused_until_a_replicas_cluster_exists() {
        let (serving, _) = members();
        let replicas = Effect::Route(RouteEffect::Restore {
            name: GuestName(String::from("forgejo")),
            proxy: spec(false).proxy,
            to: Backend::of(&serving, Endpoint::Replicas).unwrap(),
        });
        let sozu = Sozu::default();
        let events = run(vec![replicas], &sozu, &Proxmox::default(), Provisioner::default(), &BTreeMap::new());
        assert!(matches!(events[0].outcome, Outcome::Failed(EffectError::Refused(_))));
        assert!(sozu.calls.borrow().is_empty());
    }

    #[test]
    fn every_planned_effect_gets_exactly_one_event_in_order() {
        let (serving, _) = members();
        let sozu = Sozu::default();
        let events = run(
            vec![point(false), Effect::Probe(ProbeEffect::GuestCheck(serving.clone())), point(false)],
            &sozu,
            &Proxmox::default(),
            Provisioner::default(),
            &BTreeMap::new(),
        );
        assert_eq!(events.iter().map(|event| event.effect).collect::<Vec<_>>(), vec![EffectId(0), EffectId(1), EffectId(2)]);
    }

    #[test]
    fn recording_an_address_rewrites_the_tags_the_core_will_read_back() {
        let (serving, _) = members();
        let fake = Proxmox::replying(vec![(Method::Put, "/nodes/pve01/lxc/944/config", Ok("null"))]);
        let events = run(
            vec![Effect::Guest(GuestEffect::Record { guest: serving.clone(), address: Ipv4Addr::new(10, 0, 0, 99) })],
            &Sozu::default(),
            &fake,
            Provisioner::default(),
            &BTreeMap::new(),
        );
        assert_eq!(events[0].outcome, Outcome::Done);
        let written = fake.requests()[0].body.clone().unwrap()["tags"].as_str().unwrap().to_string();
        assert_eq!(
            Ownership::from(&RawTags::from(written)),
            Ownership::Managed(ManagedTags { service_ip: Some(Ipv4Addr::new(10, 0, 0, 99)), ..serving.tags().clone() })
        );
    }

    #[test]
    fn rendered_tags_round_trip_through_the_core_parser() {
        let (serving, _) = members();
        let variants = [
            serving.tags().clone(),
            ManagedTags { service_ip: None, generation: None, role: None, ..serving.tags().clone() },
            ManagedTags { service_ip: None, generation: None, pending: true, ..serving.tags().clone() },
            ManagedTags { role: Some("reader".parse().unwrap()), ..serving.tags().clone() },
        ];
        for tags in variants {
            assert_eq!(Ownership::from(&RawTags::from(render_managed(&tags))), Ownership::Managed(tags));
        }
    }

    #[test]
    fn a_create_for_a_workload_this_run_did_not_declare_is_refused_without_touching_the_host() {
        let observed = Observation::new(Audited::try_from(Permissions { vm_audit: Grant::Granted }).unwrap(), vec![]);
        let vacant = match observed.slot(Vmid::new(944)) {
            proxnix_core::SlotState::Vacant(vacant) => vacant,
            proxnix_core::SlotState::Occupied(_) => unreachable!(),
        };
        let artifact = Artifact { path: format!("/nix/store/{NEW}-forgejo").parse().unwrap() };
        let push = proxnix_core::Push::new(COMMIT.parse().unwrap());
        let fresh = proxnix_core::Fresh::new(&push, &artifact, &spec(false), vacant, None).unwrap();
        let create = Effect::Guest(GuestEffect::Create { target: vacant, artifact, spec: Box::new(spec(false)), fresh });
        let fake = Proxmox::default();
        let events = run(vec![create], &Sozu::default(), &fake, Provisioner::default(), &BTreeMap::new());
        assert!(matches!(events[0].outcome, Outcome::Failed(EffectError::Refused(_))));
        assert!(fake.requests().is_empty());
    }

    #[test]
    fn in_place_resource_changes_send_the_argv_the_old_update_used() {
        let vm = GuestOp::<Qemu>::Set(
            Vmid::new(101),
            qemu_changes(&[
                ResourceChange::Memory(proxnix_core::MemoryMb(2048)),
                ResourceChange::Cores(proxnix_core::Cores(4)),
                ResourceChange::Sockets(proxnix_core::Sockets(1)),
            ]),
        );
        assert_eq!(vm.invocation().unwrap().to_string(), "qm set 101 --cores 4 --memory 2048 --sockets 1");
        let container = GuestOp::<Lxc>::Set(
            Vmid::new(200),
            lxc_changes(&[ResourceChange::Memory(proxnix_core::MemoryMb(512)), ResourceChange::Cores(proxnix_core::Cores(2))]).unwrap(),
        );
        assert_eq!(container.invocation().unwrap().to_string(), "pct set 200 --cores 2 --memory 512");
    }

    fn failing_create(listed: &str, then: Vec<(Method, &str, std::result::Result<&str, &str>)>) -> (Vec<Event>, Proxmox) {
        let observed = Observation::new(Audited::try_from(Permissions { vm_audit: Grant::Granted }).unwrap(), vec![]);
        let vacant = match observed.slot(Vmid::new(944)) {
            proxnix_core::SlotState::Vacant(vacant) => vacant,
            proxnix_core::SlotState::Occupied(_) => unreachable!(),
        };
        let artifact = Artifact { path: format!("/nix/store/{NEW}-forgejo").parse().unwrap() };
        let push = proxnix_core::Push::new(COMMIT.parse().unwrap());
        let fresh = proxnix_core::Fresh::new(&push, &artifact, &spec(false), vacant, None).unwrap();
        let create = Effect::Guest(GuestEffect::Create { target: vacant, artifact, spec: Box::new(spec(false)), fresh });
        let config: crate::types::ContainerConfig = serde_json::from_value(serde_json::json!({
            "name": "forgejo", "hostname": "forgejo", "dhcp_timeout_seconds": 1, "health_check_timeout_seconds": 1,
            "blue_id": 844, "green_id": 944, "image_type": "build-lxc-forgejo", "cores": 2, "memory_mb": 2048,
            "storage_location": "ZFS", "disk_gb": 20, "protected": false, "impure": false
        }))
        .unwrap();
        let declared = BTreeMap::from([(
            GuestName(String::from("forgejo")),
            Declared::Container(Box::new(Placed { config, storage: Err(proxnix_core::StorageFault::NoLayout) })),
        )]);
        let fake = Proxmox::replying([(Method::Get, "/nodes/pve01/lxc", Ok(listed))].into_iter().chain(then).collect());
        let provision = Provisioner { fail: true, ..Provisioner::default() };
        (run(vec![create], &Sozu::default(), &fake, provision, &declared), fake)
    }

    #[test]
    fn a_create_that_fails_part_way_removes_only_the_guest_it_made() {
        let ours = format!(r#"[{{"vmid": 944, "name": "forgejo", "status": "stopped", "tags": "proxnix;nix-{NEW};commit-{COMMIT};slot-green"}}]"#);
        let upid = serde_json::Value::from("UPID:pve01:000EAA5B:5CAA1660:6AB95076:vzstop:944:root@pam:").to_string();
        let done = include_str!("../fixtures/api/tasks/UPID_pve01_000EAA5B_5CAA1660_6AB95076_vzstop_946_root@pam_/status.json");
        let task = "/nodes/pve01/tasks/UPID:pve01:000EAA5B:5CAA1660:6AB95076:vzstop:944:root@pam:/status";
        let (events, fake) = failing_create(
            &ours,
            vec![
                (Method::Put, "/nodes/pve01/lxc/944/config", Ok("null")),
                (Method::Post, "/nodes/pve01/lxc/944/status/stop", Ok(upid.as_str())),
                (Method::Get, task, Ok(done)),
                (Method::Delete, "/nodes/pve01/lxc/944", Ok(upid.as_str())),
                (Method::Get, task, Ok(done)),
            ],
        );
        assert!(matches!(events[0].outcome, Outcome::Failed(EffectError::Refused(_))));
        assert!(fake.exhausted(), "the half-created guest was not reclaimed: {:?}", fake.requests());
    }

    #[test]
    fn a_create_that_lost_its_slot_to_someone_else_never_touches_their_guest() {
        let foreign = [
            String::from(r#"[{"vmid": 944, "name": "somebody-elses", "status": "running"}]"#),
            format!(r#"[{{"vmid": 944, "name": "forgejo", "status": "running", "tags": "proxnix;nix-{OLD};commit-{COMMIT};slot-green"}}]"#),
            format!(r#"[{{"vmid": 944, "name": "forgejo", "status": "running", "tags": "proxnix;nix-{NEW};commit-{COMMIT};slot-green;gen-4"}}]"#),
        ];
        for listed in foreign {
            let (events, fake) = failing_create(&listed, vec![]);
            assert!(matches!(events[0].outcome, Outcome::Failed(EffectError::Refused(_))));
            assert_eq!(fake.requests().len(), 1, "only the listing may be read: {:?}", fake.requests());
            assert_eq!(fake.requests()[0].method, Method::Get);
        }
    }
}
