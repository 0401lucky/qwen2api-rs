//! 非串流回應組裝，對應 Python `services/response_formatters.py`。

use super::translator::usage_json;
use super::{CollectedResult, Usage};
use crate::toolcall::ParsedToolCall;
use crate::util::{now_unix, short_id};
use serde_json::{json, Value};

/// OpenAI chat.completion（非串流）。
pub fn build_openai_completion(model: &str, r: &CollectedResult) -> Value {
    let mut message = json!({
        "role": "assistant",
        "content": if r.content.is_empty() { Value::Null } else { json!(r.content) },
    });
    if !r.reasoning.is_empty() {
        message["reasoning_content"] = json!(r.reasoning);
    }
    if !r.tool_calls.is_empty() {
        let tcs: Vec<Value> = r
            .tool_calls
            .iter()
            .map(|tc| {
                json!({
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tc.name,
                        "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".into()),
                    },
                })
            })
            .collect();
        message["tool_calls"] = json!(tcs);
    }
    json!({
        "id": format!("chatcmpl-{}", short_id(12)),
        "object": "chat.completion",
        "created": now_unix(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": r.finish_reason,
        }],
        "usage": usage_json(&r.usage),
    })
}

/// Anthropic Messages（非串流）。
pub fn build_anthropic_message(model: &str, r: &CollectedResult) -> Value {
    let mut content: Vec<Value> = Vec::new();
    if !r.content.is_empty() {
        content.push(json!({ "type": "text", "text": r.content }));
    }
    for tc in &r.tool_calls {
        content.push(json!({
            "type": "tool_use",
            "id": tc.id,
            "name": tc.name,
            "input": tc.arguments,
        }));
    }
    if content.is_empty() {
        content.push(json!({ "type": "text", "text": "" }));
    }
    let stop_reason = if !r.tool_calls.is_empty() { "tool_use" } else { "end_turn" };
    json!({
        "id": format!("msg_{}", short_id(12)),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": r.usage.prompt_tokens,
            "output_tokens": r.usage.completion_tokens,
        },
    })
}

/// Gemini generateContent（非串流）。
pub fn build_gemini_generate(r: &CollectedResult) -> Value {
    let mut parts: Vec<Value> = Vec::new();
    if !r.content.is_empty() {
        parts.push(json!({ "text": r.content }));
    }
    for tc in &r.tool_calls {
        parts.push(json!({
            "functionCall": { "name": tc.name, "args": tc.arguments }
        }));
    }
    if parts.is_empty() {
        parts.push(json!({ "text": "" }));
    }
    let finish = if !r.tool_calls.is_empty() { "STOP" } else { "STOP" };
    json!({
        "candidates": [{
            "content": { "parts": parts, "role": "model" },
            "finishReason": finish,
            "index": 0,
        }],
        "usageMetadata": {
            "promptTokenCount": r.usage.prompt_tokens,
            "candidatesTokenCount": r.usage.completion_tokens,
            "totalTokenCount": r.usage.total_tokens,
        },
    })
}

/// 給 Anthropic tool_use id 用。
pub fn anthropic_tool_use_block(tc: &ParsedToolCall) -> Value {
    json!({ "type": "tool_use", "id": tc.id, "name": tc.name, "input": tc.arguments })
}

/// 簡易 usage（給需要 Usage 物件的地方）。
pub fn empty_usage() -> Usage {
    Usage::default()
}
