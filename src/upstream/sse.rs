//! 解析 Qwen 上游 SSE，對應 Python `upstream/sse_consumer.py`（並擴充 qwen3.7 的 summary_thought 格式）。

use serde_json::Value;

/// 一個正規化後的上游 delta。
#[derive(Debug, Clone, Default)]
pub struct QwenDelta {
    pub phase: String,
    /// 原始 delta.content（answer 階段為增量文字）。
    pub content: String,
    /// 思考內容（累積）：qwen3.7 的 extra.summary_thought.content join 後字串。
    pub reasoning_cumulative: Option<String>,
    /// 思考內容（增量）：舊格式 reasoning_content/reasoning/thinking 等。
    pub reasoning_incremental: String,
    pub status: String,
    /// 該事件的 usage（若有）。
    pub usage: Option<Value>,
}

fn first_text(values: &[Option<&Value>]) -> String {
    for v in values {
        if let Some(Value::String(s)) = v {
            if !s.is_empty() {
                return s.clone();
            }
        }
    }
    String::new()
}

/// 從 delta 抓舊格式增量 reasoning。
fn extract_reasoning_incremental(delta: &Value) -> String {
    let extra = delta.get("extra");
    fn ge<'a>(obj: Option<&'a Value>, key: &str) -> Option<&'a Value> {
        obj.and_then(|o| o.get(key))
    }
    first_text(&[
        delta.get("reasoning_content"),
        delta.get("reasoning"),
        delta.get("reasoning_text"),
        delta.get("thinking"),
        delta.get("thoughts"),
        ge(extra, "reasoning_content"),
        ge(extra, "reasoning"),
        ge(extra, "reasoning_text"),
        ge(extra, "thinking"),
        ge(extra, "thoughts"),
    ])
}

/// 從 extra.summary_thought.content（陣列）join 出累積思考文字。
fn extract_reasoning_cumulative(delta: &Value) -> Option<String> {
    let arr = delta
        .get("extra")?
        .get("summary_thought")?
        .get("content")?
        .as_array()?;
    let joined: String = arr
        .iter()
        .filter_map(|v| v.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

/// 解析一個 SSE 訊息塊（可能含多行 data:）。回傳正規化 delta 列表。
pub fn parse_sse_chunk(chunk: &str) -> Vec<QwenDelta> {
    let mut out = Vec::new();
    for raw_line in chunk.lines() {
        let line = raw_line.trim();
        if !line.starts_with("data:") {
            continue;
        }
        let data = line[5..].trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let obj: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // 首事件 response.created 沒有 choices
        let choices = match obj.get("choices").and_then(|c| c.as_array()) {
            Some(c) if !c.is_empty() => c,
            _ => continue,
        };
        let delta = match choices[0].get("delta") {
            Some(d) => d,
            None => continue,
        };
        let phase = delta.get("phase").and_then(|v| v.as_str()).unwrap_or("answer").to_string();
        let content = delta.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let status = delta.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let reasoning_cumulative = extract_reasoning_cumulative(delta);
        let reasoning_incremental = extract_reasoning_incremental(delta);
        let usage = obj.get("usage").cloned();
        out.push(QwenDelta {
            phase,
            content,
            reasoning_cumulative,
            reasoning_incremental,
            status,
            usage,
        });
    }
    out
}

/// 偵測上游明確 JSON 錯誤（{"success":false} 或 {"error":...}），回錯誤訊息。
pub fn extract_upstream_error(text: &str) -> Option<String> {
    for raw_line in text.lines() {
        let mut line = raw_line.trim();
        if line.starts_with("data:") {
            line = line[5..].trim();
        }
        if line.is_empty() || line == "[DONE]" || !line.starts_with('{') {
            continue;
        }
        let obj: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(msg) = format_upstream_error(&obj) {
            return Some(msg);
        }
    }
    None
}

fn format_upstream_error(obj: &Value) -> Option<String> {
    let request_id = obj
        .get("request_id")
        .or_else(|| obj.get("response_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("-");
    if obj.get("success") == Some(&Value::Bool(false)) {
        let data = obj.get("data");
        let code = data
            .and_then(|d| d.get("code"))
            .or_else(|| obj.get("code"))
            .and_then(|v| v.as_str())
            .unwrap_or("upstream_error");
        let details = data
            .and_then(|d| d.get("details").or_else(|| d.get("message")))
            .or_else(|| obj.get("details"))
            .or_else(|| obj.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return Some(format!("Qwen upstream error code={code} request_id={request_id} details={details}"));
    }
    if let Some(err) = obj.get("error") {
        if let Some(eo) = err.as_object() {
            let code = eo.get("code").and_then(|v| v.as_str()).unwrap_or("upstream_error");
            let details = eo
                .get("details")
                .or_else(|| eo.get("message"))
                .or_else(|| eo.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            return Some(format!("Qwen upstream error code={code} request_id={request_id} details={details}"));
        }
        if let Some(s) = err.as_str() {
            if !s.is_empty() {
                return Some(format!("Qwen upstream error request_id={request_id} details={s}"));
            }
        }
    }
    None
}
