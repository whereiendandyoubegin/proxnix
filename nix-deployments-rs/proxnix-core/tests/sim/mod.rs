#![allow(dead_code)]

use proxnix_core::{
    Artifact, Audited, BridgeName, Built, BySlot, Cores, Cutover, Desired, Detail, DiskGib,
    DurationMs, Effect, EffectError, Endpoint, Event, Grant, GuestEffect, GuestName, GuestStatus,
    Hostname, ImageType, Images, Input, KindFacts, KindSpec, Memo, MemoryMb, Moment, Observation,
    Outcome, Pacing, Permissions, Port, Privilege, ProbeEffect, ProxySpec, Purity, Push, RawTags,
    Registry, Report, ResourceChange, Resources, RouteEffect, Settled, Sighting, Slot, SlotPair,
    Sockets, Stage, Tick, Timeouts, Unsettled, Vmid, WorkloadSpec, step,
};
use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;

pub const NIX_A: &str = "78s0iadvjz6s48aqvx4rw78lwrzkjzlw";
pub const NIX_B: &str = "i3d00236fdkfw1v9cmasajkjhzl8zi5j";
pub const COMMIT_A: &str = "66d0ba6b605de2703e0fb7bbf58b922d5b36597e";
pub const COMMIT_B: &str = "9d6c5a707946c3ccde19b6f5cd2f9bd365caadeb";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimTags {
    pub nix: String,
    pub commit: String,
    pub slot: Slot,
    pub ip: Option<Ipv4Addr>,
    pub generation: Option<u64>,
    pub pending: bool,
    pub role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimGuest {
    pub name: String,
    pub tags: Option<SimTags>,
    pub running: bool,
    pub resources: Resources,
    pub facts: KindFacts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimRoute {
    pub generation: u64,
    pub nix: String,
    pub address: Ipv4Addr,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Faults {
    pub fail: BTreeSet<usize>,
    pub flaky: BTreeSet<usize>,
    pub unhealthy: BTreeSet<Vmid>,
    pub silent: BTreeSet<Vmid>,
    pub die_after: Option<usize>,
    pub lying: BTreeSet<usize>,
    pub address_after: BTreeMap<Vmid, usize>,
    pub failing_checks: BTreeSet<Vmid>,
    pub crashing: BTreeSet<Vmid>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct World {
    pub guests: BTreeMap<Vmid, SimGuest>,
    pub unsettled: BTreeMap<Vmid, Unsettled>,
    pub routes: BTreeMap<(String, Endpoint), SimRoute>,
    pub faults: Faults,
    pub applied: usize,
    pub reads: BTreeMap<Vmid, usize>,
    pub costed: bool,
}

pub fn cost(effect: &Effect, outcome: &Outcome) -> u64 {
    match (effect, outcome) {
        (Effect::Guest(GuestEffect::Create { .. }), _) => 120_000,
        (Effect::Probe(ProbeEffect::GuestCheck(_)), Outcome::Failed(_)) => 60_000,
        (Effect::Probe(ProbeEffect::PortOpen { .. }), Outcome::Failed(_)) => 2_000,
        _ => 1_000,
    }
}

pub fn render(tags: &SimTags) -> String {
    let slot = match tags.slot {
        Slot::Blue => "blue",
        Slot::Green => "green",
    };
    [
        Some(String::from("proxnix")),
        Some(format!("nix-{}", tags.nix)),
        Some(format!("commit-{}", tags.commit)),
        Some(format!("slot-{slot}")),
        tags.ip.map(|ip| format!("ip-{ip}")),
        tags.generation
            .map(|generation| format!("gen-{generation}")),
        tags.pending.then(|| String::from("pending")),
        tags.role.as_ref().map(|role| format!("role-{role}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(";")
}

pub fn lease(id: Vmid) -> Ipv4Addr {
    Ipv4Addr::new(
        10,
        0,
        u8::try_from(id.get() / 250).unwrap(),
        u8::try_from(id.get() % 250).unwrap() + 1,
    )
}

pub fn qemu() -> KindSpec {
    KindSpec::Qemu {
        sockets: Sockets(1),
    }
}

pub fn lxc() -> KindSpec {
    KindSpec::Lxc {
        privilege: Privilege::Unprivileged,
        mounts: BySlot::default(),
    }
}

pub fn spec(
    name: &str,
    blue: u32,
    green: u32,
    kind: KindSpec,
    cutover: Cutover,
    routed: bool,
) -> WorkloadSpec {
    WorkloadSpec {
        name: GuestName(String::from(name)),
        slots: SlotPair::new(Vmid::new(blue), Vmid::new(green)).unwrap(),
        image: ImageType(format!("build-{name}")),
        resources: Resources {
            memory: MemoryMb(2048),
            disk: DiskGib(10),
            cores: Cores(2),
        },
        cutover,
        purity: Purity::Pure,
        proxy: ProxySpec {
            hostname: Hostname(format!("{name}.thesta.rs")),
            service_address: routed
                .then(|| Ipv4Addr::new(192, 168, 1, u8::try_from(blue % 250).unwrap())),
            backend_port: Port(80),
            tcp_ports: vec![],
            bridge: BridgeName(String::from("vmbr0")),
        },
        timeouts: Timeouts {
            dhcp: DurationMs(240_000),
            health_check: DurationMs(180_000),
        },
        kind,
    }
}

pub fn pacing() -> Pacing {
    Pacing {
        address: DurationMs(2_000),
        port: DurationMs(2_000),
        guest: DurationMs(3_000),
    }
}

pub fn push(commit: &str) -> Tick {
    Tick::Push(Push::new(commit.parse().unwrap()))
}

pub fn built(spec: &WorkloadSpec, nix: &str) -> Built {
    Built {
        image: spec.image.clone(),
        outcome: Ok(Artifact {
            path: format!("/nix/store/{nix}-{}", spec.name.0).parse().unwrap(),
        }),
    }
}

impl World {
    pub fn with_guest(self, id: Vmid, guest: SimGuest) -> World {
        World {
            guests: self.guests.into_iter().chain([(id, guest)]).collect(),
            ..self
        }
    }

    pub fn legacy(self, spec: &WorkloadSpec, slot: Slot, nix: &str) -> World {
        let id = spec.slots.id(slot).inner();
        let routes = match spec.proxy.service_address {
            Some(_) => self
                .routes
                .clone()
                .into_iter()
                .chain([(
                    (spec.name.0.clone(), Endpoint::Primary),
                    SimRoute {
                        generation: 0,
                        nix: String::from(nix),
                        address: lease(id),
                    },
                )])
                .collect(),
            None => self.routes.clone(),
        };
        World { routes, ..self }.with_guest(
            id,
            SimGuest {
                name: spec.name.0.clone(),
                tags: Some(SimTags {
                    nix: String::from(nix),
                    commit: String::from(COMMIT_A),
                    slot,
                    ip: Some(lease(id)),
                    generation: None,
                    pending: false,
                    role: None,
                }),
                running: true,
                resources: spec.resources,
                facts: spec.kind.facts(slot),
            },
        )
    }

    pub fn half_made(self, id: Vmid, why: Unsettled) -> World {
        World {
            unsettled: self.unsettled.into_iter().chain([(id, why)]).collect(),
            ..self
        }
    }

    pub fn unmanaged(self, id: Vmid, name: &str) -> World {
        self.with_guest(
            id,
            SimGuest {
                name: String::from(name),
                tags: None,
                running: true,
                resources: Resources {
                    memory: MemoryMb(1024),
                    disk: DiskGib(8),
                    cores: Cores(1),
                },
                facts: KindFacts::Qemu {
                    sockets: Sockets(1),
                },
            },
        )
    }

    pub fn observe(&self) -> Observation {
        Observation::new(
            Audited::try_from(Permissions {
                vm_audit: Grant::Granted,
            })
            .unwrap(),
            self.guests
                .iter()
                .map(|(id, guest)| {
                    Sighting::Settled(Settled {
                        id: *id,
                        name: GuestName(guest.name.clone()),
                        status: if guest.running {
                            GuestStatus::Running
                        } else {
                            GuestStatus::Stopped
                        },
                        tags: RawTags::from(guest.tags.as_ref().map(render).unwrap_or_default()),
                        resources: guest.resources,
                        facts: guest.facts.clone(),
                    })
                })
                .chain(
                    self.unsettled
                        .iter()
                        .map(|(id, why)| Sighting::Unsettled(*id, *why)),
                )
                .collect(),
        )
    }

    pub fn members(&self, name: &str) -> Vec<(Vmid, &SimGuest)> {
        self.guests
            .iter()
            .filter(|(_, guest)| guest.name == name && guest.tags.is_some())
            .map(|(id, guest)| (*id, guest))
            .collect()
    }

    pub fn serving(&self, name: &str) -> Option<(Vmid, &SimGuest)> {
        self.members(name)
            .into_iter()
            .filter(|(_, guest)| {
                guest
                    .tags
                    .as_ref()
                    .and_then(|tags| tags.generation)
                    .is_some()
            })
            .max_by_key(|(_, guest)| guest.tags.as_ref().and_then(|tags| tags.generation))
    }

    fn update(self, id: Vmid, change: impl FnOnce(SimGuest) -> SimGuest) -> World {
        match self.guests.get(&id).cloned() {
            Some(guest) => self.with_guest(id, change(guest)),
            None => self,
        }
    }

    fn retag(self, id: Vmid, change: impl FnOnce(SimTags) -> SimTags) -> World {
        self.update(id, |guest| SimGuest {
            tags: guest.tags.map(change),
            ..guest
        })
    }

    fn remove(self, id: Vmid) -> World {
        World {
            guests: self
                .guests
                .into_iter()
                .filter(|(key, _)| *key != id)
                .collect(),
            ..self
        }
    }

    fn healthy(&self, id: Vmid) -> bool {
        self.guests.get(&id).is_some_and(|guest| guest.running)
            && !self.faults.unhealthy.contains(&id)
    }

    fn apply_guest(self, effect: &GuestEffect) -> (World, Outcome) {
        match effect {
            GuestEffect::Create {
                target,
                spec,
                fresh,
                ..
            } => {
                let tags = SimTags {
                    nix: String::from(fresh.nix().as_ref()),
                    commit: String::from(fresh.commit().as_ref()),
                    slot: fresh.slot(),
                    ip: None,
                    generation: None,
                    pending: true,
                    role: fresh.role().map(|role| String::from(role.as_ref())),
                };
                let guest = SimGuest {
                    name: spec.name.0.clone(),
                    tags: Some(tags),
                    running: false,
                    resources: spec.resources,
                    facts: spec.kind.facts(fresh.slot()),
                };
                (self.with_guest(target.id(), guest), Outcome::Done)
            }
            GuestEffect::Start(member) => {
                let stays_up = !self.faults.crashing.contains(&member.id());
                (
                    self.update(member.id(), |guest| SimGuest {
                        running: stays_up,
                        ..guest
                    }),
                    Outcome::Done,
                )
            }
            GuestEffect::Stop(member) => (
                self.update(member.id(), |guest| SimGuest {
                    running: false,
                    ..guest
                }),
                Outcome::Done,
            ),
            GuestEffect::Record { guest, address } => (
                self.retag(guest.id(), |tags| SimTags {
                    ip: Some(*address),
                    ..tags
                }),
                Outcome::Done,
            ),
            GuestEffect::Role { guest, role } => (
                self.retag(guest.id(), |tags| SimTags {
                    role: Some(String::from(role.as_ref())),
                    ..tags
                }),
                Outcome::Done,
            ),
            GuestEffect::Commit(promotion) => (
                self.retag(promotion.guest().id(), |tags| SimTags {
                    generation: Some(promotion.generation().get()),
                    pending: false,
                    ..tags
                }),
                Outcome::Done,
            ),
            GuestEffect::Update { guest, changes } => (
                self.update(guest.id(), |sim| SimGuest {
                    resources: changes.iter().fold(
                        sim.resources,
                        |resources, change| match change {
                            ResourceChange::Memory(memory) => Resources {
                                memory: *memory,
                                ..resources
                            },
                            ResourceChange::Cores(cores) => Resources {
                                cores: *cores,
                                ..resources
                            },
                            ResourceChange::Sockets(_) => resources,
                        },
                    ),
                    facts: changes.iter().fold(sim.facts.clone(), |facts, change| {
                        match (change, facts) {
                            (ResourceChange::Sockets(sockets), KindFacts::Qemu { .. }) => {
                                KindFacts::Qemu { sockets: *sockets }
                            }
                            (_, facts) => facts,
                        }
                    }),
                    ..sim
                }),
                Outcome::Done,
            ),
            GuestEffect::Undo(provisioned) => (self.remove(provisioned.id()), Outcome::Done),
            GuestEffect::Reclaim(doomed) | GuestEffect::Retire(doomed) => {
                (self.remove(doomed.id()), Outcome::Done)
            }
        }
    }

    fn apply_probe(self, probe: &ProbeEffect) -> (World, Outcome) {
        let id = probe.guest().id();
        let read = self.reads.get(&id).copied().unwrap_or(0) + 1;
        let reads = match probe {
            ProbeEffect::ReadAddress(_) => {
                self.reads.clone().into_iter().chain([(id, read)]).collect()
            }
            _ => self.reads.clone(),
        };
        let answered = self
            .faults
            .address_after
            .get(&id)
            .is_none_or(|after| read >= *after);
        let outcome = match probe {
            ProbeEffect::ReadAddress(_)
                if self.guests.get(&id).is_some_and(|guest| guest.running) =>
            {
                if self.faults.silent.contains(&id) || !answered {
                    Outcome::Address(Ipv4Addr::new(169, 254, 1, 1))
                } else {
                    Outcome::Address(lease(id))
                }
            }
            ProbeEffect::PortOpen { address, .. } if self.healthy(id) && *address == lease(id) => {
                Outcome::Done
            }
            ProbeEffect::GuestCheck(_)
                if self.healthy(id) && !self.faults.failing_checks.contains(&id) =>
            {
                Outcome::Done
            }
            _ => Outcome::Failed(EffectError::Unreachable(Detail(String::from(
                "probe failed",
            )))),
        };
        (World { reads, ..self }, outcome)
    }

    fn apply_route(self, route: &RouteEffect) -> (World, Outcome) {
        match route {
            RouteEffect::Point { name, to, .. } | RouteEffect::Restore { name, to, .. } => {
                let backing = self.guests.values().find(|guest| {
                    guest.name == name.0
                        && guest.tags.as_ref().is_some_and(|tags| {
                            tags.ip == Some(to.address())
                                && tags.generation == Some(to.generation().get())
                                && tags.nix == to.nix().as_ref()
                        })
                });
                assert!(
                    backing.is_some(),
                    "route {name:?} -> {to:?} does not match a guest's gen and nix"
                );
                let route = SimRoute {
                    generation: to.generation().get(),
                    nix: String::from(to.nix().as_ref()),
                    address: to.address(),
                };
                let routes = self
                    .routes
                    .clone()
                    .into_iter()
                    .chain([((name.0.clone(), to.endpoint()), route)])
                    .collect();
                (World { routes, ..self }, Outcome::Done)
            }
            RouteEffect::RemoveCluster(name) => {
                let routes = self
                    .routes
                    .clone()
                    .into_iter()
                    .filter(|((owner, _), _)| *owner != name.0)
                    .collect();
                (World { routes, ..self }, Outcome::Done)
            }
        }
    }

    pub fn apply(self, effect: &Effect) -> (World, Outcome) {
        let index = self.applied;
        let counted = World {
            applied: index + 1,
            ..self
        };
        if counted.faults.fail.contains(&index) {
            return (
                counted,
                Outcome::Failed(EffectError::Refused(Detail(String::from("injected")))),
            );
        }
        if counted.faults.flaky.contains(&index) {
            return (
                counted,
                Outcome::Failed(EffectError::Unreachable(Detail(String::from(
                    "command socket reset",
                )))),
            );
        }
        let (world, outcome) = match effect {
            Effect::Guest(guest) => counted.apply_guest(guest),
            Effect::Probe(probe) => counted.apply_probe(probe),
            Effect::Route(route) => counted.apply_route(route),
        };
        let outcome = if world.faults.lying.contains(&index) {
            Outcome::Failed(EffectError::TaskFailed(Detail(String::from(
                "applied, then reported as failed",
            ))))
        } else {
            outcome
        };
        match world.faults.die_after {
            Some(after) if after == index => (world.stop_everything(), outcome),
            _ => (world, outcome),
        }
    }

    fn stop_everything(self) -> World {
        World {
            guests: self
                .guests
                .into_iter()
                .map(|(id, guest)| {
                    (
                        id,
                        SimGuest {
                            running: false,
                            ..guest
                        },
                    )
                })
                .collect(),
            ..self
        }
    }
}

pub fn destroys(effect: &Effect) -> Option<Vmid> {
    match effect {
        Effect::Guest(
            guest @ (GuestEffect::Undo(_) | GuestEffect::Reclaim(_) | GuestEffect::Retire(_)),
        ) => Some(guest.id()),
        _ => None,
    }
}

pub fn guard(world: &World, effect: &Effect, desired: &Desired, tick: &Tick) {
    if let Effect::Guest(guest) = effect {
        let id = guest.id();
        match guest {
            GuestEffect::Create { .. } => assert!(
                !world.guests.contains_key(&id) && !world.unsettled.contains_key(&id),
                "created into occupied {id:?}"
            ),
            _ => assert!(
                world.guests.get(&id).is_some_and(|sim| sim.tags.is_some()),
                "{effect:?} targets {id:?}, which is not a managed guest"
            ),
        }
        if let GuestEffect::Start(member) = guest {
            let sim = world.guests.get(&member.id()).unwrap();
            let generation = sim.tags.as_ref().and_then(|tags| tags.generation);
            let highest = world
                .members(&sim.name)
                .iter()
                .filter_map(|(_, other)| other.tags.as_ref().and_then(|tags| tags.generation))
                .max();
            assert!(
                generation.is_none() || generation == highest,
                "started {id:?} at gen {generation:?} while gen {highest:?} exists"
            );
        }
        if matches!(tick, Tick::Periodic) {
            assert!(
                !matches!(
                    guest,
                    GuestEffect::Create { .. } | GuestEffect::Update { .. }
                ),
                "a periodic tick emitted a config-derived {effect:?}"
            );
        }
    }
    if let Some(id) = destroys(effect) {
        let sim = world.guests.get(&id).unwrap();
        let declared = desired.declares(&GuestName(sim.name.clone()));
        if declared {
            assert_ne!(
                world.serving(&sim.name).map(|(serving, _)| serving),
                Some(id),
                "destroyed the serving guest {id:?}"
            );
            let routed = world
                .routes
                .get(&(sim.name.clone(), Endpoint::Primary))
                .map(|route| route.address);
            assert!(
                routed.is_none() || routed != sim.tags.as_ref().and_then(|tags| tags.ip),
                "destroyed {id:?} while it still receives traffic"
            );
        } else {
            assert!(
                matches!(tick, Tick::Push(_)),
                "an orphan was destroyed on a periodic tick"
            );
        }
    }
}

#[derive(Debug, Clone)]
pub struct Run {
    pub world: World,
    pub effects: Vec<Effect>,
    pub reports: Vec<Report>,
    pub steps: usize,
}

pub fn run<R: Registry>(
    world: World,
    desired: &Desired,
    images: &Images,
    tick: &Tick,
    crash_at: Option<usize>,
) -> Run {
    let pace = pacing();
    let limit = 2_000;
    let start = (
        world,
        Memo::default(),
        Vec::<Event>::new(),
        Moment(0),
        Vec::<Effect>::new(),
        Vec::<Report>::new(),
    );
    let finished = (0..limit).try_fold(
        start,
        |(world, memo, events, now, effects, reports), index| {
            let observed = world.observe();
            let stepped = step::<R>(Input {
                memo,
                desired,
                images,
                observed: &observed,
                events,
                now,
                tick,
                pacing: &pace,
            });
            let reports: Vec<Report> = reports
                .into_iter()
                .chain([stepped.report.clone()])
                .collect();
            if stepped.quiescent() {
                return Err(Box::new(Run {
                    world,
                    effects,
                    reports,
                    steps: index,
                }));
            }
            let (world, events, spent) = stepped.effects.iter().fold(
                (world, Vec::new(), 0),
                |(world, events, spent), planned| {
                    guard(&world, &planned.effect, desired, tick);
                    let (world, outcome) = world.apply(&planned.effect);
                    let spent = spent + cost(&planned.effect, &outcome);
                    (
                        world,
                        events
                            .into_iter()
                            .chain([Event {
                                effect: planned.id,
                                outcome,
                            }])
                            .collect::<Vec<_>>(),
                        spent,
                    )
                },
            );
            let effects = effects
                .into_iter()
                .chain(stepped.effects.iter().map(|planned| planned.effect.clone()))
                .collect();
            let memo = if crash_at == Some(index) {
                Memo::default()
            } else {
                stepped.memo
            };
            let now = match (stepped.effects.is_empty(), stepped.wake) {
                (true, Some(wake)) => wake.max(Moment(now.0 + 1)),
                _ if world.costed => Moment(now.0 + spent.max(500)),
                _ => Moment(now.0 + 500),
            };
            Ok((world, memo, events, now, effects, reports))
        },
    );
    match finished {
        Err(done) => *done,
        Ok(_) => panic!("the simulation did not settle within {limit} steps"),
    }
}

pub fn settled(world: &World, desired: &Desired) {
    for spec in desired.valid() {
        let name = &spec.name.0;
        if world.members(name).is_empty() {
            continue;
        }
        let (id, serving) = world
            .serving(name)
            .unwrap_or_else(|| panic!("{name} has guests but none is serving: {world:#?}"));
        assert!(
            serving.running,
            "{name}'s serving guest {id:?} is not running"
        );
        if spec.proxy.service_address.is_some() {
            let tags = serving.tags.as_ref().unwrap();
            assert_eq!(
                world.routes.get(&(name.clone(), Endpoint::Primary)),
                Some(&SimRoute {
                    generation: tags.generation.unwrap(),
                    nix: tags.nix.clone(),
                    address: tags.ip.unwrap()
                }),
                "{name}'s route does not point at its serving guest"
            );
        }
    }
}

pub fn last_stage(run: &Run, name: &str) -> Stage {
    run.reports
        .iter()
        .rev()
        .find_map(|report| {
            report
                .workloads
                .iter()
                .find(|workload| workload.name.0 == name)
                .map(|workload| workload.stage.clone())
        })
        .unwrap()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Create,
    Start,
    Stop,
    Record,
    Role,
    Commit,
    Update,
    Undo,
    Reclaim,
    Retire,
    ReadAddress,
    PortOpen,
    GuestCheck,
    Point,
    Restore,
    RemoveCluster,
}

pub fn kind(effect: &Effect) -> Kind {
    match effect {
        Effect::Guest(GuestEffect::Create { .. }) => Kind::Create,
        Effect::Guest(GuestEffect::Start(_)) => Kind::Start,
        Effect::Guest(GuestEffect::Stop(_)) => Kind::Stop,
        Effect::Guest(GuestEffect::Record { .. }) => Kind::Record,
        Effect::Guest(GuestEffect::Role { .. }) => Kind::Role,
        Effect::Guest(GuestEffect::Commit(_)) => Kind::Commit,
        Effect::Guest(GuestEffect::Update { .. }) => Kind::Update,
        Effect::Guest(GuestEffect::Undo(_)) => Kind::Undo,
        Effect::Guest(GuestEffect::Reclaim(_)) => Kind::Reclaim,
        Effect::Guest(GuestEffect::Retire(_)) => Kind::Retire,
        Effect::Probe(ProbeEffect::ReadAddress(_)) => Kind::ReadAddress,
        Effect::Probe(ProbeEffect::PortOpen { .. }) => Kind::PortOpen,
        Effect::Probe(ProbeEffect::GuestCheck(_)) => Kind::GuestCheck,
        Effect::Route(RouteEffect::Point { .. }) => Kind::Point,
        Effect::Route(RouteEffect::Restore { .. }) => Kind::Restore,
        Effect::Route(RouteEffect::RemoveCluster(_)) => Kind::RemoveCluster,
    }
}

pub fn kinds(effects: &[Effect]) -> Vec<Kind> {
    effects
        .iter()
        .map(kind)
        .fold(Vec::new(), |seen, next| match seen.last() {
            Some(last)
                if *last == next
                    && matches!(next, Kind::ReadAddress | Kind::PortOpen | Kind::GuestCheck) =>
            {
                seen
            }
            _ => seen.into_iter().chain([next]).collect(),
        })
}

pub fn images(built: Vec<Built>) -> Images {
    built.into_iter().collect()
}
