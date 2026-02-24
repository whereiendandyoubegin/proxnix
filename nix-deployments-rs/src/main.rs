use axum::{Json, Router, extract::State, http::StatusCode, routing::post};

use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Clone)]
struct AppState {
    semaphore: Arc<Semaphore>,
    config_path: String,
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
            println!("Failed to parse webhook: {:?}", e);
            return StatusCode::BAD_REQUEST;
        }
    };

    let git_repo_url = parsed.repository.clone();
    let current_git_commit = parsed.hash.clone();

    let permit = match state.semaphore.try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return StatusCode::TOO_MANY_REQUESTS,
    };

    let config_path = state.config_path.clone();

    tokio::task::spawn_blocking(move || {
        println!(
            "Pipeline started for repo: {}, commit: {}",
            git_repo_url, current_git_commit
        );
        match build::run_pipeline(&git_repo_url, &current_git_commit, &config_path) {
            Ok(_) => println!(
                "Pipeline finished for repo: {}, commit: {}",
                git_repo_url, current_git_commit
            ),
            Err(e) => println!(
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
    let app_state = AppState {
        semaphore: Arc::new(Semaphore::new(1)),
        config_path: "definitions/config.json".to_string(),
    };

    let app = Router::new()
        .route("/whlisten", post(webhook_handler))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:6780").await.unwrap();
    axum::serve(listener, app).await.unwrap_or_default()
}
