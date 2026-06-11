//! chat_id 預熱池，對應 Python `services/chat_id_pool.py`。
//! 預先建立 chat_id 規避上游 /chats/new 握手 (0.5~6s，經風控代理更慢)。
//!
//! Pillar 1（dev/LATENCY.md）：消費即回補（consume-triggered refill）。
//! 舊版只靠背景迴圈每 30s「每帳號補 1 個、僅覆蓋前 N 帳號」，穩態供給上限 ≈ 0.27 個/秒，
//! 任何實際負載都會被消費掏空 → 命中率趨近 0。改為：acquire 命中/未命中都立即 spawn 回補到 target，
//! 供給速率=消費速率、且自動跟隨「熱帳號」（被 acquire 選到的帳號才補）。背景迴圈退化為 bootstrap + GC。
//!
//! ⚠️ 回補必須使用呼叫端（executor）傳入的 token，**絕不**在熱路徑呼叫 pool.token_of()
//! （那會重新取得 state Mutex 並對上萬帳號做 O(n) 查找，正好抵銷 Pillar 2 消除的鎖競爭）。

use super::client::QwenClient;
use crate::account::AccountPool;
use crate::util::{now_secs, now_unix};
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

const WAF_PREWARM_PAUSE_SECONDS: i64 = 600;

struct Entry {
    chat_id: String,
    created: f64,
}

/// 受同一把鎖保護：cache（每帳號預熱會話佇列）+ pending（每帳號進行中的回補配額）。
/// 合一是為了讓「size+pending < target」的檢查與佔位原子化，避免並發回補過量。
#[derive(Default)]
struct Inner {
    cache: HashMap<String, VecDeque<Entry>>,
    pending: HashMap<String, usize>,
}

pub struct ChatIdPool {
    client: Arc<QwenClient>,
    pool: Arc<AccountPool>,
    inner: Mutex<Inner>,
    target: AtomicUsize,
    ttl: AtomicU64,
    max_accounts: AtomicUsize,
    waf_pause_until: AtomicU64,
    default_model: String,
    running: AtomicBool,
}

impl ChatIdPool {
    pub fn new(
        client: Arc<QwenClient>,
        pool: Arc<AccountPool>,
        target: usize,
        ttl_seconds: u64,
        max_accounts: usize,
        default_model: String,
    ) -> Arc<Self> {
        Arc::new(ChatIdPool {
            client,
            pool,
            inner: Mutex::new(Inner::default()),
            target: AtomicUsize::new(target),
            ttl: AtomicU64::new(ttl_seconds),
            max_accounts: AtomicUsize::new(max_accounts),
            waf_pause_until: AtomicU64::new(0),
            default_model,
            running: AtomicBool::new(false),
        })
    }

    pub fn target(&self) -> usize {
        self.target.load(Ordering::Relaxed)
    }
    pub fn ttl(&self) -> u64 {
        self.ttl.load(Ordering::Relaxed)
    }

    pub async fn apply_config(&self, target: Option<usize>, ttl_seconds: Option<u64>) {
        if let Some(t) = target {
            self.target.store(t, Ordering::Relaxed);
        }
        if let Some(t) = ttl_seconds {
            self.ttl.store(t, Ordering::Relaxed);
        }
    }

    pub fn pause_for_waf(&self, source: &str, err: &str) {
        let until = now_unix().saturating_add(WAF_PREWARM_PAUSE_SECONDS) as u64;
        let previous = self.waf_pause_until.swap(until, Ordering::Relaxed);
        if previous < until {
            tracing::warn!(
                "[ChatIdPool] 命中 WAF/滑动验证，暂停预热 {} 秒 source={source} err={err}",
                WAF_PREWARM_PAUSE_SECONDS
            );
        }
    }

    fn is_waf_paused(&self) -> bool {
        let until = self.waf_pause_until.load(Ordering::Relaxed);
        until > 0 && (now_unix() as u64) < until
    }

    /// 取一個未過期的預熱 chat_id（pop）；過期的丟棄並背景刪除（用傳入 token）。
    /// 取用後立即 spawn 回補（命中與未命中皆補）以維持水位、跟隨熱帳號。
    pub async fn acquire(
        self: &Arc<Self>,
        email: &str,
        token: &str,
        _model: &str,
    ) -> Option<String> {
        let ttl = self.ttl() as f64;
        let now = now_secs();
        let mut expired: Vec<String> = Vec::new();
        let result = {
            let mut inner = self.inner.lock().await;
            match inner.cache.get_mut(email) {
                Some(dq) => {
                    let mut chosen = None;
                    while let Some(e) = dq.pop_front() {
                        if now - e.created <= ttl {
                            chosen = Some(e.chat_id);
                            break;
                        } else {
                            expired.push(e.chat_id);
                        }
                    }
                    if dq.is_empty() {
                        inner.cache.remove(email);
                    }
                    chosen
                }
                None => None,
            }
        };

        // 背景刪除過期會話（用傳入 token，不查帳號池）
        if !expired.is_empty() && !token.is_empty() {
            let client = self.client.clone();
            let token = token.to_string();
            tokio::spawn(async move {
                for cid in expired {
                    client.delete_chat(&token, &cid).await;
                }
            });
        }

        // 即時回補（消費即補；未命中亦補以暖機新熱帳號）
        self.spawn_refill(email.to_string(), token.to_string());

        result
    }

    /// 對指定帳號補滿至 target（背景任務）。以 pending 佔位避免並發過量；用傳入 token，不查帳號池。
    fn spawn_refill(self: &Arc<Self>, email: String, token: String) {
        let target = self.target();
        if target == 0 || self.is_waf_paused() || email.is_empty() || token.is_empty() {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            // 原子計算缺口並佔位
            let need = {
                let mut inner = this.inner.lock().await;
                let size = inner.cache.get(&email).map(|d| d.len()).unwrap_or(0);
                let pending = *inner.pending.get(&email).unwrap_or(&0);
                let need = target.saturating_sub(size + pending);
                if need == 0 {
                    return;
                }
                *inner.pending.entry(email.clone()).or_insert(0) += need;
                need
            };
            // 建會話（await 期間不持鎖）
            for _ in 0..need {
                match this
                    .client
                    .create_chat(&token, &this.default_model, "t2t")
                    .await
                {
                    Ok(cid) => {
                        let mut inner = this.inner.lock().await;
                        inner
                            .cache
                            .entry(email.clone())
                            .or_default()
                            .push_back(Entry {
                                chat_id: cid,
                                created: now_secs(),
                            });
                    }
                    Err(e) => {
                        // WAF 是出口/节奏问题，后台预热继续打只会延长挑战状态。
                        let msg = e.to_string();
                        if is_waf_challenge_error(&msg) {
                            this.pause_for_waf("refill", &msg);
                        }
                        break;
                    }
                }
            }
            // 退回本次全部佔位（已建立者已進 cache，不再計 pending）
            let mut inner = this.inner.lock().await;
            if let Some(p) = inner.pending.get_mut(&email) {
                *p = p.saturating_sub(need);
                if *p == 0 {
                    inner.pending.remove(&email);
                }
            }
        });
    }

    pub async fn size(&self, email: &str) -> usize {
        self.inner
            .lock()
            .await
            .cache
            .get(email)
            .map(|d| d.len())
            .unwrap_or(0)
    }

    pub async fn total_size(&self) -> usize {
        self.inner
            .lock()
            .await
            .cache
            .values()
            .map(|d| d.len())
            .sum()
    }

    pub async fn per_account_sizes(&self) -> HashMap<String, usize> {
        self.inner
            .lock()
            .await
            .cache
            .iter()
            .map(|(k, v)| (k.clone(), v.len()))
            .collect()
    }

    /// 啟動背景補滿迴圈（bootstrap + GC）。
    pub fn start(self: &Arc<Self>) {
        if self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                if !this.running.load(Ordering::SeqCst) {
                    break;
                }
                this.refill_round().await;
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        });
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// 背景一輪：GC 過期會話 + bootstrap（對前 max_accounts 個帳號觸發回補到 target）。
    /// 注意：日常供給已由 acquire 的 consume-triggered refill 承擔，此處僅啟動暖機與淘汰。
    async fn refill_round(self: &Arc<Self>) {
        let target = self.target();
        if target == 0 || self.is_waf_paused() {
            return;
        }

        // 先淘汰過期：收集過期 (email, chat_id) 後背景刪除上游會話；順手移除空佇列。
        let ttl = self.ttl() as f64;
        let now = now_secs();
        let mut expired: Vec<(String, String)> = Vec::new();
        {
            let mut inner = self.inner.lock().await;
            inner.cache.retain(|email, dq| {
                dq.retain(|e| {
                    let alive = now - e.created <= ttl;
                    if !alive {
                        expired.push((email.clone(), e.chat_id.clone()));
                    }
                    alive
                });
                !dq.is_empty()
            });
        }
        for (email, cid) in expired {
            if let Some(token) = self.pool.token_of(&email).await {
                let client = self.client.clone();
                tokio::spawn(async move {
                    client.delete_chat(&token, &cid).await;
                });
            }
        }

        // bootstrap：只覆蓋前 N 個可用帳號（帳號可能上萬），對未滿者觸發回補。
        let max_accounts = self.max_accounts.load(Ordering::Relaxed);
        let mut accounts = self.pool.all_emails_tokens().await;
        accounts.truncate(max_accounts);
        for (email, token) in accounts {
            self.spawn_refill(email, token);
        }
    }
}

fn is_waf_challenge_error(err: &str) -> bool {
    let lower = err.to_lowercase();
    lower.contains("html/waf")
        || lower.contains("aliyun_waf")
        || lower.contains("captcha")
        || lower.contains("滑动验证")
        || lower.contains("challenge response")
}

#[cfg(test)]
mod tests {
    use super::is_waf_challenge_error;

    #[test]
    fn detects_waf_challenge_errors() {
        for err in [
            "create_chat HTTP 200 HTML/WAF challenge response",
            r#"<!doctype html><meta name="aliyun_waf_aa" content="x">"#,
            "captcha required",
        ] {
            assert!(is_waf_challenge_error(err), "应识别 WAF: {err}");
        }
    }
}
