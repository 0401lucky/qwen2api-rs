//! 解析模型名後綴 → 模式，對應 Python `services/model_modes.py`。
//! 例：`qwen3.7-plus-thinking` → base=qwen3.7-plus, chat_type=t2t, force_thinking=true。

#[derive(Debug, Clone)]
pub struct ModelMode {
    pub base_model: String,
    pub chat_type: String,
    pub force_thinking: bool,
    pub mode: String,
}

/// 後綴 → (chat_type, force_thinking, mode 標籤)
fn suffix_table() -> &'static [(&'static str, &'static str, bool, &'static str)] {
    &[
        ("-thinking", "t2t", true, "thinking"),
        ("-search", "t2t", false, "search"),
        ("-deep-research", "deep_research", true, "deep_research"),
        ("-deep_research", "deep_research", true, "deep_research"),
        ("-image", "t2i", false, "image"),
        ("-t2i", "t2i", false, "image"),
        ("-video", "t2v", false, "video"),
        ("-t2v", "t2v", false, "video"),
        ("-webdev", "t2t", false, "web_dev"),
        ("-web-dev", "t2t", false, "web_dev"),
        ("-slides", "t2t", false, "slides"),
    ]
}

pub fn parse_model_mode(model: &str) -> ModelMode {
    let lower = model.to_lowercase();
    for (suffix, chat_type, force_thinking, mode) in suffix_table() {
        if lower.ends_with(suffix) {
            let base = &model[..model.len() - suffix.len()];
            return ModelMode {
                base_model: base.to_string(),
                chat_type: chat_type.to_string(),
                force_thinking: *force_thinking,
                mode: mode.to_string(),
            };
        }
    }
    ModelMode {
        base_model: model.to_string(),
        chat_type: "t2t".to_string(),
        force_thinking: false,
        mode: "chat".to_string(),
    }
}
