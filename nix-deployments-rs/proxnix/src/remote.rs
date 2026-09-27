use crate::api::{GuestOp, Kind, Lxc, Qemu, vmid};
use crate::sozu::Settled;
use crate::state::{agent_ipv4, cidr_ipv4};
use crate::types::{AppError, Result};
use proxmox_api::access::{AccessClient, permissions};
use proxmox_api::client::Client;
use proxmox_api::nodes::NodesClient;
use proxmox_api::nodes::node::NodeClient;
use proxmox_api::nodes::node::tasks::upid::status::Status as TaskStatus;
use proxmox_api::nodes::node::{lxc, qemu};
use proxmox_api::types::bounded_integer::BoundedInteger;
use proxnix_core::{GuestStatus, Ownership, RawTags, TaskExit, Upid, Vmid};
use std::future::Future;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};
use tokio::runtime::Handle;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiFault {
    NotAUpid(String),
    Task { upid: Upid, exit: TaskExit },
    TimedOut { upid: Upid, after: Duration },
    CreationNeedsRootPam,
}

pub trait ApiError: Into<AppError> {
    fn empty_success(&self) -> bool;
}

impl ApiError for proxmox_api::ReqwestError {
    fn empty_success(&self) -> bool {
        matches!(self, proxmox_api::ReqwestError::UnknownFailure(status, _) if status.is_success())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Done,
    Task(Upid),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    Absent,
    Present { status: GuestStatus, ownership: Ownership },
}

pub struct Api<C> {
    client: C,
    node: String,
    runtime: Handle,
    poll: Duration,
    timeout: Duration,
}

impl<C: Client> Api<C>
where
    C::Error: ApiError,
{
    pub fn new(client: C, node: String, runtime: Handle, poll: Duration, timeout: Duration) -> Self {
        Api { client, node, runtime, poll, timeout }
    }

    fn node(&self) -> NodeClient<&C> {
        NodesClient::new(&self.client).node(&self.node)
    }

    fn call<T>(&self, request: impl Future<Output = std::result::Result<T, C::Error>>) -> Result<T> {
        self.runtime.block_on(request).map_err(Into::into)
    }

    fn unit(&self, request: impl Future<Output = std::result::Result<(), C::Error>>) -> Result<Reply> {
        match self.runtime.block_on(request) {
            Ok(()) => Ok(Reply::Done),
            Err(error) if error.empty_success() => Ok(Reply::Done),
            Err(error) => Err(error.into()),
        }
    }

    fn task(&self, request: impl Future<Output = std::result::Result<String, C::Error>>) -> Result<Reply> {
        let raw = self.call(request)?;
        match raw.parse::<Upid>() {
            Ok(upid) => Ok(Reply::Task(upid)),
            Err(_) => Err(AppError::Api(ApiFault::NotAUpid(raw))),
        }
    }

    fn wait(&self, upid: Upid) -> Result<()> {
        let started = Instant::now();
        let finished = std::iter::repeat(()).find_map(|()| {
            match self.call(self.node().tasks().upid(upid.as_ref()).status().get()) {
                Err(error) => Some(Err(error)),
                Ok(status) if status.status == TaskStatus::Stopped => Some(Ok(TaskExit::from(status.exitstatus))),
                Ok(_) if started.elapsed() >= self.timeout => Some(Err(AppError::Api(ApiFault::TimedOut {
                    upid: upid.clone(),
                    after: self.timeout,
                }))),
                Ok(_) => {
                    std::thread::sleep(self.poll);
                    None
                }
            }
        });
        match finished {
            Some(Ok(exit)) if exit.succeeded() => Ok(()),
            Some(Ok(exit)) => Err(AppError::Api(ApiFault::Task { upid, exit })),
            Some(Err(error)) => Err(error),
            None => Err(AppError::Api(ApiFault::TimedOut { upid, after: self.timeout })),
        }
    }

    pub fn apply<K: Remote>(&self, op: &GuestOp<K>) -> Result<Settled> {
        let attempted = K::submit(self, op).and_then(|reply| match reply {
            Reply::Done => Ok(()),
            Reply::Task(upid) => self.wait(upid),
        });
        match attempted {
            Ok(()) => Ok(Settled::Changed),
            Err(error) => match target(op).map(|id| self.presence::<K>(id)) {
                Some(Ok(presence)) if already(op, &presence) => Ok(Settled::AlreadyApplied),
                _ => Err(error),
            },
        }
    }

    pub fn apply_all<K: Remote>(&self, ops: &[GuestOp<K>]) -> Result<()> {
        ops.iter().try_for_each(|op| self.apply(op).map(|_| ()))
    }

    pub fn presence<K: Remote>(&self, id: Vmid) -> Result<Presence> {
        match K::presence(self, id)? {
            Presence::Absent => self.audited().map(|()| Presence::Absent),
            present @ Presence::Present { .. } => Ok(present),
        }
    }

    fn audited(&self) -> Result<()> {
        let granted = self.call(AccessClient::new(&self.client).permissions().get(permissions::GetParams {
            path: Some(String::from("/vms")),
            ..permissions::GetParams::default()
        }))?;
        crate::state::audited(&granted.additional_properties).map(|_| ())
    }

    pub fn address<K: Remote>(&self, id: Vmid) -> Result<Option<Ipv4Addr>> {
        K::address(self, id)
    }
}

fn target<K: Kind>(op: &GuestOp<K>) -> Option<Vmid> {
    match op {
        GuestOp::Create(_) => None,
        GuestOp::Set(id, _) | GuestOp::Start(id, _) | GuestOp::Stop(id, _) | GuestOp::Destroy(id, _) | GuestOp::Resize(id, _) => {
            Some(*id)
        }
    }
}

fn already<K: Kind>(op: &GuestOp<K>, presence: &Presence) -> bool {
    matches!(
        (op, presence),
        (GuestOp::Start(..), Presence::Present { status: GuestStatus::Running, .. })
            | (GuestOp::Stop(..), Presence::Present { status: GuestStatus::Stopped, .. } | Presence::Absent)
            | (GuestOp::Destroy(..), Presence::Absent)
    )
}

fn listed(id: Vmid, found: impl Iterator<Item = (i128, GuestStatus, Option<String>)>) -> Presence {
    found
        .into_iter()
        .find(|(listed, _, _)| *listed == i128::from(id.get()))
        .map_or(Presence::Absent, |(_, status, tags)| Presence::Present {
            status,
            ownership: Ownership::from(&RawTags::from(tags.unwrap_or_default())),
        })
}

pub trait Remote: Kind + Sized {
    fn submit<C: Client>(api: &Api<C>, op: &GuestOp<Self>) -> Result<Reply>
    where
        C::Error: ApiError;
    fn presence<C: Client>(api: &Api<C>, id: Vmid) -> Result<Presence>
    where
        C::Error: ApiError;
    fn address<C: Client>(api: &Api<C>, id: Vmid) -> Result<Option<Ipv4Addr>>
    where
        C::Error: ApiError;
}

impl Remote for Qemu {
    fn submit<C: Client>(api: &Api<C>, op: &GuestOp<Qemu>) -> Result<Reply>
    where
        C::Error: ApiError,
    {
        let guests = api.node().qemu();
        match op {
            GuestOp::Create(_) => Err(AppError::Api(ApiFault::CreationNeedsRootPam)),
            GuestOp::Set(id, params) => api.unit(guests.vmid(vmid(*id)?).config().put(params.clone())),
            GuestOp::Start(id, params) => api.task(guests.vmid(vmid(*id)?).status().start().post(params.clone())),
            GuestOp::Stop(id, params) => api.task(guests.vmid(vmid(*id)?).status().stop().post(params.clone())),
            GuestOp::Destroy(id, params) => api.task(guests.vmid(vmid(*id)?).delete(params.clone())),
            GuestOp::Resize(id, params) => api.task(guests.vmid(vmid(*id)?).resize().put(params.clone())),
        }
    }

    fn presence<C: Client>(api: &Api<C>, id: Vmid) -> Result<Presence>
    where
        C::Error: ApiError,
    {
        let found = api.call(api.node().qemu().get(qemu::GetParams::default()))?;
        Ok(listed(
            id,
            found.into_iter().map(|item| {
                (
                    item.vmid.get(),
                    match item.status {
                        qemu::Status::Running => GuestStatus::Running,
                        qemu::Status::Stopped => GuestStatus::Stopped,
                    },
                    item.tags,
                )
            }),
        ))
    }

    fn address<C: Client>(api: &Api<C>, id: Vmid) -> Result<Option<Ipv4Addr>>
    where
        C::Error: ApiError,
    {
        let reply = api.call(api.node().qemu().vmid(vmid(id)?).agent().network_get_interfaces().get())?;
        Ok(reply.additional_properties.get("result").and_then(agent_ipv4))
    }
}

impl Remote for Lxc {
    fn submit<C: Client>(api: &Api<C>, op: &GuestOp<Lxc>) -> Result<Reply>
    where
        C::Error: ApiError,
    {
        let guests = api.node().lxc();
        match op {
            GuestOp::Create(_) => Err(AppError::Api(ApiFault::CreationNeedsRootPam)),
            GuestOp::Set(id, params) => api.unit(guests.vmid(vmid(*id)?).config().put(params.clone())),
            GuestOp::Start(id, params) => api.task(guests.vmid(vmid(*id)?).status().start().post(params.clone())),
            GuestOp::Stop(id, params) => api.task(guests.vmid(vmid(*id)?).status().stop().post(params.clone())),
            GuestOp::Destroy(id, params) => api.task(guests.vmid(vmid(*id)?).delete(params.clone())),
            GuestOp::Resize(id, params) => api.task(guests.vmid(vmid(*id)?).resize().put(params.clone())),
        }
    }

    fn presence<C: Client>(api: &Api<C>, id: Vmid) -> Result<Presence>
    where
        C::Error: ApiError,
    {
        let found = api.call(api.node().lxc().get())?;
        Ok(listed(
            id,
            found.into_iter().map(|item| {
                (
                    item.vmid.get(),
                    match item.status {
                        lxc::Status::Running => GuestStatus::Running,
                        lxc::Status::Stopped => GuestStatus::Stopped,
                    },
                    item.tags,
                )
            }),
        ))
    }

    fn address<C: Client>(api: &Api<C>, id: Vmid) -> Result<Option<Ipv4Addr>>
    where
        C::Error: ApiError,
    {
        let interfaces = api.call(api.node().lxc().vmid(vmid(id)?).interfaces().get())?;
        Ok(interfaces.into_iter().filter(|iface| iface.name != "lo").find_map(|iface| iface.inet.as_deref().and_then(cidr_ipv4)))
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use proxmox_api::client::{Client, Method};
    use serde::{Serialize, de::DeserializeOwned};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    #[derive(Debug, Clone, PartialEq)]
    pub struct Request {
        pub method: Method,
        pub path: String,
        pub body: Option<serde_json::Value>,
        pub query: Option<serde_json::Value>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum FakeError {
        Status(String),
        NoData,
        Decode(String),
    }

    impl From<FakeError> for crate::types::AppError {
        fn from(error: FakeError) -> Self {
            crate::types::AppError::ProxmoxError(format!("{error:?}"))
        }
    }

    impl super::ApiError for FakeError {
        fn empty_success(&self) -> bool {
            *self == FakeError::NoData
        }
    }

    type Script = VecDeque<(Method, String, Result<String, String>)>;

    #[derive(Clone, Default)]
    pub struct Proxmox {
        pub log: Rc<RefCell<Vec<Request>>>,
        replies: Rc<RefCell<Script>>,
    }

    impl Proxmox {
        pub fn replying(replies: Vec<(Method, &str, Result<&str, &str>)>) -> Proxmox {
            Proxmox {
                log: Rc::default(),
                replies: Rc::new(RefCell::new(
                    replies
                        .into_iter()
                        .map(|(method, path, reply)| (method, String::from(path), reply.map(String::from).map_err(String::from)))
                        .collect(),
                )),
            }
        }

        pub fn requests(&self) -> Vec<Request> {
            self.log.borrow().clone()
        }

        pub fn exhausted(&self) -> bool {
            self.replies.borrow().is_empty()
        }
    }

    impl Client for Proxmox {
        type Error = FakeError;

        fn request_with_body_and_query<B, Q, R>(
            &self,
            method: Method,
            path: &str,
            body: Option<&B>,
            query: Option<&Q>,
        ) -> impl Future<Output = Result<R, FakeError>>
        where
            B: Serialize,
            Q: Serialize,
            R: DeserializeOwned,
        {
            self.log.borrow_mut().push(Request {
                method,
                path: String::from(path),
                body: body.map(|b| serde_json::to_value(b).unwrap()),
                query: query.map(|q| serde_json::to_value(q).unwrap()),
            });
            let scripted = self.replies.borrow_mut().pop_front();
            let asked = (method, String::from(path));
            async move {
                match scripted {
                    Some((expected, at, reply)) if (expected, at.clone()) == asked => match reply {
                        Err(status) => Err(FakeError::Status(status)),
                        Ok(text) if text.trim() == "null" => Err(FakeError::NoData),
                        Ok(text) => serde_json::from_str(&text).map_err(|e| FakeError::Decode(e.to_string())),
                    },
                    other => Err(FakeError::Status(format!("unexpected {:?} {}, scripted {other:?}", asked.0, asked.1))),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::Proxmox;
    use super::*;
    use proxmox_api::client::Method;

    const UPID: &str = "UPID:pve01:000EAA5B:5CAA1660:6AB95076:vzstop:946:root@pam:";
    const STOPPED_OK: &str = include_str!("../fixtures/api/tasks/UPID_pve01_000EAA5B_5CAA1660_6AB95076_vzstop_946_root@pam_/status.json");
    const LXC_LIST: &str = include_str!("../fixtures/api/lxc.json");
    const PERMISSIONS: &str = include_str!("../fixtures/api/permissions.json");

    fn status(state: &str, exit: Option<&str>) -> String {
        let value = serde_json::from_str::<serde_json::Value>(STOPPED_OK).unwrap();
        let object = value.as_object().unwrap().clone();
        let object = object
            .into_iter()
            .filter(|(key, _)| key != "exitstatus" && key != "status")
            .chain([(String::from("status"), serde_json::Value::from(state))])
            .chain(exit.map(|exit| (String::from("exitstatus"), serde_json::Value::from(exit))))
            .collect::<serde_json::Map<_, _>>();
        serde_json::Value::Object(object).to_string()
    }

    fn api(fake: &Proxmox) -> (tokio::runtime::Runtime, Api<Proxmox>) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let handle = runtime.handle().clone();
        (runtime, Api::new(fake.clone(), String::from("pve01"), handle, Duration::ZERO, Duration::from_secs(5)))
    }

    const TASK: &str = "/nodes/pve01/tasks/UPID:pve01:000EAA5B:5CAA1660:6AB95076:vzstop:946:root@pam:/status";

    fn quoted(text: &str) -> String {
        serde_json::Value::from(text).to_string()
    }

    #[test]
    #[ignore = "needs fixtures/api/failed-tasks.json: run `nu scripts/fixtures.nu pve01`"]
    fn every_task_proxmox_reported_as_failed_classifies_as_a_failure() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/api");
        let listed: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(dir.join("failed-tasks.json")).unwrap()).unwrap();
        assert!(!listed.is_empty(), "the capture holds no failed tasks to check against");
        for task in &listed {
            let upid = task["upid"].as_str().unwrap();
            assert!(upid.parse::<Upid>().is_ok(), "{upid} does not parse");
            let exit = TaskExit::from(task["status"].as_str().map(String::from));
            assert!(!exit.succeeded(), "{upid} failed on pve01 but classifies as {exit:?}");
            let status = dir.join("tasks").join(upid.replace(':', "_")).join("status.json");
            if let Ok(text) = std::fs::read_to_string(status) {
                let decoded: proxmox_api::nodes::node::tasks::upid::status::GetOutput = serde_json::from_str(&text).unwrap();
                assert!(matches!(TaskExit::from(decoded.exitstatus), TaskExit::Failed(_)), "{upid}");
            }
        }
    }

    #[test]
    fn a_stop_posts_to_the_guest_and_waits_for_its_task() {
        let running = status("running", None);
        let upid = quoted(UPID);
        let fake = Proxmox::replying(vec![
            (Method::Post, "/nodes/pve01/lxc/946/status/stop", Ok(upid.as_str())),
            (Method::Get, TASK, Ok(running.as_str())),
            (Method::Get, TASK, Ok(STOPPED_OK)),
        ]);
        let (_runtime, api) = api(&fake);
        assert_eq!(api.apply(&GuestOp::<Lxc>::stop(Vmid::new(946))).unwrap(), Settled::Changed);
        assert!(fake.exhausted());
        assert_eq!(fake.requests()[0].body, Some(serde_json::json!({})));
    }

    #[test]
    fn tags_are_a_synchronous_config_put_with_the_same_params_the_cli_used() {
        let op = GuestOp::<Lxc>::Set(
            Vmid::new(844),
            lxc::vmid::config::PutParams { tags: Some(String::from("proxnix;slot-blue;gen-1")), ..Default::default() },
        );
        let fake = Proxmox::replying(vec![(Method::Put, "/nodes/pve01/lxc/844/config", Ok("null"))]);
        let (_runtime, api) = api(&fake);
        assert_eq!(api.apply(&op).unwrap(), Settled::Changed);
        let body = fake.requests()[0].body.clone().unwrap();
        let flags = op.invocation().unwrap().flags;
        assert_eq!(body.as_object().unwrap().keys().collect::<Vec<_>>(), flags.keys().collect::<Vec<_>>());
        assert_eq!(body["tags"], "proxnix;slot-blue;gen-1");
    }

    #[test]
    fn every_retire_and_reclaim_step_reaches_the_api_with_the_cli_params() {
        let ops = GuestOp::<Qemu>::reclaim(Vmid::new(823));
        let upid = quoted(UPID);
        let fake = Proxmox::replying(vec![
            (Method::Put, "/nodes/pve01/qemu/823/config", Ok("null")),
            (Method::Post, "/nodes/pve01/qemu/823/status/stop", Ok(upid.as_str())),
            (Method::Get, TASK, Ok(STOPPED_OK)),
            (Method::Delete, "/nodes/pve01/qemu/823", Ok(upid.as_str())),
            (Method::Get, TASK, Ok(STOPPED_OK)),
        ]);
        let (_runtime, api) = api(&fake);
        api.apply_all(&ops).unwrap();
        assert!(fake.exhausted());
        let sent: Vec<serde_json::Value> = fake
            .requests()
            .into_iter()
            .filter(|request| !request.path.contains("/tasks/"))
            .map(|request| request.body.or(request.query).unwrap())
            .collect();
        let expected: Vec<Vec<String>> = ops.iter().map(|op| op.invocation().unwrap().flags.into_keys().collect()).collect();
        let actual: Vec<Vec<String>> = sent.iter().map(|value| value.as_object().unwrap().keys().cloned().collect()).collect();
        assert_eq!(actual, expected);
        assert_eq!(sent[0]["protection"], serde_json::json!(0));
    }

    #[test]
    fn a_failed_task_is_an_error_unless_the_guest_already_ended_up_that_way() {
        let upid = quoted(UPID);
        let failed = status("stopped", Some("CT 946 not running"));
        let stopped_list = LXC_LIST.replace("\"vmid\": 846", "\"vmid\": 946").replace("\"status\": \"running\"", "\"status\": \"stopped\"");
        let fake = Proxmox::replying(vec![
            (Method::Post, "/nodes/pve01/lxc/946/status/stop", Ok(upid.as_str())),
            (Method::Get, TASK, Ok(failed.as_str())),
            (Method::Get, "/nodes/pve01/lxc", Ok(stopped_list.as_str())),
        ]);
        let (_runtime, api) = api(&fake);
        assert_eq!(api.apply(&GuestOp::<Lxc>::stop(Vmid::new(946))).unwrap(), Settled::AlreadyApplied);

        let still_running = Proxmox::replying(vec![
            (Method::Post, "/nodes/pve01/lxc/844/status/stop", Ok(upid.as_str())),
            (Method::Get, TASK, Ok(failed.as_str())),
            (Method::Get, "/nodes/pve01/lxc", Ok(LXC_LIST)),
        ]);
        let (_runtime, api) = self::api(&still_running);
        match api.apply(&GuestOp::<Lxc>::stop(Vmid::new(844))) {
            Err(AppError::Api(ApiFault::Task { exit: TaskExit::Failed(_), .. })) => {}
            other => panic!("expected a failed task, got {other:?}"),
        }
    }

    #[test]
    fn destroying_a_guest_that_is_already_gone_is_already_applied() {
        let fake = Proxmox::replying(vec![
            (Method::Delete, "/nodes/pve01/lxc/999", Err("500 CT 999 does not exist")),
            (Method::Get, "/nodes/pve01/lxc", Ok(LXC_LIST)),
            (Method::Get, "/access/permissions", Ok(PERMISSIONS)),
        ]);
        let (_runtime, api) = api(&fake);
        assert_eq!(api.apply(&GuestOp::<Lxc>::destroy(Vmid::new(999))).unwrap(), Settled::AlreadyApplied);
        assert!(fake.exhausted());
    }

    #[test]
    fn an_absence_the_token_cannot_vouch_for_is_never_taken_as_done() {
        let blind = PERMISSIONS.replace("\"VM.Audit\": 1", "\"VM.Audit\": 0");
        let fake = Proxmox::replying(vec![
            (Method::Delete, "/nodes/pve01/lxc/844", Err("500 something went wrong")),
            (Method::Get, "/nodes/pve01/lxc", Ok("[]")),
            (Method::Get, "/access/permissions", Ok(blind.as_str())),
        ]);
        let (_runtime, api) = api(&fake);
        assert!(api.apply(&GuestOp::<Lxc>::destroy(Vmid::new(844))).is_err());
        assert!(fake.exhausted());
    }

    #[test]
    fn a_task_that_never_finishes_times_out() {
        let upid = quoted(UPID);
        let running = status("running", None);
        let stopped = LXC_LIST.replace("\"status\": \"running\"", "\"status\": \"stopped\"");
        let fake = Proxmox::replying(
            [(Method::Post, "/nodes/pve01/lxc/844/status/start", Ok(upid.as_str()))]
                .into_iter()
                .chain(std::iter::repeat_n((Method::Get, TASK, Ok(running.as_str())), 50))
                .chain([(Method::Get, "/nodes/pve01/lxc", Ok(stopped.as_str()))])
                .collect(),
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let api = Api::new(fake.clone(), String::from("pve01"), runtime.handle().clone(), Duration::ZERO, Duration::ZERO);
        match api.apply(&GuestOp::<Lxc>::start(Vmid::new(844))) {
            Err(AppError::Api(ApiFault::TimedOut { .. })) => {}
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    #[test]
    fn creation_is_refused_over_the_api_because_bind_mounts_need_root_pam() {
        let fake = Proxmox::replying(vec![]);
        let (_runtime, api) = api(&fake);
        let create = GuestOp::<Qemu>::Create(qemu::PostParams::new(vmid(Vmid::new(823)).unwrap()));
        assert!(matches!(api.apply(&create), Err(AppError::Api(ApiFault::CreationNeedsRootPam))));
        assert!(fake.requests().is_empty());
    }

    #[test]
    fn addresses_come_from_the_guest_agent_or_the_container_interfaces() {
        let agent = include_str!("../fixtures/api/qemu/823/agent/network-get-interfaces.json");
        let interfaces = include_str!("../fixtures/api/lxc/844/interfaces.json");
        let fake = Proxmox::replying(vec![
            (Method::Get, "/nodes/pve01/qemu/823/agent/network-get-interfaces", Ok(agent)),
            (Method::Get, "/nodes/pve01/lxc/844/interfaces", Ok(interfaces)),
        ]);
        let (_runtime, api) = api(&fake);
        assert!(api.address::<Qemu>(Vmid::new(823)).unwrap().is_some());
        assert_eq!(api.address::<Lxc>(Vmid::new(844)).unwrap(), Some(Ipv4Addr::new(192, 168, 1, 214)));
    }
}
