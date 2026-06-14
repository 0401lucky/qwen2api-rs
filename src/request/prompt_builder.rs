//! 將多輪訊息 + system + 工具定義壓平成單一 prompt，對應 Python `services/prompt_builder.py`。
//! Qwen Web 只吃單一 prompt，故所有結構需有損壓平，並保留工具語意（文字注入）。

use crate::toolcall::{build_tool_instruction_block, normalize_tools, NormalizedTool};
use serde_json::Value;

/// 歷史字元預算（有工具時收緊，避免上游截斷）。
const MAX_CHARS_WITH_TOOLS: usize = 40_000;
const MAX_CHARS_NO_TOOLS: usize = 120_000;

pub struct PromptBuildResult {
    pub prompt: String,
    pub tools: Vec<Value>,
    pub tool_names: Vec<String>,
}

/// 從訊息 content 抽出純文字（string 或 parts 陣列）。
pub fn extract_text_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                let ptype = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match ptype {
                    "text" | "input_text" | "output_text" => {
                        if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                            out.push_str(t);
                        }
                    }
                    "image_url" => {
                        // 視覺輸入：附 URL 提示（Qwen Web 端走 files，此處保底文字）
                        if let Some(url) = part
                            .get("image_url")
                            .and_then(|u| u.get("url"))
                            .and_then(|v| v.as_str())
                        {
                            if url.starts_with("http") {
                                out.push_str(&format!("\n[圖片: {url}]\n"));
                            } else {
                                out.push_str("\n[圖片附件]\n");
                            }
                        }
                    }
                    "input_image" | "image" => {
                        out.push_str("\n[圖片附件]\n");
                    }
                    "input_file" | "file" => {
                        let name = part
                            .get("filename")
                            .or_else(|| part.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("file");
                        out.push_str(&format!("\n[檔案附件: {name}]\n"));
                    }
                    "tool_result" => {
                        // Anthropic tool_result：content 可能是字串或 parts
                        if let Some(c) = part.get("content") {
                            out.push_str(&extract_text_content(c));
                        }
                    }
                    _ => {
                        if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                            out.push_str(t);
                        }
                    }
                }
            }
            out
        }
        _ => String::new(),
    }
}

fn role_label(role: &str) -> &str {
    match role {
        "system" => "System",
        "assistant" | "model" => "Assistant",
        "tool" => "Tool",
        _ => "Human",
    }
}

fn is_instruction_role(role: &str) -> bool {
    matches!(role, "system" | "developer")
}

fn push_trimmed_unique(parts: &mut Vec<String>, text: String) {
    let trimmed = text.trim();
    if trimmed.is_empty() || parts.iter().any(|part| part == trimmed) {
        return;
    }
    parts.push(trimmed.to_string());
}

/// 從 OpenAI/Anthropic 風格 body 取出 system prompt 文字。
fn extract_system_prompt(body: &Value, messages: &[Value]) -> String {
    let mut parts = Vec::new();

    // Anthropic：頂層 system（字串或 [{text}]）
    if let Some(sys) = body.get("system") {
        let t = extract_text_content(sys);
        push_trimmed_unique(&mut parts, t);
    }

    // OpenAI：system / developer 都是高優先級指令，合併後再壓平。
    for m in messages {
        let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if is_instruction_role(role) {
            push_trimmed_unique(
                &mut parts,
                extract_text_content(m.get("content").unwrap_or(&Value::Null)),
            );
        }
    }

    parts.join("\n\n")
}

fn render_system_prompt(system_prompt: &str, has_tools: bool) -> String {
    let trimmed = system_prompt.trim();
    if has_tools {
        return format!("<system>\n{trimmed}\n</system>");
    }

    format!(
        "[System - MUST FOLLOW]\n\
以下内容是最高优先级的角色、人设、关系、风格与输出规则。后续回复必须持续遵守，不得退回通用 AI 助手口吻；如果包含角色扮演设定，请直接以角色身份继续对话。\n\n\
{trimmed}\n\
[/System - MUST FOLLOW]"
    )
}

fn roleplay_followup_instruction(system_prompt: &str, has_tools: bool) -> Option<&'static str> {
    if has_tools || system_prompt.trim().is_empty() {
        return None;
    }
    Some(
        "请直接遵循上述 System 要求回复最后一条 Human 消息；若其中包含角色、人设、关系或世界观设定，必须以该角色身份延续对话，不要退回通用 AI 助手口吻。",
    )
}

/// 渲染歷史中的 assistant tool_use / tool 訊息為文字（保留語意）。
fn render_message(m: &Value) -> Option<String> {
    let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("user");
    if is_instruction_role(role) {
        return None; // 高優先級指令另外合併處理
    }
    let content = m.get("content").unwrap_or(&Value::Null);

    // assistant 的 tool_calls（OpenAI）
    if role == "assistant" {
        if let Some(tcs) = m.get("tool_calls").and_then(|v| v.as_array()) {
            let mut s = String::new();
            let text = extract_text_content(content);
            if !text.is_empty() {
                s.push_str(&text);
                s.push('\n');
            }
            for tc in tcs {
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let args = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                s.push_str(&format!("[呼叫工具 {name}({args})]"));
            }
            return Some(format!("{}: {}", role_label(role), s));
        }
    }

    // tool 角色（OpenAI tool result）
    if role == "tool" {
        let name = m.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
        let text = extract_text_content(content);
        return Some(format!("Tool [{name}] 結果: {text}"));
    }

    let text = extract_text_content(content);
    if text.trim().is_empty() {
        return None;
    }
    Some(format!("{}: {}", role_label(role), text))
}

/// 主流程：messages → prompt。extra_context 為附件內聯文字（插在 Assistant: 之前）。
pub fn messages_to_prompt(
    body: &Value,
    client_profile: &str,
    extra_context: &str,
) -> PromptBuildResult {
    let empty = Vec::new();
    let messages = body
        .get("messages")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);

    let tools_raw = body
        .get("tools")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let normalized: Vec<NormalizedTool> = normalize_tools(&tools_raw);
    let has_tools = !normalized.is_empty();
    let tool_names: Vec<String> = normalized.iter().map(|t| t.name.clone()).collect();

    let system_prompt = extract_system_prompt(body, messages);
    let max_chars = if has_tools {
        MAX_CHARS_WITH_TOOLS
    } else {
        MAX_CHARS_NO_TOOLS
    };

    let mut parts: Vec<String> = Vec::new();

    // system（claude_code + 有工具時，原版會省略 system 包裹，這裡保留但精簡）
    if !system_prompt.is_empty() {
        parts.push(render_system_prompt(&system_prompt, has_tools));
    }

    // 工具指令塊
    if has_tools {
        parts.push(build_tool_instruction_block(&normalized, client_profile));
    }

    // 歷史（反向累加，受字元預算限制，再正序輸出）
    let mut history: Vec<String> = Vec::new();
    let mut used = 0usize;
    for m in messages.iter().rev() {
        if let Some(rendered) = render_message(m) {
            let len = rendered.chars().count();
            if used + len > max_chars && !history.is_empty() {
                break;
            }
            used += len;
            history.push(rendered);
        }
    }
    history.reverse();
    parts.extend(history);

    // 附件內聯文字
    if !extra_context.trim().is_empty() {
        parts.push(extra_context.trim().to_string());
    }

    // 收尾，引導模型回覆
    if let Some(instruction) = roleplay_followup_instruction(&system_prompt, has_tools) {
        parts.push(instruction.to_string());
    }
    parts.push("Assistant:".to_string());

    PromptBuildResult {
        prompt: parts.join("\n\n"),
        tools: tools_raw,
        tool_names,
    }
}

#[cfg(test)]
mod tests {
    use super::messages_to_prompt;
    use serde_json::json;

    #[test]
    fn non_tool_prompt_reinforces_system_roleplay() {
        let body = json!({
            "messages": [
                { "role": "system", "content": "你是年上的恋人，称呼用户为宝贝，语气成熟亲昵。" },
                { "role": "user", "content": "daddy，早上好" }
            ]
        });

        let built = messages_to_prompt(&body, "generic", "");

        assert!(built.prompt.contains("[System - MUST FOLLOW]"));
        assert!(built.prompt.contains("你是年上的恋人"));
        assert!(built.prompt.contains("不得退回通用 AI 助手口吻"));
        assert!(built.prompt.contains("Human: daddy，早上好"));
        assert!(built.prompt.contains("必须以该角色身份延续对话"));
        assert!(built.prompt.ends_with("Assistant:"));
    }

    #[test]
    fn developer_messages_are_merged_into_system_prompt() {
        let body = json!({
            "messages": [
                { "role": "developer", "content": "必须保持角色卡关系，不要切回通用助手。" },
                { "role": "user", "content": "早上好" }
            ]
        });

        let built = messages_to_prompt(&body, "generic", "");

        assert!(built.prompt.contains("必须保持角色卡关系"));
        assert!(!built.prompt.contains("Human: 必须保持角色卡关系"));
        assert!(built.prompt.contains("Human: 早上好"));
    }

    #[test]
    fn tool_prompt_keeps_compact_system_without_roleplay_tail() {
        let body = json!({
            "messages": [
                { "role": "system", "content": "你是代码助手。" },
                { "role": "user", "content": "读取项目结构" }
            ],
            "tools": [
                {
                    "type": "function",
                    "function": {
                        "name": "list_files",
                        "description": "列出文件",
                        "parameters": { "type": "object", "properties": {} }
                    }
                }
            ]
        });

        let built = messages_to_prompt(&body, "codex", "");

        assert!(built.prompt.contains("<system>\n你是代码助手。\n</system>"));
        assert!(!built.prompt.contains("[System - MUST FOLLOW]"));
        assert!(!built.prompt.contains("必须以该角色身份延续对话"));
        assert_eq!(built.tool_names, vec!["list_files".to_string()]);
        assert!(built.prompt.ends_with("Assistant:"));
    }
}
