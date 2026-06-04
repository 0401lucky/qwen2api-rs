//! 工具調用：定義正規化、prompt 指令注入、從模型文字輸出解析 tool call。
//! 對應 Python `toolcall/*` + `services/tool_parser.py`、`tool_few_shot.py`。
//!
//! 設計（見 dev/UPSTREAM.md 刻意差異）：Qwen Web 無原生工具，故將工具定義以文字注入，
//! 並指示模型以 `<tool_call>{json}</tool_call>` 或 ```tool_call fenced 區塊輸出，再本地解析。
//! 名稱經 obfuscation 映射以避免被上游攔截，解析時反向還原。

pub mod obfuscation;

use obfuscation::{norm_key, to_qwen_name};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct NormalizedTool {
    pub name: String,      // 客戶端原始名
    pub qwen_name: String, // 上游安全名
    pub description: String,
    pub parameters: Value, // JSON schema
}

#[derive(Debug, Clone)]
pub struct ParsedToolCall {
    pub id: String,
    pub name: String, // 已還原為客戶端原始名
    pub arguments: Value,
}

/// 將 OpenAI / Anthropic 工具定義正規化。
pub fn normalize_tools(raw: &[Value]) -> Vec<NormalizedTool> {
    let mut out = Vec::new();
    for t in raw {
        // OpenAI: {type:"function", function:{name,description,parameters}}
        if let Some(func) = t.get("function") {
            let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }
            out.push(NormalizedTool {
                qwen_name: to_qwen_name(&name),
                description: func.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                parameters: func.get("parameters").cloned().unwrap_or_else(|| json!({})),
                name,
            });
            continue;
        }
        // Anthropic: {name, description, input_schema}
        if let Some(name) = t.get("name").and_then(|v| v.as_str()) {
            if name.is_empty() {
                continue;
            }
            out.push(NormalizedTool {
                qwen_name: to_qwen_name(name),
                description: t.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                parameters: t
                    .get("input_schema")
                    .or_else(|| t.get("parameters"))
                    .cloned()
                    .unwrap_or_else(|| json!({})),
                name: name.to_string(),
            });
        }
    }
    out
}

/// 建立 qwen_name(正規化) → 原始名 的反向註冊表。
pub fn build_registry(tools: &[NormalizedTool]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for t in tools {
        m.insert(norm_key(&t.qwen_name), t.name.clone());
        m.insert(norm_key(&t.name), t.name.clone());
    }
    m
}

/// 壓縮 JSON schema（移除冗長欄位、截斷），對齊原版 compact_schema 精神。
fn compact_schema(schema: &Value, cap: usize) -> String {
    let mut s = serde_json::to_string(schema).unwrap_or_else(|_| "{}".into());
    if s.chars().count() > cap {
        s = s.chars().take(cap).collect::<String>();
        s.push('…');
    }
    s
}

/// 建立工具指令塊（注入 prompt）。
pub fn build_tool_instruction_block(tools: &[NormalizedTool], _client_profile: &str) -> String {
    let mut s = String::new();
    s.push_str("# 可用工具\n");
    s.push_str("你可以呼叫下列工具。**需要呼叫工具時**，請輸出一個或多個 `<tool_call>` 區塊，格式如下（每個工具一個區塊，內容為合法 JSON）：\n");
    s.push_str("<tool_call>{\"name\": \"工具名\", \"arguments\": {\"參數\": \"值\"}}</tool_call>\n");
    s.push_str("不要在 <tool_call> 之外解釋你要呼叫工具；若不需要工具則正常自然語言回答。\n\n");
    s.push_str("## 工具列表\n");
    for t in tools {
        s.push_str(&format!("### {}\n", t.qwen_name));
        if !t.description.is_empty() {
            s.push_str(&format!("說明: {}\n", t.description));
        }
        s.push_str(&format!("參數(JSON Schema): {}\n\n", compact_schema(&t.parameters, 700)));
    }
    s
}

static RE_TOOL_TAG: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)<tool_call>\s*(\{.*?\})\s*</tool_call>").unwrap());
static RE_FENCED: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)```(?:tool_call|json)?\s*(\{.*?\})\s*```").unwrap());

/// 從模型輸出文字解析 tool call。registry 用於還原名稱。
pub fn parse_tool_calls(text: &str, registry: &HashMap<String, String>) -> Vec<ParsedToolCall> {
    let mut calls = Vec::new();

    // 1) <tool_call>{...}</tool_call>
    for cap in RE_TOOL_TAG.captures_iter(text) {
        if let Some(c) = parse_one(&cap[1], registry) {
            calls.push(c);
        }
    }
    // 2) fenced ```tool_call / ```json
    if calls.is_empty() {
        for cap in RE_FENCED.captures_iter(text) {
            if let Some(c) = parse_one(&cap[1], registry) {
                calls.push(c);
            }
        }
    }
    calls
}

fn parse_one(json_str: &str, registry: &HashMap<String, String>) -> Option<ParsedToolCall> {
    let v: Value = serde_json::from_str(json_str).ok()?;
    // 兼容 {tool_calls:[...]}
    if let Some(arr) = v.get("tool_calls").and_then(|x| x.as_array()) {
        // 只取第一個（外層呼叫者會逐個處理；此處簡化）
        if let Some(first) = arr.first() {
            return parse_obj(first, registry);
        }
        return None;
    }
    parse_obj(&v, registry)
}

fn parse_obj(v: &Value, registry: &HashMap<String, String>) -> Option<ParsedToolCall> {
    let raw_name = v
        .get("name")
        .or_else(|| v.get("tool"))
        .and_then(|x| x.as_str())?;
    let canonical = registry
        .get(&norm_key(raw_name))
        .cloned()
        .unwrap_or_else(|| raw_name.to_string());
    let arguments = v
        .get("arguments")
        .or_else(|| v.get("input"))
        .or_else(|| v.get("parameters"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    // arguments 可能是 JSON 字串
    let arguments = match arguments {
        Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
        other => other,
    };
    Some(ParsedToolCall {
        id: format!("toolu_{}", crate::util::short_id(8)),
        name: canonical,
        arguments,
    })
}

/// 移除文字中的 tool_call 標記（串流給客戶端的可見文字不應含這些）。
pub fn strip_tool_calls(text: &str) -> String {
    let s = RE_TOOL_TAG.replace_all(text, "");
    RE_FENCED.replace_all(&s, "").to_string()
}

/// 文字中是否含有疑似 tool_call 標記（用於串流時暫緩輸出判斷）。
pub fn looks_like_tool_call(text: &str) -> bool {
    text.contains("<tool_call>") || text.contains("```tool_call")
}
