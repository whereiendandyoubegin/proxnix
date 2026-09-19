use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use proxnix_core::{Slot, SlotId, Workload};
use rayon::prelude::*;
use tracing::{debug, info, warn};

use crate::{
    context::{BackendId, BackendPool, NixHash, ReconcileContext, SozuSocketPath, StorePath, Tags},
    materialise::Materialise,
    pct::{pct_destroy, pct_list, pct_set_protection, pct_set_resources, pct_set_tags, pct_start, pct_stop},
    qm::{qm_destroy, qm_get_running_ip, qm_set_protection, qm_set_resources, qm_set_tags, qm_start, qm_stop},
    sozu::{Proxied, Settled, SozuClient},
    state::{
        container_exists, container_tags, enrich_container_info, enrich_cpu_info,
        is_proxnix_managed, list_to_deployed_vm, parse_pct_list, parse_qm_list, qm_list, vm_exists,
        vm_tags,
    },
    types::{
        AppError, ContainerConfig, ContainerFieldChange, DeployedContainer, DeployedVM,
        FieldChange, Outcome, OutcomeKind, Result, SkipReason, VMConfig,
    },
};

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

pub struct DeployContext<'a, T: Deployments> {
    config: &'a T,
    new_slot: Slot,
    old_backend_id: Option<BackendId>,
    old_ip: Option<Ipv4Addr>,
    old_slot_id: Option<SlotId>,
    sozu: SozuClient,
    artifact: StorePath,
    tags: Tags,
    template_cache_path: &'a str,
    backend_pool: Option<&'a BackendPool>,
    phase: Phase,
}

impl<'a, T: Deployments> DeployContext<'a, T> {
    fn from_create(
        config: &'a T,
        artifact: StorePath,
        commit_hash: &'a str,
        template_cache_path: &'a str,
        sozu_socket_path: &str,
        backend_pool: Option<&'a BackendPool>,
    ) -> Result<Self> {
        let new_slot = Slot::Blue;
        let tags = Tags::new(nix_hash_of(&artifact)?, commit_hash, new_slot);
        let sozu = SozuClient::connect(sozu_socket_path)?;
        Ok(Self {
            config,
            new_slot,
            old_backend_id: None,
            old_ip: None,
            old_slot_id: None,
            sozu,
            artifact,
            tags,
            template_cache_path,
            backend_pool,
            phase: Phase::Initial,
        })
    }

    fn from_rebuild(
        config: &'a T,
        deployed: &T::Deployed,
        deployed_id: u32,
        artifact: StorePath,
        commit_hash: &'a str,
        template_cache_path: &'a str,
        sozu_socket_path: &str,
        backend_pool: Option<&'a BackendPool>,
    ) -> Result<Self> {
        let deployed_slot = deployed.active_slot();
        let new_slot = deployed_slot.switch_slot();
        let tags = Tags::new(nix_hash_of(&artifact)?, commit_hash, new_slot);
        let old_backend_id = deployed
            .nix_hash()
            .map(|h| BackendId::new(config.name(), h));
        let old_ip = match deployed.service_ip() {
            Some(ip) => Some(ip),
            None => match T::get_ip(deployed_id).ok().and_then(|raw| raw.trim().parse().ok()) {
                Some(ip) => Some(ip),
                None => {
                    warn!(
                        "{} has no recorded service ip and its address could not be read; its backend cannot be deregistered by address",
                        config.name()
                    );
                    None
                }
            },
        };
        let old_slot_id = match deployed_slot {
            Slot::Blue => SlotId::Blue(deployed_id),
            Slot::Green => SlotId::Green(deployed_id),
        };
        let sozu = SozuClient::connect(sozu_socket_path)?;
        Ok(Self {
            config,
            new_slot,
            old_backend_id,
            old_ip,
            old_slot_id: Some(old_slot_id),
            sozu,
            artifact,
            tags,
            template_cache_path,
            backend_pool,
            phase: Phase::Initial,
        })
    }

    fn provision_inactive(self) -> Result<Self> {
        let target = self.config.id_for_slot(self.new_slot);
        let new_backend_id = BackendId::new(self.config.name(), &self.tags.nix_hash);
        prepare_target::<T>(self.config.name(), target)?;
        info!(
            "[{}] provisioning {:?} as {} (inactive, not started)",
            self.config.name(),
            self.new_slot,
            target.inner()
        );
        self.config.provision_inactive(
            &self.artifact,
            &self.tags,
            self.template_cache_path,
            target,
        )?;
        Ok(Self {
            phase: Phase::Provisioned { target, new_backend_id },
            ..self
        })
    }

    fn start_and_check(self) -> Result<Self> {
        let Self { phase, config, new_slot, old_backend_id, old_ip, old_slot_id, sozu, artifact, tags, template_cache_path, backend_pool } = self;
        let (target, new_backend_id) = match phase {
            Phase::Provisioned { target, new_backend_id } => (target, new_backend_id),
            _ => unreachable!("start_and_check called outside Provisioned phase"),
        };
        info!("[{}] starting {}", config.name(), target.inner());
        T::start(target.inner())?;
        let new_ip = await_ip::<T>(config, target.inner())?;
        info!("[{}] {} came up at {}", config.name(), target.inner(), new_ip);
        match backend_pool {
            Some(pool) if !pool.contains(new_ip) => warn!(
                "{} came up on {}, which is outside the declared backend pool {}-{}; the pool declaration and the dhcp scope disagree",
                config.name(), new_ip, pool.start, pool.end
            ),
            _ => {}
        }
        let tags = tags.with_service_ip(new_ip);
        T::set_tags(target.inner(), &tags)?;
        config.post_check()?;
        let addr = SocketAddr::from((new_ip, config.backend_port()));
        info!("[{}] health checking {}", config.name(), addr);
        config.health_check(addr)?;
        info!("[{}] healthy at {}", config.name(), addr);
        Ok(Self {
            config,
            new_slot,
            old_backend_id,
            old_ip,
            old_slot_id,
            sozu,
            artifact,
            tags,
            template_cache_path,
            backend_pool,
            phase: Phase::Healthy { new_backend_id, new_ip },
        })
    }

    fn register_and_switch(self) -> Result<Self> {
        let Self { phase, config, new_slot, old_backend_id, old_ip, old_slot_id, mut sozu, artifact, tags, template_cache_path, backend_pool } = self;
        let (new_backend_id, new_ip) = match phase {
            Phase::Healthy { new_backend_id, new_ip, .. } => (new_backend_id, new_ip),
            _ => unreachable!("register_and_switch called outside Healthy phase"),
        };
        match config.service_address() {
            None => info!(
                "{} has no service address, leaving it unproxied",
                config.name()
            ),
            Some(service) => {
                info!(
                    "[{}] cutting traffic over: {} -> {} (service address {})",
                    config.name(),
                    old_ip.map(|i| i.to_string()).unwrap_or_else(|| "nothing".to_string()),
                    new_ip,
                    service
                );
                sozu.ensure_cluster(config)?;
                sozu.register_backend(config, &new_backend_id, new_ip)?;
                if let (Some(old_bid), Some(old_ip_val)) = (old_backend_id.as_ref(), old_ip) {
                    match sozu.remove_backend(config, old_bid, old_ip_val) {
                        Ok(()) => {}
                        Err(e) => {
                            warn!(
                                "failed to deregister old backend {}, rolling back new registration: {}",
                                old_bid, e
                            );
                            match sozu.remove_backend(config, &new_backend_id, new_ip) {
                                Ok(()) => {}
                                Err(undo) => warn!(
                                    "could not deregister new backend {}: {}",
                                    new_backend_id, undo
                                ),
                            }
                            return Err(e);
                        }
                    }
                }
            }
        }
        Ok(Self {
            config,
            new_slot,
            old_backend_id,
            old_ip,
            old_slot_id,
            sozu,
            artifact,
            tags,
            template_cache_path,
            backend_pool,
            phase: Phase::BackendRegistered,
        })
    }

    fn maybe_destroy_old(self) -> Result<()> {
        let Phase::BackendRegistered = self.phase else {
            unreachable!("maybe_destroy_old called outside BackendRegistered phase")
        };
        match self.old_slot_id {
            Some(slot_id) => {
                info!(
                    "[{}] traffic is on the new instance, retiring {}",
                    self.config.name(),
                    slot_id.inner()
                );
                T::stop(&slot_id.inner())?;
                T::destroy(slot_id.inner())
            }
            None => Ok(()),
        }
    }

    pub fn run(self) -> Result<()> {
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
            self.old_slot_id
                .map(|s| s.inner().to_string())
                .unwrap_or_else(|| "none".to_string())
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
                abort::<T>(target);
                Err(e)
            }
        }
    }
}

fn nix_hash_of(artifact: &StorePath) -> Result<NixHash> {
    artifact.nix_hash().ok_or_else(|| {
        AppError::CmdError(format!("could not extract nix hash from {}", artifact))
    })
}

fn await_ip<T: Deployments>(config: &T, id: u32) -> Result<Ipv4Addr> {
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
            match T::get_ip(id)
                .ok()
                .and_then(|raw| raw.trim().parse::<Ipv4Addr>().ok())
                .filter(|ip| {
                    let usable = !ip.is_link_local() && !ip.is_unspecified() && !ip.is_loopback();
                    if !usable {
                        warn!(
                            "[{}] {} self-assigned {}, dhcp has not answered",
                            config.name(), id, ip
                        );
                    }
                    usable
                })
            {
                Some(ip) => Some(ip),
                None => {
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
    Start { id: u32, status: String },
    Unproxied,
    Unroutable { gap: RouteGap },
    Route { backend_id: BackendId, ip: Ipv4Addr },
}

fn upkeep<T: Deployments>(config: &T, deployed: Option<&T::Deployed>) -> Upkeep {
    match deployed {
        None => Upkeep::Undeployed,
        Some(d) => match d.status() {
            "running" => match config.service_address() {
                None => Upkeep::Unproxied,
                Some(_) => match (d.service_ip(), d.nix_hash()) {
                    (Some(ip), Some(hash)) => Upkeep::Route {
                        backend_id: BackendId::new(config.name(), hash),
                        ip,
                    },
                    (None, _) => Upkeep::Unroutable { gap: RouteGap::NoServiceIp },
                    (_, None) => Upkeep::Unroutable { gap: RouteGap::NoNixHash },
                },
            },
            status => Upkeep::Start { id: d.id(), status: status.to_string() },
        },
    }
}

fn restore_routes<T: Deployments>(
    routes: &[(&T, &BackendId, Ipv4Addr)],
    sozu_socket_path: SozuSocketPath<'_>,
) {
    if routes.is_empty() {
        return;
    }
    let mut sozu = match SozuClient::connect(sozu_socket_path.as_str()) {
        Ok(client) => client,
        Err(e) => {
            warn!("periodic reconcile: could not reach sozu: {}", e);
            return;
        }
    };
    routes.iter().for_each(|(config, backend_id, ip)| {
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
    });
}

pub fn ensure_running<T: Deployments>(configs: &[T], sozu_socket_path: SozuSocketPath<'_>) {
    let deployed = match T::load_deployed() {
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

    plans.iter().for_each(|(config, plan)| match plan {
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
            match T::start(*id) {
                Ok(true) => info!("periodic reconcile: started {}", config.name()),
                Ok(false) => {}
                Err(e) => warn!("periodic reconcile: could not start {}: {}", config.name(), e),
            }
        }
        Upkeep::Unroutable { gap } => warn!(
            "periodic reconcile: {} is running but cannot be routed because {}",
            config.name(),
            gap
        ),
        Upkeep::Unproxied | Upkeep::Route { .. } => {}
    });

    let routes: Vec<(&T, &BackendId, Ipv4Addr)> = plans
        .iter()
        .filter_map(|(config, plan)| match plan {
            Upkeep::Route { backend_id, ip } => Some((*config, backend_id, *ip)),
            _ => None,
        })
        .collect();

    restore_routes(&routes, sozu_socket_path);
}

fn target_state<T: Deployments>(id: u32) -> Result<TargetState> {
    match T::exists(id)? {
        false => Ok(TargetState::Vacant),
        true => T::tags(id).map(|tags| classify_target(true, tags.as_deref())),
    }
}

fn classify_target(exists: bool, tags: Option<&str>) -> TargetState {
    match (exists, is_proxnix_managed(tags)) {
        (false, _) => TargetState::Vacant,
        (true, true) => TargetState::Managed,
        (true, false) => TargetState::Unmanaged,
    }
}

fn destroy_managed<T: Deployments>(id: u32) -> Result<()> {
    T::set_protection(id, false)?;
    T::stop(&id)?;
    T::destroy(id)
}

fn prepare_target<T: Deployments>(name: &str, target: SlotId) -> Result<()> {
    let id = target.inner();
    match target_state::<T>(id)? {
        TargetState::Vacant => Ok(()),
        TargetState::Managed => {
            info!(
                "[{}] reclaiming Proxnix-managed inactive slot {} before provisioning",
                name, id
            );
            destroy_managed::<T>(id)
        }
        TargetState::Unmanaged => Err(AppError::CmdError(format!(
            "refusing to replace instance {} because it is not tagged 'proxnix'",
            id
        ))),
    }
}

fn abort<T: Deployments>(target: SlotId) {
    let id = target.inner();
    match target_state::<T>(id) {
        Ok(TargetState::Vacant) => {}
        Ok(TargetState::Unmanaged) => warn!(
            "abort: refusing to destroy {}, it is not tagged 'proxnix'",
            id
        ),
        Ok(TargetState::Managed) => {
            warn!("deploy failed, destroying Proxnix-managed instance {}", id);
            if let Err(e) = destroy_managed::<T>(id) {
                warn!("abort: could not destroy {}: {}", id, e);
            }
        }
        Err(e) => warn!("abort: could not inspect {}: {}", id, e),
    }
}

pub trait DeployedState {
    fn id(&self) -> u32;
    fn name(&self) -> &str;
    fn nix_hash(&self) -> Option<&NixHash>;
    fn status(&self) -> &str;
    fn active_slot(&self) -> Slot;
    fn service_ip(&self) -> Option<Ipv4Addr>;
}

impl DeployedState for DeployedVM {
    fn id(&self) -> u32 {
        self.vm_id
    }
    fn name(&self) -> &str {
        &self.vm_name
    }
    fn nix_hash(&self) -> Option<&NixHash> {
        self.nix_hash.as_ref()
    }
    fn status(&self) -> &str {
        &self.status
    }
    fn active_slot(&self) -> Slot {
        self.active_slot
    }
    fn service_ip(&self) -> Option<Ipv4Addr> {
        self.service_ip
    }
}

impl DeployedState for DeployedContainer {
    fn id(&self) -> u32 {
        self.ct_id
    }
    fn name(&self) -> &str {
        &self.ct_name
    }
    fn nix_hash(&self) -> Option<&NixHash> {
        self.nix_hash.as_ref()
    }
    fn status(&self) -> &str {
        &self.status
    }
    fn active_slot(&self) -> Slot {
        self.active_slot
    }
    fn service_ip(&self) -> Option<Ipv4Addr> {
        self.service_ip
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
    type Deployed: DeployedState + Send + Sync;
    type FieldChange: PartialEq + Clone + Send + Sync;

    fn load_deployed() -> Result<HashMap<String, Self::Deployed>>;

    fn compute_changes(
        &self,
        deployed: &Self::Deployed,
        image_hashes: &HashMap<crate::context::ImageType, NixHash>,
    ) -> Vec<Self::FieldChange>;
    fn requires_rebuild(changes: &[Self::FieldChange]) -> bool;

    fn is_protected(&self) -> bool;

    fn get_ip(id: u32) -> Result<String>;
    fn exists(id: u32) -> Result<bool>;
    fn tags(id: u32) -> Result<Option<String>>;
    fn set_tags(id: u32, tags: &Tags) -> Result<()>;
    fn set_protection(id: u32, protected: bool) -> Result<()>;
    fn stop(id: &u32) -> Result<()>;
    fn destroy(id: u32) -> Result<()>;
    fn start(id: u32) -> Result<bool>;
    fn apply_in_place(&self, deployed_id: u32, changes: &[Self::FieldChange]) -> Result<()>;
}

impl Deployments for VMConfig {
    type Deployed = DeployedVM;
    type FieldChange = FieldChange;
    fn load_deployed() -> Result<HashMap<String, Self::Deployed>> {
        let qm_raw = qm_list()?;
        let parsed_qm_list = parse_qm_list(&qm_raw)?;
        let deployed_vms = list_to_deployed_vm(parsed_qm_list);
        let enriched = enrich_cpu_info(deployed_vms)?;
        Ok(enriched.vms)
    }
    fn compute_changes(
        &self,
        deployed: &Self::Deployed,
        image_hashes: &HashMap<crate::context::ImageType, NixHash>,
    ) -> Vec<Self::FieldChange> {
        let desired_nix_hash = image_hashes.get(self.image_type());
        let image_changed = desired_nix_hash
            .zip(deployed.nix_hash.as_ref())
            .map(|(desired, deployed)| desired != deployed)
            .unwrap_or(true);
        [
            (self.memory_mb != deployed.mem_mb, FieldChange::Memory),
            (self.disk_gb > deployed.bootdisk_gb.round() as u32, FieldChange::Disk),
            (self.cores != deployed.cores, FieldChange::Cores),
            (self.sockets != deployed.sockets, FieldChange::Sockets),
            (image_changed, FieldChange::Image),
        ]
        .into_iter()
        .filter_map(|(changed, field)| changed.then_some(field))
        .collect()
    }
    fn requires_rebuild(changes: &[Self::FieldChange]) -> bool {
        changes.iter().any(|s| matches!(s, FieldChange::Image | FieldChange::Disk))
    }
    fn is_protected(&self) -> bool {
        self.protected
    }
    fn get_ip(id: u32) -> Result<String> {
        qm_get_running_ip(&id)
    }
    fn exists(id: u32) -> Result<bool> {
        vm_exists(id)
    }
    fn tags(id: u32) -> Result<Option<String>> {
        vm_tags(id)
    }
    fn set_tags(id: u32, tags: &Tags) -> Result<()> {
        qm_set_tags(id, tags)
    }
    fn set_protection(id: u32, protected: bool) -> Result<()> {
        qm_set_protection(id, protected)
    }
    fn stop(id: &u32) -> Result<()> {
        qm_stop(id)
    }
    fn destroy(id: u32) -> Result<()> {
        qm_destroy(id)
    }
    fn start(id: u32) -> Result<bool> {
        qm_start(id)
    }
    fn apply_in_place(&self, deployed_id: u32, changes: &[Self::FieldChange]) -> Result<()> {
        qm_set_resources(deployed_id, self, changes)
    }
}

impl Deployments for ContainerConfig {
    type Deployed = DeployedContainer;
    type FieldChange = ContainerFieldChange;
    fn load_deployed() -> Result<HashMap<String, Self::Deployed>> {
        let pct_raw = pct_list()?;
        let pct_entries = parse_pct_list(&pct_raw)?;
        enrich_container_info(pct_entries)
    }
    fn compute_changes(
        &self,
        deployed: &Self::Deployed,
        image_hashes: &HashMap<crate::context::ImageType, NixHash>,
    ) -> Vec<Self::FieldChange> {
        let desired_nix_hash = image_hashes.get(self.image_type());
        let image_changed = desired_nix_hash
            .zip(deployed.nix_hash.as_ref())
            .map(|(desired, deployed)| desired != deployed)
            .unwrap_or(true);
        [
            (self.memory_mb != deployed.mem_mb, ContainerFieldChange::Memory),
            (self.disk_gb > deployed.bootdisk_gb.round() as u32, ContainerFieldChange::Disk),
            (self.cores != deployed.cores, ContainerFieldChange::Cores),
            (image_changed, ContainerFieldChange::Image),
        ]
        .into_iter()
        .filter_map(|(changed, field)| changed.then_some(field))
        .collect()
    }
    fn requires_rebuild(changes: &[Self::FieldChange]) -> bool {
        changes.iter().any(|s| matches!(s, ContainerFieldChange::Image | ContainerFieldChange::Disk))
    }
    fn is_protected(&self) -> bool {
        self.protected
    }
    fn get_ip(id: u32) -> Result<String> {
        let output = std::process::Command::new("lxc-info")
            .arg("-n")
            .arg(id.to_string())
            .arg("-i")
            .arg("-H")
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AppError::CmdError(format!(
                "lxc-info failed for container {}: {}",
                id, stderr
            )));
        }
        String::from_utf8(output.stdout)?
            .lines()
            .find_map(|line| line.trim().parse::<Ipv4Addr>().ok())
            .map(|ip| ip.to_string())
            .ok_or_else(|| AppError::CmdError(format!("no IPv4 found for container {}", id)))
    }
    fn exists(id: u32) -> Result<bool> {
        container_exists(id)
    }
    fn tags(id: u32) -> Result<Option<String>> {
        container_tags(id)
    }
    fn set_tags(id: u32, tags: &Tags) -> Result<()> {
        pct_set_tags(id, tags)
    }
    fn set_protection(id: u32, protected: bool) -> Result<()> {
        pct_set_protection(id, protected)
    }
    fn stop(id: &u32) -> Result<()> {
        pct_stop(id)
    }
    fn destroy(id: u32) -> Result<()> {
        pct_destroy(id)
    }
    fn start(id: u32) -> Result<bool> {
        pct_start(id)
    }
    fn apply_in_place(&self, deployed_id: u32, changes: &[Self::FieldChange]) -> Result<()> {
        pct_set_resources(deployed_id, self, changes)
    }
}

enum Action<'a, T: Deployments> {
    Create {
        config: &'a T,
    },
    Rebuild {
        config: &'a T,
        deployed: &'a T::Deployed,
        deployed_id: u32,
    },
    UpdateInPlace {
        config: &'a T,
        deployed_id: u32,
        service_ip: Option<Ipv4Addr>,
        changes: Vec<T::FieldChange>,
    },
    Destroy {
        name: String,
        id: u32,
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
    let deployed = T::load_deployed()?;
    let actions = plan(configs, &deployed, ctx.image_hashes);
    Ok(actions
        .into_par_iter()
        .map(|action| match action {
            Action::Create { config } => {
                let result = (|| -> Result<()> {
                    let artifact = get_artifact(config, ctx)?;
                    DeployContext::from_create(
                        config,
                        artifact,
                        ctx.commit_hash.as_str(),
                        ctx.template_cache_path.as_str(),
                        ctx.sozu_socket_path.as_str(),
                        ctx.backend_pool,
                    )?.run()
                })();
                Outcome::new(config.name(), OutcomeKind::Created, result)
            }
            Action::Rebuild { config, deployed, deployed_id } => {
                let result = (|| -> Result<()> {
                    let artifact = get_artifact(config, ctx)?;
                    DeployContext::from_rebuild(
                        config,
                        deployed,
                        deployed_id,
                        artifact,
                        ctx.commit_hash.as_str(),
                        ctx.template_cache_path.as_str(),
                        ctx.sozu_socket_path.as_str(),
                        ctx.backend_pool,
                    )?.run()
                })();
                Outcome::new(config.name(), OutcomeKind::Rebuilt, result)
            }
            Action::UpdateInPlace { config, deployed_id, service_ip, changes } => {
                let result = (|| -> Result<()> {
                    config.pre_check()?;
                    config.apply_in_place(deployed_id, &changes)?;
                    config.post_check()?;
                    match service_ip {
                        Some(ip) => {
                            config.health_check(SocketAddr::from((ip, config.backend_port())))
                        }
                        None => Ok(()),
                    }
                })();
                Outcome::new(config.name(), OutcomeKind::Updated, result)
            }
            Action::Destroy { name, id } => {
                let result = (|| -> Result<()> {
                    let mut sozu = SozuClient::connect(ctx.sozu_socket_path.as_str())?;
                    match sozu.remove_cluster(&name) {
                        Ok(_) => {}
                        Err(e) => info!(
                            "[{}] sozu had no cluster to remove ({}), continuing with teardown",
                            name, e
                        ),
                    }
                    info!("[{}] destroying orphaned instance {}", name, id);
                    T::stop(&id)?;
                    T::destroy(id)
                })();
                Outcome::new(&name, OutcomeKind::Destroyed, result)
            }
            Action::Skip { name, reason } => {
                Outcome::new(&name, OutcomeKind::Skipped(reason), Ok(()))
            }
            Action::NoOp { name } => Outcome::new(&name, OutcomeKind::NoOp, Ok(())),
        })
        .collect())
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
    deployed: &'a HashMap<String, T::Deployed>,
    image_hashes: &HashMap<crate::context::ImageType, NixHash>,
) -> Vec<Action<'a, T>> {
    let desired: HashSet<&str> = configs.iter().map(|c| c.name()).collect();

    configs
        .iter()
        .map(|c| classify(c, deployed, image_hashes))
        .chain(
            deployed
                .iter()
                .filter(|(n, _)| !desired.contains(n.as_str()))
                .map(|(n, d)| Action::Destroy {
                    name: n.clone(),
                    id: d.id(),
                }),
        )
        .collect()
}

fn classify<'a, T: Deployments>(
    config: &'a T,
    deployed: &'a HashMap<String, T::Deployed>,
    image_hashes: &HashMap<crate::context::ImageType, NixHash>,
) -> Action<'a, T> {
    let Some(d) = deployed.get(config.name()) else {
        return Action::Create { config };
    };

    let changes = config.compute_changes(d, image_hashes);

    match (changes.is_empty(), T::requires_rebuild(&changes), config.is_protected()) {
        (true, _, _) => Action::NoOp { name: config.name().into() },
        (_, _, true) => Action::Skip { name: config.name().into(), reason: SkipReason::Protected },
        (_, true, _) => Action::Rebuild {
            config,
            deployed: d,
            deployed_id: d.id(),
        },
        (_, false, _) => Action::UpdateInPlace {
            config,
            deployed_id: d.id(),
            service_ip: d.service_ip(),
            changes,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ImageType;

    fn vm(protected: bool) -> VMConfig {
        VMConfig {
            name: "test-website".to_string(),
            blue_id: 823,
            green_id: 824,
            hostname: "test-website".to_string(),
            service_address: Some(Ipv4Addr::new(192, 168, 1, 23)),
            backend_port: 80,
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
            scsi_hw: "virtio-scsi-pci".to_string(),
            disk_slot: "scsi0".to_string(),
            impure: false,
        }
    }

    fn deployed(hash: &str, slot: Slot) -> DeployedVM {
        DeployedVM {
            vm_id: match slot {
                Slot::Blue => 823,
                Slot::Green => 824,
            },
            vm_name: "test-website".to_string(),
            nix_hash: Some(NixHash::try_from(hash).unwrap()),
            template_id: None,
            mem_mb: 2048,
            bootdisk_gb: 10.0,
            status: "running".to_string(),
            pid: 1234,
            cores: 2,
            sockets: 1,
            active_slot: slot,
            service_ip: Some(Ipv4Addr::new(10, 0, 0, 10)),
        }
    }

    fn hashes(hash: &str) -> HashMap<ImageType, NixHash> {
        HashMap::from([(
            ImageType::from("build-qcow2-website"),
            NixHash::try_from(hash).unwrap(),
        )])
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
                assert_eq!(backend_id.as_str(), deploy_time.as_str())
            }
            _ => panic!("expected a route"),
        }
    }

    #[test]
    fn a_stopped_workload_is_started_and_not_routed() {
        let config = vm(false);
        let stopped = DeployedVM {
            status: "stopped".to_string(),
            ..deployed("abc123", Slot::Blue)
        };
        match upkeep(&config, Some(&stopped)) {
            Upkeep::Start { id, status } => {
                assert_eq!(id, 823);
                assert_eq!(status, "stopped");
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
        let untagged = DeployedVM { service_ip: None, ..deployed("abc123", Slot::Blue) };
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
        assert_eq!(c.id_for_slot(Slot::Blue), SlotId::Blue(823));
        assert_eq!(c.id_for_slot(Slot::Green), SlotId::Green(824));
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
        assert!(VMConfig::requires_rebuild(&changes));
    }

    #[test]
    fn a_memory_change_alone_does_not_require_rebuild() {
        let c = VMConfig { memory_mb: 4096, ..vm(false) };
        let d = deployed("abc123", Slot::Blue);
        let changes = c.compute_changes(&d, &hashes("abc123"));
        assert_eq!(changes, vec![FieldChange::Memory]);
        assert!(!VMConfig::requires_rebuild(&changes));
    }

    #[test]
    fn a_disk_grow_requires_rebuild() {
        let c = VMConfig { disk_gb: 20, ..vm(false) };
        let d = deployed("abc123", Slot::Blue);
        let changes = c.compute_changes(&d, &hashes("abc123"));
        assert!(VMConfig::requires_rebuild(&changes));
    }

    #[test]
    fn an_unbuilt_image_counts_as_changed() {
        let c = vm(false);
        let d = deployed("abc123", Slot::Blue);
        let changes = c.compute_changes(&d, &HashMap::new());
        assert!(changes.contains(&FieldChange::Image));
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
            Action::Rebuild { deployed, deployed_id, .. } => {
                assert_eq!(deployed_id, 824);
                assert_eq!(deployed.active_slot(), Slot::Green);
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
                assert_eq!(deployed_id, 824);
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
                assert_eq!(*id, 823);
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
