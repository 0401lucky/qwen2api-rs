//! 應用全域狀態（對應 Python main.py 的 app.state），以 Arc 共享。

use crate::account::AccountPool;
use crate::auth::User;
use crate::config::{ApiKeysFile, Settings};
use crate::db::{write_json_atomic, JsonDb};
use crate::upstream::{ChatIdPool, Executor, QwenClient};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

pub type AppState = Arc<AppStateInner>;

/// 持久化的執行期設定（目前僅出口代理；存於 data/config.json）。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RuntimeConfig {
    #[serde(default)]
    pub upstream_proxy: Option<String>,
}

pub struct AppStateInner {
    pub settings: Settings,
    /// 模型別名映射，可在管理台運行時更新。
    pub model_map: RwLock<HashMap<String, String>>,
    pub pool: Arc<AccountPool>,
    pub client: Arc<QwenClient>,
    pub chat_id_pool: Arc<ChatIdPool>,
    pub executor: Arc<Executor>,
    pub users_db: JsonDb<Vec<User>>,
    pub api_keys: RwLock<HashSet<String>>,
    pub file_store: Arc<crate::context::file_store::FileStore>,
    /// 請求統計子系統（背景批次寫入 SQLite）。
    pub stats: Arc<crate::stats::Stats>,
    /// 媒體任務佇列（圖片/影片背景生成 + 本地保存）。
    pub media_queue: Arc<crate::media::MediaQueue>,
    /// t2v 已知無權限的帳號（持久化跳過集）。
    pub no_t2v: JsonDb<HashSet<String>>,
    api_keys_file: PathBuf,
    /// 快取上游模型列表（避免每次 /v1/models 都打上游）。
    pub upstream_models: RwLock<UpstreamModelsCache>,
    /// 持久化執行期設定（出口代理）。
    runtime_cfg: JsonDb<RuntimeConfig>,
}

#[derive(Default)]
pub struct UpstreamModelsCache {
    pub data: Vec<serde_json::Value>,
    pub fetched_at: f64,
}

impl AppStateInner {
    pub async fn new(settings: Settings) -> AppState {
        let model_map = crate::config::default_model_map();

        let pool = AccountPool::load(&settings).await;

        // 出口代理：持久化設定優先，否則用環境變數（docker env）
        let runtime_cfg = JsonDb::load(&settings.config_file, RuntimeConfig::default()).await;
        let initial_proxy = {
            let cfg = runtime_cfg.get().await;
            cfg.upstream_proxy.clone().or_else(|| settings.upstream_proxy.clone())
        };
        let client = Arc::new(QwenClient::new(initial_proxy));
        let chat_id_pool = ChatIdPool::new(
            client.clone(),
            pool.clone(),
            settings.chat_id_prewarm_target_per_account,
            settings.chat_id_prewarm_ttl_seconds,
            settings.chat_id_prewarm_max_accounts,
            settings.default_model.clone(),
        );
        let executor = Arc::new(Executor::new(pool.clone(), client.clone(), chat_id_pool.clone(), &settings));

        let users_db = JsonDb::load(&settings.users_file, Vec::<User>::new()).await;
        let file_store = crate::context::file_store::FileStore::new(
            settings.context_generated_dir.clone(),
            settings.uploaded_files_file.clone(),
        )
        .await;

        // 載入 api_keys.json
        let keys_file: ApiKeysFile =
            crate::db::read_json_or(&settings.api_keys_file, ApiKeysFile::default()).await;
        let api_keys: HashSet<String> = keys_file.keys.into_iter().collect();

        let stats = crate::stats::Stats::new(&settings.stats_file);

        let media_store = crate::media::MediaStore::new(&settings.media_db_file, &settings.media_dir);
        let media_queue = crate::media::MediaQueue::new(media_store, settings.media_concurrency, settings.media_max_attempts);

        let no_t2v = JsonDb::load(&settings.no_t2v_file, HashSet::<String>::new()).await;

        Arc::new(AppStateInner {
            api_keys_file: settings.api_keys_file.clone(),
            model_map: RwLock::new(model_map),
            pool,
            client,
            chat_id_pool,
            executor,
            users_db,
            file_store,
            stats,
            media_queue,
            no_t2v,
            api_keys: RwLock::new(api_keys),
            upstream_models: RwLock::new(UpstreamModelsCache::default()),
            runtime_cfg,
            settings,
        })
    }

    /// 持久化 api_keys 到磁碟。
    pub async fn save_api_keys(&self) {
        let keys: Vec<String> = self.api_keys.read().await.iter().cloned().collect();
        write_json_atomic(&self.api_keys_file, &ApiKeysFile { keys }).await;
    }

    /// 設定出口全局代理：即時切換 client + 持久化（None/空 = 清除，回退環境變數）。
    pub async fn set_upstream_proxy(&self, proxy: Option<String>) {
        let normalized = proxy.and_then(|p| {
            let t = p.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        });
        self.client.set_proxy(normalized.clone());
        self.runtime_cfg.set(RuntimeConfig { upstream_proxy: normalized }).await;
    }

    /// 解析模型別名。映射命中則用映射值；否則若不是 qwen 系模型，回退到預設模型，
    /// 以避免把下游(claude-/gpt-/gemini-)模型名原樣丟給上游導致 "Model not found"。
    pub async fn resolve_model(&self, name: &str) -> String {
        let resolved = {
            let map = self.model_map.read().await;
            crate::config::resolve_model(&map, name)
        };
        if resolved.to_lowercase().starts_with("qwen") {
            resolved
        } else {
            self.settings.default_model.clone()
        }
    }
}
