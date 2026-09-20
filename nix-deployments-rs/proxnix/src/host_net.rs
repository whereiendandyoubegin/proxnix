use std::net::Ipv4Addr;
use std::process::Command;

use tracing::{info, warn};

use crate::types::{AppError, Result};

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

#[derive(Debug, PartialEq, Eq)]
pub enum ServiceAddress {
    Held,
    Absent { prefix_len: u8 },
    NoPrefix,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AddressesHeld {
    pub added: usize,
    pub already: usize,
    pub failed: usize,
}

impl AddressesHeld {
    fn added(self) -> Self {
        Self { added: self.added + 1, ..self }
    }
    fn already(self) -> Self {
        Self { already: self.already + 1, ..self }
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

pub fn by_bridge(bindings: &[ServiceBinding]) -> Vec<BridgeBindings> {
    bindings
        .iter()
        .map(|b| b.bridge.as_str())
        .collect::<std::collections::BTreeSet<&str>>()
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

pub fn ensure_service_addresses(bridge: &str, wanted: &[Ipv4Addr]) -> Result<AddressesHeld> {
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
                    match add_address(bridge, *address, prefix_len) {
                        Ok(()) => {
                            info!("holding service address {}/{} on {}", address, prefix_len, bridge);
                            acc.added()
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
        let bindings = vec![
            ServiceBinding { bridge: "vmbr0".into(), address: Ipv4Addr::new(192, 168, 1, 23) },
            ServiceBinding { bridge: "vmbr1".into(), address: Ipv4Addr::new(10, 0, 0, 5) },
            ServiceBinding { bridge: "vmbr0".into(), address: Ipv4Addr::new(192, 168, 1, 41) },
        ];
        assert_eq!(
            by_bridge(&bindings),
            vec![
                BridgeBindings {
                    bridge: "vmbr0".into(),
                    addresses: vec![Ipv4Addr::new(192, 168, 1, 23), Ipv4Addr::new(192, 168, 1, 41)],
                },
                BridgeBindings {
                    bridge: "vmbr1".into(),
                    addresses: vec![Ipv4Addr::new(10, 0, 0, 5)],
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
        let counted = AddressesHeld::default().added().added().already().failed();
        assert_eq!(counted, AddressesHeld { added: 2, already: 1, failed: 1 });
    }
}
