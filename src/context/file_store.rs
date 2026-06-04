//! 本地檔案存儲，對應 Python `services/file_store.py`。
//! /v1/files 上傳後存本地；chat 時依 file_id 取回 bytes 上傳 OSS。

use crate::db::JsonDb;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMeta {
    pub id: String,
    pub filename: String,
    pub content_type: String,
    pub size: usize,
    pub path: String,
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub created_at: i64,
}

#[derive(Clone)]
pub struct FileStore {
    dir: PathBuf,
    db: JsonDb<Vec<FileMeta>>,
}

impl FileStore {
    pub async fn new(dir: PathBuf, meta_file: PathBuf) -> Arc<Self> {
        let _ = tokio::fs::create_dir_all(&dir).await;
        let db = JsonDb::load(&meta_file, Vec::<FileMeta>::new()).await;
        Arc::new(FileStore { dir, db })
    }

    pub async fn save_bytes(&self, filename: &str, content_type: &str, bytes: &[u8], purpose: &str) -> FileMeta {
        let id = format!("file-{}", crate::util::short_id(24));
        let sub = self.dir.join(purpose);
        let _ = tokio::fs::create_dir_all(&sub).await;
        let path = sub.join(&id);
        let _ = tokio::fs::write(&path, bytes).await;
        let meta = FileMeta {
            id: id.clone(),
            filename: filename.to_string(),
            content_type: content_type.to_string(),
            size: bytes.len(),
            path: path.to_string_lossy().to_string(),
            purpose: purpose.to_string(),
            created_at: crate::util::now_unix(),
        };
        self.db.update(|v| v.push(meta.clone())).await;
        meta
    }

    pub async fn get(&self, file_id: &str) -> Option<(FileMeta, Vec<u8>)> {
        let metas = self.db.get().await;
        let meta = metas.into_iter().find(|m| m.id == file_id)?;
        let bytes = tokio::fs::read(&meta.path).await.ok()?;
        Some((meta, bytes))
    }

    pub async fn delete(&self, file_id: &str) -> bool {
        let mut found = false;
        let path = {
            let metas = self.db.get().await;
            metas.iter().find(|m| m.id == file_id).map(|m| m.path.clone())
        };
        if let Some(p) = path {
            let _ = tokio::fs::remove_file(&p).await;
            self.db
                .update(|v| {
                    let before = v.len();
                    v.retain(|m| m.id != file_id);
                    found = v.len() != before;
                })
                .await;
        }
        found
    }
}
