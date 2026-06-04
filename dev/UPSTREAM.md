# 上游版本追蹤（最重要的開發心得）

本專案 `qwen2api-rs` 是 **基於原 Python 專案 `YuJunZhiXue/qwen2API` 改造（後端 → Rust，前端 → 純 HTML/JS/CSS）** 而來。

## 🔖 基準版本（fork 起點）

| 項目 | 值 |
|---|---|
| 上游倉庫 | https://github.com/YuJunZhiXue/qwen2API |
| **基準 commit** | **`c62a6f4b51ad047e2710dd2b746c16f587f96c33`** |
| 基準日期 | 2026-06-03 (PR #70 merge from `YuJunZhiXue/trae`) |
| 基準分支 | `main` |
| 參考的本機部署版本 | `5245e6ad61b3cebf0fa51e3a1199c50b3693ef78`（2026-06-01, PR #65 by 123hi123，比基準略舊） |

> 本 Rust 移植的「行為對齊基準」= 上游 `c62a6f4`。所有架構分析、SSE 格式、payload 結構皆以此版本為準。

## 🔄 未來同步上游更新的標準流程

當想檢查原專案是否有值得同步的更新時：

```bash
# 1. 取得上游最新
cd /tmp && rm -rf qwen2API_upstream
git clone https://github.com/YuJunZhiXue/qwen2API.git qwen2API_upstream
cd qwen2API_upstream

# 2. 與我們的基準版本做 diff，只看功能性差異
git diff c62a6f4b51ad047e2710dd2b746c16f587f96c33..HEAD -- backend/ | less
# 重點關注：
#   - backend/upstream/   (上游協議：URL/headers/payload/SSE 是否變更 → 最高優先)
#   - backend/core/account_pool/ (帳號池策略)
#   - backend/services/prompt_builder.py, tool_parser.py (工具調用)
#   - backend/api/ (端點變化)
#   - core/config.py 的 MODEL_MAP / 預設模型

# 3. 判斷哪些功能性改動需要同步到 Rust 端，逐項移植

# 4. 同步完成後，更新本檔「基準 commit」為新的上游 HEAD，
#    並把舊基準存到下方「歷史基準」表，然後重新覆蓋（commit）。
```

### 歷史基準（每次同步後追加一列）

| 同步日期 | 從 commit | 到 commit | 同步了哪些功能 |
|---|---|---|---|
| 2026-06-03 | （初始） | `c62a6f4` | 初始移植 |

## ⚠️ 與上游的「刻意差異」（移植時的決策，不要當成需要同步的 bug）

1. **帳號自動註冊已移除**：上游用 camoufox/Playwright 無頭瀏覽器 + 臨時郵箱自動申請 Qwen 帳號（`auth_resolver.py` 的 `register_qwen_account`/`activate_account`/`refresh_token`）。此功能本質無法移植到 Rust，已移除，只保留「手動貼 token」。管理台 `/accounts/register`、`/accounts/{email}/activate` 回 501。
2. **TLS 指紋**：上游實際走 `httpx`（http2）；備援有 `curl_cffi(chrome124)`。Rust 端用 `rquest`（Chrome 指紋）以繞過阿里 WAF。
3. **Token usage 改用上游真實數值**：上游 Python 用 `len(prompt)` 字元數估算 usage；Qwen SSE 其實每個事件都回傳真實 `usage`（input/output/reasoning tokens），Rust 端改用真實值（見 PROTOCOL.md）。
4. **預設模型**：上游 `MODEL_MAP` 預設 `qwen3.6-plus`；實測上游現役已是 `qwen3.7-plus`。Rust 端預設改為動態抓 `/api/models` 第一個 base model，並把 fallback 預設設為 `qwen3.7-plus`。
5. **engine 簡化**：上游 tree 內有 `httpx_engine`/`browser_engine`/`hybrid_engine` 三種傳輸，但 `main.py` 實際只接 `QwenClient(httpx)`。Rust 只實作這一條真實路徑。
6. **影像/影片 chat_type 修正**：實測上游用 `t2i`/`t2v`（非 `image_gen`/視 payload 旗標）。見 PROTOCOL.md。
7. **刪除與調用解耦（風控）**：每次對話結束「在背景」刪除上游會話（`tokio::spawn`，取消安全的 RAII guard），且刪除期間該帳號保持佔用、釋放排在刪除之後 → 下一次調用因「最久未用」排序會選別的帳號，刪除完全離開請求路徑。對應小杨同学的風控顧慮與 Joe 的「刪除跟調用分開」建議。可再用 `ACCOUNT_MIN_INTERVAL_MS` 設定同帳號最小間隔進一步降低風控風險。
