use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, Semaphore};
use tracing::{error, info, warn};

use crate::state::parse_appconfig;
use crate::types::AppConfig;

#[derive(Clone)]
struct AppState {
    semaphore: Arc<Semaphore>,
    last_repo: Arc<RwLock<Option<(String, String)>>>,
    appconfig: AppConfig,
}

mod build;
mod deployments;
mod git;
mod materialise;
mod nix;
mod parsing;
mod pct;
mod pipeline;
mod qm;
mod sozu;
mod state;
mod types;

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

    let permit = match state.semaphore.try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            warn!(
                "Pipeline already running, rejecting webhook for commit {}",
                current_git_commit
            );
            return StatusCode::TOO_MANY_REQUESTS;
        }
    };

    {
        let mut guard = state.last_repo.write().await;
        *guard = Some((git_repo_url.clone(), current_git_commit.clone()));
    }

    let appconfig = state.appconfig.clone();
    tokio::task::spawn_blocking(move || {
        info!(
            "Pipeline started for repo: {}, commit: {}",
            git_repo_url, current_git_commit
        );
        match pipeline::run_pipeline(&git_repo_url, &current_git_commit, &appconfig) {
            Ok(_) => info!(
                "Pipeline finished for repo: {}, commit: {}",
                git_repo_url, current_git_commit
            ),
            Err(e) => error!(
                "Pipeline failed for repo: {}, commit: {}, error: {:?}",
                git_repo_url, current_git_commit, e
            ),
        }
        drop(permit);
    });

    StatusCode::OK
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

    let last_repo = Arc::new(RwLock::new(None));
    let app_state = AppState {
        semaphore: Arc::new(Semaphore::new(1)),
        last_repo,
        appconfig,
    };

    let periodic_state = app_state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            let permit = match periodic_state.semaphore.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    info!("Pipeline is running, skipping periodic reconcile");
                    continue;
                }
            };
            let lr = periodic_state.last_repo.read().await.clone();
            match lr {
                None => {
                    info!("No pipeline has run yet")
                }
                Some((_, commit_hash)) => {
                    let dest_path = format!("{}/{}", nix::BASE_REPO_PATH, commit_hash);
                    tokio::task::spawn_blocking(move || {
                        build::ensure_vms_running(&dest_path);
                        drop(permit);
                    });
                }
            }
        }
    });

    let app = Router::new()
        .route("/whlisten", post(webhook_handler))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind(server_address).await.unwrap();
    info!("Listening on {}", server_address);
    axum::serve(listener, app).await.unwrap_or_default()
}
