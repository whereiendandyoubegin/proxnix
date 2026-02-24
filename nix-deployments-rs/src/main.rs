use axum::{Json, Router, extract::State, http::StatusCode, routing::post};

use serde::Deserialize;
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;

#[derive(Clone)]
struct AppState {
    main_semaphore: Arc<Semaphore>,
}

mod build;
mod git;
mod nix;
mod parsing;
mod qm;
mod state;
mod types;

#[derive(Deserialize)]
struct Response {
    response: String,
}

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

    let permit = match state.main_semaphore.try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return StatusCode::TOO_MANY_REQUESTS,
    };

    tokio::spawn(async move {
        println!(
            "Pipeline started for repo: {}, commit: {}",
            git_repo_url, current_git_commit
        );
        tokio::time::sleep(Duration::from_secs(10)).await;
        println!(
            "Pipeline finished for repo: {}, commit: {}",
            git_repo_url, current_git_commit
        );
        drop(permit);
    });
    StatusCode::OK
}

#[tokio::main]
async fn main() {
    let config = state::load_json("definitions/config.json");
    println!("{:#?}", config);
    let main_semaphore = Arc::new(Semaphore::new(4));
    let app_state = AppState { main_semaphore };

    let app = Router::new()
        .route("/whlisten", post(webhook_handler))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:6780").await.unwrap();
    axum::serve(listener, app).await.unwrap_or_default()
}

pub fn strip_role(roles: Vec<String>) -> Vec<String> {
    let stripped = roles
        .into_iter()
        .filter(|role: &String| role.contains("build-qcow2"))
        .map(|role| role.strip_prefix("build-qcow2").unwrap_or("").to_string())
        .collect();
    stripped
}
