use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::num::NonZeroU64;
use std::time::{Duration, Instant};

use proxmox_api::nodes::node::{lxc, qemu};
use proxnix_core::{GuestStatus, Slot, SlotId, Vmid, Workload};
use rayon::prelude::*;
use tracing::{debug, info, warn};

use crate::{
    api::{Cli, Execute, GuestOp, Kind, Lxc, Qemu},
    context::{BackendId, BackendPool, ImageStore, ImageType, NixHash, ReconcileContext, StorePath, Tags},
    materialise::Materialise,
    probe::Probe,
    pve::Pve,
    sozu::{Proxied, Pruned, Settled, SozuClient},
    state::{self, Deployed, Observe, is_proxnix_managed},
    types::{
        AppConfig, AppError, ContainerConfig, FieldChange, Outcome, OutcomeKind, Result, SkipReason, Timing,
        VMConfig,
    },
};

static PROVISION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub type DeployedOf<T> = Deployed<<<T as Deployments>::Kind as Observe>::Extra>;

enum Phase {
    Initial,
    Provisioned { target: SlotId, new_backend_id: BackendId },
    Healthy { new_backend_id: BackendId, new_ip: Ipv4Addr },
    BackendRegistered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetState {
    Vacant,
    Managed,
    Unmanaged,
}

struct Retiring {
    slot_id: SlotId,
    backend_id: Option<BackendId>,
    ip: Option<Ipv4Addr>,
}

pub struct DeployContext<'a, T: Deployments> {
    config: &'a T,
    new_slot: Slot,
    retiring: Option<Retiring>,
    sozu: SozuClient,
    artifact: StorePath,
    tags: Tags,
    image_store: ImageStore<'a>,
    backend_pool: Option<&'a BackendPool>,
    pve: &'a Pve,
    settings: &'a AppConfig,
    phase: Phase,
}

impl<'a, T: Deployments> DeployContext<'a, T> {
    fn from_create(config: &'a T, artifact: StorePath, ctx: &ReconcileContext<'a>) -> Result<Self> {
        let new_slot = Slot::Blue;
        let tags = Tags::new(nix_hash_of(&artifact)?, ctx.commit_hash.as_str(), new_slot);
        Self::connect(config, new_slot, None, artifact, tags, ctx)
    }

    fn from_rebuild(
        config: &'a T,
        deployed: &DeployedOf<T>,
        artifact: StorePath,
        ctx: &ReconcileContext<'a>,
    ) -> Result<Self> {
        let new_slot = deployed.active_slot.switch_slot();
        let tags = Tags::new(nix_hash_of(&artifact)?, ctx.commit_hash.as_str(), new_slot);
        let retiring = Retiring {
            slot_id: match deployed.active_slot {
                Slot::Blue => SlotId::Blue(deployed.id),
                Slot::Green => SlotId::Green(deployed.id),
            },
            backend_id: deployed
                .nix_hash
                .as_ref()
                .map(|h| BackendId::new(config.name(), h)),
            ip: deployed.service_ip.or_else(|| {
                let read = <T::Kind as Observe>::address(ctx.pve, deployed.id).ok();
                if read.is_none() {
                    warn!(
                        "{} has no recorded service ip and its address could not be read; its backend cannot be deregistered by address",
                        config.name()
                    );
                }
                read
            }),
        };
        Self::connect(config, new_slot, Some(retiring), artifact, tags, ctx)
    }

    fn connect(
        config: &'a T,
        new_slot: Slot,
        retiring: Option<Retiring>,
        artifact: StorePath,
        tags: Tags,
        ctx: &ReconcileContext<'a>,
    ) -> Result<Self> {
        Ok(Self {
            config,
            new_slot,
            retiring,
            sozu: SozuClient::connect(ctx.settings)?,
            artifact,
            tags,
            image_store: ctx.image_store,
            backend_pool: ctx.backend_pool,
            pve: ctx.pve,
            settings: ctx.settings,
            phase: Phase::Initial,
        })
    }

    fn provision_inactive(self) -> Result<Self> {
        let target = self.config.id_for_slot(self.new_slot);
        let new_backend_id = BackendId::new(self.config.name(), &self.tags.nix_hash);
        prepare_target::<T>(self.pve, self.config.name(), target)?;
        info!(
            "[{}] provisioning {:?} as {} (inactive, not started)",
            self.config.name(),
            self.new_slot,
            target.inner()
        );
        {
            let _storage = PROVISION_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            self.config.provision_inactive(
                &self.artifact,
                &self.tags,
                self.image_store,
                target,
            )?;
        }
        Ok(Self {
            phase: Phase::Provisioned { target, new_backend_id },
            ..self
        })
    }

    fn start_and_check(self) -> Result<Self> {
        let Phase::Provisioned { target, new_backend_id } = &self.phase else {
            unreachable!("start_and_check called outside Provisioned phase")
        };
        let (target, new_backend_id) = (*target, new_backend_id.clone());
        let config = self.config;
        info!("[{}] starting {}", config.name(), target.inner());
        Cli.run(&GuestOp::<T::Kind>::start(target.inner()))?;
        let new_ip = await_ip::<T>(self.pve, config, target.inner())?;
        info!("[{}] {} came up at {}", config.name(), target.inner(), new_ip);
        match self.backend_pool {
            Some(pool) if !pool.contains(new_ip) => warn!(
                "{} came up on {}, which is outside the declared backend pool {}-{}; the pool declaration and the dhcp scope disagree",
                config.name(), new_ip, pool.start, pool.end
            ),
            _ => {}
        }
        let tags = self.tags.with_service_ip(new_ip);
        Cli.run(&GuestOp::<T::Kind>::tags(target.inner(), &tags))?;
        config.post_check()?;
        let addr = SocketAddr::from((new_ip, config.backend_port()));
        info!("[{}] health checking {}", config.name(), addr);
        config.health_check(addr)?;
        info!("[{}] port {} is open, running the guest health check", config.name(), addr);
        <T::Kind as Probe>::guest_check(
            config.name(),
            target.inner(),
            config.health_check_timeout(),
            &self.settings.guest_check,
            &self.settings.timings_ms,
        )?;
        info!("[{}] healthy at {}", config.name(), addr);
        Ok(Self {
            tags,
            phase: Phase::Healthy { new_backend_id, new_ip },
            ..self
        })
    }

    fn register_and_switch(mut self) -> Result<Self> {
        let Phase::Healthy { new_backend_id, new_ip } = &self.phase else {
            unreachable!("register_and_switch called outside Healthy phase")
        };
        let (new_backend_id, new_ip) = (new_backend_id.clone(), *new_ip);
        let config = self.config;
        if !config.tcp_ports().is_empty() {
            info!(
                "[{}] forwarding tcp ports {:?} to {}",
                config.name(),
                config.tcp_ports(),
                new_ip
            );
            self.sozu.register_tcp_backends(config, &new_backend_id, new_ip)?;
        }
        if let Err(e) = cut_over(&mut self.sozu, config, &new_backend_id, new_ip, self.retiring.as_ref()) {
            self.sozu.remove_tcp_backends(config, &new_backend_id, new_ip);
            return Err(e);
        }
        match self.sozu.prune_tcp_backends(config, new_ip) {
            Ok(Pruned { removed: 0, failed: 0 }) => {}
            Ok(pruned) => info!(
                "[{}] dropped {} stale tcp backends ({} could not be dropped)",
                config.name(), pruned.removed, pruned.failed
            ),
            Err(e) => warn!(
                "[{}] could not check for stale tcp backends, tcp traffic may still reach a retired instance: {}",
                config.name(), e
            ),
        }
        Ok(Self {
            phase: Phase::BackendRegistered,
            ..self
        })
    }

    fn maybe_destroy_old(self) -> Result<()> {
        let Phase::BackendRegistered = self.phase else {
            unreachable!("maybe_destroy_old called outside BackendRegistered phase")
        };
        self.retiring.map_or(Ok(()), |old| {
            info!(
                "[{}] traffic is on the new instance, retiring {}",
                self.config.name(),
                old.slot_id.inner()
            );
            Cli.run_all(&GuestOp::<T::Kind>::retire(old.slot_id.inner()))
        })
    }

    pub fn run(self) -> Result<()> {
        let pve = self.pve;
        self.config.pre_check()?;
        let target = self.config.id_for_slot(self.new_slot);
        let new_hash = self.tags.nix_hash.clone();
        let name = self.config.name().to_string();
        info!(
            "[{}] deploying {} into {:?} (id {}), current instance {}",
            name,
            new_hash,
            self.new_slot,
            target.inner(),
            self.retiring
                .as_ref()
                .map_or_else(|| "none".to_string(), |old| old.slot_id.inner().to_string())
        );

        let switched = self
            .provision_inactive()
            .and_then(Self::start_and_check)
            .and_then(Self::register_and_switch);

        match switched {
            Ok(ctx) => {
                let result = ctx.maybe_destroy_old();
                match &result {
                    Ok(()) => info!("[{}] deployed, now serving from {}", name, target.inner()),
                    Err(e) => warn!(
                        "[{}] traffic is on {} but retiring the old instance failed: {}",
                        name,
                        target.inner(),
                        e
                    ),
                }
                result
            }
            Err(e) => {
                warn!("[{}] deploy failed: {}", name, e);
                abort::<T>(pve, target);
                Err(e)
            }
        }
    }
}

fn cut_over<T: Deployments>(
    sozu: &mut SozuClient,
    config: &T,
    new_backend_id: &BackendId,
    new_ip: Ipv4Addr,
    retiring: Option<&Retiring>,
) -> Result<()> {
    let old_ip = retiring.and_then(|old| old.ip);
    match config.service_address() {
        None => info!(
            "{} has no service address, leaving it unproxied",
            config.name()
        ),
        Some(service) => {
            info!(
                "[{}] cutting traffic over: {} -> {} (service address {})",
                config.name(),
                old_ip.map_or_else(|| "nothing".to_string(), |i| i.to_string()),
                new_ip,
                service
            );
            sozu.ensure_cluster(config)?;
            sozu.register_backend(config, new_backend_id, new_ip)?;
            match sozu.prune_backends(config, new_ip) {
                Ok(Pruned { removed: 0, failed: 0 }) => {}
                Ok(pruned) => info!(
                    "[{}] dropped {} stale backends ({} could not be dropped)",
                    config.name(), pruned.removed, pruned.failed
                ),
                Err(e) => warn!(
                    "[{}] could not check for stale backends, traffic may still reach a retired instance: {}",
                    config.name(), e
                ),
            }
            if let (Some(old_bid), Some(old_ip_val)) = (retiring.and_then(|old| old.backend_id.as_ref()), old_ip)
                && let Err(e) = sozu.remove_backend(config, old_bid, old_ip_val)
            {
                warn!(
                    "failed to deregister old backend {}, rolling back new registration: {}",
                    old_bid, e
                );
                if let Err(undo) = sozu.remove_backend(config, new_backend_id, new_ip) {
                    warn!(
                        "could not deregister new backend {}: {}",
                        new_backend_id, undo
                    );
                }
                return Err(e);
            }
        }
    }
    Ok(())
}

fn nix_hash_of(artifact: &StorePath) -> Result<NixHash> {
    artifact.nix_hash().ok_or_else(|| {
        AppError::CmdError(format!("could not extract nix hash from {artifact}"))
    })
}

fn await_ip<T: Deployments>(pve: &Pve, config: &T, id: Vmid) -> Result<Ipv4Addr> {
    let timeout = config.dhcp_timeout();
    let started = Instant::now();
    info!(
        "[{}] waiting up to {}s for {} to report an address",
        config.name(),
        timeout.as_secs(),
        id
    );
    (0_u32..)
        .take_while(|_| started.elapsed() < timeout)
        .find_map(|attempt| {
            if let Some(ip) = <T::Kind as Observe>::address(pve, id).ok().filter(|ip| {
                let usable = !ip.is_link_local() && !ip.is_unspecified() && !ip.is_loopback();
                if !usable {
                    warn!(
                        "[{}] {} self-assigned {}, dhcp has not answered",
                        config.name(), id, ip
                    );
                }
                usable
            }) { Some(ip) } else {
                if attempt > 0 && attempt % 5 == 0 {
                    info!(
                        "[{}] still no address on {} after {}s (attempt {}, timeout {}s)",
                        config.name(),
                        id,
                        started.elapsed().as_secs(),
                        attempt,
                        timeout.as_secs()
                    );
                }
                std::thread::sleep(
                    Duration::from_secs(2).min(timeout.saturating_sub(started.elapsed())),
                );
                None
            }
        })
        .ok_or(AppError::IpTimeoutError(id))
}

enum RouteGap {
    NoServiceIp,
    NoNixHash,
}

impl fmt::Display for RouteGap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RouteGap::NoServiceIp => write!(f, "no service ip is recorded in its tags"),
            RouteGap::NoNixHash => write!(f, "no nix hash is recorded in its tags"),
        }
    }
}

enum Upkeep {
    Undeployed,
    Start { id: Vmid, status: GuestStatus },
    Unproxied,
    Unroutable { gap: RouteGap },
    Route { backend_id: BackendId, ip: Ipv4Addr },
}

fn upkeep<T: Deployments>(config: &T, deployed: Option<&DeployedOf<T>>) -> Upkeep {
    match deployed {
        None => Upkeep::Undeployed,
        Some(d) => match &d.status {
            GuestStatus::Running => match (config.service_address(), config.tcp_ports().is_empty()) {
                (None, true) => Upkeep::Unproxied,
                _ => match (d.service_ip, d.nix_hash.as_ref()) {
                    (Some(ip), Some(hash)) => Upkeep::Route {
                        backend_id: BackendId::new(config.name(), hash),
                        ip,
                    },
                    (None, _) => Upkeep::Unroutable { gap: RouteGap::NoServiceIp },
                    (_, None) => Upkeep::Unroutable { gap: RouteGap::NoNixHash },
                },
            },
            GuestStatus::Other(_) => Upkeep::Start { id: d.id, status: d.status.clone() },
        },
    }
}

fn restore_routes<T: Deployments>(
    routes: &[(&T, &BackendId, Ipv4Addr)],
    settings: &AppConfig,
) {
    if routes.is_empty() {
        return;
    }
    let mut sozu = match SozuClient::connect(settings) {
        Ok(client) => client,
        Err(e) => {
            warn!("periodic reconcile: could not reach sozu: {}", e);
            return;
        }
    };
    for (config, backend_id, ip) in routes {
        restore_tcp_routes(&mut sozu, *config, backend_id, *ip);
        if config.service_address().is_none() {
            continue;
        }
        let restored = sozu
            .ensure_cluster(*config)
            .and_then(|routing| sozu.register_backend(*config, backend_id, *ip).map(|_| routing));
        match restored {
            Ok(Settled::Changed) => info!(
                "periodic reconcile: restored sozu route for {} -> {}:{}",
                config.hostname(),
                ip,
                config.backend_port()
            ),
            Ok(Settled::AlreadyApplied) => {}
            Err(e) => warn!(
                "periodic reconcile: could not restore sozu route for {}: {}",
                config.name(),
                e
            ),
        }
        match sozu.prune_backends(*config, *ip) {
            Ok(Pruned { removed: 0, failed: 0 }) => {}
            Ok(pruned) => info!(
                "periodic reconcile: dropped {} stale backends for {} ({} could not be dropped)",
                pruned.removed,
                config.name(),
                pruned.failed
            ),
            Err(e) => warn!(
                "periodic reconcile: could not check {} for stale backends: {}",
                config.name(),
                e
            ),
        }
    }
}

fn restore_tcp_routes<T: Deployments>(
    sozu: &mut SozuClient,
    config: &T,
    backend_id: &BackendId,
    ip: Ipv4Addr,
) {
    if config.tcp_ports().is_empty() {
        return;
    }
    match sozu.register_tcp_backends(config, backend_id, ip) {
        Ok(Settled::Changed) => info!(
            "periodic reconcile: restored tcp forwarding for {} ports {:?} -> {}",
            config.name(),
            config.tcp_ports(),
            ip
        ),
        Ok(Settled::AlreadyApplied) => {}
        Err(e) => warn!(
            "periodic reconcile: could not restore tcp forwarding for {}: {}",
            config.name(),
            e
        ),
    }
    match sozu.prune_tcp_backends(config, ip) {
        Ok(Pruned { removed: 0, failed: 0 }) => {}
        Ok(pruned) => info!(
            "periodic reconcile: dropped {} stale tcp backends for {} ({} could not be dropped)",
            pruned.removed,
            config.name(),
            pruned.failed
        ),
        Err(e) => warn!(
            "periodic reconcile: could not check {} for stale tcp backends: {}",
            config.name(),
            e
        ),
    }
}

pub fn ensure_running<T: Deployments>(configs: &[T], settings: &AppConfig, pve: &Pve) {
    let deployed = match state::deployed::<T::Kind>(pve) {
        Ok(d) => d,
        Err(e) => {
            warn!("periodic reconcile: could not load deployed state: {}", e);
            return;
        }
    };

    let plans: Vec<(&T, Upkeep)> = configs
        .iter()
        .map(|config| (config, upkeep(config, deployed.get(config.name()))))
        .collect();

    for (config, plan) in &plans {
        match plan {
            Upkeep::Undeployed => debug!(
                "periodic reconcile: {} is not deployed, it will be created on the next push",
                config.name()
            ),
            Upkeep::Start { id, status } => {
                info!(
                    "periodic reconcile: {} (id {}) is {}, starting",
                    config.name(),
                    id,
                    status
                );
                match Cli.run(&GuestOp::<T::Kind>::start(*id)) {
                    Ok(Settled::Changed) => info!("periodic reconcile: started {}", config.name()),
                    Ok(Settled::AlreadyApplied) => {}
                    Err(e) => warn!("periodic reconcile: could not start {}: {}", config.name(), e),
                }
            }
            Upkeep::Unroutable { gap } => warn!(
                "periodic reconcile: {} is running but cannot be routed because {}",
                config.name(),
                gap
            ),
            Upkeep::Unproxied | Upkeep::Route { .. } => {}
        }
    }

    let routes: Vec<(&T, &BackendId, Ipv4Addr)> = plans
        .iter()
        .filter_map(|(config, plan)| match plan {
            Upkeep::Route { backend_id, ip } => Some((*config, backend_id, *ip)),
            _ => None,
        })
        .collect();

    restore_routes(&routes, settings);
}

fn target_state<T: Deployments>(pve: &Pve, id: Vmid) -> Result<TargetState> {
    if state::exists::<T::Kind>(pve, id)? {
        state::tags::<T::Kind>(pve, id).map(|tags| classify_target(true, tags.as_deref()))
    } else {
        Ok(TargetState::Vacant)
    }
}

fn classify_target(exists: bool, tags: Option<&str>) -> TargetState {
    match (exists, is_proxnix_managed(tags)) {
        (false, _) => TargetState::Vacant,
        (true, true) => TargetState::Managed,
        (true, false) => TargetState::Unmanaged,
    }
}

fn prepare_target<T: Deployments>(pve: &Pve, name: &str, target: SlotId) -> Result<()> {
    let id = target.inner();
    match target_state::<T>(pve, id)? {
        TargetState::Vacant => Ok(()),
        TargetState::Managed => {
            info!(
                "[{}] reclaiming Proxnix-managed inactive slot {} before provisioning",
                name, id
            );
            Cli.run_all(&GuestOp::<T::Kind>::reclaim(id))
        }
        TargetState::Unmanaged => Err(AppError::CmdError(format!(
            "refusing to replace instance {id} because it is not tagged 'proxnix'"
        ))),
    }
}

fn abort<T: Deployments>(pve: &Pve, target: SlotId) {
    let id = target.inner();
    match target_state::<T>(pve, id) {
        Ok(TargetState::Vacant) => {}
        Ok(TargetState::Unmanaged) => warn!(
            "abort: refusing to destroy {}, it is not tagged 'proxnix'",
            id
        ),
        Ok(TargetState::Managed) => {
            warn!("deploy failed, destroying Proxnix-managed instance {}", id);
            if let Err(e) = Cli.run_all(&GuestOp::<T::Kind>::reclaim(id)) {
                warn!("abort: could not destroy {}: {}", id, e);
            }
        }
        Err(e) => warn!("abort: could not inspect {}: {}", id, e),
    }
}

pub trait Dangerous {
    fn dhcp_timeout(&self) -> Duration;
    fn health_check_timeout(&self) -> Duration;

    fn pre_check(&self) -> Result<()> {
        Ok(())
    }
    fn post_check(&self) -> Result<()> {
        Ok(())
    }
    fn health_check(&self, addr: SocketAddr) -> Result<()> {
        let timeout = self.health_check_timeout();
        let started = Instant::now();
        (0_u32..)
            .take_while(|_| started.elapsed() < timeout)
            .find_map(
                |attempt| match TcpStream::connect_timeout(
                    &addr,
                    Duration::from_secs(2).min(timeout.saturating_sub(started.elapsed())),
                ) {
                    Ok(_) => Some(()),
                    Err(e) => {
                        if attempt > 0 && attempt % 5 == 0 {
                            info!(
                                "still waiting on {} after {}s (attempt {}): {}",
                                addr,
                                started.elapsed().as_secs(),
                                attempt,
                                e
                            );
                        }
                        std::thread::sleep(
                            Duration::from_secs(2)
                                .min(timeout.saturating_sub(started.elapsed())),
                        );
                        None
                    }
                },
            )
            .ok_or(AppError::HealthCheckError(addr))
    }
}

impl Dangerous for VMConfig {
    fn dhcp_timeout(&self) -> Duration {
        Duration::from_secs(self.dhcp_timeout_seconds)
    }

    fn health_check_timeout(&self) -> Duration {
        Duration::from_secs(self.health_check_timeout_seconds)
    }
}

impl Dangerous for ContainerConfig {
    fn dhcp_timeout(&self) -> Duration {
        Duration::from_secs(self.dhcp_timeout_seconds)
    }

    fn health_check_timeout(&self) -> Duration {
        Duration::from_secs(self.health_check_timeout_seconds)
    }
}

pub trait Deployments: Dangerous + Materialise + Workload + Sized + Send + Sync + Proxied {
    type Kind: Observe + Probe;

    fn is_protected(&self) -> bool;
    fn resources(&self, changes: &[FieldChange]) -> Result<<Self::Kind as Kind>::Set>;

    fn kind_change(&self, _deployed: &DeployedOf<Self>) -> Option<FieldChange> {
        None
    }

    fn compute_changes(
        &self,
        deployed: &DeployedOf<Self>,
        image_hashes: &HashMap<ImageType, NixHash>,
    ) -> Vec<FieldChange> {
        let image_changed = image_hashes
            .get(self.image_type())
            .zip(deployed.nix_hash.as_ref())
            .is_none_or(|(desired, deployed)| desired != deployed);
        [
            (self.memory_mb() != deployed.resources.memory_mb, FieldChange::Memory),
            (self.disk_gb() > deployed.resources.disk_gb.round() as u32, FieldChange::Disk),
            (self.cores() != deployed.resources.cores, FieldChange::Cores),
        ]
        .into_iter()
        .filter_map(|(changed, field)| changed.then_some(field))
        .chain(self.kind_change(deployed))
        .chain(image_changed.then_some(FieldChange::Image))
        .collect()
    }
}

fn requires_rebuild(changes: &[FieldChange]) -> bool {
    changes.iter().any(|c| matches!(c, FieldChange::Image | FieldChange::Disk))
}

impl Deployments for VMConfig {
    type Kind = Qemu;

    fn is_protected(&self) -> bool {
        self.protected
    }

    fn kind_change(&self, deployed: &DeployedOf<Self>) -> Option<FieldChange> {
        (self.sockets != deployed.extra.sockets).then_some(FieldChange::Sockets)
    }

    fn resources(&self, changes: &[FieldChange]) -> Result<qemu::vmid::config::PutParams> {
        Ok(qemu::vmid::config::PutParams {
            memory: changes
                .contains(&FieldChange::Memory)
                .then(|| self.memory_mb.to_string()),
            cores: changes
                .contains(&FieldChange::Cores)
                .then(|| NonZeroU64::new(u64::from(self.cores)))
                .flatten(),
            sockets: changes
                .contains(&FieldChange::Sockets)
                .then(|| NonZeroU64::new(u64::from(self.sockets)))
                .flatten(),
            ..Default::default()
        })
    }
}

impl Deployments for ContainerConfig {
    type Kind = Lxc;

    fn is_protected(&self) -> bool {
        self.protected
    }

    fn resources(&self, changes: &[FieldChange]) -> Result<lxc::vmid::config::PutParams> {
        Ok(lxc::vmid::config::PutParams {
            memory: changes
                .contains(&FieldChange::Memory)
                .then(|| i128::from(self.memory_mb).try_into())
                .transpose()?,
            cores: changes
                .contains(&FieldChange::Cores)
                .then(|| i128::from(self.cores).try_into())
                .transpose()?,
            ..Default::default()
        })
    }
}

enum Action<'a, T: Deployments> {
    Create {
        config: &'a T,
    },
    Rebuild {
        config: &'a T,
        deployed: &'a DeployedOf<T>,
    },
    UpdateInPlace {
        config: &'a T,
        deployed_id: Vmid,
        service_ip: Option<Ipv4Addr>,
        changes: Vec<FieldChange>,
    },
    Destroy {
        name: String,
        id: Vmid,
    },
    Skip {
        name: String,
        reason: SkipReason,
    },
    NoOp {
        name: String,
    },
}

pub fn reconcile<T: Deployments>(
    configs: &[T],
    ctx: &ReconcileContext<'_>,
) -> Result<Vec<Outcome>> {
    let deployed = state::deployed::<T::Kind>(ctx.pve)?;
    let actions = plan(configs, &deployed, ctx.image_hashes);
    Ok(actions
        .into_par_iter()
        .enumerate()
        .map(|(index, action)| {
            std::thread::sleep(match u32::try_from(index) {
                Ok(nth) => ctx.settings.timings_ms.get(Timing::ProvisionStagger).saturating_mul(nth),
                Err(_) => Duration::ZERO,
            });
            execute(action, ctx)
        })
        .collect())
}

fn execute<T: Deployments>(action: Action<'_, T>, ctx: &ReconcileContext<'_>) -> Outcome {
    match action {
        Action::Create { config } => Outcome::new(
            config.name(),
            OutcomeKind::Created,
            get_artifact(config, ctx)
                .and_then(|artifact| DeployContext::from_create(config, artifact, ctx))
                .and_then(DeployContext::run),
        ),
        Action::Rebuild { config, deployed } => Outcome::new(
            config.name(),
            OutcomeKind::Rebuilt,
            get_artifact(config, ctx)
                .and_then(|artifact| DeployContext::from_rebuild(config, deployed, artifact, ctx))
                .and_then(DeployContext::run),
        ),
        Action::UpdateInPlace { config, deployed_id, service_ip, changes } => Outcome::new(
            config.name(),
            OutcomeKind::Updated,
            update_in_place(config, deployed_id, service_ip, &changes),
        ),
        Action::Destroy { name, id } => {
            let result = destroy_orphan::<T>(&name, id, ctx.settings);
            Outcome::new(&name, OutcomeKind::Destroyed, result)
        }
        Action::Skip { name, reason } => Outcome::new(&name, OutcomeKind::Skipped(reason), Ok(())),
        Action::NoOp { name } => Outcome::new(&name, OutcomeKind::NoOp, Ok(())),
    }
}

fn update_in_place<T: Deployments>(
    config: &T,
    deployed_id: Vmid,
    service_ip: Option<Ipv4Addr>,
    changes: &[FieldChange],
) -> Result<()> {
    config.pre_check()?;
    Cli.run(&GuestOp::<T::Kind>::Set(deployed_id, config.resources(changes)?))?;
    config.post_check()?;
    service_ip.map_or(Ok(()), |ip| {
        config.health_check(SocketAddr::from((ip, config.backend_port())))
    })
}

fn destroy_orphan<T: Deployments>(name: &str, id: Vmid, settings: &AppConfig) -> Result<()> {
    let mut sozu = SozuClient::connect(settings)?;
    if let Err(e) = sozu.remove_cluster(name) {
        info!(
            "[{}] sozu had no cluster to remove ({}), continuing with teardown",
            name, e
        );
    }
    match sozu.remove_tcp_clusters(name) {
        Ok(0) => {}
        Ok(n) => info!("[{}] removed {} tcp clusters", name, n),
        Err(e) => warn!(
            "[{}] could not remove tcp clusters ({}), continuing with teardown",
            name, e
        ),
    }
    info!("[{}] destroying orphaned instance {}", name, id);
    Cli.run_all(&GuestOp::<T::Kind>::retire(id))
}

fn get_artifact<T: Deployments>(
    config: &T,
    ctx: &ReconcileContext<'_>,
) -> Result<StorePath> {
    match ctx.pre_built.get(config.image_type()) {
        Some(path) => Ok(path.clone()),
        None => match ctx.image_type_errors.get(config.image_type()) {
            Some(err) => Err(AppError::CmdError(format!(
                "image type '{}' failed to build: {}",
                config.image_type(),
                err
            ))),
            None => config.nix_build(ctx.repo_path.as_str()),
        },
    }
}

fn plan<'a, T: Deployments>(
    configs: &'a [T],
    deployed: &'a HashMap<String, DeployedOf<T>>,
    image_hashes: &HashMap<ImageType, NixHash>,
) -> Vec<Action<'a, T>> {
    let desired: HashSet<&str> = configs.iter().map(Workload::name).collect();

    configs
        .iter()
        .map(|c| classify(c, deployed, image_hashes))
        .chain(
            deployed
                .iter()
                .filter(|(n, _)| !desired.contains(n.as_str()))
                .map(|(n, d)| Action::Destroy {
                    name: n.clone(),
                    id: d.id,
                }),
        )
        .collect()
}

fn classify<'a, T: Deployments>(
    config: &'a T,
    deployed: &'a HashMap<String, DeployedOf<T>>,
    image_hashes: &HashMap<ImageType, NixHash>,
) -> Action<'a, T> {
    let Some(d) = deployed.get(config.name()) else {
        return Action::Create { config };
    };

    let changes = config.compute_changes(d, image_hashes);

    match (changes.is_empty(), requires_rebuild(&changes), config.is_protected()) {
        (true, _, _) => Action::NoOp { name: config.name().into() },
        (_, _, true) => Action::Skip { name: config.name().into(), reason: SkipReason::Protected },
        (_, true, _) => Action::Rebuild { config, deployed: d },
        (_, false, _) => Action::UpdateInPlace {
            config,
            deployed_id: d.id,
            service_ip: d.service_ip,
            changes,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{LxcExtra, QemuExtra, Resources};
    use crate::types::{BindMount, MountMode};

    fn vm(protected: bool) -> VMConfig {
        VMConfig {
            name: "test-website".to_string(),
            blue_id: Vmid::new(823),
            green_id: Vmid::new(824),
            hostname: "test-website".to_string(),
            service_address: Some(Ipv4Addr::new(192, 168, 1, 23)),
            backend_port: 80,
            tcp_ports: vec![],
            dhcp_timeout_seconds: 240,
            health_check_timeout_seconds: 180,
            image_type: ImageType::from("build-qcow2-website"),
            cores: 2,
            sockets: 1,
            memory_mb: 2048,
            storage_location: "local-lvm".to_string(),
            disk_gb: 10,
            protected,
            network_bridge: "vmbr0".to_string(),
            scsi_hw: qemu::Scsihw::VirtioScsiPci,
            disk_slot: "scsi0".parse().unwrap(),
            impure: false,
        }
    }

    fn container() -> ContainerConfig {
        ContainerConfig {
            name: "pihole".to_string(),
            hostname: "pihole".to_string(),
            service_address: None,
            backend_port: 80,
            tcp_ports: vec![],
            dhcp_timeout_seconds: 240,
            health_check_timeout_seconds: 180,
            blue_id: Vmid::new(833),
            green_id: Vmid::new(933),
            image_type: ImageType::from("build-lxc-pihole"),
            cores: 2,
            memory_mb: 1024,
            storage_location: "local-lvm".to_string(),
            disk_gb: 8,
            protected: false,
            privileged: true,
            bind_mounts: vec![],
            network_bridge: "vmbr0".to_string(),
            impure: false,
        }
    }

    fn deployed(hash: &str, slot: Slot) -> DeployedOf<VMConfig> {
        Deployed {
            id: Vmid::new(match slot {
                Slot::Blue => 823,
                Slot::Green => 824,
            }),
            name: "test-website".to_string(),
            status: GuestStatus::Running,
            nix_hash: Some(NixHash::try_from(hash).unwrap()),
            active_slot: slot,
            service_ip: Some(Ipv4Addr::new(10, 0, 0, 10)),
            resources: Resources { memory_mb: 2048, disk_gb: 10.0, cores: 2 },
            extra: QemuExtra { sockets: 1 },
        }
    }

    fn deployed_container(hash: &str) -> DeployedOf<ContainerConfig> {
        Deployed {
            id: Vmid::new(833),
            name: "pihole".to_string(),
            status: GuestStatus::Running,
            nix_hash: Some(NixHash::try_from(hash).unwrap()),
            active_slot: Slot::Blue,
            service_ip: None,
            resources: Resources { memory_mb: 1024, disk_gb: 8.0, cores: 2 },
            extra: LxcExtra {
                privileged: true,
                bind_mounts: vec![BindMount {
                    host_path: "/srv".to_string(),
                    container_path: "/srv".to_string(),
                    mode: MountMode::ReadOnly,
                }],
            },
        }
    }

    fn hashes(hash: &str) -> HashMap<ImageType, NixHash> {
        HashMap::from([(
            ImageType::from("build-qcow2-website"),
            NixHash::try_from(hash).unwrap(),
        )])
    }

    fn rendered<K: Kind>(op: &GuestOp<K>) -> String {
        op.invocation().unwrap().to_string()
    }

    #[test]
    fn a_running_proxied_workload_is_routed_at_its_recorded_address() {
        let config = vm(false);
        let d = deployed("abc123", Slot::Blue);
        match upkeep(&config, Some(&d)) {
            Upkeep::Route { backend_id, ip } => {
                assert_eq!(ip, Ipv4Addr::new(10, 0, 0, 10));
                assert_eq!(
                    backend_id.as_str(),
                    BackendId::new("test-website", &NixHash::try_from("abc123").unwrap()).as_str()
                );
            }
            _ => panic!("a running proxied workload should be routable"),
        }
    }

    #[test]
    fn the_reconcile_backend_id_matches_the_one_a_deploy_registers() {
        let config = vm(false);
        let hash = NixHash::try_from("abc123").unwrap();
        let deploy_time = BackendId::new(config.name(), &hash);
        match upkeep(&config, Some(&deployed("abc123", Slot::Blue))) {
            Upkeep::Route { backend_id, .. } => {
                assert_eq!(backend_id.as_str(), deploy_time.as_str());
            }
            _ => panic!("expected a route"),
        }
    }

    #[test]
    fn a_stopped_workload_is_started_and_not_routed() {
        let config = vm(false);
        let stopped = Deployed {
            status: GuestStatus::from("stopped"),
            ..deployed("abc123", Slot::Blue)
        };
        match upkeep(&config, Some(&stopped)) {
            Upkeep::Start { id, status } => {
                assert_eq!(id, Vmid::new(823));
                assert_eq!(status.to_string(), "stopped");
            }
            _ => panic!("a stopped workload should be started"),
        }
    }

    #[test]
    fn a_workload_without_a_service_address_is_never_routed() {
        let config = VMConfig { service_address: None, ..vm(false) };
        match upkeep(&config, Some(&deployed("abc123", Slot::Blue))) {
            Upkeep::Unproxied => {}
            _ => panic!("a workload with no service address must not be proxied"),
        }
    }

    #[test]
    fn a_running_workload_with_no_recorded_address_is_reported_not_guessed() {
        let config = vm(false);
        let untagged = Deployed { service_ip: None, ..deployed("abc123", Slot::Blue) };
        match upkeep(&config, Some(&untagged)) {
            Upkeep::Unroutable { gap: RouteGap::NoServiceIp } => {}
            _ => panic!("a missing service ip must surface as a gap"),
        }
    }

    #[test]
    fn an_undeployed_workload_needs_no_upkeep() {
        match upkeep(&vm(false), None) {
            Upkeep::Undeployed => {}
            _ => panic!("nothing is deployed, nothing to do"),
        }
    }

    #[test]
    fn blue_and_green_resolve_to_distinct_identities() {
        let c = vm(false);
        assert_eq!(c.id_for_slot(Slot::Blue), SlotId::Blue(Vmid::new(823)));
        assert_eq!(c.id_for_slot(Slot::Green), SlotId::Green(Vmid::new(824)));
        assert_ne!(
            c.id_for_slot(Slot::Blue).inner(),
            c.id_for_slot(Slot::Green).inner()
        );
    }

    #[test]
    fn a_deploy_always_targets_the_inactive_slot() {
        let c = vm(false);
        for active in [Slot::Blue, Slot::Green] {
            let target = c.id_for_slot(active.switch_slot());
            assert_ne!(target.inner(), c.id_for_slot(active).inner());
            assert_ne!(target.slot(), active);
        }
    }

    #[test]
    fn builds_reference_the_image_type_not_the_workload_name() {
        let c = vm(false);
        assert_eq!(Materialise::image_type(&c), "build-qcow2-website");
        assert_ne!(Materialise::image_type(&c), Workload::name(&c));
    }

    #[test]
    fn unchanged_image_and_resources_is_a_noop() {
        let c = vm(false);
        let d = deployed("abc123", Slot::Blue);
        assert!(c.compute_changes(&d, &hashes("abc123")).is_empty());
    }

    #[test]
    fn a_new_image_hash_requires_rebuild() {
        let c = vm(false);
        let d = deployed("abc123", Slot::Blue);
        let changes = c.compute_changes(&d, &hashes("def456"));
        assert!(changes.contains(&FieldChange::Image));
        assert!(requires_rebuild(&changes));
    }

    #[test]
    fn a_memory_change_alone_does_not_require_rebuild() {
        let c = VMConfig { memory_mb: 4096, ..vm(false) };
        let d = deployed("abc123", Slot::Blue);
        let changes = c.compute_changes(&d, &hashes("abc123"));
        assert_eq!(changes, vec![FieldChange::Memory]);
        assert!(!requires_rebuild(&changes));
    }

    #[test]
    fn a_disk_grow_requires_rebuild() {
        let c = VMConfig { disk_gb: 20, ..vm(false) };
        let d = deployed("abc123", Slot::Blue);
        let changes = c.compute_changes(&d, &hashes("abc123"));
        assert!(requires_rebuild(&changes));
    }

    #[test]
    fn an_unbuilt_image_counts_as_changed() {
        let c = vm(false);
        let d = deployed("abc123", Slot::Blue);
        let changes = c.compute_changes(&d, &HashMap::new());
        assert!(changes.contains(&FieldChange::Image));
    }

    #[test]
    fn vm_changes_are_reported_in_a_fixed_order() {
        let c = VMConfig { memory_mb: 4096, disk_gb: 20, cores: 4, sockets: 2, ..vm(false) };
        let d = deployed("abc123", Slot::Blue);
        assert_eq!(
            c.compute_changes(&d, &hashes("def456")),
            vec![
                FieldChange::Memory,
                FieldChange::Disk,
                FieldChange::Cores,
                FieldChange::Sockets,
                FieldChange::Image,
            ]
        );
    }

    #[test]
    fn containers_have_no_socket_change() {
        let c = ContainerConfig { memory_mb: 2048, cores: 4, ..container() };
        let image = HashMap::from([(
            ImageType::from("build-lxc-pihole"),
            NixHash::try_from("def456").unwrap(),
        )]);
        assert_eq!(
            c.compute_changes(&deployed_container("abc123"), &image),
            vec![FieldChange::Memory, FieldChange::Cores, FieldChange::Image]
        );
    }

    #[test]
    fn an_in_place_update_only_sets_what_changed() {
        let c = VMConfig { memory_mb: 4096, sockets: 2, ..vm(false) };
        let set = c.resources(&[FieldChange::Memory, FieldChange::Sockets]).unwrap();
        assert_eq!(
            rendered(&GuestOp::<Qemu>::Set(Vmid::new(823), set)),
            "qm set 823 --memory 4096 --sockets 2"
        );
        let ct = ContainerConfig { cores: 4, ..container() };
        let set = ct.resources(&[FieldChange::Cores]).unwrap();
        assert_eq!(
            rendered(&GuestOp::<Lxc>::Set(Vmid::new(833), set)),
            "pct set 833 --cores 4"
        );
    }

    #[test]
    fn an_undeployed_workload_is_created() {
        let c = vm(false);
        let empty = HashMap::new();
        match classify(&c, &empty, &hashes("abc123")) {
            Action::Create { .. } => {}
            _ => panic!("expected Create for a workload with no deployed state"),
        }
    }

    #[test]
    fn a_changed_image_rebuilds_from_the_deployed_slot() {
        let c = vm(false);
        let d = HashMap::from([("test-website".to_string(), deployed("abc123", Slot::Green))]);
        match classify(&c, &d, &hashes("def456")) {
            Action::Rebuild { deployed, .. } => {
                assert_eq!(deployed.id, Vmid::new(824));
                assert_eq!(deployed.active_slot, Slot::Green);
            }
            _ => panic!("expected Rebuild when the image hash changed"),
        }
    }

    #[test]
    fn a_protected_workload_is_skipped_not_rebuilt() {
        let c = vm(true);
        let d = HashMap::from([("test-website".to_string(), deployed("abc123", Slot::Blue))]);
        match classify(&c, &d, &hashes("def456")) {
            Action::Skip { reason: SkipReason::Protected, .. } => {}
            _ => panic!("expected protected workloads to be skipped"),
        }
    }

    #[test]
    fn protection_does_not_mask_a_noop() {
        let c = vm(true);
        let d = HashMap::from([("test-website".to_string(), deployed("abc123", Slot::Blue))]);
        match classify(&c, &d, &hashes("abc123")) {
            Action::NoOp { .. } => {}
            _ => panic!("expected NoOp to win over Skip when nothing changed"),
        }
    }

    #[test]
    fn an_in_place_update_targets_the_deployed_slot() {
        let c = VMConfig { memory_mb: 4096, ..vm(false) };
        let d = HashMap::from([("test-website".to_string(), deployed("abc123", Slot::Green))]);
        match classify(&c, &d, &hashes("abc123")) {
            Action::UpdateInPlace { deployed_id, service_ip, .. } => {
                assert_eq!(deployed_id, Vmid::new(824));
                assert_eq!(service_ip, Some(Ipv4Addr::new(10, 0, 0, 10)));
            }
            _ => panic!("expected UpdateInPlace for a resource-only change"),
        }
    }

    #[test]
    fn a_workload_dropped_from_config_is_destroyed() {
        let d = HashMap::from([("orphan".to_string(), deployed("abc123", Slot::Blue))]);
        let actions = plan::<VMConfig>(&[], &d, &hashes("abc123"));
        match actions.as_slice() {
            [Action::Destroy { name, id }] => {
                assert_eq!(name, "orphan");
                assert_eq!(*id, Vmid::new(823));
            }
            _ => panic!("expected a single Destroy for the orphaned workload"),
        }
    }

    #[test]
    fn a_still_desired_workload_is_not_destroyed() {
        let c = vm(false);
        let d = HashMap::from([("test-website".to_string(), deployed("abc123", Slot::Blue))]);
        let actions = plan(std::slice::from_ref(&c), &d, &hashes("abc123"));
        assert!(!actions.iter().any(|a| matches!(a, Action::Destroy { .. })));
    }

    #[test]
    fn a_vacant_target_is_ready_for_provisioning() {
        assert_eq!(
            classify_target(false, Some("proxnix;nix-old-hash")),
            TargetState::Vacant
        );
    }

    #[test]
    fn any_proxnix_managed_target_is_reclaimable_regardless_of_hash() {
        assert_eq!(
            classify_target(true, Some("proxnix;nix-old-hash;slot-blue")),
            TargetState::Managed
        );
        assert_eq!(
            classify_target(true, Some("proxnix;nix-new-hash;slot-blue")),
            TargetState::Managed
        );
    }

    #[test]
    fn an_unmanaged_target_is_not_reclaimable() {
        assert_eq!(
            classify_target(true, Some("nix-old-hash;slot-blue")),
            TargetState::Unmanaged
        );
        assert_eq!(classify_target(true, None), TargetState::Unmanaged);
    }
}
