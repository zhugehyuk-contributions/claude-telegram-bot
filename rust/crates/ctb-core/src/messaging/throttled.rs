use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::sync::Mutex;
use tokio::time::{sleep, Instant};

use crate::{
    domain::{ChatId, MessageRef},
    messaging::{
        port::MessagingPort,
        types::{ChatAction, InlineKeyboard, MessagingCapabilities},
    },
    Result,
};

#[derive(Clone, Copy, Debug)]
pub struct ThrottleConfig {
    /// Minimum spacing between *any* Telegram API calls (global flood control).
    pub global_min_interval: Duration,
    /// Minimum spacing between calls per chat (Telegram 1 msg/sec style limits).
    pub per_chat_min_interval: Duration,
}

impl Default for ThrottleConfig {
    fn default() -> Self {
        // Mirrors TS defaults in src/index.ts (conservative).
        Self {
            global_min_interval: Duration::from_millis(40), // ~25/sec
            per_chat_min_interval: Duration::from_millis(1050), // ~0.95/sec
        }
    }
}

#[derive(Debug)]
struct IntervalLimiter {
    interval: Duration,
    next: Instant,
}

impl IntervalLimiter {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            next: Instant::now(),
        }
    }

    /// Reserve the next slot and return the wait duration required before executing.
    fn reserve(&mut self) -> Duration {
        let now = Instant::now();
        let start = if now >= self.next { now } else { self.next };
        self.next = start + self.interval;
        start.saturating_duration_since(now)
    }
}

/// MessagingPort decorator that rate-limits outbound calls.
///
/// This is a best-effort defense against Telegram 429 errors, primarily for streaming/edit-heavy
/// workloads. It does not guarantee zero 429s, but it should drastically reduce them.
pub struct ThrottledMessenger {
    inner: Arc<dyn MessagingPort>,
    cfg: ThrottleConfig,
    global: Mutex<IntervalLimiter>,
    per_chat: Mutex<HashMap<i64, Arc<Mutex<IntervalLimiter>>>>,
}

impl ThrottledMessenger {
    pub fn new(inner: Arc<dyn MessagingPort>, cfg: ThrottleConfig) -> Self {
        Self {
            inner,
            cfg,
            global: Mutex::new(IntervalLimiter::new(cfg.global_min_interval)),
            per_chat: Mutex::new(HashMap::new()),
        }
    }

    async fn limiter_for_chat(&self, chat_id: i64) -> Arc<Mutex<IntervalLimiter>> {
        let mut map = self.per_chat.lock().await;
        map.entry(chat_id)
            .or_insert_with(|| {
                Arc::new(Mutex::new(IntervalLimiter::new(
                    self.cfg.per_chat_min_interval,
                )))
            })
            .clone()
    }

    async fn throttle_chat(&self, chat_id: i64) {
        let global_wait = { self.global.lock().await.reserve() };
        let chat_wait = {
            let lim = self.limiter_for_chat(chat_id).await;
            let mut guard = lim.lock().await;
            guard.reserve()
        };

        let wait = if global_wait > chat_wait {
            global_wait
        } else {
            chat_wait
        };
        if wait > Duration::from_millis(0) {
            sleep(wait).await;
        }
    }

    async fn throttle_global(&self) {
        let wait = { self.global.lock().await.reserve() };
        if wait > Duration::from_millis(0) {
            sleep(wait).await;
        }
    }
}

#[async_trait::async_trait]
impl MessagingPort for ThrottledMessenger {
    fn capabilities(&self) -> MessagingCapabilities {
        self.inner.capabilities()
    }

    async fn send_html(&self, chat_id: ChatId, html: &str) -> Result<MessageRef> {
        self.throttle_chat(chat_id.0).await;
        self.inner.send_html(chat_id, html).await
    }

    async fn edit_html(&self, msg: MessageRef, html: &str) -> Result<()> {
        self.throttle_chat(msg.chat_id.0).await;
        self.inner.edit_html(msg, html).await
    }

    async fn delete_message(&self, msg: MessageRef) -> Result<()> {
        self.throttle_chat(msg.chat_id.0).await;
        self.inner.delete_message(msg).await
    }

    async fn send_chat_action(&self, chat_id: ChatId, action: ChatAction) -> Result<()> {
        self.throttle_chat(chat_id.0).await;
        self.inner.send_chat_action(chat_id, action).await
    }

    async fn set_reaction(&self, msg: MessageRef, emoji: &str) -> Result<()> {
        self.throttle_chat(msg.chat_id.0).await;
        self.inner.set_reaction(msg, emoji).await
    }

    async fn send_inline_keyboard(
        &self,
        chat_id: ChatId,
        text: &str,
        keyboard: InlineKeyboard,
    ) -> Result<MessageRef> {
        self.throttle_chat(chat_id.0).await;
        self.inner
            .send_inline_keyboard(chat_id, text, keyboard)
            .await
    }

    async fn answer_callback_query(&self, callback_id: &str, text: Option<&str>) -> Result<()> {
        // No chat_id available here; apply global throttling only.
        self.throttle_global().await;
        self.inner.answer_callback_query(callback_id, text).await
    }
}
