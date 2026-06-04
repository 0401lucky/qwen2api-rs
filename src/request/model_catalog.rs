//! /v1/models 列表構建，對應 Python `services/model_catalog.py`。
//! 從上游模型 metadata（capabilities + chat_type 列表）合成 OpenAI 風格列表 + 能力後綴變體。

use crate::util::now_unix;
use serde_json::{json, Value};

/// 取上游 model 的 info.meta。
fn meta(m: &Value) -> &Value {
    m.get("info").and_then(|i| i.get("meta")).unwrap_or(&Value::Null)
}

/// 由 capabilities + chat_type 列表合成能力旗標（鍵對齊前端 Test 頁）。
fn extract_capabilities(m: &Value) -> Value {
    let meta = meta(m);
    let caps = meta.get("capabilities").cloned().unwrap_or(json!({}));
    let abilities = meta.get("abilities").cloned().unwrap_or(json!({}));
    let chat_types: Vec<String> = meta
        .get("chat_type")
        .or_else(|| meta.get("chatType"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    let has = |t: &str| chat_types.iter().any(|c| c == t);
    let cap_bool = |k: &str| {
        caps.get(k).and_then(|v| v.as_bool()).unwrap_or_else(|| abilities.get(k).and_then(|v| v.as_bool()).unwrap_or(false))
    };
    json!({
        "thinking": cap_bool("thinking"),
        "vision": cap_bool("vision"),
        "search": cap_bool("search") || has("search"),
        "deep_research": has("deep_research"),
        "image_gen": has("t2i"),
        "video_gen": has("t2v"),
        "web_dev": has("web_dev"),
        "slides": has("slides"),
    })
}

/// 建立 OpenAI 風格模型列表。upstream 為 /api/models 的 data 陣列。
pub fn build_openai_model_list(upstream: &[Value]) -> Vec<Value> {
    let created = now_unix();
    let mut out = Vec::new();
    for m in upstream {
        let id = match m.get("id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        let caps = extract_capabilities(m);
        let family = id.split('-').next().unwrap_or(&id).to_string();
        let display = m.get("name").and_then(|v| v.as_str()).unwrap_or(&id).to_string();

        out.push(json!({
            "id": id, "object": "model", "created": created, "owned_by": "qwen",
            "base_model": id, "family": family, "mode": "chat",
            "display_name": display, "capabilities": caps,
        }));

        let cb = |k: &str| caps.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
        if cb("thinking") {
            out.push(variant(&id, "-thinking", "thinking", created, &caps, &family));
        }
        if cb("image_gen") {
            out.push(variant(&id, "-image", "image", created, &caps, &family));
        }
        if cb("video_gen") {
            out.push(variant(&id, "-video", "video", created, &caps, &family));
        }
    }
    out
}

fn variant(base: &str, suffix: &str, mode: &str, created: i64, caps: &Value, family: &str) -> Value {
    json!({
        "id": format!("{base}{suffix}"), "object": "model", "created": created, "owned_by": "qwen",
        "base_model": base, "family": family, "mode": mode, "capabilities": caps,
    })
}

/// 無法取得上游時的 fallback。
pub fn fallback_model_list(default_model: &str) -> Vec<Value> {
    let created = now_unix();
    vec![json!({
        "id": default_model, "object": "model", "created": created, "owned_by": "qwen",
        "base_model": default_model,
        "family": default_model.split('-').next().unwrap_or(default_model),
        "mode": "chat",
        "capabilities": {"thinking": true, "search": true, "vision": true, "image_gen": true, "video_gen": true},
    })]
}
