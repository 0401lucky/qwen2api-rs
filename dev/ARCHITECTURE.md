# 架構與 Python→Rust 模組對應

技術棧：`axum` + `tokio` + `rquest`(Chrome TLS 指紋) + `serde` + `tiktoken-rs`。

## 模組對應表

| Rust 模組 | 對應 Python | 職責 |
|---|---|---|
| `src/config.rs` | `core/config.py` | Settings(env)、MODEL_MAP、resolve_model |
| `src/db.rs` | `core/database.py` | AsyncJsonDB → `Mutex<Vec<Value>>` + tokio::fs |
| `src/state.rs` | `main.py` app.state | AppState(Arc) |
| `src/account/account.rs` | `core/account_pool/pool_core.py` Account | 帳號結構（持久欄位 + 執行期欄位） |
| `src/account/pool.rs` | `core/account_pool/*` | 4 層並發控制、acquire/release/cooldown/affinity |
| `src/upstream/client.rs` | `services/qwen_client.py` + `core/httpx_engine.py` | rquest client、headers、create/delete/verify/models |
| `src/upstream/payload.rs` | `upstream/payload_builder.py` | build_chat_payload |
| `src/upstream/sse.rs` | `upstream/sse_consumer.py` | 解析 Qwen SSE（含 summary_thought 新格式） |
| `src/upstream/executor.rs` | `upstream/qwen_executor.py` | chat_stream_events_with_retry、重試分類 |
| `src/upstream/chat_id_pool.rs` | `services/chat_id_pool.py` | chat_id 預熱池 |
| `src/auth.rs` | `services/auth_quota.py` | API-key 解析、quota |
| `src/request/standard.rs` | `adapter/standard_request.py` | StandardRequest |
| `src/request/model_modes.rs` | `services/model_modes.py` | parse_model_mode（-thinking/-image…後綴） |
| `src/request/prompt_builder.rs` | `services/prompt_builder.py` | messages_to_prompt（壓平、system、工具注入、歷史預算） |
| `src/request/client_profiles.rs` | `services/client_profiles.py` | 偵測 Claude Code / Codex / qwen-code |
| `src/request/model_catalog.rs` | `services/model_catalog.py` | /v1/models 列表 + 能力後綴變體 |
| `src/toolcall/obfuscation.rs` | `services/tool_name_obfuscation.py` | Read→fs_open_file 等名稱混淆 |
| `src/toolcall/parser.rs` | `toolcall/parser.py` + formats_* | 從文字解析 tool call（QNML 主、XML/JSON/textkv 備援） |
| `src/toolcall/render.rs` | `toolcall/*` render | render_qnml_tool_calls |
| `src/execution/run.rs` | `runtime/execution.py` + `completion_bridge.py` | 編排：串流消費、工具偵測、重試 |
| `src/execution/translator.rs` | `services/openai_stream_translator.py` | OpenAI chunk 串流翻譯器 |
| `src/execution/presenter.rs` | `runtime/stream_presenter.py` | Anthropic/OpenAI/Gemini SSE 字串格式器 |
| `src/execution/formatters.rs` | `services/response_formatters.py` | 非串流回應組裝 |
| `src/context/oss.rs` | `services/upstream_file_uploader.py` | 阿里 OSS V4 簽名 + 上傳 |
| `src/context/*` | `services/context_*`、`file_store.py` | 上下文卸載、附件、本地檔案 |
| `src/api/*.rs` | `api/*.py` | 各協議路由 |
| `web/{index.html,app.js,style.css}` | `frontend/src/**`(React) | 純前端三檔重寫 |

## 請求主流程（對齊 Python dataFlow）
```
HTTP 進 → auth(resolve_auth_context) → 解析 body → preprocess_attachments
  → build StandardRequest(各協議) → prompt_builder 壓平成單一 prompt
  → execution::run（account_pool.acquire → create_chat[預熱池] → POST 串流
       → 解析 Qwen SSE delta(phase=thinking/answer/tool) → 翻譯成目標協議 chunk
       → 工具偵測/重試 → 結束刪會話/釋放帳號）
  → 串流(SSE)或非串流(JSON)回應；usage 用上游真實值
```

## 已知關鍵點 / 載荷不變式
- payload `function_calling=false` 等四旗標不可動（見 PROTOCOL.md）。
- chat_id 同時在 query 與 body。
- usage 用上游 `output_tokens`；prompt 用本地 tiktoken（cl100k_base）。
- Python `len()` 算的是 Unicode 標量數 → Rust 用 `.chars().count()` 對齊。
- account JSON 持久欄位：email,password,token,cookies,username,activation_pending,status_code,last_error,last_request_started,last_request_finished,consecutive_failures,rate_limit_strikes；執行期(不持久)：valid,inflight,rate_limited_until,last_used,healing。
