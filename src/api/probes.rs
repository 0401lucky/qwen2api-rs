//! 健康/就緒探針，對應 Python `api/probes.py`。

use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

pub async fn healthz() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

pub async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    let accounts = state.pool.count().await;
    Json(json!({ "status": "ready", "accounts": accounts }))
}

pub async fn root() -> impl IntoResponse {
    Json(json!({
        "status": "qwen2API Enterprise Gateway (Rust) is running",
        "version": "2.0.0"
    }))
}

pub async fn keepalive() -> impl IntoResponse {
    Json(json!({ "ok": true, "service": "qwen2api-rs" }))
}

pub async fn keepalive_head() -> StatusCode {
    StatusCode::NO_CONTENT
}
