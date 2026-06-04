//! OpenAI chat.completion.chunk 串流翻譯器，對應 Python `services/openai_stream_translator.py`。

use super::{OutEvent, Usage};
use crate::toolcall::ParsedToolCall;
use crate::util::{now_unix, short_id};
use serde_json::{json, Value};

pub struct OpenAiStreamTranslator {
    pub id: String,
    pub created: i64,
    pub model: String,
    role_sent: bool,
}

impl OpenAiStreamTranslator {
    pub fn new(model: &str) -> Self {
        OpenAiStreamTranslator {
            id: format!("chatcmpl-{}", short_id(12)),
            created: now_unix(),
            model: model.to_string(),
            role_sent: false,
        }
    }

    fn chunk(&self, delta: Value, finish_reason: Option<&str>) -> String {
        let v = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason,
            }],
        });
        format!("data: {}\n\n", v)
    }

    /// 將一個 OutEvent 轉成 0..N 個 SSE 字串。
    pub fn on_event(&mut self, ev: &OutEvent) -> Vec<String> {
        let mut out = Vec::new();
        match ev {
            OutEvent::ReasoningDelta(r) => {
                let mut delta = json!({ "reasoning_content": r });
                if !self.role_sent {
                    delta["role"] = json!("assistant");
                    self.role_sent = true;
                }
                out.push(self.chunk(delta, None));
            }
            OutEvent::ContentDelta(c) => {
                let mut delta = json!({ "content": c });
                if !self.role_sent {
                    delta["role"] = json!("assistant");
                    self.role_sent = true;
                }
                out.push(self.chunk(delta, None));
            }
            OutEvent::ToolCalls(tcs) => {
                let tool_calls: Vec<Value> = tcs
                    .iter()
                    .enumerate()
                    .map(|(i, tc)| tool_call_chunk(i, tc))
                    .collect();
                let mut delta = json!({ "tool_calls": tool_calls });
                if !self.role_sent {
                    delta["role"] = json!("assistant");
                    self.role_sent = true;
                }
                out.push(self.chunk(delta, None));
            }
            OutEvent::Done { usage, finish_reason, .. } => {
                // 最終 chunk：空 delta + finish_reason + usage
                let v = json!({
                    "id": self.id,
                    "object": "chat.completion.chunk",
                    "created": self.created,
                    "model": self.model,
                    "choices": [{
                        "index": 0,
                        "delta": {},
                        "finish_reason": finish_reason,
                    }],
                    "usage": usage_json(usage),
                });
                out.push(format!("data: {}\n\n", v));
                out.push("data: [DONE]\n\n".to_string());
            }
            OutEvent::Error(e) => {
                let v = json!({ "error": { "message": e, "type": "upstream_error" } });
                out.push(format!("data: {}\n\n", v));
                out.push("data: [DONE]\n\n".to_string());
            }
        }
        out
    }
}

pub fn tool_call_chunk(index: usize, tc: &ParsedToolCall) -> Value {
    json!({
        "index": index,
        "id": tc.id,
        "type": "function",
        "function": {
            "name": tc.name,
            "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".into()),
        },
    })
}

pub fn usage_json(u: &Usage) -> Value {
    json!({
        "prompt_tokens": u.prompt_tokens,
        "completion_tokens": u.completion_tokens,
        "total_tokens": u.total_tokens,
        "completion_tokens_details": { "reasoning_tokens": u.reasoning_tokens },
    })
}
