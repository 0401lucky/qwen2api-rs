//! API-key 認證與配額，對應 Python `services/auth_quota.py` + admin verify_admin。

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// users.json 的單一使用者記錄。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_quota")]
    pub quota: i64,
    #[serde(default)]
    pub used_tokens: i64,
}

fn default_quota() -> i64 {
    1_000_000
}

/// 解析後的認證上下文。
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub token: String,
    pub user: Option<User>,
}

/// 從 headers / query 取出 API key：Authorization: Bearer → x-api-key → ?key/?api_key。
pub fn extract_api_token(headers: &HeaderMap, query: &HashMap<String, String>) -> Option<String> {
    if let Some(auth) = headers.get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(rest) = auth.strip_prefix("Bearer ") {
            let t = rest.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        } else if !auth.trim().is_empty() {
            // 部分客戶端直接帶裸 token
            return Some(auth.trim().to_string());
        }
    }
    if let Some(k) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        if !k.trim().is_empty() {
            return Some(k.trim().to_string());
        }
    }
    // Gemini 常用 x-goog-api-key
    if let Some(k) = headers.get("x-goog-api-key").and_then(|v| v.to_str().ok()) {
        if !k.trim().is_empty() {
            return Some(k.trim().to_string());
        }
    }
    if let Some(k) = query.get("key").or_else(|| query.get("api_key")) {
        if !k.trim().is_empty() {
            return Some(k.trim().to_string());
        }
    }
    None
}

/// 完整認證：取 token → 驗證 → 配額。對應 resolve_auth_context。
pub async fn resolve_auth(
    state: &AppState,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> AppResult<AuthContext> {
    let token = extract_api_token(headers, query)
        .ok_or_else(|| AppError::Unauthorized("缺少 API Key".into()))?;

    let users = state.users_db.get().await;
    let user = users.iter().find(|u| u.id == token).cloned();

    let api_keys = state.api_keys.read().await;
    let keys_configured = !api_keys.is_empty();
    let is_admin = token == state.settings.admin_key;
    let is_known_key = api_keys.contains(&token);
    drop(api_keys);

    // 若已設定 API_KEYS：token 必須是 admin / 已知 key / 對應到 user，否則 401
    if keys_configured && !is_admin && !is_known_key && user.is_none() {
        return Err(AppError::Unauthorized("API Key 無效".into()));
    }

    // 配額檢查
    if let Some(u) = &user {
        if u.quota <= u.used_tokens {
            return Err(AppError::QuotaExceeded("配額已用盡".into()));
        }
    }

    Ok(AuthContext { token, user })
}

/// 管理台認證：Bearer 必須等於 ADMIN_KEY 或在 API_KEYS 內。
pub async fn verify_admin(state: &AppState, headers: &HeaderMap) -> AppResult<String> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Unauthorized("Unauthorized".into()))?;
    let token = auth
        .strip_prefix("Bearer ")
        .ok_or_else(|| AppError::Unauthorized("Unauthorized".into()))?
        .trim()
        .to_string();
    if token == state.settings.admin_key {
        return Ok(token);
    }
    let api_keys = state.api_keys.read().await;
    if api_keys.contains(&token) {
        return Ok(token);
    }
    Err(AppError::Forbidden("Forbidden: Admin Key Mismatch".into()))
}

/// 增加 user 已用 token（依 token 匹配 user.id）。
pub async fn add_used_tokens(state: &AppState, token: &str, delta: i64) {
    if delta <= 0 {
        return;
    }
    state
        .users_db
        .update(|users| {
            if let Some(u) = users.iter_mut().find(|u| u.id == token) {
                u.used_tokens += delta;
            }
        })
        .await;
}
