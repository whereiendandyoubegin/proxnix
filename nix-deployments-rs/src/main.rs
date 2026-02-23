use axum::{Json, Router, extract::State, http::StatusCode, routing::post};

use serde::Deserialize;
use std::fmt::Display;
use std::{sync::Arc, thread::sleep, time::Duration};
use tokio::sync::Semaphore;

mod build;
mod git;
mod nix;
mod qm;
mod state;
mod types;

#[derive(Deserialize)]
struct Response {
    response: String,
}

#[derive(Deserialize, Debug)]
struct Repository {
    ssh_url: String,
}

#[derive(Deserialize, Debug)]
struct GiteaWebhook {
    repository: Repository,
    after: String,
}

#[derive(Clone)]
struct AppState {
    main_semaphore: Arc<Semaphore>,
}

#[axum::debug_handler]
async fn webhook_handler(
    State(state): State<AppState>,
    Json(payload): Json<GiteaWebhook>,
) -> StatusCode {
    println!("Raw JSON:\n{}", payload);
    let git_repo_url = payload.repository.ssh_url.clone();
    let current_git_commit = payload.after.clone();

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
        .filter(|role: String| role.contains("build-qcow2"))
        .map(|role| role.strip_prefix("build-qcow2").unwrap)
        .collect();
    stripped
}
