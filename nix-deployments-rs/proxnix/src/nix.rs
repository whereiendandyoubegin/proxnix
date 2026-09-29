use crate::child::{self, Exit};
use crate::types::{AppError, Result};
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use tracing::{info, warn};

fn walk_for_file(dir: &Path, filename: &str, results: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_none_or(|n| n != ".git") {
                walk_for_file(&path, filename, results)?;
            }
        } else if path.file_name().is_some_and(|n| n == filename) {
            results.push(path);
        }
    }
    Ok(())
}

pub fn find_in_repo(repo_path: &str, filename: &str) -> Result<String> {
    let mut results = Vec::new();
    walk_for_file(Path::new(repo_path), filename, &mut results)?;
    match results.len() {
        0 => Err(AppError::CmdError(format!(
            "'{filename}' not found in repo"
        ))),
        1 => Ok(results.remove(0).to_string_lossy().to_string()),
        n => Err(AppError::CmdError(format!(
            "Found {n} copies of '{filename}' in repo, expected exactly 1"
        ))),
    }
}

pub fn eval_appconfig(nixology_path: &str) -> Result<String> {
    let installable = format!("{nixology_path}#proxnixcfg");
    let nix_eval = Command::new("nix")
        .arg("eval")
        .arg(&installable)
        .arg("--json")
        .output()
        .map_err(|e| AppError::CmdError(format!("Failed to run nix eval: {e}")))?;
    if !nix_eval.status.success() {
        let stderr = String::from_utf8_lossy(&nix_eval.stderr);
        return Err(AppError::CmdError(format!(
            "Nix eval failed (exit: {:?}): {}",
            nix_eval.status.code(),
            stderr
        )));
    }
    Ok(String::from_utf8(nix_eval.stdout)?)
}

pub fn eval_config(repo_path: &str, timeout: Duration) -> Result<String> {
    let flake_path = find_in_repo(repo_path, "flake.nix")?;
    let nix_dir = Path::new(&flake_path)
        .parent()
        .ok_or_else(|| AppError::CmdError("Failed to get parent path".to_string()))?;

    let mut child = Command::new("nix")
        .current_dir(nix_dir)
        .arg("eval")
        .arg(".#proxnix")
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| AppError::CmdError(format!("Failed to run nix eval: {e}")))?;

    let drain = |stream: Option<std::process::ChildStdout>| {
        stream.map(|s| {
            std::thread::spawn(move || {
                let mut buf = String::new();
                std::io::Read::read_to_string(&mut BufReader::new(s), &mut buf).ok();
                buf
            })
        })
    };
    let stdout_pump = drain(child.stdout.take());
    let stderr_pump = child.stderr.take().map(|s| {
        std::thread::spawn(move || {
            BufReader::new(s)
                .lines()
                .map_while(std::result::Result::ok)
                .filter(|l| !l.trim().is_empty())
                .collect::<Vec<_>>()
                .join("; ")
        })
    });

    let status = match child::wait(&mut child, timeout)? {
        Exit::Finished(status) => status,
        Exit::TimedOut => {
            warn!("nix eval exceeded {}s, killing it", timeout.as_secs());
            child::kill_group(&mut child);
            return Err(AppError::NixError(format!("eval of .#proxnix timed out after {}s", timeout.as_secs())));
        }
    };
    let stdout = stdout_pump.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr = stderr_pump.and_then(|h| h.join().ok()).unwrap_or_default();
    if status.success() {
        Ok(stdout)
    } else {
        Err(AppError::NixError(format!("eval of .#proxnix failed (exit: {:?}): {}", status.code(), stderr)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NixFault {
    NoFlake(String),
    Spawn(String),
    TimedOut(Duration),
    Exited { code: Option<i32>, stderr: String },
    NoOutput,
}

impl From<NixFault> for AppError {
    fn from(fault: NixFault) -> Self {
        AppError::NixError(format!("{fault:?}"))
    }
}

const STDERR_TAIL: usize = 20;

fn flake_dir(repo_path: &str) -> std::result::Result<PathBuf, NixFault> {
    let flake_path = find_in_repo(repo_path, "flake.nix").map_err(|e| NixFault::NoFlake(e.to_string()))?;
    Path::new(&flake_path)
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| NixFault::NoFlake(String::from("flake.nix has no parent directory")))
}

fn nix(dir: &Path, label: &str, args: &[&str], timeout: Duration) -> std::result::Result<String, NixFault> {
    run("nix", dir, label, args, timeout)
}

pub(crate) fn run(program: &str, dir: &Path, label: &str, args: &[&str], timeout: Duration) -> std::result::Result<String, NixFault> {
    let mut child = Command::new(program)
        .current_dir(dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| NixFault::Spawn(e.to_string()))?;
    let stdout_pump = child.stdout.take().map(|out| {
        std::thread::spawn(move || {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut BufReader::new(out), &mut text).ok();
            text
        })
    });
    let stderr_pump = child.stderr.take().map(|err| {
        let label = label.to_string();
        std::thread::spawn(move || {
            let lines: Vec<String> = BufReader::new(err)
                .lines()
                .map_while(std::result::Result::ok)
                .filter(|line| !line.trim().is_empty())
                .inspect(|line| info!("[nix {}] {}", label, line.trim()))
                .collect();
            lines[lines.len().saturating_sub(STDERR_TAIL)..].to_vec()
        })
    });
    let status = match child::wait(&mut child, timeout).map_err(|e| NixFault::Spawn(e.to_string()))? {
        Exit::Finished(status) => status,
        Exit::TimedOut => {
            warn!("nix {} exceeded {}s, killing it", label, timeout.as_secs());
            child::kill_group(&mut child);
            return Err(NixFault::TimedOut(timeout));
        }
    };
    let stdout = stdout_pump.and_then(|h| h.join().ok()).unwrap_or_default();
    let stderr: Vec<String> = stderr_pump.and_then(|h| h.join().ok()).unwrap_or_default();
    if status.success() { Ok(stdout) } else { Err(NixFault::Exited { code: status.code(), stderr: stderr.join("\n") }) }
}

fn first_line(stdout: &str) -> std::result::Result<String, NixFault> {
    stdout.lines().map(str::trim).find(|line| !line.is_empty()).map(str::to_string).ok_or(NixFault::NoOutput)
}

fn installable(config_name: &str, build_attr: &str) -> String {
    format!(".#nixosConfigurations.{config_name}.{build_attr}")
}

pub fn realise(config_name: &str, build_attr: &str, repo_path: &str, impure: bool, timeout: Duration) -> std::result::Result<String, NixFault> {
    let dir = flake_dir(repo_path)?;
    let target = installable(config_name, build_attr);
    let flags: &[&str] = if impure { &["--impure"] } else { &[] };
    let build: Vec<&str> = ["build", target.as_str(), "--no-link"].into_iter().chain(flags.iter().copied()).collect();
    nix(&dir, config_name, &build, timeout)?;
    let path_info: Vec<&str> = ["path-info", target.as_str()].into_iter().chain(flags.iter().copied()).collect();
    let store_path = first_line(&nix(&dir, config_name, &path_info, timeout)?)?;
    info!("Nix build succeeded for '{}': {}", config_name, store_path);
    Ok(store_path)
}

pub fn out_path(config_name: &str, build_attr: &str, repo_path: &str, impure: bool, timeout: Duration) -> std::result::Result<String, NixFault> {
    let dir = flake_dir(repo_path)?;
    let target = format!("{}.outPath", installable(config_name, build_attr));
    let flags: &[&str] = if impure { &["--impure"] } else { &[] };
    let eval: Vec<&str> = ["eval", "--raw", target.as_str()].into_iter().chain(flags.iter().copied()).collect();
    first_line(&nix(&dir, config_name, &eval, timeout)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_hung_command_is_abandoned_at_its_timeout_even_when_a_child_holds_its_output() {
        let script = std::env::temp_dir().join(format!("proxnix-hung-{}", std::process::id()));
        std::fs::write(&script, "#!/bin/sh\necho working >&2\nsleep 30\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        let result = run(script.to_str().unwrap(), Path::new("/"), "hung", &[], Duration::from_secs(1));
        let _ = std::fs::remove_file(&script);
        assert_eq!(result, Err(NixFault::TimedOut(Duration::from_secs(1))));
        assert!(started.elapsed() < Duration::from_secs(10), "waited {:?} on a killed command", started.elapsed());
    }
}
