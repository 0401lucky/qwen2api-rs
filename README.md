# qwen2api-rs

把通義千問（Qwen）Web 端能力轉換成 **OpenAI / Anthropic Claude / Gemini** 相容介面的自託管網關。

本專案是 [YuJunZhiXue/qwen2API](https://github.com/YuJunZhiXue/qwen2API)（Python + React）的 **Rust 後端 + 純原生前端** 重寫版：
- **後端**：Rust（`axum` + `tokio` + `reqwest` + `serde`），單一靜態二進位，低記憶體、高並發。
- **前端**：純 `HTML + CSS + JS` 三檔（`web/`），零框架、零建置、可離線。
- 基準上游版本與同步流程見 [`dev/UPSTREAM.md`](dev/UPSTREAM.md)。

## 功能

- ✅ OpenAI Chat Completions（`/v1/chat/completions`）串流 + 非串流
- ✅ OpenAI Responses（`/v1/responses`）typed SSE events
- ✅ Anthropic Messages（`/v1/messages`、`/anthropic/v1/messages`）串流 + 非串流 + `count_tokens`
- ✅ Gemini `generateContent` / `streamGenerateContent`
- ✅ OpenAI Images（`/v1/images/generations`）— 驅動 Qwen 影像生成
- ✅ OpenAI Embeddings（佔位，確定性向量）
- ✅ 檔案上傳（`/v1/files`）+ 對話附件（自動阿里 OSS V4 上傳 / 小文字檔內聯）
- ✅ 工具/函式調用：工具定義注入 prompt + 從輸出解析 `tool_call`（Qwen Web 無原生工具）
- ✅ 思考模式（reasoning）串流，**usage 採上游真實 token 數**
- ✅ 帳號池：4 層並發控制、最少負載選號、限流指數退避、跨帳號重試
- ✅ chat_id 預熱池（規避上游 `/chats/new` 0.5~6s 握手；對上萬帳號有覆蓋數上限保護）
- ✅ 管理台 WebUI：運行狀態、帳號管理、API Key、接口測試、圖片生成、系統設置
- ✅ `/healthz`、`/readyz` 探針

## 快速開始

需求：Rust 1.80+（已測 1.93）。

```bash
cp .env.example .env          # 設定 ADMIN_KEY、PORT 等
mkdir -p data
# 放入帳號：data/accounts.json = [{"email","token", ...}, ...]
#   token = 在 chat.qwen.ai 登入後，localStorage 裡的 token 原始值
cargo run --release
```

啟動後：
- WebUI：`http://127.0.0.1:7860/`（系統設置頁貼上 `ADMIN_KEY` 或任一 API Key 作為會話金鑰）
- API Base：`http://127.0.0.1:7860`

呼叫範例：

```bash
curl http://127.0.0.1:7860/v1/chat/completions \
  -H "Authorization: Bearer <你的 API Key 或 ADMIN_KEY>" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4o","messages":[{"role":"user","content":"你好"}],"stream":true}'
```

模型名可用任意 OpenAI/Claude/Gemini 名稱（自動映射到 Qwen，未知者回退 `DEFAULT_MODEL`），
或直接用 `qwen3.7-plus`、`qwen3.7-plus-thinking` 等（`/v1/models` 可查全部）。

## 部署

兩種皆可（單一靜態 binary，rustls 無需系統 OpenSSL）。

### Docker（推薦，尤其從原 Python 版遷移者）
與原專案相同的 docker-compose 工作流；映像約 145MB（debian-slim 基底，比原 Python 版含 camoufox 小很多；可改 distroless/musl 再瘦身）。
```bash
# data/ 可直接沿用原版（放 accounts.json 等）
mkdir -p data
vim docker-compose.yml      # 修改 ADMIN_KEY 等
docker compose up -d --build
docker compose logs -f
```
- 資料持久化：`./data` 掛到容器 `/app/data`。
- 內建 `HEALTHCHECK`（打 `/healthz`）。
- 更新：`git pull && docker compose up -d --build`。

### Binary（最輕量，單機/VPS）
```bash
cargo build --release          # 產出 target/release/qwen2api-rs
cp .env.example .env && vim .env
mkdir -p data                  # 放 accounts.json
WEB_DIR=web ./target/release/qwen2api-rs
```
建議用 systemd 常駐（`/etc/systemd/system/qwen2api-rs.service`）：
```ini
[Unit]
Description=qwen2api-rs gateway
After=network.target
[Service]
WorkingDirectory=/opt/qwen2api-rs
EnvironmentFile=/opt/qwen2api-rs/.env
ExecStart=/opt/qwen2api-rs/qwen2api-rs
Restart=always
[Install]
WantedBy=multi-user.target
```

> Docker vs Binary 取捨：Docker = 可重現、隔離、跨發行版可攜、與原版同流程、易更新/重啟；Binary = 啟動最快、佔用最小、無需 docker，但需自行用 systemd 常駐且跨機需注意 glibc 版本（或用 musl 靜態編譯）。對你（已用 docker-compose 跑原版）→ **Docker**。

## 環境變數

見 [`.env.example`](.env.example)。變數名與原 Python 版相容，可直接指向同一份 `data/`。

## 認證

- 下游請求：`Authorization: Bearer <key>`、`x-api-key`、或 `?key=`。
- 若 `data/api_keys.json` 有設定 key，則必須使用 `ADMIN_KEY` / 已建立的 key；否則放行任意 key。
- 管理台 `/api/admin/*`：`Bearer` 須等於 `ADMIN_KEY` 或已建立的 key。

## 架構

技術棧與 Python→Rust 模組對應見 [`dev/ARCHITECTURE.md`](dev/ARCHITECTURE.md)；
實測捕捉的上游協議（含 SSE 格式）見 [`dev/PROTOCOL.md`](dev/PROTOCOL.md)。

```
src/
  main.rs            入口 / 路由
  config.rs state.rs db.rs error.rs util.rs auth.rs
  account/           帳號池（account.rs / pool.rs）
  upstream/          上游傳輸（client/payload/sse/executor/chat_id_pool）
  request/           標準請求構建（model_modes/prompt_builder/client_profiles/model_catalog）
  toolcall/          工具調用（注入 + 解析 + 名稱混淆）
  execution/         編排 + 串流翻譯（translator/presenter/formatters）
  context/           附件 / OSS V4 上傳 / 本地檔案庫
  api/               各協議端點
web/                 純前端三檔（index.html / app.js / style.css）
dev/                 開發筆記（上游版本追蹤、架構、協議捕捉）
```

## 與原專案的刻意差異

1. 移除瀏覽器自動註冊（camoufox/Playwright + 臨時郵箱）→ 僅手動貼 token。
2. usage 改用上游真實 token 數（原版用字元數估算）。
3. 預設旗艦模型更新為 `qwen3.7-plus`。
4. 工具調用採單一穩定文字格式（`<tool_call>{json}</tool_call>`）注入 + 解析。

詳見 [`dev/UPSTREAM.md`](dev/UPSTREAM.md)。

## 授權

僅供學習與自託管研究。Qwen 為阿里巴巴商標，使用需遵守其服務條款。
