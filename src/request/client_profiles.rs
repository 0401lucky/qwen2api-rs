//! 客戶端識別，對應 Python `services/client_profiles.py`（精簡）。
//! 用於微調 prompt 組裝。

use axum::http::HeaderMap;
use serde_json::Value;

pub const GENERIC: &str = "generic";
pub const CLAUDE_CODE: &str = "claude_code";
pub const CODEX: &str = "codex";
pub const QWEN_CODE: &str = "qwen_code";

/// 由 headers + body 推測客戶端 profile。
pub fn detect_profile(headers: &HeaderMap, body: &Value) -> String {
    let ua = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    if ua.contains("claude-cli") || ua.contains("claude code") || ua.contains("anthropic") {
        return CLAUDE_CODE.into();
    }
    if ua.contains("codex") || ua.contains("openai") {
        return CODEX.into();
    }
    if ua.contains("qwen-code") || ua.contains("qwen_code") {
        return QWEN_CODE.into();
    }

    // body 啟發式：system 內含 Claude Code 標誌
    let sys = body.get("system").map(crate::request::prompt_builder::extract_text_content).unwrap_or_default();
    let lower = sys.to_lowercase();
    if lower.contains("claude code") || lower.contains("you are claude") {
        return CLAUDE_CODE.into();
    }
    GENERIC.into()
}
