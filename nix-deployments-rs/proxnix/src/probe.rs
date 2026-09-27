use crate::api::{Kind, Lxc, Qemu};
use crate::child::{self, Exit};
use crate::types::{AppError, GuestCheck, Result, Timing, Timings};
use proxnix_core::Vmid;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tracing::info;

pub trait Probe: Kind {
    fn guest_check(name: &str, id: Vmid, timeout: Duration, check: &GuestCheck, timings: &Timings) -> Result<()>;
}

impl Probe for Qemu {
    fn guest_check(name: &str, _id: Vmid, _timeout: Duration, _check: &GuestCheck, _timings: &Timings) -> Result<()> {
        info!(
            "[{}] guest health checks are only supported for containers; only the port check ran",
            name
        );
        Ok(())
    }
}

impl Probe for Lxc {
    fn guest_check(name: &str, id: Vmid, timeout: Duration, check: &GuestCheck, timings: &Timings) -> Result<()> {
        let script = guest_check_script(&check.command);
        let argv = [check.shell.as_str(), "-c", script.as_str()];
        let poll = timings.get(Timing::GuestCheckPoll);
        let run = timings.get(Timing::GuestCheckRun);
        let started = Instant::now();
        (0_u32..)
            .take_while(|_| started.elapsed() < timeout)
            .find_map(|attempt| match exec(id, &argv, run) {
                Ok(ExecOutcome::Succeeded { .. }) => Some(Ok(())),
                Ok(ExecOutcome::Failed { code, output }) => {
                    if attempt % 5 == 0 {
                        info!(
                            "[{}] guest health check not passing yet after {}s (exit {:?}): {}",
                            name,
                            started.elapsed().as_secs(),
                            code,
                            output
                        );
                    }
                    std::thread::sleep(poll.min(timeout.saturating_sub(started.elapsed())));
                    None
                }
                Err(e) => Some(Err(e)),
            })
            .unwrap_or_else(|| {
                Err(AppError::CmdError(format!(
                    "{} guest health check did not pass within {}s",
                    name,
                    timeout.as_secs()
                )))
            })
    }
}

pub(crate) fn guest_check_script(command: &str) -> String {
    format!("if [ -x {command} ]; then exec {command}; fi")
}

#[derive(Debug, PartialEq)]
pub enum ExecOutcome {
    Succeeded { stdout: String },
    Failed { code: Option<i32>, output: String },
}

pub fn exec(ct_id: Vmid, argv: &[&str], timeout: Duration) -> Result<ExecOutcome> {
    let mut child = Command::new(Lxc::TOOL)
        .arg("exec")
        .arg(ct_id.get().to_string())
        .arg("--")
        .args(argv)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    match child::wait(&mut child, timeout)? {
        Exit::Finished(_) => {
            let out = child.wait_with_output()?;
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            Ok(if out.status.success() {
                ExecOutcome::Succeeded { stdout }
            } else {
                ExecOutcome::Failed {
                    code: out.status.code(),
                    output: format!("{}{}", stdout, String::from_utf8_lossy(&out.stderr))
                        .trim()
                        .to_string(),
                }
            })
        }
        Exit::TimedOut => {
            let _ = child.kill();
            let _ = child.wait();
            Err(AppError::CmdError(format!(
                "pct exec {} {:?} did not return within {}s",
                ct_id.get(),
                argv,
                timeout.as_secs()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMAND: &str = "/run/current-system/sw/bin/proxnix-health-check";

    #[test]
    fn a_guest_without_a_check_script_passes_rather_than_erroring() {
        let script = guest_check_script(COMMAND);
        assert!(script.contains(COMMAND));
        assert!(
            script.starts_with("if [ -x "),
            "a guest that declares no check must not fail the deploy"
        );
    }

    #[test]
    fn the_check_replaces_the_shell_so_its_exit_code_is_the_verdict() {
        assert!(
            guest_check_script(COMMAND).contains(&format!("exec {COMMAND}")),
            "the script's exit status must be what proxnix sees"
        );
    }
}
