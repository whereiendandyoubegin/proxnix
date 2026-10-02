use crate::sozu::Settled;
use crate::types::{AppError, Result};
use proxmox_api::nodes::node::{lxc, qemu};
use proxmox_api::types::VmId;
use proxmox_api::types::bounded_integer::BoundedIntegerError;
use proxnix_core::Vmid;
use serde::Serialize;
use serde_json::{Map, Number, Value};
use std::collections::BTreeMap;
use std::fmt;
use std::process::Command;

pub fn vmid<T: TryFrom<i128, Error = BoundedIntegerError>>(id: Vmid) -> Result<T> {
    Ok(T::try_from(i128::from(id.get()))?)
}

pub trait Kind {
    const TOOL: &'static str;
    const CREATE_POSITIONAL: &'static [&'static str];
    type Create: Serialize;
    type Set: Serialize + Default;
    type Start: Serialize + Default;
    type Stop: Serialize + Default;
    type Destroy: Serialize + Default;
    type Resize: Serialize;

    fn words(verb: Verb) -> &'static [&'static str] {
        verb.words()
    }
    fn protection(protected: bool) -> Self::Set;
}

pub enum Qemu {}
pub enum Lxc {}

impl Kind for Qemu {
    const TOOL: &'static str = "qm";
    const CREATE_POSITIONAL: &'static [&'static str] = &["vmid"];
    type Create = qemu::PostParams;
    type Set = qemu::vmid::config::PutParams;
    type Start = qemu::vmid::status::start::PostParams;
    type Stop = qemu::vmid::status::stop::PostParams;
    type Destroy = qemu::vmid::DeleteParams;
    type Resize = qemu::vmid::resize::PutParams;

    fn words(verb: Verb) -> &'static [&'static str] {
        match verb {
            Verb::Resize => &["disk", "resize"],
            other => other.words(),
        }
    }
    fn protection(protected: bool) -> Self::Set {
        Self::Set {
            protection: Some(protected),
            ..Default::default()
        }
    }
}

impl Kind for Lxc {
    const TOOL: &'static str = "pct";
    const CREATE_POSITIONAL: &'static [&'static str] = &["vmid", "ostemplate"];
    type Create = lxc::PostParams;
    type Set = lxc::vmid::config::PutParams;
    type Start = lxc::vmid::status::start::PostParams;
    type Stop = lxc::vmid::status::stop::PostParams;
    type Destroy = lxc::vmid::DeleteParams;
    type Resize = lxc::vmid::resize::PutParams;

    fn protection(protected: bool) -> Self::Set {
        Self::Set {
            protection: Some(protected),
            ..Default::default()
        }
    }
}

pub enum GuestOp<K: Kind> {
    Create(K::Create),
    Set(Vmid, K::Set),
    Start(Vmid, K::Start),
    Stop(Vmid, K::Stop),
    Destroy(Vmid, K::Destroy),
    Resize(Vmid, K::Resize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Create,
    Set,
    Start,
    Stop,
    Destroy,
    Resize,
}

impl Verb {
    fn words(self) -> &'static [&'static str] {
        match self {
            Verb::Create => &["create"],
            Verb::Set => &["set"],
            Verb::Start => &["start"],
            Verb::Stop => &["stop"],
            Verb::Destroy => &["destroy"],
            Verb::Resize => &["resize"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scalar {
    Text(String),
    Number(Number),
    Bool(bool),
}

impl TryFrom<Value> for Scalar {
    type Error = AppError;

    fn try_from(value: Value) -> Result<Self> {
        match value {
            Value::String(s) => Ok(Scalar::Text(s)),
            Value::Number(n) => Ok(Scalar::Number(n)),
            Value::Bool(b) => Ok(Scalar::Bool(b)),
            other => Err(AppError::ProxmoxError(format!("{other} has no CLI form"))),
        }
    }
}

impl fmt::Display for Scalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scalar::Text(s) => f.write_str(s),
            Scalar::Number(n) => write!(f, "{n}"),
            Scalar::Bool(b) => write!(f, "{}", u8::from(*b)),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Invocation {
    pub program: &'static str,
    pub verb: Verb,
    pub command: &'static [&'static str],
    pub vmid: Option<VmId>,
    pub positional: Vec<Scalar>,
    pub flags: BTreeMap<String, Scalar>,
}

pub enum Arg<'a> {
    Word(&'static str),
    Id(VmId),
    Positional(&'a Scalar),
    Flag(&'a str),
    Value(&'a Scalar),
}

impl fmt::Display for Arg<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Arg::Word(w) => f.write_str(w),
            Arg::Id(id) => write!(f, "{id}"),
            Arg::Positional(s) | Arg::Value(s) => write!(f, "{s}"),
            Arg::Flag(key) => write!(f, "--{key}"),
        }
    }
}

impl Invocation {
    fn new<K: Kind>(
        verb: Verb,
        vmid: Option<VmId>,
        params: &impl Serialize,
        positional: &[&str],
    ) -> Result<Self> {
        let command = K::words(verb);
        let Value::Object(fields) = serde_json::to_value(params)? else {
            return Err(AppError::ProxmoxError(format!(
                "{} {} params did not serialise to an object",
                K::TOOL,
                command.join(" ")
            )));
        };
        let (leading, rest): (Map<String, Value>, Map<String, Value>) = fields
            .into_iter()
            .partition(|(key, _)| positional.contains(&key.as_str()));
        Ok(Invocation {
            program: K::TOOL,
            verb,
            command,
            vmid,
            positional: positional
                .iter()
                .map(|key| {
                    leading
                        .get(*key)
                        .cloned()
                        .ok_or_else(|| {
                            AppError::ProxmoxError(format!(
                                "{} {} params have no `{key}`",
                                K::TOOL,
                                command.join(" ")
                            ))
                        })
                        .and_then(Scalar::try_from)
                })
                .collect::<Result<_>>()?,
            flags: rest
                .into_iter()
                .map(|(key, value)| Scalar::try_from(value).map(|s| (key, s)))
                .collect::<Result<_>>()?,
        })
    }

    pub fn args(&self) -> impl Iterator<Item = Arg<'_>> {
        self.command
            .iter()
            .copied()
            .map(Arg::Word)
            .chain(self.vmid.map(Arg::Id))
            .chain(self.positional.iter().map(Arg::Positional))
            .chain(
                self.flags
                    .iter()
                    .flat_map(|(key, value)| [Arg::Flag(key), Arg::Value(value)]),
            )
    }
}

impl fmt::Display for Invocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.program)?;
        self.args().try_for_each(|arg| write!(f, " {arg}"))
    }
}

impl<K: Kind> GuestOp<K> {
    pub fn start(id: Vmid) -> Self {
        GuestOp::Start(id, K::Start::default())
    }

    pub fn stop(id: Vmid) -> Self {
        GuestOp::Stop(id, K::Stop::default())
    }

    pub fn destroy(id: Vmid) -> Self {
        GuestOp::Destroy(id, K::Destroy::default())
    }

    pub fn protection(id: Vmid, protected: bool) -> Self {
        GuestOp::Set(id, K::protection(protected))
    }

    pub fn retire(id: Vmid) -> [Self; 2] {
        [Self::stop(id), Self::destroy(id)]
    }

    pub fn reclaim(id: Vmid) -> [Self; 3] {
        [Self::protection(id, false), Self::stop(id), Self::destroy(id)]
    }

    pub fn invocation(&self) -> Result<Invocation> {
        match self {
            GuestOp::Create(p) => Invocation::new::<K>(Verb::Create, None, p, K::CREATE_POSITIONAL),
            GuestOp::Set(id, p) => Invocation::new::<K>(Verb::Set, Some(vmid(*id)?), p, &[]),
            GuestOp::Start(id, p) => Invocation::new::<K>(Verb::Start, Some(vmid(*id)?), p, &[]),
            GuestOp::Stop(id, p) => Invocation::new::<K>(Verb::Stop, Some(vmid(*id)?), p, &[]),
            GuestOp::Destroy(id, p) => Invocation::new::<K>(Verb::Destroy, Some(vmid(*id)?), p, &[]),
            GuestOp::Resize(id, p) => {
                Invocation::new::<K>(Verb::Resize, Some(vmid(*id)?), p, &["disk", "size"])
            }
        }
    }

    fn already_applied_marker(&self) -> Option<&'static str> {
        match self {
            GuestOp::Start(..) => Some("already running"),
            GuestOp::Stop(..) => Some("not running"),
            _ => None,
        }
    }
}

pub trait Execute {
    fn run<K: Kind>(&self, op: &GuestOp<K>) -> Result<Settled>;

}

pub struct Cli;

impl Execute for Cli {
    fn run<K: Kind>(&self, op: &GuestOp<K>) -> Result<Settled> {
        let invocation = op.invocation()?;
        let output = Command::new(invocation.program)
            .args(invocation.args().map(|arg| arg.to_string()))
            .output()?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        match (output.status.success(), op.already_applied_marker()) {
            (true, _) => Ok(Settled::Changed),
            (false, Some(marker)) if stderr.contains(marker) => Ok(Settled::AlreadyApplied),
            (false, _) => Err(AppError::CmdError(format!(
                "{invocation} failed (exit: {:?}): {stderr}",
                output.status.code()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;

    fn id(n: u32) -> Vmid {
        Vmid::new(n)
    }

    fn rendered<K: Kind>(op: &GuestOp<K>) -> String {
        op.invocation().unwrap().to_string()
    }

    #[test]
    fn qm_set_tags_matches_the_old_argv() {
        let op = GuestOp::<Qemu>::Set(
            id(101),
            qemu::vmid::config::PutParams {
                tags: Some("proxnix;blue".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(rendered(&op), "qm set 101 --tags proxnix;blue");
    }

    #[test]
    fn bools_render_as_proxmox_flags() {
        assert_eq!(
            rendered(&GuestOp::<Qemu>::Set(id(101), Qemu::protection(true))),
            "qm set 101 --protection 1"
        );
        assert_eq!(
            rendered(&GuestOp::<Lxc>::Set(id(200), Lxc::protection(false))),
            "pct set 200 --protection 0"
        );
    }

    #[test]
    fn numbered_fields_become_numbered_flags() {
        let op = GuestOp::<Qemu>::Set(
            id(101),
            qemu::vmid::config::PutParams {
                agent: Some("1".to_string()),
                serials: [(0, "socket".to_string().try_into().unwrap())].into(),
                ..Default::default()
            },
        );
        assert_eq!(rendered(&op), "qm set 101 --agent 1 --serial0 socket");
    }

    #[test]
    fn qm_set_resources_matches_the_old_argv() {
        let op = GuestOp::<Qemu>::Set(
            id(101),
            qemu::vmid::config::PutParams {
                memory: Some("2048".to_string()),
                cores: NonZeroU64::new(4),
                sockets: NonZeroU64::new(1),
                ..Default::default()
            },
        );
        assert_eq!(
            rendered(&op),
            "qm set 101 --cores 4 --memory 2048 --sockets 1"
        );
    }

    #[test]
    fn pct_set_resources_matches_the_old_argv() {
        let op = GuestOp::<Lxc>::Set(
            id(200),
            lxc::vmid::config::PutParams {
                memory: Some(512.try_into().unwrap()),
                cores: Some(2.try_into().unwrap()),
                ..Default::default()
            },
        );
        assert_eq!(rendered(&op), "pct set 200 --cores 2 --memory 512");
    }

    #[test]
    fn default_params_add_no_flags() {
        assert_eq!(
            rendered(&GuestOp::<Qemu>::Start(
                id(101),
                qemu::vmid::status::start::PostParams::default()
            )),
            "qm start 101"
        );
        assert_eq!(
            rendered(&GuestOp::<Lxc>::Stop(
                id(200),
                lxc::vmid::status::stop::PostParams::default()
            )),
            "pct stop 200"
        );
        assert_eq!(
            rendered(&GuestOp::<Lxc>::Destroy(id(200), lxc::vmid::DeleteParams::default())),
            "pct destroy 200"
        );
    }

    #[test]
    fn qm_resize_matches_the_old_argv() {
        let op = GuestOp::<Qemu>::Resize(
            id(101),
            qemu::vmid::resize::PutParams::new(
                "scsi0".try_into().unwrap(),
                "20G".to_string().try_into().unwrap(),
            ),
        );
        assert_eq!(rendered(&op), "qm disk resize 101 scsi0 20G");
    }

    #[test]
    fn pct_create_puts_vmid_and_template_first() {
        let params = lxc::PostParams {
            hostname: Some("web".to_string().try_into().unwrap()),
            unprivileged: Some(true),
            mps: [(0, "/srv/web,mp=/var/lib/web,ro=1".to_string())].into(),
            ..lxc::PostParams::new(
                "local:vztmpl/web.tar.xz".to_string().try_into().unwrap(),
                200.try_into().unwrap(),
            )
        };
        assert_eq!(
            rendered(&GuestOp::<Lxc>::Create(params)),
            "pct create 200 local:vztmpl/web.tar.xz --hostname web --mp0 /srv/web,mp=/var/lib/web,ro=1 --unprivileged 1"
        );
    }

    #[test]
    fn qm_create_puts_vmid_first() {
        let params = qemu::PostParams {
            name: Some("web".to_string()),
            nets: [(0, "virtio,bridge=vmbr0".to_string())].into(),
            ..qemu::PostParams::new(101.try_into().unwrap())
        };
        assert_eq!(
            rendered(&GuestOp::<Qemu>::Create(params)),
            "qm create 101 --name web --net0 virtio,bridge=vmbr0"
        );
    }

    #[test]
    fn values_stay_typed_until_rendered() {
        let invocation = GuestOp::<Lxc>::Set(id(200), Lxc::protection(false))
            .invocation()
            .unwrap();
        assert_eq!(invocation.verb, Verb::Set);
        assert_eq!(invocation.vmid, Some(vmid(id(200)).unwrap()));
        assert_eq!(
            invocation.flags,
            BTreeMap::from([("protection".to_string(), Scalar::Number(0.into()))])
        );
    }

    #[test]
    fn vmids_outside_the_proxmox_range_are_rejected_before_anything_runs() {
        assert!(GuestOp::<Qemu>::start(id(99)).invocation().is_err());
        assert!(GuestOp::<Qemu>::start(id(100)).invocation().is_ok());
    }

    #[test]
    fn retiring_stops_before_destroying() {
        let rendered: Vec<String> = GuestOp::<Lxc>::retire(id(200)).iter().map(rendered).collect();
        assert_eq!(rendered, ["pct stop 200", "pct destroy 200"]);
    }

    #[test]
    fn reclaiming_unprotects_then_stops_then_destroys() {
        let rendered: Vec<String> = GuestOp::<Qemu>::reclaim(id(823)).iter().map(rendered).collect();
        assert_eq!(
            rendered,
            ["qm set 823 --protection 0", "qm stop 823", "qm destroy 823"]
        );
    }
}
