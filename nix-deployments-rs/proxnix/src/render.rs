use proxnix_core::{
    Backend, Effect, GuestEffect, NixHash, ProbeEffect, Projection, ResourceChange, RouteEffect, Scope, Slot, Stage, Vmid,
    WorkloadReport, placeholder_address,
};
use std::net::Ipv4Addr;

fn id(id: Vmid) -> String {
    id.get().to_string()
}

fn short(nix: &NixHash) -> &str {
    &nix.as_ref()[..8]
}

fn address(ip: Ipv4Addr) -> String {
    if ip.octets()[..3] == placeholder_address(Vmid::new(0)).octets()[..3] { String::from("<address from dhcp>") } else { ip.to_string() }
}

fn backend(to: &Backend) -> String {
    format!("gen-{} nix {} at {}", to.generation().get(), short(to.nix()), address(to.address()))
}

fn slot(slot: Slot) -> &'static str {
    match slot {
        Slot::Blue => "blue",
        Slot::Green => "green",
    }
}

fn change(change: ResourceChange) -> String {
    match change {
        ResourceChange::Memory(memory) => format!("memory {} MB", memory.0),
        ResourceChange::Cores(cores) => format!("{} cores", cores.0),
        ResourceChange::Sockets(sockets) => format!("{} sockets", sockets.0),
    }
}

pub fn effect(effect: &Effect, created: &[Vmid]) -> String {
    match effect {
        Effect::Guest(GuestEffect::Create { target, fresh, artifact, .. }) => format!(
            "create {} in the {} slot from nix {} ({})",
            id(target.id()),
            slot(fresh.slot()),
            short(artifact.nix()),
            artifact.path.name().as_ref()
        ),
        Effect::Guest(GuestEffect::Start(member)) => format!("start {}", id(member.id())),
        Effect::Guest(GuestEffect::Stop(member)) => format!("stop {} (fence)", id(member.id())),
        Effect::Guest(GuestEffect::Record { guest, address: ip }) => format!("record {}'s address {}", id(guest.id()), address(*ip)),
        Effect::Guest(GuestEffect::Role { guest, role }) => format!("tag {} role-{}", id(guest.id()), role.as_ref()),
        Effect::Guest(GuestEffect::Commit(promotion)) => {
            let guest = promotion.guest();
            if created.contains(&guest.id()) {
                format!("commit {} as gen-{} (point of no return)", id(guest.id()), promotion.generation().get())
            } else {
                format!("adopt {} as gen-{} (tag only, it keeps serving)", id(guest.id()), promotion.generation().get())
            }
        }
        Effect::Guest(GuestEffect::Update { guest, changes }) => {
            format!("set {} on {}", changes.iter().copied().map(change).collect::<Vec<_>>().join(", "), id(guest.id()))
        }
        Effect::Guest(GuestEffect::Undo(provisioned)) => format!("undo the create of {}", id(provisioned.id())),
        Effect::Guest(GuestEffect::Reclaim(doomed)) => format!("reclaim {} (unprotect, stop, destroy)", id(doomed.id())),
        Effect::Guest(GuestEffect::Retire(doomed)) => format!("retire {} (stop, destroy)", id(doomed.id())),
        Effect::Probe(ProbeEffect::ReadAddress(member)) => format!("wait for {} to get an address", id(member.id())),
        Effect::Probe(ProbeEffect::PortOpen { guest, port, .. }) => format!("check port {} on {}", port.0, id(guest.id())),
        Effect::Probe(ProbeEffect::GuestCheck(member)) => format!("run the guest health check on {}", id(member.id())),
        Effect::Route(RouteEffect::Point { name, to, from, .. }) => format!(
            "route {} to {}{}",
            name.0,
            backend(to),
            from.as_ref().map_or_else(String::new, |old| format!(", replacing {}", backend(old)))
        ),
        Effect::Route(RouteEffect::Restore { name, to, .. }) => format!("restore route {} to {}", name.0, backend(to)),
        Effect::Route(RouteEffect::RemoveCluster(name)) => format!("remove sozu cluster {}", name.0),
    }
}

fn stage(report: &WorkloadReport) -> String {
    format!("{}: {:?}", report.name.0, report.stage)
}

fn needs_attention(stage: &Stage) -> bool {
    matches!(stage, Stage::Failed(_) | Stage::Blocked(_) | Stage::Conflict(_) | Stage::Invalid(_) | Stage::Skipped(_))
}

pub fn host(effects: &[proxnix_core::HostEffect]) -> String {
    [String::from("== host (ensured before any create, never during --plan)")]
        .into_iter()
        .chain(effects.iter().map(|effect| format!("  {}", crate::host::host_effect_text(effect))))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn plan(commit: &proxnix_core::CommitHash, projections: &[Projection]) -> String {
    let header = format!("proxnix plan for commit {}\naddresses marked <address from dhcp> are only known once the guest boots\n", commit.as_ref());
    let sections = projections.iter().map(|projection| {
        let title = match &projection.scope {
            Scope::Workload(name) => format!("== {}", name.0),
            Scope::Teardown => String::from("== teardown (workloads no longer in the config, and config errors)"),
        };
        let created: Vec<Vmid> = projection
            .effects
            .iter()
            .filter_map(|planned| match planned {
                Effect::Guest(GuestEffect::Create { target, .. }) => Some(target.id()),
                _ => None,
            })
            .collect();
        let steps: Vec<String> = projection
            .effects
            .iter()
            .enumerate()
            .map(|(index, planned)| format!("  {:>2}. {}", index + 1, effect(planned, &created)))
            .collect();
        let outcome: Vec<String> = projection
            .last
            .workloads
            .iter()
            .map(|report| format!("  {} {}", if needs_attention(&report.stage) { "!!" } else { "->" }, stage(report)))
            .collect();
        let anomalies: Vec<String> = projection.first.anomalies.iter().map(|anomaly| format!("  !! anomaly: {anomaly:?}")).collect();
        let settled = if projection.settled { Vec::new() } else { vec![String::from("  !! the projection did not settle")] };
        [vec![title], if steps.is_empty() { vec![String::from("  (nothing to do)")] } else { steps }, outcome, anomalies, settled].concat().join("\n")
    });
    std::iter::once(header).chain(sections).collect::<Vec<_>>().join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxnix_core::{
        Artifact, Audited, Builtin, Built, BridgeName, Cores, Cutover, Desired, DiskGib, DurationMs, Grant, GuestName, GuestStatus,
        Hostname, ImageType, Images, KindFacts, KindSpec, MemoryMb, Observation, Pacing, Permissions, Port, Privilege, ProxySpec,
        Purity, Push, RawTags, Resources, Settled, Sighting, SlotPair, Tick, Timeouts, WorkloadSpec, project,
    };

    const OLD: &str = "78s0iadvjz6s48aqvx4rw78lwrzkjzlw";
    const NEW: &str = "i3d00236fdkfw1v9cmasajkjhzl8zi5j";
    const COMMIT: &str = "66d0ba6b605de2703e0fb7bbf58b922d5b36597e";

    #[test]
    fn a_rebuild_plan_reads_as_the_deploy_it_will_be() {
        let spec = WorkloadSpec {
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
                tcp_ports: vec![],
                bridge: BridgeName(String::from("vmbr0")),
            },
            timeouts: Timeouts { dhcp: DurationMs(240_000), health_check: DurationMs(180_000) },
            kind: KindSpec::Lxc { privilege: Privilege::Unprivileged, mounts: vec![] },
        };
        let observed = Observation::new(
            Audited::try_from(Permissions { vm_audit: Grant::Granted }).unwrap(),
            vec![Sighting::Settled(Settled {
                id: Vmid::new(844),
                name: GuestName(String::from("forgejo")),
                status: GuestStatus::Running,
                tags: RawTags::from(format!("commit-{COMMIT};ip-192.168.1.214;nix-{OLD};proxnix;slot-blue")),
                resources: spec.resources,
                facts: KindFacts::Lxc { privilege: Privilege::Unprivileged, mounts: vec![] },
            })],
        );
        let images: Images = [Built {
            image: spec.image.clone(),
            outcome: Ok(Artifact { path: format!("/nix/store/{NEW}-nixos-system-forgejo").parse().unwrap() }),
        }]
        .into_iter()
        .collect();
        let commit: proxnix_core::CommitHash = COMMIT.parse().unwrap();
        let pacing = Pacing { address: DurationMs(2000), port: DurationMs(2000), guest: DurationMs(3000) };
        let projections = project::<Builtin>(&Desired::validate(vec![spec]), &images, &observed, &Tick::Push(Push::new(commit.clone())), &pacing);
        let text = plan(&commit, &projections);
        let expected = [
            "== forgejo",
            "   1. adopt 844 as gen-1 (tag only, it keeps serving)",
            "   2. create 944 in the green slot from nix i3d00236 (nixos-system-forgejo)",
            "   3. start 944",
            "   4. wait for 944 to get an address",
            "   5. record 944's address <address from dhcp>",
            "   6. check port 3000 on 944",
            "   7. run the guest health check on 944",
            "   8. commit 944 as gen-2 (point of no return)",
            "   9. route forgejo to gen-2 nix i3d00236 at <address from dhcp>, replacing gen-1 nix 78s0iadv at 192.168.1.214",
            "  10. retire 844 (stop, destroy)",
            "  -> forgejo: Converged",
            "== teardown (workloads no longer in the config, and config errors)",
            "  (nothing to do)",
        ];
        let lines: Vec<&str> = text.lines().collect();
        let positions: Vec<Option<usize>> = expected.iter().map(|line| lines.iter().position(|seen| seen == line)).collect();
        assert!(positions.iter().all(Option::is_some), "missing lines in:\n{text}");
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "lines out of order in:\n{text}");
    }
}
