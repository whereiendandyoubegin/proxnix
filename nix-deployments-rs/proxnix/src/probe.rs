use crate::api::{Kind, Lxc};
use crate::child::{self, Exit};
use crate::types::{AppError, Result};
use proxnix_core::Vmid;
use std::process::{Command, Stdio};
use std::time::Duration;

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
