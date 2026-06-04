//! 工具名稱混淆，對應 Python `services/tool_name_obfuscation.py`。
//! 將客戶端工具名映射成 Qwen 不會攔截的安全別名，並可反向還原。

/// 客戶端名 → Qwen 安全名。
pub fn to_qwen_name(name: &str) -> String {
    match name {
        "Read" => "fs_open_file".into(),
        "Write" => "fs_put_file".into(),
        "Edit" => "fs_patch_file".into(),
        "MultiEdit" => "fs_multi_patch".into(),
        "Bash" => "shell_run".into(),
        "Grep" => "text_search".into(),
        "Glob" => "path_find".into(),
        "NotebookEdit" => "notebook_patch".into(),
        "WebFetch" => "http_get_url".into(),
        "WebSearch" => "web_query".into(),
        "LS" => "dir_list".into(),
        "TodoWrite" => "todo_put".into(),
        "Task" => "agent_task".into(),
        other => {
            // 其餘：前綴 u_ + 清理非法字元
            let cleaned: String = other
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
                .collect();
            format!("u_{cleaned}")
        }
    }
}

/// 標準化鍵（小寫、去非英數），用於反向匹配。
pub fn norm_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}
