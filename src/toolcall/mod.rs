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

// 只定位「**起始**」位置；JSON 範圍交給 brace-balanced 掃描處理。
// 過去用 `(?s)<tool_call>\s*(\{.*?\})\s*</tool_call>` 這種非貪婪 regex，遇到
// `<tool_call>{json1}{json2}</tool_call>`（模型把多個 tool 寫在同一個區塊內，
// 沒用 `,` 分隔）會把 capture 拿到 `{json1}{json2}` → 非法 JSON 解析失敗，
// 同時 replace_all 把整段剝光 → cleaned 也空 → client 啥都沒看到。
static RE_TOOL_OPEN: Lazy<Regex> = Lazy::new(|| Regex::new(r"<tool_call>").unwrap());
static RE_FENCE_OPEN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"```(?:tool_call|json)?[ \t]*\n?").unwrap());

/// 從某個 `{` 位置（byte index）出發，做 brace-balanced 掃描，回傳該 JSON 物件
/// 結束位置（含末尾 `}`）的 byte index。會正確處理字串字面值中的 `{`/`}`/`\"`/反斜線跳脫。
/// 若到字串尾仍未配對成功，回 None（呼叫端視為「不完整 JSON」忽略）。
fn scan_json_object_end(bytes: &[u8], start: usize) -> Option<usize> {
    if start >= bytes.len() || bytes[start] != b'{' {
        return None;
    }
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut escape = false;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else {
            match c {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// 從一個 tool_call 區塊（內含 1+ 個 JSON 物件、可能用 `,` 或換行分隔、可能未閉合）
/// 切出所有「頂層 JSON 物件」的 byte 範圍 `(start, end_inclusive)`。
fn split_json_objects(bytes: &[u8], region_start: usize, region_end: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = region_start;
    while i < region_end {
        if bytes[i] == b'{' {
            if let Some(end) = scan_json_object_end(&bytes[..region_end], i) {
                out.push((i, end));
                i = end + 1;
                continue;
            } else {
                break; // 不完整 JSON：直接停（截斷的 tool_call）
            }
        }
        i += 1;
    }
    out
}

/// 找出 text 內所有 tool_call 區塊（含 `<tool_call>...</tool_call>` 與 ```` ``` 圍欄）。
/// 回傳 (整個區塊起始位置, 整個區塊結束後位置, 內部 JSON 區域起始, 結束)。
/// - 結束位置考慮三種：
///   1. `</tool_call>` / `\`\`\`` 配對成功 → 用配對位置
///   2. 後續還有別的 `<tool_call>` / 圍欄起始 → 用下一個起始位置（隱式閉合）
///   3. 都沒有 → 用 text 末（截斷情境）
fn find_tool_blocks(text: &str) -> Vec<(usize, usize, usize, usize)> {
    let bytes = text.as_bytes();
    let mut blocks = Vec::new();

    // <tool_call> ... </tool_call>
    for m in RE_TOOL_OPEN.find_iter(text) {
        let open_start = m.start();
        let inner_start = m.end();
        // 找 </tool_call>
        let close_tag = b"</tool_call>";
        let mut block_end = text.len();
        let mut inner_end = text.len();
        if let Some(rel) = find_subslice(&bytes[inner_start..], close_tag) {
            inner_end = inner_start + rel;
            block_end = inner_end + close_tag.len();
        } else if let Some(next_open) = RE_TOOL_OPEN.find_at(text, inner_start) {
            // 沒有閉合但有另一個 <tool_call> → 隱式截止於下一個 open
            inner_end = next_open.start();
            block_end = next_open.start();
        }
        blocks.push((open_start, block_end, inner_start, inner_end));
    }

    // ```tool_call / ```json (僅在沒有 <tool_call> 區塊時嘗試，避免重複處理)
    if blocks.is_empty() {
        for m in RE_FENCE_OPEN.find_iter(text) {
            let open_start = m.start();
            let inner_start = m.end();
            let close_tag = b"```";
            let mut block_end = text.len();
            let mut inner_end = text.len();
            if let Some(rel) = find_subslice(&bytes[inner_start..], close_tag) {
                inner_end = inner_start + rel;
                block_end = inner_end + close_tag.len();
            }
            blocks.push((open_start, block_end, inner_start, inner_end));
        }
    }

    blocks
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// 從模型輸出文字解析 tool call。registry 用於還原名稱。
/// 支援：`<tool_call>{json}</tool_call>`、`<tool_call>{json1}{json2}…</tool_call>`、
/// `<tool_call>{json1}\n{json2}</tool_call>`、未閉合的 `<tool_call>{json}…`、
/// 以及 ```` ```tool_call ```` / ```` ```json ```` 圍欄；
/// JSON 內含巢狀 `{}` 也能正確切（brace-balanced 掃描）。
pub fn parse_tool_calls(text: &str, registry: &HashMap<String, String>) -> Vec<ParsedToolCall> {
    let bytes = text.as_bytes();
    let mut calls = Vec::new();
    for (_, _, inner_start, inner_end) in find_tool_blocks(text) {
        for (s, e) in split_json_objects(bytes, inner_start, inner_end) {
            let slice = match std::str::from_utf8(&bytes[s..=e]) {
                Ok(v) => v,
                Err(_) => continue, // brace 切點落在多 byte 中間（理論上不會）
            };
            if let Some(c) = parse_one(slice, registry) {
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
/// 用 find_tool_blocks 找出每個區塊範圍，整段切除。
pub fn strip_tool_calls(text: &str) -> String {
    let blocks = find_tool_blocks(text);
    if blocks.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    let bytes = text.as_bytes();
    for (open_start, block_end, _, _) in blocks {
        if cursor < open_start {
            // 留下區塊外的文字
            if let Ok(s) = std::str::from_utf8(&bytes[cursor..open_start]) {
                out.push_str(s);
            }
        }
        cursor = block_end;
    }
    if cursor < bytes.len() {
        if let Ok(s) = std::str::from_utf8(&bytes[cursor..]) {
            out.push_str(s);
        }
    }
    out
}

/// 文字中是否含有疑似 tool_call 標記（用於串流時暫緩輸出判斷）。
pub fn looks_like_tool_call(text: &str) -> bool {
    text.contains("<tool_call>") || text.contains("```tool_call")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash_registry() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert(norm_key("shell_run"), "Bash".to_string());
        m.insert(norm_key("Bash"), "Bash".to_string());
        m
    }

    /// 單一 tool_call、JSON 內含巢狀 `{}`（如 jq filter）→ 應正確解析（不被 `.*?` 截斷）。
    /// 這是線上實測 anomaly 的縮影：args.command 含 jq filter，內有 `{a: 1}` 字面值。
    #[test]
    fn nested_braces_in_args_parse_correctly() {
        let reg = bash_registry();
        let text = r#"<tool_call>{"name":"shell_run","arguments":{"command":"jq -r '{a: 1, b: {c: 2}}'"}}</tool_call>"#;
        let calls = parse_tool_calls(text, &reg);
        assert_eq!(calls.len(), 1, "巢狀 {{}} 不該破壞解析: {calls:?}");
        assert_eq!(calls[0].name, "Bash");
        assert_eq!(calls[0].arguments["command"], r#"jq -r '{a: 1, b: {c: 2}}'"#);
        // strip 後應該整段被剝光（區塊外無其他文字）
        assert_eq!(strip_tool_calls(text), "");
    }

    /// 多個 JSON 物件寫在同一個 <tool_call> 區塊內（模型偶發行為）→ 全部都該解析出。
    /// 這是觸發「客戶端零輸出」的真正 anomaly：舊版 regex 會把整塊吃光，
    /// capture 拿到 `{json1}{json2}` 非法 JSON → tool_calls 空 + cleaned 空 → client 看不到。
    #[test]
    fn multiple_json_objects_in_one_block_all_parse() {
        let reg = bash_registry();
        let text = r#"<tool_call>{"name":"shell_run","arguments":{"command":"ls"}}
{"name":"shell_run","arguments":{"command":"pwd"}}</tool_call>"#;
        let calls = parse_tool_calls(text, &reg);
        assert_eq!(calls.len(), 2, "多 JSON 連寫應全部解析: {calls:?}");
        assert_eq!(calls[0].arguments["command"], "ls");
        assert_eq!(calls[1].arguments["command"], "pwd");
        assert_eq!(strip_tool_calls(text), "");
    }

    /// 未閉合 `<tool_call>{json}` → 仍應解析該 JSON 物件，避免把答案吞光。
    #[test]
    fn unclosed_tool_call_still_parses_inner_json() {
        let reg = bash_registry();
        let text = r#"<tool_call>{"name":"shell_run","arguments":{"command":"ls /tmp"}}"#;
        let calls = parse_tool_calls(text, &reg);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments["command"], "ls /tmp");
        // strip：整段被剝（區塊到 text 末）
        assert_eq!(strip_tool_calls(text), "");
    }

    /// 兩個獨立 `<tool_call>` 區塊（標準格式）→ 各自解析。
    #[test]
    fn two_separate_tool_call_blocks_parse_both() {
        let reg = bash_registry();
        let text = r#"<tool_call>{"name":"shell_run","arguments":{"command":"a"}}</tool_call>
some text
<tool_call>{"name":"shell_run","arguments":{"command":"b"}}</tool_call>"#;
        let calls = parse_tool_calls(text, &reg);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].arguments["command"], "a");
        assert_eq!(calls[1].arguments["command"], "b");
        // strip：兩個區塊都剝、中間「some text」留下
        assert_eq!(strip_tool_calls(text).trim(), "some text");
    }

    /// 圍欄格式 ```tool_call / ```json：在沒 `<tool_call>` 時也支援；
    /// 內含巢狀 {} 同樣不該被截斷。
    #[test]
    fn fenced_block_parses_and_strips() {
        let reg = bash_registry();
        let text = "```tool_call\n{\"name\":\"shell_run\",\"arguments\":{\"command\":\"jq '{x:1}'\"}}\n```";
        let calls = parse_tool_calls(text, &reg);
        assert_eq!(calls.len(), 1, "圍欄區塊應正確解析: {calls:?}");
        assert_eq!(calls[0].arguments["command"], "jq '{x:1}'");
        // strip 應把圍欄整段剝光
        assert!(strip_tool_calls(text).trim().is_empty(), "圍欄區塊應被 strip 乾淨");
    }

    /// 純自然語言（無 tool 標記）→ parse 空、strip 不動。
    #[test]
    fn plain_text_passes_through_untouched() {
        let reg = bash_registry();
        let text = "這是一段普通回覆，沒有任何 tool_call 標記。";
        assert!(parse_tool_calls(text, &reg).is_empty());
        assert_eq!(strip_tool_calls(text), text);
    }

    /// 字串字面值中的 `{` `}` 不該被當成物件邊界（brace scanner 須認 JSON 字串引號 + 跳脫）。
    #[test]
    fn braces_inside_strings_dont_confuse_scanner() {
        let reg = bash_registry();
        // command 字串內含 `{` `}` 和跳脫的 `\"`，且後面接第二個 JSON
        let text = r#"<tool_call>{"name":"shell_run","arguments":{"command":"echo \"{not_json}\" }"}}
{"name":"shell_run","arguments":{"command":"true"}}</tool_call>"#;
        let calls = parse_tool_calls(text, &reg);
        assert_eq!(calls.len(), 2, "字串內 {{}} 不該破壞物件邊界: {calls:?}");
        assert_eq!(calls[1].arguments["command"], "true");
    }

    /// tool name obfuscation 反向還原：shell_run → Bash。
    #[test]
    fn obfuscated_tool_name_resolves_to_canonical() {
        let reg = bash_registry();
        let text = r#"<tool_call>{"name":"shell_run","arguments":{"command":"ls"}}</tool_call>"#;
        let calls = parse_tool_calls(text, &reg);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "Bash", "應該還原為原始客戶端名");
    }
}
