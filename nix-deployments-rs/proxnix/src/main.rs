use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use std::sync::Arc;
use tokio::sync::{RwLock, Semaphore};
use tracing::{debug, error, info, warn};

use crate::pve::Pve;
use crate::state::parse_appconfig;
use crate::types::{AppConfig, Timing};

#[derive(Clone)]
struct AppState {
    semaphore: Arc<Semaphore>,
    last_repo: Arc<RwLock<Option<String>>>,
    appconfig: AppConfig,
    pve: Pve,
}

mod api;
mod child;
mod context;
mod engine;
mod git;
mod host;
mod host_net;
mod interpret;
mod materialise;
mod nix;
mod parsing;
mod pipeline;
mod probe;
mod pve;
mod remote;
mod render;
mod sozu;
mod state;
mod types;
mod zfs;

#[axum::debug_handler]
async fn webhook_handler(State(state): State<AppState>, Json(payload): Json<serde_json::Value>) -> StatusCode {
    let parsed = match parsing::webhook_parse(payload) {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to parse webhook: {:?}", e);
            return StatusCode::BAD_REQUEST;
        }
    };

    let lock_wait = state.appconfig.timings_ms.get(Timing::WebhookLockWait);
    let Ok(Ok(permit)) = tokio::time::timeout(lock_wait, state.semaphore.clone().acquire_owned()).await else {
        warn!("Deploy still busy after {}s, rejecting webhook for commit {}", lock_wait.as_secs(), parsed.hash);
        return StatusCode::TOO_MANY_REQUESTS;
    };

    let appconfig = state.appconfig.clone();
    let pve = state.pve.clone();
    let last_repo = state.last_repo.clone();
    tokio::task::spawn_blocking(move || {
        info!("Deploy started for repo: {}, commit: {}", parsed.repository, parsed.hash);
        let resolved = pipeline::RepoSource::pushed(&appconfig, &parsed.repository, &parsed.hash).resolve(&appconfig);
        match resolved.and_then(|repo| engine::deploy(&appconfig, &pve, &repo).map(|outcomes| (repo, outcomes))) {
            Ok((repo, outcomes)) => {
                let clean = engine::outcome_ok(&outcomes);
                *last_repo.blocking_write() = Some(repo);
                info!("Deploy finished for commit {}{}", parsed.hash, if clean { "" } else { " with failures" });
            }
            Err(e) => error!("Deploy failed for repo: {}, commit: {}, error: {:?}", parsed.repository, parsed.hash, e),
        }
        drop(permit);
    });

    StatusCode::OK
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Serve,
    DeployOnce,
    Plan,
}

#[derive(Debug, PartialEq, Eq)]
struct Cli {
    mode: Mode,
    repo: Option<std::path::PathBuf>,
}

const USAGE: &str = "usage:\n  proxnix                            serve webhooks and run the periodic reconcile\n  proxnix --deploy-once [--repo DIR] deploy the repo once\n  proxnix --plan [--repo DIR]        print what a deploy would do; writes nothing\n--repo defaults to services.proxnix.local_repo";

fn parse_args(args: &[String]) -> std::result::Result<Cli, String> {
    let (flags, repo) = match args.iter().position(|arg| arg == "--repo") {
        None => (args.to_vec(), None),
        Some(at) => match args.get(at + 1) {
            Some(path) if !path.starts_with("--") => (
                args.iter().enumerate().filter(|(index, _)| *index != at && *index != at + 1).map(|(_, arg)| arg.clone()).collect(),
                Some(std::path::PathBuf::from(path)),
            ),
            _ => return Err(String::from("--repo needs a directory")),
        },
    };
    let words: Vec<&str> = flags.iter().map(String::as_str).collect();
    let mode = match words.as_slice() {
        [] => Mode::Serve,
        ["--deploy-once"] => Mode::DeployOnce,
        ["--plan"] => Mode::Plan,
        other => return Err(format!("unrecognised arguments {other:?}")),
    };
    match (mode, &repo) {
        (Mode::Serve, Some(_)) => Err(String::from("--repo only applies to --plan and --deploy-once")),
        _ => Ok(Cli { mode, repo }),
    }
}

async fn run_once(mode: Mode, repo: Option<std::path::PathBuf>, appconfig: AppConfig, pve: Pve) {
    let Some(repo) = repo.or_else(|| appconfig.local_repo.clone().map(std::path::PathBuf::from)) else {
        error!("--plan and --deploy-once need --repo PATH or services.proxnix.local_repo");
        std::process::exit(2);
    };
    if !repo.is_dir() {
        error!("--repo {} is not a directory; pass the path of a checkout, not a commit", repo.display());
        std::process::exit(2);
    }
    let repo = repo.to_string_lossy().to_string();
    let finished = tokio::task::spawn_blocking(move || match mode {
        Mode::Plan => engine::plan(&appconfig, &pve, &repo).map(|(prepared, projections)| {
            println!("{}", render::plan(&prepared.commit, &projections));
            true
        }),
        _ => engine::deploy(&appconfig, &pve, &repo).map(|outcomes| engine::outcome_ok(&outcomes)),
    })
    .await
    .expect("engine task panicked");
    match finished {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(e) => {
            error!("{:?}", e);
            std::process::exit(1);
        }
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cli = match parse_args(&args) {
        Ok(cli) => cli,
        Err(problem) => {
            eprintln!("proxnix: {problem}\n{USAGE}");
            std::process::exit(2);
        }
    };
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")))
        .init();

    let nixology_path = option_env!("PROXNIX_NIXOLOGY_PATH").unwrap_or("/root/nixology");
    let appconfig_json = nix::eval_appconfig(nixology_path).expect("Failed to eval appconfig");
    let appconfig = parse_appconfig(&appconfig_json).expect("Failed to parse appconfig");
    let server_address = appconfig.server_address;
    let pve = Pve::connect(&appconfig.proxmox, tokio::runtime::Handle::current()).expect("Failed to set up the Proxmox API client");

    if let Mode::Plan | Mode::DeployOnce = cli.mode {
        run_once(cli.mode, cli.repo, appconfig, pve).await;
        return;
    }

    let app_state = AppState { semaphore: Arc::new(Semaphore::new(1)), last_repo: Arc::new(RwLock::new(None)), appconfig, pve };

    let periodic_state = app_state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(periodic_state.appconfig.timings_ms.get(Timing::PeriodicReconcile));
        loop {
            interval.tick().await;
            let Ok(permit) = periodic_state.semaphore.clone().try_acquire_owned() else {
                debug!("A deploy is running, skipping periodic reconcile");
                continue;
            };
            let repo = periodic_state
                .last_repo
                .read()
                .await
                .clone()
                .or_else(|| periodic_state.appconfig.local_repo.clone())
                .unwrap_or_else(|| nixology_path.to_string());
            let settings = periodic_state.appconfig.clone();
            let pve = periodic_state.pve.clone();
            tokio::task::spawn_blocking(move || {
                match engine::periodic(&settings, &pve, &repo) {
                    Ok(outcomes) => {
                        engine::outcome_ok(&outcomes);
                    }
                    Err(e) => warn!("periodic reconcile failed: {:?}", e),
                }
                drop(permit);
            });
        }
    });

    let app = Router::new().route("/whlisten", post(webhook_handler)).with_state(app_state);

    let listener = tokio::net::TcpListener::bind(server_address).await.unwrap();
    info!("Listening on {}", server_address);
    axum::serve(listener, app).await.unwrap_or_default();
}


#[cfg(test)]
mod cli_tests {
    use super::*;

    fn parse(args: &[&str]) -> std::result::Result<Cli, String> {
        parse_args(&args.iter().map(|arg| (*arg).to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn only_no_arguments_serves() {
        assert_eq!(parse(&[]).unwrap().mode, Mode::Serve);
        assert!(parse(&["--plna"]).is_err());
        assert!(parse(&["--plan", "--deploy-once"]).is_err());
        assert!(parse(&["--core"]).is_err());
        assert!(parse(&["e10b8a3d07fda12c829b028ee5dd5a0ae7710cc4"]).is_err());
    }

    #[test]
    fn the_engine_modes_take_an_optional_repo() {
        assert_eq!(parse(&["--plan"]).unwrap(), Cli { mode: Mode::Plan, repo: None });
        assert_eq!(
            parse(&["--plan", "--repo", "/root/nixology"]).unwrap(),
            Cli { mode: Mode::Plan, repo: Some(std::path::PathBuf::from("/root/nixology")) }
        );
        assert_eq!(parse(&["--repo", "/r", "--deploy-once"]).unwrap().mode, Mode::DeployOnce);
        assert!(parse(&["--plan", "--repo"]).is_err());
        assert!(parse(&["--plan", "--repo", "--deploy-once"]).is_err());
        assert!(parse(&["--deploy-once", "--core"]).is_err());
        assert!(parse(&["--repo", "/r"]).is_err());
    }
}

