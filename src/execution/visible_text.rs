//! 客户端可见文字净化。
//!
//! 上游新版 Python 会剥除 `HIDDEN_INSTRUCTION_START/END` 包裹的内容，避免长工具链中
//! 内部提示被模型回显到客户端。这里用状态机处理串流 chunk 边界被拆开的情况。

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

const HIDDEN_START: &str = "HIDDEN_INSTRUCTION_START";
const HIDDEN_END: &str = "HIDDEN_INSTRUCTION_END";

static INTERNAL_MARKER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(?:<!--\s*)?(?:</?\s*)?(HIDDEN_INSTRUCTION_(?:START|END))(?:\s*(?:-->|/?>))?")
        .unwrap()
});

fn partial_marker_prefixes() -> [&'static str; 10] {
    [
        "hidden_instruction_start",
        "<hidden_instruction_start",
        "</hidden_instruction_start",
        "<!--hidden_instruction_start",
        "<!-- hidden_instruction_start",
        "hidden_instruction_end",
        "<hidden_instruction_end",
        "</hidden_instruction_end",
        "<!--hidden_instruction_end",
        "<!-- hidden_instruction_end",
    ]
}

fn internal_marker_partial_suffix_len(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    let lowered = text.to_ascii_lowercase();
    let mut best = 0usize;
    for prefix in partial_marker_prefixes() {
        let limit = prefix.len().saturating_sub(1).min(lowered.len());
        for len in 1..=limit {
            if lowered.ends_with(&prefix[..len]) {
                best = best.max(len);
            }
        }
    }
    best
}

#[derive(Debug, Default, Clone)]
pub struct VisibleTextSanitizer {
    pending: String,
    inside_hidden_block: bool,
}

impl VisibleTextSanitizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.inside_hidden_block = false;
    }

    pub fn feed(&mut self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let combined = format!("{}{}", self.pending, text);
        let hold = internal_marker_partial_suffix_len(&combined);
        let (process_text, pending) = if hold > 0 {
            let split = combined.len() - hold;
            (&combined[..split], combined[split..].to_string())
        } else {
            (combined.as_str(), String::new())
        };
        self.pending = pending;
        self.sanitize_complete_text(process_text)
    }

    pub fn flush(&mut self) -> String {
        if self.pending.is_empty() {
            return String::new();
        }
        let pending = std::mem::take(&mut self.pending);
        self.sanitize_complete_text(&pending)
    }

    fn find_end_marker(&self, text: &str, mut pos: usize) -> Option<(usize, usize)> {
        while let Some(caps) = INTERNAL_MARKER_RE.captures_at(text, pos) {
            let Some(full) = caps.get(0) else {
                break;
            };
            let marker = caps
                .get(1)
                .map(|m| m.as_str().to_ascii_uppercase())
                .unwrap_or_default();
            if marker == HIDDEN_END {
                return Some((full.start(), full.end()));
            }
            pos = full.end();
        }
        None
    }

    fn sanitize_complete_text(&mut self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let mut output = String::new();
        let mut pos = 0usize;
        while pos < text.len() {
            if self.inside_hidden_block {
                if let Some((_, end)) = self.find_end_marker(text, pos) {
                    pos = end;
                    self.inside_hidden_block = false;
                    continue;
                }
                return output;
            }

            let Some(caps) = INTERNAL_MARKER_RE.captures_at(text, pos) else {
                output.push_str(&text[pos..]);
                break;
            };
            let Some(full) = caps.get(0) else {
                output.push_str(&text[pos..]);
                break;
            };
            output.push_str(&text[pos..full.start()]);
            let marker = caps
                .get(1)
                .map(|m| m.as_str().to_ascii_uppercase())
                .unwrap_or_default();
            pos = full.end();
            if marker == HIDDEN_START {
                self.inside_hidden_block = true;
            }
            // 单独 END 标记直接丢弃。
        }
        output
    }
}

pub fn sanitize_visible_text(text: &str) -> String {
    let mut sanitizer = VisibleTextSanitizer::new();
    let mut out = sanitizer.feed(text);
    out.push_str(&sanitizer.flush());
    out
}

pub fn sanitize_text_block(block: &Value) -> Option<Value> {
    let obj = block.as_object()?;
    if obj.get("type").and_then(|v| v.as_str()) != Some("text") {
        return Some(block.clone());
    }
    let text = obj.get("text").and_then(|v| v.as_str())?;
    let cleaned = sanitize_visible_text(text);
    if cleaned.is_empty() {
        return None;
    }
    let mut next = obj.clone();
    next.insert("text".to_string(), Value::String(cleaned));
    Some(Value::Object(next))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_hidden_block_markers_and_content() {
        let text = "hello HIDDEN_INSTRUCTION_START secret HIDDEN_INSTRUCTION_END world";
        assert_eq!(sanitize_visible_text(text), "hello  world");
    }

    #[test]
    fn stream_handles_split_marker() {
        let mut s = VisibleTextSanitizer::new();
        assert_eq!(s.feed("hello HID"), "hello ");
        assert_eq!(s.feed("DEN_INSTRUCTION_START secret "), "");
        assert_eq!(s.feed("HIDDEN_INSTRUCTION_END world"), " world");
        assert_eq!(s.flush(), "");
    }

    #[test]
    fn html_comment_markers_are_removed() {
        let text = "a <!-- HIDDEN_INSTRUCTION_START -->b<!-- HIDDEN_INSTRUCTION_END --> c";
        assert_eq!(sanitize_visible_text(text), "a  c");
    }
}
