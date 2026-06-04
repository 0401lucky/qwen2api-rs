# Qwen 上游協議（實測捕捉，2026-06-03）

Base URL: `https://chat.qwen.ai`。所有請求帶下列 headers：

```
Authorization: Bearer <account_token>      # token = 帳號在 localStorage 的 token
User-Agent: Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36
Accept: application/json, text/plain, */*
Accept-Language: zh-CN,zh;q=0.9,en;q=0.8
Referer: https://chat.qwen.ai/
Origin: https://chat.qwen.ai
Content-Type: application/json
```
串流請求另加 `Accept: text/event-stream`。不需要 Cookie / x-* / bx-* headers（實測 200 OK）。

## 端點

| 用途 | 方法/路徑 | 成功判定 |
|---|---|---|
| 驗證 token | `GET /api/v1/auths/` | 200 且 body `role=="user"` |
| 模型列表 | `GET /api/models` | 200 `{data:[...]}` |
| 建會話 | `POST /api/v2/chats/new` | 200 `{success:true, data:{id}}` |
| 串流補全 | `POST /api/v2/chat/completions?chat_id=<id>` | 200 SSE |
| 刪會話 | `DELETE /api/v2/chats/{chat_id}` | 200/204/404 |
| 列會話 | `GET /api/v2/chats?limit=N` | 200 `{data:[...]}` |
| 檔案上傳 | `POST /api/v2/files/getstsToken` → OSS put → `POST /api/v2/files/parse` → poll `POST /api/v2/files/parse/status` | data[0].status=="success" |

### 建會話 body
```json
{"title":"api_<unixts>","models":["qwen3.7-plus"],"chat_mode":"normal","chat_type":"t2t","timestamp":<unixts>}
```
回應：`{"success":true,"request_id":"...","data":{"id":"<chat_id>"}}`

### 串流補全 body（payload_builder）
```json
{"stream":true,"version":"2.1","incremental_output":true,"chat_id":"<id>","chat_mode":"normal",
 "model":"qwen3.7-plus","parent_id":null,
 "messages":[{"fid":"<uuid4>","parentId":null,"childrenIds":["<uuid4>"],"role":"user",
   "content":"<prompt>","user_action":"chat","files":[],"timestamp":<ts>,"models":["qwen3.7-plus"],
   "chat_type":"t2t",
   "feature_config":{"thinking_enabled":true,"output_schema":"phase","research_mode":"normal",
     "auto_thinking":true,"thinking_mode":"Auto","thinking_format":"summary","auto_search":false,
     "code_interpreter":false,"plugins_enabled":false,
     "function_calling":false,"enable_tools":false,"enable_function_call":false,"tool_choice":"none",
     "image_gen":false,"image_generation":false},
   "extra":{"meta":{"subChatType":"t2t"}},"sub_chat_type":"t2t","parent_id":null}],
 "timestamp":<ts>}
```
- ⚠️ `function_calling/enable_tools/enable_function_call=false`、`tool_choice:"none"` 是**載荷關鍵**：開啟原生工具會被上游攔截回 `Tool X does not exists.`。工具調用一律走 prompt 文字注入 + 本地解析。
- fast 模式：`thinking_enabled/auto_thinking=false`、`thinking_mode:"Disabled"`。
- `has_custom_tools=true` 時強制關 thinking（低延遲）。
- image_gen：feature_config 另加 image_size/image_ratio/aspect_ratio/width/height；extra.meta 加 imageSize 等；chat_type 用 `image_gen`/`t2i`。

## SSE 串流格式（實測，最重要）

事件以空行 `\n\n` 分隔，每行 `data: {json}`。**無 `[DONE]` 終止符**（原 Python 仍兼容處理）。

**(1) 首事件**（無 choices，可忽略或用來取 response_id）：
```
data: {"response.created":{"chat_id":"...","parent_id":"...","response_id":"...","response_index":"0"}}
```

**(2) thinking 階段**（`phase:"thinking_summary"`，content 為空；思考內容在 extra，且為**累積陣列**）：
```json
{"choices":[{"delta":{"role":"assistant","content":"","phase":"thinking_summary",
  "extra":{"summary_title":{"content":["標題1","標題2"]},
           "summary_thought":{"content":["思考段1","思考段2"]}},
  "status":"typing"}}],
 "response_id":"...","usage":{"input_tokens":1438,"output_tokens":104,"total_tokens":1542,
   "output_tokens_details":{"reasoning_tokens":102,"text_tokens":104}},"timestamp":...}
```
- ⚠️ qwen3.7 的思考文字在 `delta.extra.summary_thought.content`（list，**每次回傳到目前為止的完整陣列**，不是增量）。
- 原 Python `sse_consumer._extract_reasoning` 只看 `reasoning_content/reasoning/thinking/thoughts`，**不解析此新格式** → 對 qwen3.7 思考會漏接。Rust 端要額外處理 `summary_thought.content`（join 後與已發送的部分做 diff 取增量）。
- thinking 結束：一筆 `phase:"thinking_summary", status:"finished"`。

**(3) answer 階段**（`phase:"answer"`，`delta.content` 為**增量**文字）：
```json
{"choices":[{"delta":{"role":"assistant","content":"我是通義千","phase":"answer","status":"typing"}}],
 "response_id":"...","usage":{...},"timestamp":...}
```

**(4) 結束事件**：
```json
{"choices":[{"delta":{"content":"","role":"assistant","status":"finished","phase":"answer"}}],"response_id":"..."}
```

## 影像 / 影片生成（實測，2026-06-03）
- **chat_type 必須是 `t2i`（圖片）/ `t2v`（影片）**——不是 `image_gen`！用 `image_gen` 上游回 `Bad_Request / Internal error`。
- 建會話與 payload 的 chat_type、extra.meta.subChatType、sub_chat_type 都用 `t2i`/`t2v`。
- 比例用 `size` 欄位（字串，如 `"1:1"`/`"16:9"`），同時放「訊息物件層」與「payload 頂層」。feature_config 思考關閉即可，無需 image_size/plugins 等旗標。
- 圖片結果：`delta.content` 為完整 URL，`phase` 是 **`image_gen`**（非 answer！故 SSE 消費端要收集非思考階段的 content），形如 `https://cdn.qwenlm.ai/output/<uid>/t2i/<rid>/<ts>.png?key=<JWT>`（**key 必帶，否則無法存取**）。`extra.output_image_hw` 為尺寸。
- 影片較慢且部分帳號實測回傳空內容（疑為帳號權限或非同步任務流程）；管線已就緒，有 URL 即可用。
- 模型能力來自 `/api/models` 的 `info.meta.chat_type` 陣列（含 `t2i`/`t2v`/`deep_research`/`web_dev`/`slides`）與 `capabilities`。

## Usage（優化點）
每個事件帶真實 `usage`：`input_tokens`、`output_tokens`、`total_tokens`、`output_tokens_details.{reasoning_tokens,text_tokens}`、`prompt_tokens_details.cached_tokens`。
- 取「最後一筆有 usage 的事件」即為最終用量。
- 注意：`input_tokens` 包含 Qwen 自家隱藏 system prompt（實測小 prompt 也 ~1000+ tokens），故面向客戶端的 `prompt_tokens` 若要貼近客戶實際輸入，可改用本地 tiktoken 計算 prompt 字串；`completion_tokens` 用上游 `output_tokens` 最準。

捕捉樣本見 `dev/captures/`。
