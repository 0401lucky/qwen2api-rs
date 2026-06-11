# 部署 / 遷移 runbook

把正式部署從原 Python 版（`/home/joe/文件/docker/qwen2API`，端口 7860，走代理）切換到本 Rust 版。

- **源碼**：`/home/joe/文件/dev/qwen2api-rs`（開發在此）
- **部署資料夾**：`/home/joe/文件/docker/qwen2api-rs`（只放 compose + data；源碼用 build context 引用 dev）
- **對外端口**：7860（取代原版）
- **風控代理**：出口走 `http://ramdon:joe@192.168.1.203:2260`（必帶，否則出口變單一 IP，帳號易死）

## A. 一次性遷移（首次切換）

```bash
OLD=/home/joe/文件/docker/qwen2API
NEW=/home/joe/文件/docker/qwen2api-rs

# 1) 備份原版憑證（accounts.json 約 9.6MB / 16k 帳號，務必先備份）
mkdir -p ~/下載/qwen2api-backup-$(date +%Y%m%d)
cp "$OLD"/data/accounts.json "$OLD"/data/api_keys.json "$OLD"/data/users.json ~/下載/qwen2api-backup-$(date +%Y%m%d)/

# 2) 建立部署資料夾與 data
mkdir -p "$NEW/data"

# 3) 遷移憑證（只搬這三個；session_affinity/context_cache/uploaded_files 是各自內部狀態，不要搬）
cp "$OLD"/data/accounts.json "$NEW"/data/
cp "$OLD"/data/api_keys.json "$NEW"/data/
cp "$OLD"/data/users.json    "$NEW"/data/ 2>/dev/null || echo '[]' > "$NEW"/data/users.json

# 4) 放入 NEW/docker-compose.yml（內容見下方範本，已含端口/代理/ADMIN_KEY）

# 5) 停掉原 Python 服務（釋放 7860；先不刪資料夾，留作回滾）
cd "$OLD" && docker compose down

# 6) 啟動新 Rust 服務（從 dev 源碼建置）
cd "$NEW" && docker compose up -d --build
docker compose logs -f --tail=40        # 看到「帳號池已載入 16857 個帳號」「已啟動 ... 7860」

# 7) 驗證
curl -s http://127.0.0.1:7860/healthz                       # {"status":"ok"}
curl -s http://127.0.0.1:7860/api/admin/status -H "Authorization: Bearer <原ADMIN_KEY>" | head -c 300
#   用原 ADMIN_KEY 打一次真實對話，確認「經代理」連上游成功
```

切換確認無誤後，原 `qwen2API` 資料夾可保留數日作回滾，確定穩定再清理。

## B. NEW/docker-compose.yml 範本

```yaml
services:
  qwen2api-rs:
    build:
      context: ../../dev/qwen2api-rs     # 從 dev 源碼建置（相對於本 compose 檔）
    image: qwen2api-rs:latest
    container_name: qwen2api-rs
    restart: unless-stopped
    init: true
    ports:
      - "7860:7860"                       # 取代原 Python 版端口
    volumes:
      - ./data:/app/data                  # 帳號/金鑰持久化（本資料夾底下）
    environment:
      ADMIN_KEY: "Db586ZRIWtvvJeOqlP4QZ5KkwXCkWAmB"   # 沿用原版，既有 API Key 不失效
      PORT: "7860"
      LOG_LEVEL: "info"
      MAX_INFLIGHT_PER_ACCOUNT: "2"
      ACCOUNT_MIN_INTERVAL_MS: "3000"     # 風控休息（同帳號最小間隔）
      CHAT_ID_PREWARM_TARGET_PER_ACCOUNT: "0"     # 默认关闭预热，避免启动批量建会话触发 WAF
      CHAT_ID_PREWARM_MAX_ACCOUNTS: "8"
      DEFAULT_MODEL: "qwen3.7-plus"
      # —— 風控代理（沿用原 override，務必保留）——
      HTTP_PROXY: "http://ramdon:joe@192.168.1.203:2260"
      HTTPS_PROXY: "http://ramdon:joe@192.168.1.203:2260"
      http_proxy: "http://ramdon:joe@192.168.1.203:2260"
      https_proxy: "http://ramdon:joe@192.168.1.203:2260"
      NO_PROXY: "localhost,127.0.0.1,192.168.0.0/16,10.0.0.0/8,172.16.0.0/12"
      no_proxy: "localhost,127.0.0.1,192.168.0.0/16,10.0.0.0/8,172.16.0.0/12"
    healthcheck:
      test: ["CMD-SHELL", "wget -qO- http://127.0.0.1:7860/healthz >/dev/null 2>&1 || exit 1"]
      interval: 30s
      timeout: 5s
      start_period: 30s
      retries: 3
```

> reqwest 預設讀 `HTTP_PROXY/HTTPS_PROXY/NO_PROXY` → 無需改碼即走代理。cutover 前可先本機 `HTTPS_PROXY=... ./qwen2api-rs` 跑一筆對話驗證代理鏈路。

## C. 每次開發後重新部署
```bash
cd /home/joe/文件/docker/qwen2api-rs
docker compose up -d --build        # 從 dev 源碼重建並滾動更新；data/ 不動
docker compose logs -f --tail=30
```

## D. 回滾
```bash
cd /home/joe/文件/docker/qwen2api-rs && docker compose down
cd /home/joe/文件/docker/qwen2API   && docker compose up -d   # 切回原 Python 版
```

## 注意事項
- accounts.json 與原版**格式相容**（Rust 以 serde 讀已知欄位、忽略多餘欄位；16k 筆載入正常）。
- api_keys.json 格式 `{"keys":[...]}` 相容。
- ⚠️ 帳號數上萬時，帳號狀態變更（限流/失效/驗證）會整檔重寫 ~9.6MB；目前可接受，未來可優化為去抖/批次寫入。
- 端口衝突：務必先 `docker compose down` 原版再起新版；兩者 container_name 不同（`qwen2api` vs `qwen2api-rs`）不會撞名。
