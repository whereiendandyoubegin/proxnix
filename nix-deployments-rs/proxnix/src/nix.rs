use crate::types::{AppError, Result};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use tracing::{info, warn};

pub const BASE_REPO_PATH: &str = "/tmp/proxnix/repos";
const NIX_BUILD_TIMEOUT: Duration = Duration::from_secs(3600);
const NIX_POLL_INTERVAL: Duration = Duration::from_millis(500);

enum BuildOutcome {
    Finished(ExitStatus),
    TimedOut,
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<BuildOutcome> {
    let started = Instant::now();
    loop {
        match child.try_wait()? {
            Some(status) => return Ok(BuildOutcome::Finished(status)),
            None => match started.elapsed() >= timeout {
                true => return Ok(BuildOutcome::TimedOut),
                false => std::thread::sleep(NIX_POLL_INTERVAL),
            },
        }
    }
}

fn walk_for_file(dir: &Path, filename: &str, results: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().map(|n| n != ".git").unwrap_or(true) {
                walk_for_file(&path, filename, results)?;
            }
        } else if path.file_name().map(|n| n == filename).unwrap_or(false) {
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
            "'{}' not found in repo",
            filename
        ))),
        1 => Ok(results.remove(0).to_string_lossy().to_string()),
        n => Err(AppError::CmdError(format!(
            "Found {} copies of '{}' in repo, expected exactly 1",
            n, filename
        ))),
    }
}

pub fn eval_appconfig(nixology_path: &str) -> Result<String> {
    let installable = format!("{}#proxnixcfg", nixology_path);
    let nix_eval = Command::new("nix")
        .arg("eval")
        .arg(&installable)
        .arg("--json")
        .output()
        .map_err(|e| AppError::CmdError(format!("Failed to run nix eval: {}", e)))?;
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

pub fn eval_config(repo_path: &str) -> Result<String> {
    let flake_path = find_in_repo(repo_path, "flake.nix")?;
    let nix_dir = Path::new(&flake_path)
        .parent()
        .ok_or_else(|| AppError::CmdError("Failed to get parent path".to_string()))?;

    let nix_eval = Command::new("nix")
        .current_dir(nix_dir)
        .arg("eval")
        .arg(".#proxnix")
        .arg("--json")
        .output()
        .map_err(|e| AppError::CmdError(format!("Failed to run nix eval: {}", e)))?;
    if !nix_eval.status.success() {
        let stderr = String::from_utf8_lossy(&nix_eval.stderr);
        return Err(AppError::CmdError(format!(
            "Nix eval failed (exit: {:?}): {}",
            nix_eval.status.code(),
            stderr
        )));
    }
    let output_string = String::from_utf8(nix_eval.stdout)?;

    Ok(output_string)
}



pub fn list_nix_configs(repo_path: &str) -> Result<Vec<String>> {
    let flake_path = find_in_repo(repo_path, "flake.nix")?;
    let nix_dir = Path::new(&flake_path)
        .parent()
        .ok_or_else(|| AppError::CmdError("flake.nix has no parent directory".to_string()))?;

    let nix_eval = Command::new("nix")
        .current_dir(nix_dir)
        .arg("eval")
        .arg(".#nixosConfigurations")
        .arg("--apply")
        .arg("builtins.attrNames")
        .arg("--json")
        .output()
        .map_err(|e| AppError::CmdError(format!("Failed to run nix eval: {}", e)))?;
    if !nix_eval.status.success() {
        let stderr = String::from_utf8_lossy(&nix_eval.stderr);
        return Err(AppError::CmdError(format!(
            "Nix eval failed (exit: {:?}): {}",
            nix_eval.status.code(),
            stderr
        )));
    }
    let stdout_bytes = nix_eval.stdout;
    let output_string = String::from_utf8(stdout_bytes)?;
    let parsed: Vec<String> = serde_json::from_str(&output_string)?;

    Ok(parsed)
}

pub fn nix_build(config_name: &str, build_attr: &str, repo_path: &str) -> Result<String> {
    let flake_path = find_in_repo(repo_path, "flake.nix")?;
    let nix_dir = Path::new(&flake_path)
        .parent()
        .ok_or_else(|| AppError::CmdError("flake.nix has no parent directory".to_string()))?;

    info!(
        "Running nix build for config '{}' ({}) in {}",
        config_name,
        build_attr,
        nix_dir.display()
    );
    let installable = format!(".#nixosConfigurations.{}.{}", config_name, build_attr);
    let mut child = Command::new("nix")
        .current_dir(nix_dir)
        .arg("build")
        .arg(&installable)
        .arg("--no-link")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::CmdError(format!("Failed to run nix build: {}", e)))?;

    let pump = child.stderr.take().map(|stderr| {
        let label = config_name.to_string();
        std::thread::spawn(move || {
            BufReader::new(stderr).lines().for_each(|line| match line {
                Ok(text) if !text.trim().is_empty() => info!("[nix {}] {}", label, text.trim()),
                _ => {}
            })
        })
    });

    let outcome = wait_with_timeout(&mut child, NIX_BUILD_TIMEOUT)?;
    if let Some(handle) = pump {
        handle.join().ok();
    }

    match outcome {
        BuildOutcome::TimedOut => {
            warn!(
                "nix build for '{}' exceeded {}s, killing it",
                config_name,
                NIX_BUILD_TIMEOUT.as_secs()
            );
            child.kill().ok();
            child.wait().ok();
            return Err(AppError::NixError(format!(
                "build for '{}' timed out after {}s",
                config_name,
                NIX_BUILD_TIMEOUT.as_secs()
            )));
        }
        BuildOutcome::Finished(status) if !status.success() => {
            return Err(AppError::NixError(format!(
                "build for '{}' failed (exit: {:?})",
                config_name,
                status.code()
            )));
        }
        BuildOutcome::Finished(_) => {}
    }

    let path_output = Command::new("nix")
        .current_dir(nix_dir)
        .arg("path-info")
        .arg(&installable)
        .output()
        .map_err(|e| AppError::CmdError(format!("Failed to run nix path-info: {}", e)))?;
    if !path_output.status.success() {
        let stderr = String::from_utf8_lossy(&path_output.stderr);
        return Err(AppError::CmdError(format!(
            "nix path-info failed for '{}' (exit: {:?}): {}",
            config_name,
            path_output.status.code(),
            stderr
        )));
    }
    let stdout = String::from_utf8(path_output.stdout)?;
    let store_path = stdout.lines().find(|l| !l.trim().is_empty())
        .ok_or_else(|| AppError::CmdError(format!("nix path-info produced no output for '{}'", config_name)))?
        .trim()
        .to_string();
    info!("Nix build succeeded for '{}': {}", config_name, store_path);

    Ok(store_path)
}

// TODO I need to finish up some utils to initialise this dir on setup. I will probably do a utils module.
// I probably wil want to init the user there as well rather than in this module
pub fn configure_dirs(configs: Vec<String>, repo_path: &str) -> Result<()> {
    let repo_base = std::path::Path::new(repo_path);
    std::fs::create_dir_all(repo_base)?;

    for config in configs {
        std::fs::create_dir_all(repo_base.join(config))?;
    }

    Ok(())
}
