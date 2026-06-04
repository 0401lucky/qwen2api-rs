# 請求鏈路延遲分析與「空間換時間」新框架提案

> 調查日期 2026-06-03。方法：精讀 `src/{api,execution,upstream,account,request,context}`，並對上游 `create_chat` 做實測錨定。**本文僅分析與提案，未改任何程式碼。**

---

## 0. TL;DR（核心結論）

關鍵路徑（典型 `t2t`、無附件、串流）：

```
HTTP 進 → auth → build StandardRequest(prompt_builder) 
  → pool.acquire_wait ──①──→ obtain_chat_id ──②──→ build_payload 
  → client.start_stream ──③──→ 消費 SSE（邊收邊回）
  → [背景] StreamGuard::drop → delete_chat + release   ← fire-and-forget，不阻塞回應 ✓
```

扣掉「上游生成時間（③，1–10s，不可優化）」後，**每次請求關鍵路徑上真正可被我們消除的延遲有三塊**：

| 瓶頸 | 位置 | 現況成本 | 性質 |
|---|---|---|---|
| **B1 建會話 `create_chat`** | executor.rs:140 → client.rs:134 | 本機直連 **0.32–0.58s**；**經風控代理 0.5–6s** | 預熱池命中可省，但**舊池補給機制太弱、命中率趨近 0** |
| **B2 `acquire` 全表掃描 + 全局鎖** | pool.rs:209–266 | 16857 帳號每次請求 **O(n) filter + O(n log n) sort 且全程持 `Mutex`** | 高並發鎖競爭，被現有實作低估 |
| **B3 `min_interval` 風控休息** | pool.rs:224、account.rs `next_available_at` | 同帳號請求結束後 **3000ms** 不可再用 | 吞吐上限，**不可移除（保帳號）**，但可用更多預備資源攤平 |

**提案**：把系統從「被動、慢補給、全表掃描」改成「**預備資源前置化（Resource Pre-staging）**」——三支柱：①需求驅動的即時回補預熱池、②就緒帳號索引（Ready-Set）、③連線保活。核心是**用記憶體中的預備結構（空間），換取關鍵路徑上的 HTTP 往返與鎖等待（時間）**。

---

## 1. 現有鏈路全貌（已逐行驗證）

### 1.1 編排（`src/upstream/executor.rs::run_stream`）
1. **取帳號**：`pool.acquire_wait(None, exclude, 60.0)`（executor.rs:159）。
2. **建 `StreamGuard`**（executor.rs:176）：取消安全；任何離開路徑（成功/重試/錯誤/客戶端斷線）都在 `drop` 時 **`tokio::spawn`** 背景做 `delete_chat + release`（executor.rs:97–104）。→ **刪會話確認不在關鍵路徑** ✓
3. **取會話 `obtain_chat_id`**（executor.rs:122–142）：
   - `existing` → 直接用（不擁有，不刪）；
   - `use_prewarmed && chat_type=="t2t"` → 試**預熱池** `chat_id_pool.acquire`；
   - 否則 → **`client.create_chat`（同步 HTTP 往返，關鍵路徑）**。
4. **`build_chat_payload`**（payload.rs）→ **`start_stream`**（client.rs:229，POST `/chat/completions`，等響應頭與首事件）。
5. **消費 SSE**：`bytes_stream` 以 `\n\n` 切分 → `parse_sse_chunk` → `yield Delta`（邊收邊回，不聚合）。

### 1.2 連線層（`src/upstream/client.rs`）— 現有的空間換時間優化 ✓
- 全域單一 `reqwest::Client`，`ArcSwap` 包裝可熱抽換 proxy；`client()` 為 `Arc` clone，<1ms（client.rs:40–73）。
- 連線池：`pool_max_idle_per_host(20)`、`pool_idle_timeout(30s)`、http2、rustls、gzip（client.rs:16–21）。
- **TLS 握手在熱連線下不重複**；但 idle>30s 或經代理時仍會重握手。

### 1.3 帳號池（`src/account/pool.rs`）
- `acquire_locked`（pool.rs:213–266）：持 `state` Mutex → **filter 全部帳號** → **sort_by(inflight, last_request_started, last_used)** → 取候選[0] → `inflight++`、`global_in_use++`。
- `acquire_wait`（pool.rs:269–310）：先試即時取；失敗則排隊，**每 500ms poll 一次 + `Notify`**，最長 60s。
- `release`（pool.rs:313–323）：`inflight--`、`global_in_use--`、`notify_one`。快速、不阻塞。
- 風控 `is_available(min_ms)` → `next_available_at = max(rate_limited_until, last_started+Δ, last_finished+Δ)`，Δ=`min_interval_ms`（預設 3000，config.rs:100）。

### 1.4 預熱池（`src/upstream/chat_id_pool.rs`）— **問題核心**
- `acquire`（line 69–99）：pop 一個未過期 chat_id；過期者背景刪除。
- `start`（line 119–133）：背景迴圈 `refill_round()` 後 **`sleep(30s)`**。
- `refill_round`（line 139–191）：只取 `all_emails_tokens` 前 **`max_accounts`（預設 8）** 個帳號；**每輪每帳號最多補 1 個**（line 179–180）；TTL（預設 120s）過期淘汰並背景刪除。

---

## 2. 延遲構成與實測數據

### 2.1 實測：上游 `create_chat`（本機直連，連測 4 次）
```
#1 total=0.577s  connect=0.40s  TLS=0.46s   (首次含 TCP+TLS 握手)
#2 total=0.343s  connect=0.14s  TLS=0.21s
#3 total=0.452s  connect=0.26s  TLS=0.33s
#4 total=0.322s  connect=0.14s  TLS=0.21s
```
- 純業務（total − appconnect）≈ **0.11–0.13s**；其餘是 TCP+TLS+RTT。
- curl 每次新進程→每次重握手；**服務內連線復用後，`create_chat` ≈ 0.1–0.45s**。
- ⚠️ **正式環境出口走風控代理**（`http://…@192.168.1.203:2260` 隨機出口 IP，見 CLAUDE.md/DEPLOY.md）→ 多一跳代理 + 出口 IP 可能遠 → `create_chat` 漲到 **0.5–6s**（chat_id_pool.rs:2 註解即此值）。**這就是預熱池要消除的成本。**

### 2.2 各延遲點分類（驗證後）
| 類 | 點 | 成本 | 必經？ |
|---|---|---|---|
| A 網路 | `create_chat`（預熱 miss） | 0.1–6s | 預熱 miss 時 |
| A 網路 | `start_stream` 等首事件 | 1–10s | **是（上游生成，不可優化）** |
| A 網路 | `delete_chat` | 0（背景 spawn） | 否 |
| B 等待 | `min_interval` 同帳號休息 | 3000ms | 影響吞吐，非單請求路徑 |
| B 等待 | `acquire_wait` 排隊 poll | 500ms/輪，最長 60s | 僅全忙時 |
| C 鎖 | `acquire` 持鎖 O(n)+O(n log n) | 帳號越多越貴 | **是** |
| D CPU | `prompt_builder` 壓平 | 5–20ms | 是 |
| D CPU | `count_tokens`（tiktoken） | 10–100ms | **僅非串流** end 計費；串流無 |
| E 連線 | TLS 握手 | 熱連線 0 | 否（已復用） |

---

## 3. 三大瓶頸（驗證細節）

### B1 — 預熱池命中率趨近 0（最大浪費）
**舊池穩態補給速率上限** = `min(max_accounts, 帳號數) / 30s` = **≤ 8/30 ≈ 0.27 個/秒**（每輪每帳號補 1 個，再 sleep 30s）。
- 只要實際 QPS > 0.27，**消費速率 >> 補給速率**，池被瞬間掏空 → 之後幾乎每個請求都 fallback 到同步 `create_chat`。
- 命中率 ≈ `補給/消費` = `0.27 / QPS`。例：QPS=5 → 命中率 ≈ **5%**；QPS=1 → ≈ 27%。
- 再疊加 `max_accounts=8` 覆蓋限制：`acquire` 依負載排序選帳號，**選中的帳號未必在預熱覆蓋集內** → 命中率再打折。
- TTL=120s：低谷期備好的 chat_id 過期作廢，還要背景刪除（額外上游請求）。
- 本機 `.env CHAT_ID_PREWARM_TARGET_PER_ACCOUNT=0` → **預熱池完全關閉**，每請求必 `create_chat`。

> 結論：預熱池**理念正確（空間換時間）但補給引擎太弱**，在任何實際負載下近乎失效。

### B2 — `acquire` 的 O(n) 全表掃描 + 全局鎖
`acquire_locked`（pool.rs:227–247）對 **16857 個帳號**每次請求都：持 `Mutex` → `filter().collect()`（O(n)）→ `sort_by()`（O(n log n)）。
- 全程持鎖 → **所有並發請求的 acquire 被序列化**；帳號越多，臨界區越長，鎖競爭越嚴重。
- 即使單次只有幾 ms，在高並發下會成為吞吐天花板（請求排隊等鎖）。

### B3 — `min_interval` 3s 吞吐上限
單帳號吞吐 = `1 / (請求時長 + 3s)`。有效並發理論上限 = `valid 帳號 × per_account`。
- 風控必要，**不可移除**。但實務上「能立即投入的帳號數」常 < 理論上限（因 B1 預熱 miss 拖慢、B2 鎖競爭），使實際吞吐低於理論。

---

## 4. 新框架：預備資源前置化（Resource Pre-staging）

> 一句話：**把關鍵路徑上的「現算現連現建」，全部前移到背景用記憶體預備好；請求來時只做 O(log n) 取貨。**

### 支柱 1：需求驅動的「即時回補」預熱池（治 B1）
- **消費即回補（consume-triggered refill）**：`chat_id_pool.acquire` 成功 pop 後，**立即 `tokio::spawn` 補 1 個**，而非等 30s 輪詢。補給速率 = 消費速率（上限為帳號 `create_chat` 並發能力，遠高於 0.27/s）。
- **動態水位**：依滑動視窗 QPS 自動調 `target`（高峰多備、低谷少備），空間隨需求伸縮，避免 TTL 浪費。
- **跟隨熱帳號**：不再固定前 8 個；對 `acquire` 實際選中的帳號優先預備（與 B2 的就緒索引共享熱度資訊）。
- **背景輪詢退化為兜底**（補滿水位、淘汰過期），不再是主補給來源。

效果：穩態命中率從 `0.27/QPS` → **接近 100%**（只要回補延遲 < 水位 × 平均請求間隔）。

### 支柱 2：就緒帳號索引 Ready-Set（治 B2）
- 維護兩個結構（增量更新，取代每次全掃）：
  - **就緒堆**：可立即使用的帳號，按 `(inflight, last_used)` 排序 → `acquire` 從堆頂 **O(log n)** 取。
  - **冷卻定時堆**：`min_interval`/限流中的帳號，按 `next_available_at` 排序；到期才移回就緒堆。
- 狀態事件（release / 限流 / 冷卻到期）增量維護索引，**臨界區從 O(n log n) 降到 O(log n)**，鎖持有時間驟降。
- 空間成本：兩個索引（每帳號數十 bytes，16857 帳號約 ~1–2 MB）。

### 支柱 3：連線保活（治 E，經代理時收益大）
- 對熱帳號/熱出口維持少量 keep-alive 預連線，避免 idle>30s 後重握手；經代理時 TLS 握手成本更高，預連線收益更明顯。
- 與支柱 1 協同：背景 `create_chat` 同時把連線「焐熱」。

### 不動的部分（取捨）
- **`min_interval` 與限流退避保留**（風控紅線，B3 不靠縮短間隔，而靠支柱 1+2 讓「可立即投入帳號」最大化來攤平）。
- `delete_chat` 已是背景 fire-and-forget，無需改。
- 串流模式不跑 tiktoken，已最優；非串流的 `count_tokens` 可改背景/快取（次要）。

---

## 5. 新舊框架對比

### 5.1 單請求 TTFT（首 token），扣除上游生成、**經代理**場景（`create_chat`≈2s 取中值）
| 階段 | 舊（預熱 miss，常態） | 舊（預熱 hit，低機率） | **新框架** |
|---|---|---|---|
| acquire | O(n) 持鎖掃 16857：~ms 級且序列化 | 同左 | Ready-Set O(log n)：~微秒級 |
| create_chat | **~2s** | 0 | **0（高命中）** |
| 可控延遲合計 | **≈ 2s + 鎖等待** | ≈ 鎖等待 | **≈ 0** |

### 5.2 命中率模型（核心論證）
| QPS | 舊池命中率 `≈0.27/QPS` | 舊池每請求期望省 | 新框架命中率 | 新框架每請求期望省 |
|---|---|---|---|---|
| 1 | ~27% | 0.27×2s=0.54s | ~100% | ~2.0s |
| 5 | ~5% | 0.10s | ~100% | ~2.0s |
| 20 | ~1.4% | 0.03s | ~95–100% | ~1.9s |

> 負載越高，舊池越失效、新框架優勢越大。**新框架在中高負載下每請求平均省 ~1.9s（經代理）/ ~0.1–0.4s（直連）**。

### 5.3 吞吐
- 舊：受 B2 鎖競爭 + B1 預熱 miss 拖累，實際並發 < `valid×per_account` 理論上限。
- 新：acquire O(log n) 去除鎖瓶頸 + 預熱接近 100% → 實際並發逼近理論上限；`min_interval` 仍是最終天花板（不變，符合風控）。

---

## 6. 驗證方法與證據（本次已做 / 可續做）

**已驗證（程式碼事實 + 實測）：**
1. 鏈路與 fire-and-forget 刪會話：executor.rs:97–104、291–311（逐行確認）。
2. 預熱池補給上限 0.27/s：chat_id_pool.rs:130（sleep 30s）+ 179–180（每帳號每輪 1 個）+ 144–147（max_accounts 截斷）。
3. acquire O(n)+O(n log n) 持鎖：pool.rs:227–247。
4. `create_chat` 實測：本機直連 0.32–0.58s（§2.1），TLS 占比顯著；經代理依設計值 0.5–6s。
5. 連線池已復用：client.rs:16–21。

**建議續做的量化驗證（仍不需改動正式碼，用實驗開關）：**
- A/B 實測命中效益：臨時 `CHAT_ID_PREWARM_TARGET_PER_ACCOUNT=5` 重啟，等補給後對短請求測 **TTFT**，對比 `=0`，量出 `create_chat` 在端到端的占比。
- 鎖壓測：複製 16857 帳號到本機，用 `wrk`/併發 curl 打 `/v1/chat/completions`，觀察 acquire 鎖等待（可加臨時 tracing span）→ 驗證 B2 在大帳號數下的鎖競爭。
- 代理場景：在出口代理下重測 `create_chat`，驗證 0.5–6s 假設。

---

## 7. 落地優先序（建議，非本次實作）
1. **支柱 1 即時回補**（改 `chat_id_pool`：acquire 後 spawn 補位 + 動態 target）— 投入小、收益最大（命中率 5%→~100%）。
2. **支柱 2 Ready-Set**（改 `pool` 內部結構，對外 API 不變）— 大帳號數高並發必要。
3. **支柱 3 連線保活** — 經代理環境錦上添花。

> 三者皆為內部優化，**對外 API 與風控行為不變**；落地後每請求關鍵路徑（扣上游）可從「最壞 2s+鎖等待」降到「~微秒級取貨」。

---

## 8. 實作狀態（2026-06-03，已落地）

三支柱皆已實作、編譯通過、單元測試 + 本機(7866) 實測驗證。經設計對抗審查 + 實作對抗審查（雙 workflow）。

### Pillar 1 — 即時回補預熱池（`src/upstream/chat_id_pool.rs`）
- `acquire(email, token, model)`：命中/未命中皆 `spawn_refill` 補滿至 target；**token 由 executor 的 AccountHandle 傳入**，熱路徑零 `pool.token_of()`（避免抵銷 Pillar 2）。
- `Inner{cache,pending}` 同一把鎖，`size+pending<target` 檢查與佔位原子化，避免並發過量；`create_chat().await` 期間不持鎖。
- 背景迴圈退化為 bootstrap（前 `max_accounts` 個）+ GC（過期刪除、清空鍵）。
- **實測**：啟動數秒內 `total_cached` 即達 5×帳號數（舊版需 ~150s）；連續補全 100% 命中預熱池、消費後即時回補維持水位。

### Pillar 2 — 就緒帳號索引 Ready-Set（`src/account/pool.rs`）
- `ready` = `BinaryHeap<Reverse<(inflight, last_used, email)>>`（least-loaded + LRU，對齊舊 `acquire_scan` 主鍵）；`cooldown` = `BinaryHeap` keyed by `next_available_at`；`loc` 為真相來源（lazy 刪除）；`pos` = email→index（O(1) 取帳號）。
- `acquire` 攤銷 ~O(1)，取代舊 O(n) filter+sort。所有狀態變更點（acquire/release/mark_*/apply_verify）在**同臨界區**重置索引；add/remove/set_max_inflight 整建；`set_min_interval` 為 sync/lock-free，靠 `interval_gen` 世代在下次 acquire 惰性整建；cooldown drain 以**即時** `next_available_at` 重判（堆鍵僅排序提示）。
- **kill-switch**：`POOL_READY_INDEX=0` 回退舊 `acquire_scan`（保留作 fallback 與測試 oracle）。預設開。
- **測試**：6 項單元測試含 4000 次隨機操作一致性壓測（loc==即時真值、`global_in_use` 收支平衡、絕不取出失效/超 cap 帳號）+ 索引/掃描可取數平價 + 風控冷卻 + 最少負載排序。

### Pillar 3 — 連線保活（`src/main.rs` + `src/config.rs`）
- `CONN_KEEPALIVE_SECONDS`（預設 0=關，風控敏感）。>0 時背景每 N 秒對上游送一次輕量 `verify_token` 保溫一條連線。

### 審查結論
- 設計對抗審查（5 lens）：修正 5 項 must-fix（token 傳入、min_interval 世代整建、收斂結構、同臨界區重置、oracle 契約）後落地。
- 實作對抗審查（5 維度 → 逐項驗證）：僅 2 項 **minor**，無 fatal/major。其一（就緒排序在 min_interval=0 退化為 FIFO）已以 `(inflight,last_used)` 堆修正；其二（preferred 分支差異）為休眠路徑（熱路徑 preferred 恆 None），無需處理。

> **預設行為提醒**：Ready-Set 預設開啟。若偏好保守上線，可先 `POOL_READY_INDEX=0` 跑一版、以 shadow/實測驗證後再開。
</content>
</invoke>
