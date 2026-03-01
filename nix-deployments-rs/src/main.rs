use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use std::env;
use std::fs;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, Semaphore};
use tracing::{error, info, warn};

#[derive(Clone)]
struct AppState {
    semaphore: Arc<Semaphore>,
    last_repo: Arc<RwLock<Option<(String, String)>>>,
}

mod build;
mod git;
mod nix;
mod parsing;
mod qm;
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
    let mut guard = state.last_repo.write().await;
    *guard = Some((git_repo_url.clone(), current_git_commit.clone()));

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

    tokio::task::spawn_blocking(move || {
        info!(
            "Pipeline started for repo: {}, commit: {}",
            git_repo_url, current_git_commit
        );
        match build::run_pipeline(&git_repo_url, &current_git_commit) {
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

fn init() {
    fs::create_dir_all("/var/lib/proxnix").expect("Failed to create /var/lib/proxnix");
    println!("Init complete");
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("--init") {
        init();
        return;
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let last_repo = Arc::new(RwLock::new(None));
    let app_state = AppState {
        semaphore: Arc::new(Semaphore::new(1)),
        last_repo,
    };

    let periodic_state = app_state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;
            let lr = periodic_state.last_repo.read().await.clone();
            match lr {
                None => {
                    info!("No pipeline has run yet")
                }
                Some((_, commit_hash)) => {
                    let dest_path = format!("{}/{}", nix::BASE_REPO_PATH, commit_hash);
                    tokio::task::spawn_blocking(move || {
                        build::ensure_vms_running(&dest_path);
                    });
                }
            }
        }
    });

    let app = Router::new()
        .route("/whlisten", post(webhook_handler))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:6780").await.unwrap();
    info!("Listening on 0.0.0.0:6780");
    axum::serve(listener, app).await.unwrap_or_default()
}
