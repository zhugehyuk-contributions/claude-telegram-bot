use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use teloxide::prelude::*;
use tokio_util::sync::CancellationToken;

use ctb_core::{domain::ChatId, messaging::port::MessagingPort, utils::AuditEvent};

use crate::router::AppState;

use super::prompt::PromptContext;

pub struct MediaGroupConfig {
    pub emoji: &'static str,
    pub item_label_plural: &'static str,
}

pub type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type ProcessFn = Arc<dyn Fn(PromptContext, Vec<String>, Option<String>) -> BoxFuture + Send + Sync>;

struct PendingGroup {
    items: Vec<String>,
    caption: Option<String>,
    user_id: i64,
    username: String,
    chat_id: i64,
    status_msg: ctb_core::domain::MessageRef,
    cancel: CancellationToken,
}

pub struct MediaGroupBuffer {
    cfg: MediaGroupConfig,
    process: ProcessFn,
    pending: tokio::sync::Mutex<HashMap<String, PendingGroup>>,
}

impl MediaGroupBuffer {
    pub fn new(cfg: MediaGroupConfig, process: ProcessFn) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            process,
            pending: tokio::sync::Mutex::new(HashMap::new()),
        })
    }

    pub async fn add_to_group(
        self: &Arc<Self>,
        ctx: PromptContext,
        media_group_id: String,
        item_path: String,
        caption: Option<String>,
        timeout: Duration,
    ) -> bool {
        let PromptContext {
            bot,
            state,
            chat_id,
            user_id,
            username,
        } = ctx;

        let mut map = self.pending.lock().await;
        if !map.contains_key(&media_group_id) {
            // Rate limit on first item only (parity with TS).
            {
                let mut rl = state.rate_limiter.lock().await;
                let (ok, retry_after) = rl.check(ctb_core::domain::UserId(user_id));
                if !ok {
                    let retry = retry_after.unwrap_or_default().as_secs_f64();
                    let _ = state
                        .audit
                        .write(AuditEvent::rate_limit(user_id, &username, retry));
                    let _ = bot
                        .send_message(
                            teloxide::types::ChatId(chat_id),
                            format!("⏳ Rate limited. Please wait {:.1} seconds.", retry),
                        )
                        .await;
                    return false;
                }
            }

            let status = format!(
                "{} Receiving {}...",
                self.cfg.emoji, self.cfg.item_label_plural
            );
            let status_msg = match state.messenger.send_html(ChatId(chat_id), &status).await {
                Ok(m) => m,
                Err(_) => ctb_core::domain::MessageRef {
                    chat_id: ChatId(chat_id),
                    message_id: ctb_core::domain::MessageId(0),
                },
            };

            let cancel = CancellationToken::new();
            map.insert(
                media_group_id.clone(),
                PendingGroup {
                    items: vec![item_path],
                    caption,
                    user_id,
                    username,
                    chat_id,
                    status_msg,
                    cancel: cancel.clone(),
                },
            );

            drop(map);
            self.spawn_timer(bot, state, media_group_id, cancel, timeout);
            return true;
        }

        // Existing group: push and reset timeout.
        let group = map.get_mut(&media_group_id).expect("group exists");
        group.items.push(item_path);
        if group.caption.is_none() && caption.is_some() {
            group.caption = caption;
        }

        group.cancel.cancel();
        let cancel = CancellationToken::new();
        group.cancel = cancel.clone();
        drop(map);
        self.spawn_timer(bot, state, media_group_id, cancel, timeout);
        true
    }

    fn spawn_timer(
        self: &Arc<Self>,
        bot: Bot,
        state: Arc<AppState>,
        media_group_id: String,
        cancel: CancellationToken,
        timeout: Duration,
    ) {
        let buffer = Arc::clone(self);
        tokio::spawn(async move {
            tokio::select! {
              _ = cancel.cancelled() => {}
              _ = tokio::time::sleep(timeout) => {
                buffer.process_group(bot, state, &media_group_id).await;
              }
            }
        });
    }

    async fn process_group(self: &Arc<Self>, bot: Bot, state: Arc<AppState>, media_group_id: &str) {
        let group = {
            let mut map = self.pending.lock().await;
            map.remove(media_group_id)
        };

        let Some(group) = group else {
            return;
        };

        let count = group.items.len();
        let status = format!(
            "{} Processing {} {}...",
            self.cfg.emoji, count, self.cfg.item_label_plural
        );
        let _ = state.messenger.edit_html(group.status_msg, &status).await;

        // Sequentialize per chat (parity with text handler lock).
        let _guard = state.chat_locks.lock_chat(group.chat_id).await;

        let ctx = PromptContext {
            bot,
            state: state.clone(),
            chat_id: group.chat_id,
            user_id: group.user_id,
            username: group.username,
        };
        (self.process)(ctx, group.items, group.caption).await;

        let _ = state.messenger.delete_message(group.status_msg).await;
    }
}
