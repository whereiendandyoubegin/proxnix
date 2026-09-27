use crate::context::{ImageStore, RepoPath, StorePath, Tags};
use crate::interpret::{Declared, Interpreter, Probes, Provision, Routes};
use crate::materialise::Materialise;
use crate::nix::NixFault;
use crate::probe::{ExecOutcome, exec, guest_check_script};
use crate::remote::{Api, ApiError};
use crate::types::{AppConfig, AppError, BindMount, ContainerConfig, MountMode as ShellMountMode, Result, Timing, VMConfig};
use proxmox_api::client::Client;
use rayon::prelude::*;
use proxnix_core::{
    Artifact, BridgeName, BuildFault, Built, ConfigFault, ExitCode, Cores, Cutover, Desired, Detail, DiskGib, DurationMs, GuestKind,
    GuestName, GuestPath, HostPath, ImageType, Images, KindSpec, MemoryMb, Memo, Moment, Mount, MountMode, Observation,
    Pacing, Port, Privilege, ProxySpec, Purity, Registry, Report, SlotId, SlotPair, Sockets, Tick, Timeouts, Vmid,
    WorkloadSpec, step,
};
use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpStream};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};
use tracing::info;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildTarget {
    Qcow2,
    Tarball,
}

impl BuildTarget {
    fn attr(self) -> &'static str {
        match self {
            BuildTarget::Qcow2 => "config.system.build.qcow2",
            BuildTarget::Tarball => "config.system.build.tarball",
        }
    }
}

fn millis(seconds: u64) -> DurationMs {
    DurationMs(seconds.saturating_mul(1000))
}

fn proxy(hostname: &str, service_address: Option<std::net::Ipv4Addr>, backend_port: u16, tcp_ports: &[u16], bridge: &str) -> ProxySpec {
    ProxySpec {
        hostname: proxnix_core::Hostname(hostname.to_string()),
        service_address,
        backend_port: Port(backend_port),
        tcp_ports: tcp_ports.iter().copied().map(Port).collect(),
        bridge: BridgeName(bridge.to_string()),
    }
}

fn mount(mount: &BindMount) -> Mount {
    Mount {
        host: HostPath(mount.host_path.clone()),
        guest: GuestPath(mount.container_path.clone()),
        mode: match mount.mode {
            ShellMountMode::ReadOnly => MountMode::ReadOnly,
            ShellMountMode::ReadWrite => MountMode::ReadWrite,
        },
    }
}

impl Declared {
    pub fn name(&self) -> GuestName {
        GuestName(match self {
            Declared::Vm(config) => config.name.clone(),
            Declared::Container(config) => config.name.clone(),
        })
    }

    pub fn image(&self) -> ImageType {
        ImageType(match self {
            Declared::Vm(config) => config.image_type.as_str().to_string(),
            Declared::Container(config) => config.image_type.as_str().to_string(),
        })
    }

    pub fn target(&self) -> BuildTarget {
        match self {
            Declared::Vm(_) => BuildTarget::Qcow2,
            Declared::Container(_) => BuildTarget::Tarball,
        }
    }

    pub fn spec(&self) -> std::result::Result<WorkloadSpec, ConfigFault> {
        match self {
            Declared::Vm(config) => vm_spec(config),
            Declared::Container(config) => container_spec(config),
        }
    }
}

fn slots(blue: Vmid, green: Vmid) -> std::result::Result<SlotPair, ConfigFault> {
    SlotPair::new(blue, green).map_err(|same| ConfigFault::SameIdInBothSlots(same.0))
}

fn cutover(protected: bool, choice: Option<crate::types::CutoverChoice>) -> Cutover {
    match (protected, choice) {
        (true, _) => Cutover::Protected,
        (false, None | Some(crate::types::CutoverChoice::Overlap)) => Cutover::Overlap,
        (false, Some(crate::types::CutoverChoice::StopStart)) => Cutover::StopStart,
    }
}

fn purity(impure: bool) -> Purity {
    if impure { Purity::Impure } else { Purity::Pure }
}

fn vm_spec(config: &VMConfig) -> std::result::Result<WorkloadSpec, ConfigFault> {
    Ok(WorkloadSpec {
        name: GuestName(config.name.clone()),
        slots: slots(config.blue_id, config.green_id)?,
        image: ImageType(config.image_type.as_str().to_string()),
        resources: proxnix_core::Resources { memory: MemoryMb(config.memory_mb), disk: DiskGib(config.disk_gb), cores: Cores(config.cores) },
        cutover: cutover(config.protected, config.cutover),
        purity: purity(config.impure),
        proxy: proxy(&config.hostname, config.service_address, config.backend_port, &config.tcp_ports, &config.network_bridge),
        timeouts: Timeouts { dhcp: millis(config.dhcp_timeout_seconds), health_check: millis(config.health_check_timeout_seconds) },
        kind: KindSpec::Qemu { sockets: Sockets(config.sockets) },
    })
}

fn container_spec(config: &ContainerConfig) -> std::result::Result<WorkloadSpec, ConfigFault> {
    Ok(WorkloadSpec {
        name: GuestName(config.name.clone()),
        slots: slots(config.blue_id, config.green_id)?,
        image: ImageType(config.image_type.as_str().to_string()),
        resources: proxnix_core::Resources { memory: MemoryMb(config.memory_mb), disk: DiskGib(config.disk_gb), cores: Cores(config.cores) },
        cutover: cutover(config.protected, config.cutover),
        purity: purity(config.impure),
        proxy: proxy(&config.hostname, config.service_address, config.backend_port, &config.tcp_ports, &config.network_bridge),
        timeouts: Timeouts { dhcp: millis(config.dhcp_timeout_seconds), health_check: millis(config.health_check_timeout_seconds) },
        kind: KindSpec::Lxc {
            privilege: if config.privileged { Privilege::Privileged } else { Privilege::Unprivileged },
            mounts: config.bind_mounts.iter().map(mount).collect(),
        },
    })
}

pub fn declare(desired: crate::types::DesiredState) -> BTreeMap<GuestName, Declared> {
    desired
        .vms
        .into_values()
        .map(Declared::Vm)
        .chain(desired.containers.into_values().map(Declared::Container))
        .map(|declared| (declared.name(), declared))
        .collect()
}

pub fn desired(declared: &BTreeMap<GuestName, Declared>) -> Desired {
    let (specs, rejected): (Vec<_>, Vec<_>) = declared.values().map(|declared| (declared.name(), declared.spec())).partition(|(_, spec)| spec.is_ok());
    Desired::validate_with(
        specs.into_iter().filter_map(|(_, spec)| spec.ok()).collect(),
        rejected.into_iter().filter_map(|(name, spec)| spec.err().map(|fault| (name, fault))).collect(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Realisation {
    Build,
    Evaluate,
}

fn fault(fault: NixFault, stage: Realisation) -> BuildFault {
    let failed = |code: Option<i32>, detail: String| match stage {
        Realisation::Build => BuildFault::Build(code.map(ExitCode), Detail(detail)),
        Realisation::Evaluate => BuildFault::Eval(code.map(ExitCode), Detail(detail)),
    };
    match fault {
        NixFault::TimedOut(after) => BuildFault::TimedOut(DurationMs(u64::try_from(after.as_millis()).unwrap_or(u64::MAX))),
        NixFault::Exited { code, stderr } => failed(code, stderr),
        NixFault::NoFlake(detail) => BuildFault::Checkout(Detail(detail)),
        NixFault::Spawn(detail) => failed(None, detail),
        NixFault::NoOutput => failed(None, String::from("nix printed no store path")),
    }
}

impl Declared {
    fn impure(&self) -> bool {
        match self {
            Declared::Vm(config) => config.impure,
            Declared::Container(config) => config.impure,
        }
    }
}

pub fn build(repo: RepoPath<'_>, declared: &Declared, timeout: Duration, stage: Realisation) -> Built {
    let image = declared.image();
    let attr = declared.target().attr();
    let raw = match stage {
        Realisation::Build => crate::nix::realise(&image.0, attr, repo.as_str(), declared.impure(), timeout),
        Realisation::Evaluate => crate::nix::out_path(&image.0, attr, repo.as_str(), declared.impure(), timeout),
    };
    let outcome = raw
        .map_err(|nix| fault(nix, stage))
        .and_then(|text| text.trim().parse().map_err(BuildFault::Output))
        .map(|path| Artifact { path });
    Built { image, outcome }
}

pub fn build_all(repo: RepoPath<'_>, declared: &BTreeMap<GuestName, Declared>, timeout: Duration, stage: Realisation) -> Images {
    let unique: BTreeMap<ImageType, &Declared> = declared.values().map(|declared| (declared.image(), declared)).collect();
    let built: Vec<Built> = unique
        .into_values()
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|declared| {
            info!("{} image '{}'", if stage == Realisation::Build { "building" } else { "evaluating" }, declared.image().0);
            build(repo, declared, timeout, stage)
        })
        .collect();
    built.into_iter().collect()
}

pub fn pacing(settings: &AppConfig) -> Pacing {
    let ms = |timing| DurationMs(u64::try_from(settings.timings_ms.get(timing).as_millis()).unwrap_or(u64::MAX));
    Pacing { address: ms(Timing::AddressPoll), port: ms(Timing::PortPoll), guest: ms(Timing::GuestCheckPoll) }
}

pub struct LiveProbes<'a> {
    pub settings: &'a AppConfig,
}

impl Probes for LiveProbes<'_> {
    fn port_open(&self, address: SocketAddr) -> bool {
        TcpStream::connect_timeout(&address, self.settings.timings_ms.get(Timing::PortPoll)).is_ok()
    }

    fn guest_check(&self, id: Vmid, kind: GuestKind) -> Result<bool> {
        match kind {
            GuestKind::Qemu => Ok(true),
            GuestKind::Lxc => {
                let check = &self.settings.guest_check;
                let script = guest_check_script(&check.command);
                let argv = [check.shell.as_str(), "-c", script.as_str()];
                exec(id, &argv, self.settings.timings_ms.get(Timing::GuestCheckRun)).map(|outcome| matches!(outcome, ExecOutcome::Succeeded { .. }))
            }
        }
    }
}

pub struct HostProvision<'a> {
    pub image_store: ImageStore<'a>,
    pub lock: &'a Mutex<()>,
}

impl Provision for HostProvision<'_> {
    fn create(&self, declared: &Declared, artifact: &StorePath, tags: &Tags, target: SlotId) -> Result<()> {
        let _storage = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        match declared {
            Declared::Vm(config) => config.provision_inactive(artifact, tags, self.image_store, target),
            Declared::Container(config) => config.provision_inactive(artifact, tags, self.image_store, target),
        }
    }
}

pub struct Clock(Instant);

impl Clock {
    pub fn start() -> Clock {
        Clock(Instant::now())
    }

    fn now(&self) -> Moment {
        Moment(u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX))
    }

    fn sleep_until(&self, wake: Moment) {
        std::thread::sleep(Duration::from_millis(wake.0.saturating_sub(self.now().0)));
    }
}

pub struct Inputs<'a> {
    pub desired: &'a Desired,
    pub images: &'a Images,
    pub tick: &'a Tick,
    pub pacing: &'a Pacing,
    pub limit: usize,
}

pub fn drive<Reg: Registry, C: Client, R: Routes, P: Probes, M: Provision>(
    interpreter: &mut Interpreter<'_, C, R, P, M>,
    observe: impl Fn() -> Result<Observation>,
    inputs: &Inputs<'_>,
    clock: &Clock,
) -> Result<Report>
where
    C::Error: ApiError,
{
    let finished = (0..inputs.limit).try_fold((Memo::default(), Vec::new()), |(memo, events), _| {
        let observed = match observe() {
            Ok(observed) => observed,
            Err(error) => return Err(Err(error)),
        };
        let stepped = step::<Reg>(proxnix_core::Input {
            memo,
            desired: inputs.desired,
            images: inputs.images,
            observed: &observed,
            events,
            now: clock.now(),
            tick: inputs.tick,
            pacing: inputs.pacing,
        });
        if stepped.quiescent() {
            return Err(Ok(stepped.report));
        }
        let events = interpreter.execute(&stepped.effects);
        if let (true, Some(wake)) = (stepped.effects.is_empty(), stepped.wake) {
            clock.sleep_until(wake);
        }
        Ok((stepped.memo, events))
    });
    match finished {
        Err(done) => done,
        Ok(_) => Err(AppError::CmdError(format!("the deploy loop did not settle within {} ticks", inputs.limit))),
    }
}

struct Shared<'a, R>(&'a Mutex<R>);

impl<R: Routes> Shared<'_, R> {
    fn with<T>(&self, act: impl FnOnce(&mut R) -> T) -> T {
        act(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl<R: Routes> Routes for Shared<'_, R> {
    fn ensure_cluster(&mut self, target: &crate::interpret::Routed) -> Result<crate::sozu::Settled> {
        self.with(|routes| routes.ensure_cluster(target))
    }
    fn register_backend(&mut self, target: &crate::interpret::Routed, id: &crate::context::BackendId, ip: std::net::Ipv4Addr) -> Result<crate::sozu::Settled> {
        self.with(|routes| routes.register_backend(target, id, ip))
    }
    fn prune_backends(&mut self, target: &crate::interpret::Routed, keep: std::net::Ipv4Addr) -> Result<crate::sozu::Pruned> {
        self.with(|routes| routes.prune_backends(target, keep))
    }
    fn remove_backend(&mut self, target: &crate::interpret::Routed, id: &crate::context::BackendId, ip: std::net::Ipv4Addr) -> Result<()> {
        self.with(|routes| routes.remove_backend(target, id, ip))
    }
    fn register_tcp_backends(&mut self, target: &crate::interpret::Routed, id: &crate::context::BackendId, ip: std::net::Ipv4Addr) -> Result<crate::sozu::Settled> {
        self.with(|routes| routes.register_tcp_backends(target, id, ip))
    }
    fn prune_tcp_backends(&mut self, target: &crate::interpret::Routed, keep: std::net::Ipv4Addr) -> Result<crate::sozu::Pruned> {
        self.with(|routes| routes.prune_tcp_backends(target, keep))
    }
    fn remove_tcp_backends(&mut self, target: &crate::interpret::Routed, id: &crate::context::BackendId, ip: std::net::Ipv4Addr) {
        self.with(|routes| routes.remove_tcp_backends(target, id, ip));
    }
    fn remove_cluster(&mut self, name: &GuestName) -> Result<()> {
        self.with(|routes| routes.remove_cluster(name))
    }
    fn remove_tcp_clusters(&mut self, name: &GuestName) -> Result<usize> {
        self.with(|routes| routes.remove_tcp_clusters(name))
    }
}

pub struct Host<'a, C> {
    pub api: &'a Api<C>,
    pub settings: &'a AppConfig,
    pub image_store: ImageStore<'a>,
    pub declared: &'a BTreeMap<GuestName, Declared>,
}

pub fn drive_all<Reg: Registry, C: Client + Sync, R: Routes + Send>(
    host: &Host<'_, C>,
    routes: R,
    observe: &(dyn Fn() -> Result<Observation> + Sync),
    inputs: &Inputs<'_>,
    clock: &Clock,
) -> Vec<(GuestName, Result<Report>)>
where
    C::Error: ApiError,
{
    let shared = Mutex::new(routes);
    let lock = Mutex::new(());
    let stagger = host.settings.timings_ms.get(Timing::ProvisionStagger);
    let loop_for = |desired: &Desired| {
        let mut interpreter = Interpreter {
            api: host.api,
            routes: Shared(&shared),
            probes: LiveProbes { settings: host.settings },
            provision: HostProvision { image_store: host.image_store, lock: &lock },
            declared: host.declared,
        };
        drive::<Reg, C, _, _, _>(&mut interpreter, observe, &Inputs { desired, ..*inputs }, clock)
    };
    let names = inputs.desired.names();
    let workloads: Vec<(GuestName, Result<Report>)> = std::thread::scope(|scope| {
        let running: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let loop_for = &loop_for;
                (
                    name.clone(),
                    scope.spawn(move || {
                        std::thread::sleep(stagger.saturating_mul(u32::try_from(index).unwrap_or(u32::MAX)));
                        loop_for(&inputs.desired.workload(name))
                    }),
                )
            })
            .collect();
        running
            .into_iter()
            .map(|(name, handle)| {
                let result = handle
                    .join()
                    .unwrap_or_else(|_| Err(AppError::CmdError(format!("the deploy loop for {} panicked", name.0))));
                (name, result)
            })
            .collect()
    });
    let teardown = loop_for(&inputs.desired.teardown());
    workloads.into_iter().chain([(GuestName(String::from("(teardown)")), teardown)]).collect()
}

pub struct Prepared {
    pub commit: proxnix_core::CommitHash,
    pub declared: BTreeMap<GuestName, Declared>,
    pub desired: Desired,
    groups: Vec<crate::pipeline::WorkloadGroup>,
}

pub fn prepare(settings: &AppConfig, repo: &str) -> Result<Prepared> {
    let head = crate::git::git_head_commit(repo)?;
    let commit = head
        .parse::<proxnix_core::CommitHash>()
        .map_err(|fault| AppError::GitError(format!("HEAD of {repo} is not a commit hash ({fault:?}): {head}")))?;
    let state = crate::state::parse_config(&crate::nix::eval_config(repo, settings.timings_ms.get(Timing::NixEval))?)?;
    let groups = state.clone().into_workload_groups();
    let declared = declare(state);
    let desired = desired(&declared);
    Ok(Prepared { commit, declared, desired, groups })
}

pub fn plan(settings: &AppConfig, pve: &crate::pve::Pve, repo: &str) -> Result<(Prepared, Vec<proxnix_core::Projection>)> {
    let prepared = prepare(settings, repo)?;
    let images = build_all(RepoPath::try_from(repo)?, &prepared.declared, settings.timings_ms.get(Timing::NixEval), Realisation::Evaluate);
    let observed = crate::state::observe(pve)?;
    let tick = Tick::Push(proxnix_core::Push::new(prepared.commit.clone()));
    let projections = proxnix_core::project::<proxnix_core::Builtin>(&prepared.desired, &images, &observed, &tick, &pacing(settings));
    Ok((prepared, projections))
}

fn live_hashes(images: &Images, declared: &BTreeMap<GuestName, Declared>) -> std::collections::HashSet<crate::context::NixHash> {
    declared
        .values()
        .filter_map(|declared| match images.knowledge(&declared.image()) {
            proxnix_core::Knowledge::Built(artifact) => crate::context::NixHash::try_from(artifact.nix().as_ref()).ok(),
            _ => None,
        })
        .collect()
}

pub fn deploy(settings: &AppConfig, pve: &crate::pve::Pve, repo: &str) -> Result<Vec<(GuestName, Result<Report>)>> {
    let prepared = prepare(settings, repo)?;
    crate::pipeline::hold_service_addresses(&prepared.groups, settings.backend_pool.as_ref(), settings.timings_ms.get(Timing::ArpProbe))?;
    let images = build_all(RepoPath::try_from(repo)?, &prepared.declared, settings.timings_ms.get(Timing::NixBuild), Realisation::Build);
    let api = pve.api(settings.timings_ms.get(Timing::TaskPoll), settings.timings_ms.get(Timing::TaskTimeout));
    let host = Host {
        api: &api,
        settings,
        image_store: ImageStore {
            template_cache_path: crate::context::TemplateCachePath::try_from(settings.template_cache_path.as_str())?,
            zfs: settings.zfs_images.as_ref(),
            idmap: settings.unprivileged_idmap,
        },
        declared: &prepared.declared,
    };
    let tick = Tick::Push(proxnix_core::Push::new(prepared.commit.clone()));
    let pace = pacing(settings);
    let inputs = Inputs { desired: &prepared.desired, images: &images, tick: &tick, pacing: &pace, limit: 1_000_000 };
    let observe = || crate::state::observe(pve);
    let outcomes = drive_all::<proxnix_core::Builtin, _, _>(&host, crate::sozu::SozuClient::connect(settings)?, &observe, &inputs, &Clock::start());
    let live = live_hashes(&images, &prepared.declared);
    match crate::host::reap_template_cache(settings.template_cache_path.as_str(), &live) {
        Ok(reaped) if reaped.files > 0 => info!("reaped {} stale container templates", reaped.files),
        Ok(_) => {}
        Err(error) => tracing::warn!("could not reap the template cache: {}", error),
    }
    match settings.zfs_images.as_ref().map(|zfs| crate::zfs::reap_images(zfs, &live)) {
        Some(Ok(crate::zfs::ReapedImages(0))) | None => {}
        Some(Ok(crate::zfs::ReapedImages(count))) => info!("reaped {} stale base images", count),
        Some(Err(error)) => tracing::warn!("could not reap base images: {}", error),
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{parse_appconfig, parse_config};

    const EVAL: &str = r#"{
      "vms": {
        "test-website": {
          "name": "test-website", "hostname": "test-website", "blue_id": 823, "green_id": 923,
          "service_address": "192.168.1.23", "backend_port": 80, "dhcp_timeout_seconds": 240, "health_check_timeout_seconds": 180,
          "image_type": "build-qcow2-website", "cores": 2, "sockets": 1, "memory_mb": 2048, "disk_gb": 10,
          "storage_location": "local-lvm", "protected": false, "impure": false
        }
      },
      "containers": {
        "postgres": {
          "name": "postgres", "hostname": "postgres", "blue_id": 842, "green_id": 942, "tcp_ports": [5432],
          "dhcp_timeout_seconds": 240, "health_check_timeout_seconds": 180, "image_type": "build-lxc-postgres",
          "cores": 2, "memory_mb": 2048, "disk_gb": 10, "storage_location": "ZFS", "protected": true, "impure": false,
          "bind_mounts": [{"host_path": "/var/lib/proxnix/postgres", "container_path": "/var/lib/postgresql", "mode": "read_write"}]
        },
        "broken": {
          "name": "broken", "hostname": "broken", "blue_id": 850, "green_id": 850,
          "dhcp_timeout_seconds": 1, "health_check_timeout_seconds": 1, "image_type": "build-lxc-broken",
          "cores": 1, "memory_mb": 512, "disk_gb": 8, "storage_location": "ZFS", "protected": false, "impure": true
        }
      }
    }"#;

    fn declared() -> BTreeMap<GuestName, Declared> {
        declare(parse_config(EVAL).unwrap())
    }

    fn named(name: &str) -> GuestName {
        GuestName(String::from(name))
    }

    #[test]
    fn the_nix_config_becomes_core_specs_field_for_field() {
        let desired = desired(&declared());
        let specs: BTreeMap<GuestName, WorkloadSpec> = desired.valid().map(|spec| (spec.name.clone(), spec.clone())).collect();
        let website = &specs[&named("test-website")];
        assert_eq!(website.slots, SlotPair::new(Vmid::new(823), Vmid::new(923)).unwrap());
        assert_eq!(website.timeouts, Timeouts { dhcp: DurationMs(240_000), health_check: DurationMs(180_000) });
        assert_eq!(website.kind, KindSpec::Qemu { sockets: Sockets(1) });
        assert_eq!(website.cutover, Cutover::Overlap);
        assert_eq!(website.proxy.service_address, Some(std::net::Ipv4Addr::new(192, 168, 1, 23)));
        let postgres = &specs[&named("postgres")];
        assert_eq!(postgres.cutover, Cutover::Protected);
        assert_eq!(postgres.proxy.tcp_ports, vec![Port(5432)]);
        assert_eq!(
            postgres.kind,
            KindSpec::Lxc {
                privilege: Privilege::Unprivileged,
                mounts: vec![Mount {
                    host: HostPath(String::from("/var/lib/proxnix/postgres")),
                    guest: GuestPath(String::from("/var/lib/postgresql")),
                    mode: MountMode::ReadWrite,
                }],
            }
        );
    }

    #[test]
    fn a_workload_that_cannot_become_a_spec_stays_declared_so_it_is_never_torn_down() {
        let desired = desired(&declared());
        assert!(desired.declares(&named("broken")));
        assert_eq!(desired.invalid()[&named("broken")], vec![ConfigFault::SameIdInBothSlots(Vmid::new(850))]);
        assert!(!desired.valid().any(|spec| spec.name == named("broken")));
    }

    #[test]
    fn each_kind_builds_its_own_image_attribute() {
        let declared = declared();
        assert_eq!(declared[&named("test-website")].target(), BuildTarget::Qcow2);
        assert_eq!(declared[&named("postgres")].target(), BuildTarget::Tarball);
        assert_eq!(declared[&named("postgres")].image(), ImageType(String::from("build-lxc-postgres")));
    }

    #[test]
    fn pacing_comes_from_the_nixology_timings() {
        let settings = parse_appconfig(&crate::state::tests_support::NIXOLOGY_APPCONFIG.replace("\"port_poll\":2000", "\"port_poll\":2500")).unwrap();
        assert_eq!(pacing(&settings), Pacing { address: DurationMs(2000), port: DurationMs(2500), guest: DurationMs(3000) });
    }

    #[test]
    fn a_workload_can_ask_for_stop_start_but_protection_still_wins() {
        let container = |extra: serde_json::Value| -> ContainerConfig {
            let base = serde_json::json!({
                "name": "nixflix", "hostname": "media.thesta.rs", "dhcp_timeout_seconds": 240, "health_check_timeout_seconds": 900,
                "blue_id": 847, "green_id": 947, "image_type": "build-lxc-nixflix", "cores": 4, "memory_mb": 4096,
                "storage_location": "ZFS", "disk_gb": 16, "protected": false, "impure": false
            });
            let merged: serde_json::Map<String, serde_json::Value> =
                base.as_object().unwrap().clone().into_iter().chain(extra.as_object().unwrap().clone()).collect();
            serde_json::from_value(serde_json::Value::Object(merged)).unwrap()
        };
        assert_eq!(container_spec(&container(serde_json::json!({}))).unwrap().cutover, Cutover::Overlap);
        assert_eq!(container_spec(&container(serde_json::json!({"cutover": "stop_start"}))).unwrap().cutover, Cutover::StopStart);
        assert_eq!(
            container_spec(&container(serde_json::json!({"cutover": "stop_start", "protected": true}))).unwrap().cutover,
            Cutover::Protected
        );
        assert!(serde_json::from_value::<ContainerConfig>(serde_json::json!({"cutover": "sometimes"})).is_err());
    }
}
