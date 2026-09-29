use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use std::sync::Arc;
use tokio::sync::{RwLock, Semaphore};
use tracing::{debug, error, info, warn};

use crate::pve::Pve;
use crate::state::parse_appconfig;
use crate::types::{AppConfig, Timing};

#[derive(Clone)]
struct AppState {
    mode: Mode,
    semaphore: Arc<Semaphore>,
    synced: Arc<tokio::sync::Notify>,
    syncer: Option<Arc<sync::Syncer>>,
    last_repo: Arc<RwLock<Option<String>>>,
    appconfig: AppConfig,
    pve: Pve,
}

mod api;
mod builds;
mod child;
mod context;
mod hydra;
mod engine;
mod git;
mod host;
mod host_net;
mod interpret;
mod materialise;
mod nix;
mod nixstore;
mod parsing;
mod pipeline;
mod probe;
mod pve;
mod remote;
mod render;
mod sozu;
mod state;
mod sync;
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

    if state.mode == Mode::BuildOnly {
        let appconfig = state.appconfig.clone();
        let last_repo = state.last_repo.clone();
        let synced = state.synced.clone();
        tokio::task::spawn_blocking(move || {
            match pipeline::RepoSource::pushed(&appconfig, &parsed.repository, &parsed.hash).resolve(&appconfig) {
                Ok(repo) => {
                    info!("build-only: syncing the store for commit {}", parsed.hash);
                    *last_repo.blocking_write() = Some(repo);
                    synced.notify_one();
                }
                Err(e) => error!("build-only: could not check out commit {}: {:?}", parsed.hash, e),
            }
        });
        return StatusCode::OK;
    }

    let lock_wait = state.appconfig.timings_ms.get(Timing::WebhookLockWait);
    let Ok(Ok(permit)) = tokio::time::timeout(lock_wait, state.semaphore.clone().acquire_owned()).await else {
        warn!("Deploy still busy after {}s, rejecting webhook for commit {}", lock_wait.as_secs(), parsed.hash);
        return StatusCode::TOO_MANY_REQUESTS;
    };

    let appconfig = state.appconfig.clone();
    let pve = state.pve.clone();
    let last_repo = state.last_repo.clone();
    let synced = state.synced.clone();
    tokio::task::spawn_blocking(move || {
        info!("Deploy started for repo: {}, commit: {}", parsed.repository, parsed.hash);
        let resolved = pipeline::RepoSource::pushed(&appconfig, &parsed.repository, &parsed.hash).resolve(&appconfig);
        match resolved.and_then(|repo| engine::deploy(&appconfig, &pve, &repo).map(|outcomes| (repo, outcomes))) {
            Ok((repo, outcomes)) => {
                let clean = engine::outcome_ok(&outcomes);
                *last_repo.blocking_write() = Some(repo);
                synced.notify_one();
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
    BuildOnly,
    DeployOnce,
    Plan,
}

async fn builds_handler(State(state): State<AppState>) -> std::result::Result<Json<Vec<builds::BuildView>>, StatusCode> {
    state.syncer.as_ref().map(|syncer| Json(syncer.views())).ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

async fn build_handler(State(state): State<AppState>, Path(job): Path<String>) -> std::result::Result<Json<Vec<builds::BuildView>>, StatusCode> {
    let syncer = state.syncer.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let rows: Vec<builds::BuildView> = syncer.views().into_iter().filter(|row| row.job == job).collect();
    if rows.is_empty() { Err(StatusCode::NOT_FOUND) } else { Ok(Json(rows)) }
}

#[derive(Debug, PartialEq, Eq)]
struct Cli {
    mode: Mode,
    repo: Option<std::path::PathBuf>,
}

const USAGE: &str = "usage:\n  proxnix                            serve webhooks and run the periodic reconcile\n  proxnix --build-only               keep the store synced and serve /builds; never deploy\n  proxnix --deploy-once [--repo DIR] deploy the repo once\n  proxnix --plan [--repo DIR]        print what a deploy would do; writes nothing\n--repo defaults to services.proxnix.local_repo";

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
        ["--build-only"] => Mode::BuildOnly,
        ["--deploy-once"] => Mode::DeployOnce,
        ["--plan"] => Mode::Plan,
        other => return Err(format!("unrecognised arguments {other:?}")),
    };
    match (mode, &repo) {
        (Mode::Serve | Mode::BuildOnly, Some(_)) => Err(String::from("--repo only applies to --plan and --deploy-once")),
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
            println!("{}", render::host(&engine::host_effects(&prepared.declared)));
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

fn sync_pass(syncer: &sync::Syncer, settings: &AppConfig, pve: &Pve, repo: &str) -> types::Result<()> {
    let prepared = engine::prepare(settings, repo)?;
    syncer.pass(std::path::PathBuf::from(repo), &prepared.declared, &prepared.commit, &|| state::observe(pve).map_err(types::AppError::from))
}

fn store_syncer(appconfig: &AppConfig) -> Option<Arc<sync::Syncer>> {
    let Some(config) = appconfig.store_sync.clone() else {
        info!("store sync is off; set services.proxnix.store_sync to enable it");
        return None;
    };
    let Some(layout) = engine::layout(appconfig) else {
        warn!("store sync needs zfs_images to place the store; it stays off");
        return None;
    };
    let store = layout.store();
    let root = std::path::PathBuf::from(host::host_text(&store.mountpoint()));
    let ensured = host::ensure(&proxnix_core::HostEffect::EnsureDataset { dataset: store, owner: proxnix_core::Owner::HostRoot }, appconfig.unprivileged_idmap);
    match ensured {
        Err(error) => {
            warn!("store sync cannot prepare its dataset and stays off: {error}");
            None
        }
        Ok(()) => Some(Arc::new(sync::Syncer::new(appconfig, config, root, tokio::runtime::Handle::current()))),
    }
}

fn spawn_store_sync(state: &AppState, nixology_path: &'static str) {
    let (Some(syncer), Some(config)) = (state.syncer.clone(), state.appconfig.store_sync.clone()) else {
        return;
    };
    let interval = config.interval();
    let state = state.clone();
    tokio::spawn(async move {
        loop {
            let syncer = syncer.clone();
            let settings = state.appconfig.clone();
            let pve = state.pve.clone();
            let last_repo = state.last_repo.clone();
            let passed = tokio::task::spawn_blocking(move || {
                let repo = last_repo.blocking_read().clone().or_else(|| settings.local_repo.clone()).unwrap_or_else(|| nixology_path.to_string());
                sync_pass(&syncer, &settings, &pve, &repo)
            })
            .await;
            match passed {
                Ok(Ok(())) => {}
                Ok(Err(error)) => warn!("store sync pass failed: {:?}", error),
                Err(error) => warn!("store sync pass panicked: {error}"),
            }
            tokio::select! {
                () = tokio::time::sleep(interval) => {}
                () = state.synced.notified() => {}
            }
        }
    });
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

    let app_state = AppState {
        mode: cli.mode,
        semaphore: Arc::new(Semaphore::new(1)),
        synced: Arc::new(tokio::sync::Notify::new()),
        syncer: store_syncer(&appconfig),
        last_repo: Arc::new(RwLock::new(None)),
        appconfig,
        pve,
    };
    spawn_store_sync(&app_state, nixology_path);

    let periodic_state = app_state.clone();
    let periodic = cli.mode == Mode::Serve;
    tokio::spawn(async move {
        if !periodic {
            info!("build-only: the periodic reconcile is off");
            return;
        }
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

    let app = Router::new()
        .route("/whlisten", post(webhook_handler))
        .route("/builds", get(builds_handler))
        .route("/builds/{job}", get(build_handler))
        .with_state(app_state);

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
        assert_eq!(parse(&["--build-only"]).unwrap().mode, Mode::BuildOnly);
        assert!(parse(&["--build-only", "--repo", "/r"]).is_err());
        assert!(parse(&["--build-only", "--plan"]).is_err());
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

