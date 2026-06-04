//! 內部標準請求，對應 Python `adapter/standard_request.py` 的 StandardRequest。

use crate::upstream::ImageOptions;
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct StandardRequest {
    /// 客戶端請求的原始模型名（回應時回顯）。
    pub response_model: String,
    /// Qwen 實際 base model。
    pub resolved_model: String,
    /// 壓平後的單一 prompt。
    pub prompt: String,
    pub stream: bool,
    /// None = 不強制；Some(true/false) = 思考/快速。
    pub thinking_enabled: Option<bool>,
    pub force_thinking: bool,
    pub enable_search: bool,
    pub chat_type: String,
    /// 原始工具定義（OpenAI/Anthropic 格式）。
    pub tools: Vec<serde_json::Value>,
    /// 工具名稱（客戶端原始名）。
    pub tool_names: Vec<String>,
    pub surface: String,
    pub image_options: Option<ImageOptions>,
    pub max_tokens: Option<i64>,
    /// 客戶端 profile（claude_code / codex / qwen_code / generic）。
    pub client_profile: String,
    /// 附件上傳後的 remote_ref，放進上游 payload 的 files。
    pub files: Vec<serde_json::Value>,
    /// 綁定帳號（若有附件上傳，須用同一帳號對話）。
    pub bound_account: Option<String>,
    /// 呼叫者識別（API key，僅用於統計分組；不參與上游請求）。
    pub caller: Option<String>,
    /// 本次請求要繞過的帳號集合（如：t2v 已知無權限的帳號）。空＝不限。
    pub exclude_accounts: HashSet<String>,
}

impl StandardRequest {
    pub fn has_tools(&self) -> bool {
        !self.tools.is_empty()
    }
}
