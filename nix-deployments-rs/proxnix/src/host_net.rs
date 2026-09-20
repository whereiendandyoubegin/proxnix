use std::collections::BTreeSet;
use std::fmt;
use std::net::Ipv4Addr;
use std::process::{Command, ExitStatus};
use std::time::Duration;

use tracing::{error, info, warn};

use crate::types::{AppError, Result};

const PROBE_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceBinding {
    pub bridge: String,
    pub address: Ipv4Addr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeBindings {
    pub bridge: String,
    pub addresses: Vec<Ipv4Addr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeAddress {
    pub address: Ipv4Addr,
    pub prefix_len: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacAddress(String);

impl TryFrom<&str> for MacAddress {
    type Error = AppError;

    fn try_from(candidate: &str) -> Result<Self> {
        let octets: Vec<&str> = candidate.split(':').collect();
        let well_formed = octets.len() == 6
            && octets
                .iter()
                .all(|o| o.len() == 2 && o.chars().all(|c| c.is_ascii_hexdigit()));
        match well_formed {
            true => Ok(MacAddress(candidate.to_ascii_lowercase())),
            false => Err(AppError::MacParseError(candidate.to_string())),
        }
    }
}

impl fmt::Display for MacAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Responder {
    Mac(MacAddress),
    Unidentified,
}

impl fmt::Display for Responder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Responder::Mac(mac) => write!(f, "{}", mac),
            Responder::Unidentified => write!(f, "an unidentified host"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Occupancy {
    Free,
    Occupied { by: Responder },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prober {
    Arp,
    Neighbour,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArpProbe {
    NoReply,
    Replied,
}

impl TryFrom<ExitStatus> for ArpProbe {
    type Error = AppError;

    fn try_from(status: ExitStatus) -> Result<Self> {
        match status.code() {
            Some(0) => Ok(ArpProbe::NoReply),
            Some(1) => Ok(ArpProbe::Replied),
            Some(code) => Err(AppError::CmdError(format!(
                "arping could not probe the address (exit: {})",
                code
            ))),
            None => Err(AppError::CmdError(
                "arping was killed by a signal before it could probe the address".to_string(),
            )),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ServiceAddress {
    Held,
    Absent { prefix_len: u8 },
    NoPrefix,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Uniqueness {
    Unique,
    Clashing { duplicates: Vec<Ipv4Addr> },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AddressesHeld {
    pub added: usize,
    pub already: usize,
    pub conflicted: usize,
    pub failed: usize,
}

impl AddressesHeld {
    fn added(self) -> Self {
        Self { added: self.added + 1, ..self }
    }
    fn already(self) -> Self {
        Self { already: self.already + 1, ..self }
    }
    fn conflicted(self) -> Self {
        Self { conflicted: self.conflicted + 1, ..self }
    }
    fn failed(self) -> Self {
        Self { failed: self.failed + 1, ..self }
    }
}

pub fn parse_bridge_addresses(output: &str) -> Vec<BridgeAddress> {
    output
        .lines()
        .filter_map(|line| {
            let cidr = line
                .split_whitespace()
                .skip_while(|f| *f != "inet")
                .nth(1)?;
            let (addr, prefix) = cidr.split_once('/')?;
            Some(BridgeAddress {
                address: addr.parse().ok()?,
                prefix_len: prefix.parse().ok()?,
            })
        })
        .collect()
}

pub fn plan_service_address(held: &[BridgeAddress], wanted: Ipv4Addr) -> ServiceAddress {
    match held.iter().find(|h| h.address == wanted) {
        Some(_) => ServiceAddress::Held,
        None => match held.first() {
            Some(primary) => ServiceAddress::Absent { prefix_len: primary.prefix_len },
            None => ServiceAddress::NoPrefix,
        },
    }
}

pub fn read_arping(probe: ArpProbe, stdout: &str) -> Occupancy {
    match probe {
        ArpProbe::NoReply => Occupancy::Free,
        ArpProbe::Replied => Occupancy::Occupied { by: bracketed_mac(stdout) },
    }
}

fn bracketed_mac(stdout: &str) -> Responder {
    stdout
        .lines()
        .filter_map(|line| line.split_once('['))
        .filter_map(|(_, rest)| rest.split_once(']'))
        .find_map(|(candidate, _)| MacAddress::try_from(candidate).ok())
        .map_or(Responder::Unidentified, Responder::Mac)
}

pub fn parse_neighbour(stdout: &str) -> Occupancy {
    match stdout.lines().find(|l| l.contains(" lladdr ")) {
        None => Occupancy::Free,
        Some(line) => match line.split_whitespace().last() {
            Some("FAILED") | Some("INCOMPLETE") => Occupancy::Free,
            _ => Occupancy::Occupied { by: lladdr(line) },
        },
    }
}

fn lladdr(line: &str) -> Responder {
    line.split_whitespace()
        .skip_while(|f| *f != "lladdr")
        .nth(1)
        .and_then(|candidate| MacAddress::try_from(candidate).ok())
        .map_or(Responder::Unidentified, Responder::Mac)
}

pub fn check_uniqueness(bindings: &[ServiceBinding]) -> Uniqueness {
    let duplicates: Vec<Ipv4Addr> = bindings
        .iter()
        .map(|b| b.address)
        .filter(|address| bindings.iter().filter(|b| b.address == *address).count() > 1)
        .collect::<BTreeSet<Ipv4Addr>>()
        .into_iter()
        .collect();

    match duplicates.is_empty() {
        true => Uniqueness::Unique,
        false => Uniqueness::Clashing { duplicates },
    }
}

pub fn by_bridge(bindings: &[ServiceBinding]) -> Vec<BridgeBindings> {
    bindings
        .iter()
        .map(|b| b.bridge.as_str())
        .collect::<BTreeSet<&str>>()
        .into_iter()
        .map(|bridge| BridgeBindings {
            bridge: bridge.to_string(),
            addresses: bindings
                .iter()
                .filter(|b| b.bridge == bridge)
                .map(|b| b.address)
                .collect(),
        })
        .collect()
}

// --- Side effects ---

fn bridge_addresses(bridge: &str) -> Result<Vec<BridgeAddress>> {
    let output = Command::new("ip")
        .arg("-4")
        .arg("-o")
        .arg("addr")
        .arg("show")
        .arg("dev")
        .arg(bridge)
        .output()?;
    match output.status.success() {
        false => Err(AppError::CmdError(format!(
            "ip addr show dev {} failed (exit: {:?}): {}",
            bridge,
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        ))),
        true => Ok(parse_bridge_addresses(&String::from_utf8(output.stdout)?)),
    }
}

pub fn choose_prober() -> Prober {
    match Command::new("arping").arg("-V").output() {
        Ok(output) if output.status.success() => Prober::Arp,
        _ => {
            warn!(
                "arping is not available, falling back to the neighbour table to detect address conflicts; install iputils-arping for a reliable probe"
            );
            Prober::Neighbour
        }
    }
}

fn arp_probe(bridge: &str, address: Ipv4Addr) -> Result<Occupancy> {
    let output = Command::new("arping")
        .arg("-D")
        .arg("-c")
        .arg("1")
        .arg("-w")
        .arg(PROBE_WAIT.as_secs().to_string())
        .arg("-I")
        .arg(bridge)
        .arg(address.to_string())
        .output()?;
    Ok(read_arping(
        ArpProbe::try_from(output.status)?,
        &String::from_utf8(output.stdout)?,
    ))
}

fn neighbour_probe(bridge: &str, address: Ipv4Addr) -> Result<Occupancy> {
    Command::new("ping")
        .arg("-c")
        .arg("1")
        .arg("-W")
        .arg(PROBE_WAIT.as_secs().to_string())
        .arg("-I")
        .arg(bridge)
        .arg(address.to_string())
        .output()?;

    let output = Command::new("ip")
        .arg("neigh")
        .arg("show")
        .arg(address.to_string())
        .arg("dev")
        .arg(bridge)
        .output()?;
    Ok(parse_neighbour(&String::from_utf8(output.stdout)?))
}

fn probe(prober: Prober, bridge: &str, address: Ipv4Addr) -> Result<Occupancy> {
    match prober {
        Prober::Arp => arp_probe(bridge, address),
        Prober::Neighbour => neighbour_probe(bridge, address),
    }
}

fn add_address(bridge: &str, address: Ipv4Addr, prefix_len: u8) -> Result<()> {
    let output = Command::new("ip")
        .arg("addr")
        .arg("add")
        .arg(format!("{}/{}", address, prefix_len))
        .arg("dev")
        .arg(bridge)
        .output()?;
    match output.status.success() {
        true => Ok(()),
        false => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            match stderr.contains("File exists") {
                true => Ok(()),
                false => Err(AppError::CmdError(format!(
                    "ip addr add {}/{} dev {} failed (exit: {:?}): {}",
                    address,
                    prefix_len,
                    bridge,
                    output.status.code(),
                    stderr
                ))),
            }
        }
    }
}

fn claim(prober: Prober, bridge: &str, address: Ipv4Addr, prefix_len: u8) -> Result<()> {
    match probe(prober, bridge, address)? {
        Occupancy::Occupied { by } => Err(AppError::ServiceAddressConflict {
            address,
            bridge: bridge.to_string(),
            responder: by.to_string(),
        }),
        Occupancy::Free => add_address(bridge, address, prefix_len),
    }
}

pub fn ensure_service_addresses(
    prober: Prober,
    bridge: &str,
    wanted: &[Ipv4Addr],
) -> Result<AddressesHeld> {
    let held = bridge_addresses(bridge)?;
    Ok(wanted
        .iter()
        .fold(AddressesHeld::default(), |acc, address| {
            match plan_service_address(&held, *address) {
                ServiceAddress::Held => acc.already(),
                ServiceAddress::NoPrefix => {
                    warn!(
                        "{} has no address of its own, so {} cannot be added without a prefix length",
                        bridge, address
                    );
                    acc.failed()
                }
                ServiceAddress::Absent { prefix_len } => {
                    match claim(prober, bridge, *address, prefix_len) {
                        Ok(()) => {
                            info!("holding service address {}/{} on {}", address, prefix_len, bridge);
                            acc.added()
                        }
                        Err(e @ AppError::ServiceAddressConflict { .. }) => {
                            error!("{}, so it will not be claimed; give the workload a free address", e);
                            acc.conflicted()
                        }
                        Err(e) => {
                            warn!("could not hold service address {} on {}: {}", address, bridge, e);
                            acc.failed()
                        }
                    }
                }
            }
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IP_OUTPUT: &str = "10: vmbr0    inet 192.168.1.69/24 scope global vmbr0\\       valid_lft forever preferred_lft forever\n10: vmbr0    inet 192.168.1.40/24 scope global secondary vmbr0\\       valid_lft forever preferred_lft forever\n10: vmbr0    inet 192.168.1.41/24 scope global secondary vmbr0\\       valid_lft forever preferred_lft forever";

    fn addr(last: u8, prefix_len: u8) -> BridgeAddress {
        BridgeAddress {
            address: Ipv4Addr::new(192, 168, 1, last),
            prefix_len,
        }
    }

    fn binding(bridge: &str, last: u8) -> ServiceBinding {
        ServiceBinding {
            bridge: bridge.to_string(),
            address: Ipv4Addr::new(192, 168, 1, last),
        }
    }

    fn arp_probe_from_code(code: i32) -> Option<ArpProbe> {
        use std::os::unix::process::ExitStatusExt;
        ArpProbe::try_from(ExitStatus::from_raw(code << 8)).ok()
    }

    fn mac(text: &str) -> Responder {
        Responder::Mac(MacAddress::try_from(text).expect("test mac is well formed"))
    }

    #[test]
    fn the_primary_and_every_secondary_are_parsed() {
        assert_eq!(
            parse_bridge_addresses(IP_OUTPUT),
            vec![addr(69, 24), addr(40, 24), addr(41, 24)]
        );
    }

    #[test]
    fn an_address_the_bridge_already_holds_is_left_alone() {
        let held = parse_bridge_addresses(IP_OUTPUT);
        assert_eq!(
            plan_service_address(&held, Ipv4Addr::new(192, 168, 1, 41)),
            ServiceAddress::Held
        );
    }

    #[test]
    fn a_declared_address_the_bridge_lacks_borrows_the_primary_prefix() {
        let held = parse_bridge_addresses(IP_OUTPUT);
        assert_eq!(
            plan_service_address(&held, Ipv4Addr::new(192, 168, 1, 23)),
            ServiceAddress::Absent { prefix_len: 24 }
        );
    }

    #[test]
    fn a_bridge_with_no_address_yields_no_prefix_to_borrow() {
        assert_eq!(
            plan_service_address(&[], Ipv4Addr::new(192, 168, 1, 23)),
            ServiceAddress::NoPrefix
        );
    }

    #[test]
    fn a_prefix_other_than_24_is_honoured() {
        let held = vec![addr(69, 16)];
        assert_eq!(
            plan_service_address(&held, Ipv4Addr::new(192, 168, 1, 23)),
            ServiceAddress::Absent { prefix_len: 16 }
        );
    }

    #[test]
    fn lines_without_an_inet_field_are_ignored() {
        let noise = "1: lo    inet6 ::1/128 scope host\n10: vmbr0    inet 192.168.1.69/24 scope global vmbr0";
        assert_eq!(parse_bridge_addresses(noise), vec![addr(69, 24)]);
    }

    #[test]
    fn bindings_are_grouped_by_the_bridge_they_sit_on() {
        let bindings = vec![binding("vmbr0", 23), binding("vmbr1", 5), binding("vmbr0", 41)];
        assert_eq!(
            by_bridge(&bindings),
            vec![
                BridgeBindings {
                    bridge: "vmbr0".into(),
                    addresses: vec![Ipv4Addr::new(192, 168, 1, 23), Ipv4Addr::new(192, 168, 1, 41)],
                },
                BridgeBindings {
                    bridge: "vmbr1".into(),
                    addresses: vec![Ipv4Addr::new(192, 168, 1, 5)],
                },
            ]
        );
    }

    #[test]
    fn no_bindings_means_no_bridges_to_touch() {
        assert_eq!(by_bridge(&[]), vec![]);
    }

    #[test]
    fn outcomes_are_counted_separately() {
        let counted = AddressesHeld::default().added().added().already().conflicted().failed();
        assert_eq!(
            counted,
            AddressesHeld { added: 2, already: 1, conflicted: 1, failed: 1 }
        );
    }

    #[test]
    fn a_mac_address_round_trips_lowercased() {
        assert_eq!(
            MacAddress::try_from("3C:A8:2A:0E:03:BC").expect("valid").to_string(),
            "3c:a8:2a:0e:03:bc"
        );
    }

    #[test]
    fn text_that_is_not_a_mac_address_is_rejected() {
        ["", "3c:a8:2a:0e:03", "3c:a8:2a:0e:03:bc:de", "zz:a8:2a:0e:03:bc", "3ca82a0e03bc"]
            .iter()
            .for_each(|candidate| {
                assert!(
                    MacAddress::try_from(*candidate).is_err(),
                    "{} should not parse as a MAC address",
                    candidate
                )
            });
    }

    #[test]
    fn arping_finding_nothing_leaves_the_address_free() {
        let quiet = "ARPING 192.168.1.39 from 0.0.0.0 vmbr0\nSent 1 probes (1 broadcast(s))\nReceived 0 response(s)";
        assert_eq!(read_arping(ArpProbe::NoReply, quiet), Occupancy::Free);
    }

    #[test]
    fn arping_getting_a_reply_names_the_host_holding_the_address() {
        let answered = "ARPING 192.168.1.42 from 0.0.0.0 vmbr0\nUnicast reply from 192.168.1.42 [3C:A8:2A:0E:03:BC]  0.746ms\nSent 1 probes (1 broadcast(s))\nReceived 1 response(s)";
        assert_eq!(
            read_arping(ArpProbe::Replied, answered),
            Occupancy::Occupied { by: mac("3c:a8:2a:0e:03:bc") }
        );
    }

    #[test]
    fn a_reply_whose_output_we_cannot_read_is_still_a_conflict() {
        ["", "Unicast reply from 192.168.1.42  0.746ms", "something unexpected"]
            .iter()
            .for_each(|stdout| {
                assert_eq!(
                    read_arping(ArpProbe::Replied, stdout),
                    Occupancy::Occupied { by: Responder::Unidentified },
                    "a reply must count as a conflict even when stdout reads {:?}",
                    stdout
                )
            });
    }

    #[test]
    fn arping_exit_codes_map_onto_whether_anything_replied() {
        assert_eq!(arp_probe_from_code(0), Some(ArpProbe::NoReply));
        assert_eq!(arp_probe_from_code(1), Some(ArpProbe::Replied));
        [2, 3, 255].iter().for_each(|code| {
            assert_eq!(
                arp_probe_from_code(*code),
                None,
                "exit {} means the probe failed and must not be read as an answer",
                code
            )
        });
    }

    #[test]
    fn an_empty_neighbour_table_leaves_the_address_free() {
        assert_eq!(parse_neighbour(""), Occupancy::Free);
    }

    #[test]
    fn a_neighbour_that_never_resolved_leaves_the_address_free() {
        assert_eq!(parse_neighbour("192.168.1.207 INCOMPLETE \n"), Occupancy::Free);
        assert_eq!(parse_neighbour("192.168.1.32 FAILED \n"), Occupancy::Free);
        assert_eq!(
            parse_neighbour("192.168.1.32 lladdr 00:00:00:00:00:00 INCOMPLETE"),
            Occupancy::Free
        );
    }

    #[test]
    fn a_resolved_neighbour_holds_the_address() {
        assert_eq!(
            parse_neighbour("192.168.1.1 lladdr 80:69:1a:5b:6c:a4 REACHABLE \n"),
            Occupancy::Occupied { by: mac("80:69:1a:5b:6c:a4") }
        );
        assert_eq!(
            parse_neighbour("192.168.1.222 lladdr bc:24:11:d0:0f:1c STALE \n"),
            Occupancy::Occupied { by: mac("bc:24:11:d0:0f:1c") }
        );
    }

    #[test]
    fn distinct_addresses_are_unique() {
        let bindings = vec![binding("vmbr0", 23), binding("vmbr0", 41), binding("vmbr1", 5)];
        assert_eq!(check_uniqueness(&bindings), Uniqueness::Unique);
    }

    #[test]
    fn an_address_declared_twice_is_reported_once() {
        let bindings = vec![
            binding("vmbr0", 23),
            binding("vmbr0", 41),
            binding("vmbr0", 23),
            binding("vmbr0", 23),
        ];
        assert_eq!(
            check_uniqueness(&bindings),
            Uniqueness::Clashing { duplicates: vec![Ipv4Addr::new(192, 168, 1, 23)] }
        );
    }

    #[test]
    fn the_same_address_on_two_bridges_is_still_a_clash() {
        let bindings = vec![binding("vmbr0", 23), binding("vmbr1", 23)];
        assert_eq!(
            check_uniqueness(&bindings),
            Uniqueness::Clashing { duplicates: vec![Ipv4Addr::new(192, 168, 1, 23)] }
        );
    }

    #[test]
    fn nothing_declared_clashes_with_nothing() {
        assert_eq!(check_uniqueness(&[]), Uniqueness::Unique);
    }
}
