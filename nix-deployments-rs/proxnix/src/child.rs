use crate::types::Result;
use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(250);

pub enum Exit {
    Finished(ExitStatus),
    TimedOut,
}

pub fn wait(child: &mut Child, timeout: Duration) -> Result<Exit> {
    let started = Instant::now();
    std::iter::repeat(())
        .find_map(|()| match child.try_wait() {
            Ok(Some(status)) => Some(Ok(Exit::Finished(status))),
            Ok(None) if started.elapsed() >= timeout => Some(Ok(Exit::TimedOut)),
            Ok(None) => {
                std::thread::sleep(POLL);
                None
            }
            Err(e) => Some(Err(e.into())),
        })
        .unwrap_or(Ok(Exit::TimedOut))
}

pub fn kill_group(child: &mut Child) {
    if let Ok(group) = libc::pid_t::try_from(child.id()) {
        // SAFETY: killpg only signals the process group this child was spawned to lead.
        unsafe {
            libc::killpg(group, libc::SIGKILL);
        }
    }
    child.kill().ok();
    child.wait().ok();
}
