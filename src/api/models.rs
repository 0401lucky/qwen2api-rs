//! 模型列表：GET /v1/models（需認證）、GET /v1/models/{id}（開放）。

use crate::auth::resolve_auth;
use crate::request::model_catalog::{build_openai_model_list, fallback_model_list};
use crate::request::model_modes::parse_model_mode;
use crate::state::AppState;
use crate::util::now_secs;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::collections::HashMap;

const UPSTREAM_MODELS_TTL: f64 = 300.0;

/// 取得（快取的）上游模型列表。
async fn fetch_upstream_models(state: &AppState) -> Vec<serde_json::Value> {
    {
        let cache = state.upstream_models.read().await;
        if !cache.data.is_empty() && now_secs() - cache.fetched_at < UPSTREAM_MODELS_TTL {
            return cache.data.clone();
        }
    }
    let token = match state.pool.any_valid_token().await {
        Some(t) => t,
        None => return Vec::new(),
    };
    let models = state.client.list_models(&token).await;
    if !models.is_empty() {
        let mut cache = state.upstream_models.write().await;
        cache.data = models.clone();
        cache.fetched_at = now_secs();
    }
    models
}

pub async fn list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Err(e) = resolve_auth(&state, &headers, &query).await {
        return e.into_response();
    }
    let upstream = fetch_upstream_models(&state).await;
    let data = if upstream.is_empty() {
        fallback_model_list(&state.settings.default_model)
    } else {
        build_openai_model_list(&upstream)
    };
    Json(json!({ "object": "list", "data": data })).into_response()
}

/// GET /v1/models/{id}（開放，不需認證，對齊原版）。
pub async fn get_model(State(state): State<AppState>, Path(model_id): Path<String>) -> Response {
    let mode = parse_model_mode(&model_id);
    let _ = state; // 預留：未來可查上游能力
    Json(json!({
        "id": model_id,
        "object": "model",
        "created": crate::util::now_unix(),
        "owned_by": "qwen",
        "base_model": mode.base_model,
        "mode": mode.mode,
        "capabilities": { "thinking": mode.force_thinking || mode.mode == "thinking", "search": true, "vision": true },
    }))
    .into_response()
}
