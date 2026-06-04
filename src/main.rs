//! qwen2api-rs 入口：axum 應用組裝、生命週期、路由掛載。
// 專案保留了部分供未來上游同步使用的欄位/方法（見 dev/UPSTREAM.md），故放寬 dead_code。
#![allow(dead_code)]

mod account;
mod api;
mod auth;
mod config;
mod context;
mod db;
mod error;
mod execution;
mod media;
mod request;
mod state;
mod stats;
mod toolcall;
mod upstream;
mod util;

use axum::routing::{delete, get, post};
use axum::Router;
use config::Settings;
use state::AppStateInner;
use std::net::SocketAddr;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

fn admin_router() -> Router<state::AppState> {
    Router::new()
        .route("/status", get(api::admin::status))
        .route("/accounts", get(api::admin::list_accounts).post(api::admin::add_account))
        .route("/accounts/register", post(api::admin::register_account))
        .route("/accounts/{email}", delete(api::admin::delete_account))
        .route("/accounts/{email}/verify", post(api::admin::verify_account))
        .route("/accounts/{email}/activate", post(api::admin::activate_account))
        .route("/verify", post(api::admin::verify_all))
        .route("/keys", get(api::admin::get_keys).post(api::admin::create_key))
        .route("/keys/{key}", delete(api::admin::delete_key))
        .route("/settings", get(api::admin::get_settings).put(api::admin::update_settings))
        .route("/users", get(api::admin::list_users).post(api::admin::create_user))
        .route("/stats", get(api::admin::stats))
        .route("/stats/recent", get(api::admin::stats_recent))
        .route("/media/tasks", get(api::admin::media_tasks).post(api::admin::media_submit))
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let settings = Settings::from_env();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&settings.log_level)),
        )
        .init();

    tracing::info!("正在啟動 qwen2API Rust 企業網關 ...");
    let port = settings.port;
    let web_dir = std::env::var("WEB_DIR").unwrap_or_else(|_| "web".to_string());

    let state = AppStateInner::new(settings).await;

    // 啟動 chat_id 預熱池
    state.chat_id_pool.start();

    // 啟動媒體任務佇列背景 worker（圖片/影片生成 + 本地保存）
    state.media_queue.clone().start(state.clone());

    // Pillar 3：連線保活（opt-in，預設關）。閒置時定期輕量 ping 上游，保溫一條連線，
    // 免去 idle>30s 連線池回收後首請求重握 TLS（經風控代理時握手更貴）。風控敏感故預設關閉。
    if state.settings.conn_keepalive_seconds > 0 {
        let state3 = state.clone();
        let interval = state.settings.conn_keepalive_seconds;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                if let Some(token) = state3.pool.any_valid_token().await {
                    let _ = state3.client.verify_token(&token).await;
                }
            }
        });
        tracing::info!("連線保活已啟用：每 {interval}s 保溫一條上游連線");
    }

    // 嘗試動態抓上游模型列表，更新預設模型（best-effort）
    {
        let state2 = state.clone();
        tokio::spawn(async move {
            if let Some(token) = state2.pool.any_valid_token().await {
                let models = state2.client.list_models(&token).await;
                if let Some(first) = models.first().and_then(|m| m.get("id")).and_then(|v| v.as_str()) {
                    tracing::info!("上游現役模型樣本: {first}");
                }
                let mut cache = state2.upstream_models.write().await;
                cache.data = models;
                cache.fetched_at = crate::util::now_secs();
            }
        });
    }

    // 生成媒體本地檔案（圖片/影片）以 /media/{file} 對外（UUID 檔名，瀏覽器可直接顯示）
    let media_service = ServeDir::new(state.settings.media_dir.clone());

    let index = format!("{web_dir}/index.html");
    // 靜態資源不快取，確保管理台更新後立即生效
    let static_service = tower::ServiceBuilder::new()
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-cache, no-store, must-revalidate"),
        ))
        .service(ServeDir::new(&web_dir).not_found_service(ServeFile::new(index)));

    let app = Router::new()
        // OpenAI Chat Completions
        .route("/v1/chat/completions", post(api::openai::chat_completions))
        .route("/chat/completions", post(api::openai::chat_completions))
        // OpenAI Responses
        .route("/v1/responses", post(api::responses::create))
        .route("/responses", post(api::responses::create))
        // Anthropic Messages
        .route("/v1/messages", post(api::anthropic::messages))
        .route("/messages", post(api::anthropic::messages))
        .route("/anthropic/v1/messages", post(api::anthropic::messages))
        .route("/v1/messages/count_tokens", post(api::anthropic::count_tokens))
        .route("/messages/count_tokens", post(api::anthropic::count_tokens))
        .route("/anthropic/v1/messages/count_tokens", post(api::anthropic::count_tokens))
        // Gemini（路徑含 {model}:{action}；與 GET /v1/models/{model_id} 共用參數名以合併方法）
        .route("/v1beta/models/{model_id}", post(api::gemini::generate))
        .route("/models/{model_id}", post(api::gemini::generate))
        .route("/v1/models/{model_id}", post(api::gemini::generate))
        // OpenAI Images / Embeddings
        .route("/v1/images/generations", post(api::images::generate))
        .route("/images/generations", post(api::images::generate))
        .route("/v1/videos/generations", post(api::videos::generate))
        .route("/videos/generations", post(api::videos::generate))
        .route("/v1/embeddings", post(api::embeddings::create))
        .route("/embeddings", post(api::embeddings::create))
        // Files
        .route("/v1/files", post(api::files::upload))
        .route("/api/files/upload", post(api::files::upload))
        .route("/v1/files/{file_id}", delete(api::files::delete))
        .route("/api/files/{file_id}", delete(api::files::delete))
        // Models
        .route("/v1/models", get(api::models::list_models))
        .route("/v1/models/{model_id}", get(api::models::get_model))
        // 探針
        .route("/healthz", get(api::probes::healthz))
        .route("/readyz", get(api::probes::readyz))
        .route("/api", get(api::probes::root))
        .nest("/api/admin", admin_router())
        .nest_service("/media", media_service)
        .fallback_service(static_service)
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await.expect("綁定埠失敗");
    tracing::info!("✅ 已啟動，監聽 http://0.0.0.0:{port}  WebUI: http://127.0.0.1:{port}/");
    axum::serve(listener, app).await.expect("server 錯誤");
}
