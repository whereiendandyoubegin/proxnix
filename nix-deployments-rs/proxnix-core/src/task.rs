#[pure_only]
use crate::effect::Detail;
use proxnix_pure::pure_only;

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Upid(String);

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpidFault {
    Prefix,
    Fields,
    Node,
    Hex,
    Word,
}

#[pure_only]
impl std::str::FromStr for Upid {
    type Err = UpidFault;

    fn from_str(text: &str) -> Result<Self, UpidFault> {
        let node_name = |candidate: &str| {
            !candidate.is_empty()
                && candidate
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-')
                && candidate
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
                && candidate
                    .chars()
                    .last()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
        };
        let hex = |candidate: &str, lengths: &[usize]| {
            lengths.contains(&candidate.len()) && candidate.chars().all(|c| c.is_ascii_hexdigit())
        };
        let word = |candidate: &str, may_be_empty: bool| {
            (may_be_empty || !candidate.is_empty())
                && candidate
                    .chars()
                    .all(|c| c != ':' && c != '/' && !c.is_whitespace())
        };
        let body = text
            .strip_prefix("UPID:")
            .and_then(|rest| rest.strip_suffix(':'))
            .ok_or(UpidFault::Prefix)?;
        let fields: Vec<&str> = body.split(':').collect();
        match fields.as_slice() {
            [node, pid, pstart, starttime, kind, id, user] => {
                if !node_name(node) {
                    Err(UpidFault::Node)
                } else if !(hex(pid, &[8]) && hex(pstart, &[8, 9]) && hex(starttime, &[8])) {
                    Err(UpidFault::Hex)
                } else if !(word(kind, false) && word(id, true) && word(user, false)) {
                    Err(UpidFault::Word)
                } else {
                    Ok(Upid(String::from(text)))
                }
            }
            _ => Err(UpidFault::Fields),
        }
    }
}

#[pure_only]
impl AsRef<str> for Upid {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskExit {
    Ok,
    Warnings(u32),
    Failed(Detail),
    Unknown,
}

#[pure_only]
impl TaskExit {
    #[must_use]
    pub fn succeeded(&self) -> bool {
        matches!(self, TaskExit::Ok | TaskExit::Warnings(_))
    }
}

#[pure_only]
impl From<Option<String>> for TaskExit {
    fn from(status: Option<String>) -> TaskExit {
        match status {
            None => TaskExit::Unknown,
            Some(text) if text.is_empty() || text == "unexpected status" => TaskExit::Unknown,
            Some(text) if text == "OK" => TaskExit::Ok,
            Some(text) => match text.strip_prefix("WARNINGS: ").map(str::parse::<u32>) {
                Some(Ok(count)) => TaskExit::Warnings(count),
                _ => TaskExit::Failed(Detail(text)),
            },
        }
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    Running,
    Finished(TaskExit),
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPTURED: [&str; 3] = [
        "UPID:pve01:000EA7DB:5CAA0DF7:6AB95060:vzdestroy:830:root@pam:",
        "UPID:pve01:000EAA5B:5CAA1660:6AB95076:vzstop:946:root@pam:",
        "UPID:pve01:000EAB1E:5CAA17E1:6AB95079:vzdestroy:946:root@pam:",
    ];

    #[test]
    fn every_upid_pve01_returned_parses() {
        for text in CAPTURED {
            assert_eq!(
                text.parse::<Upid>().map(|upid| String::from(upid.as_ref())),
                Ok(String::from(text))
            );
        }
    }

    #[test]
    fn a_token_upid_with_a_nine_digit_pstart_and_empty_id_parses() {
        assert!(
            "UPID:pve01:000EA7DB:15CAA0DF7:6AB95060:vzdump::root@pam!proxnix:"
                .parse::<Upid>()
                .is_ok()
        );
    }

    #[test]
    fn anything_else_is_not_a_upid() {
        assert_eq!("".parse::<Upid>(), Err(UpidFault::Prefix));
        assert_eq!(
            "UPID:pve01:000EA7DB:5CAA0DF7:6AB95060:vzdestroy:830:root@pam".parse::<Upid>(),
            Err(UpidFault::Prefix)
        );
        assert_eq!(
            "UPID:pve01:000EA7DB:5CAA0DF7:vzdestroy:830:root@pam:".parse::<Upid>(),
            Err(UpidFault::Fields)
        );
        assert_eq!(
            "UPID:-pve01:000EA7DB:5CAA0DF7:6AB95060:vzdestroy:830:root@pam:".parse::<Upid>(),
            Err(UpidFault::Node)
        );
        assert_eq!(
            "UPID:pve01:000EA7D:5CAA0DF7:6AB95060:vzdestroy:830:root@pam:".parse::<Upid>(),
            Err(UpidFault::Hex)
        );
        assert_eq!(
            "UPID:pve01:000EA7DB:5CAA0DF7:6AB95060::830:root@pam:".parse::<Upid>(),
            Err(UpidFault::Word)
        );
        assert_eq!(
            "UPID:pve01:000EA7DB:5CAA0DF7:6AB95060:vz destroy:830:root@pam:".parse::<Upid>(),
            Err(UpidFault::Word)
        );
    }

    #[test]
    fn exit_statuses_follow_proxmox_status_is_error() {
        assert_eq!(TaskExit::from(Some(String::from("OK"))), TaskExit::Ok);
        assert_eq!(
            TaskExit::from(Some(String::from("WARNINGS: 3"))),
            TaskExit::Warnings(3)
        );
        assert_eq!(
            TaskExit::from(Some(String::from("WARNINGS: many"))),
            TaskExit::Failed(Detail(String::from("WARNINGS: many")))
        );
        assert_eq!(
            TaskExit::from(Some(String::from("CT 946 already running"))),
            TaskExit::Failed(Detail(String::from("CT 946 already running")))
        );
        assert_eq!(
            TaskExit::from(Some(String::from("unexpected status"))),
            TaskExit::Unknown
        );
        assert_eq!(TaskExit::from(None), TaskExit::Unknown);
        assert!(TaskExit::Ok.succeeded() && TaskExit::Warnings(1).succeeded());
        assert!(
            !TaskExit::Unknown.succeeded() && !TaskExit::Failed(Detail(String::new())).succeeded()
        );
    }
}
