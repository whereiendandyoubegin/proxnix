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
    last_repo: Arc<RwLock<Option<(String, String)>>>,
    appconfig: AppConfig,
    pve: Pve,
}

mod api;
mod build;
mod child;
mod context;
mod deployments;
mod git;
mod host;
mod host_net;
mod materialise;
mod nix;
mod parsing;
mod pipeline;
mod probe;
mod pve;
mod sozu;
mod state;
mod types;
mod zfs;

#[axum::debug_handler]
async fn webhook_handler(
    State(state): State<AppState>,
    Json(payload): Json<serde_json::Value>,
) -> StatusCode {
    let parsed = match parsing::webhook_parse(payload) {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to parse webhook: {:?}", e);
            return StatusCode::BAD_REQUEST;
        }
    };

    let git_repo_url = parsed.repository.clone();
    let current_git_commit = parsed.hash.clone();
    let lock_wait = state.appconfig.timings_ms.get(Timing::WebhookLockWait);

    let permit = if let Ok(Ok(permit)) =
        tokio::time::timeout(lock_wait, state.semaphore.clone().acquire_owned()).await
    {
        permit
    } else {
        warn!(
            "Pipeline still busy after {}s, rejecting webhook for commit {}",
            lock_wait.as_secs(),
            current_git_commit
        );
        return StatusCode::TOO_MANY_REQUESTS;
    };

    let appconfig = state.appconfig.clone();
    let pve = state.pve.clone();
    let last_repo = state.last_repo.clone();
    tokio::task::spawn_blocking(move || {
        info!(
            "Pipeline started for repo: {}, commit: {}",
            git_repo_url, current_git_commit
        );
        match pipeline::run_pipeline(&git_repo_url, &current_git_commit, &appconfig, &pve) {
            Ok(()) => {
                *last_repo.blocking_write() =
                    Some((git_repo_url.clone(), current_git_commit.clone()));
                info!(
                    "Pipeline finished for repo: {}, commit: {}",
                    git_repo_url, current_git_commit
                );
            }
            Err(e) => error!(
                "Pipeline failed for repo: {}, commit: {}, error: {:?}",
                git_repo_url, current_git_commit, e
            ),
        }
        drop(permit);
    });

    StatusCode::OK
}

enum Mode {
    Serve,
    DeployOnce,
}

impl Mode {
    fn from_args() -> Self {
        if std::env::args().any(|a| a == "--deploy-once") {
            Mode::DeployOnce
        } else {
            Mode::Serve
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let nixology_path = option_env!("PROXNIX_NIXOLOGY_PATH").unwrap_or("/root/nixology");
    let appconfig_json = nix::eval_appconfig(nixology_path).expect("Failed to eval appconfig");
    let appconfig = parse_appconfig(&appconfig_json).expect("Failed to parse appconfig");
    let server_address = appconfig.server_address;
    let pve = Pve::connect(&appconfig.proxmox, tokio::runtime::Handle::current())
        .expect("Failed to set up the Proxmox API client");

    if let Mode::DeployOnce = Mode::from_args() {
        let result = tokio::task::spawn_blocking(move || pipeline::run_local(&appconfig, &pve))
            .await
            .expect("deploy task panicked");
        match result {
            Ok(()) => info!("Deploy finished"),
            Err(e) => {
                error!("Deploy failed: {:?}", e);
                std::process::exit(1);
            }
        }
        return;
    }

    let last_repo = Arc::new(RwLock::new(None));
    let app_state = AppState {
        semaphore: Arc::new(Semaphore::new(1)),
        last_repo,
        appconfig,
        pve,
    };

    let periodic_state = app_state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(periodic_state.appconfig.timings_ms.get(Timing::PeriodicReconcile));
        loop {
            interval.tick().await;
            let permit = if let Ok(p) = periodic_state.semaphore.clone().try_acquire_owned() {
                p
            } else {
                debug!("Pipeline is running, skipping periodic reconcile");
                continue;
            };
            let pushed = periodic_state.last_repo.read().await.clone();
            let dest_path = match pushed {
                Some((_, commit_hash)) => format!("{}/{}", periodic_state.appconfig.repo_cache, commit_hash),
                None => periodic_state
                    .appconfig
                    .local_repo
                    .clone()
                    .unwrap_or_else(|| nixology_path.to_string()),
            };
            let settings = periodic_state.appconfig.clone();
            let pve = periodic_state.pve.clone();
            tokio::task::spawn_blocking(move || {
                build::ensure_vms_running(&dest_path, &settings, &pve);
                drop(permit);
            });
        }
    });

    let app = Router::new()
        .route("/whlisten", post(webhook_handler))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind(server_address).await.unwrap();
    info!("Listening on {}", server_address);
    axum::serve(listener, app).await.unwrap_or_default();
}
