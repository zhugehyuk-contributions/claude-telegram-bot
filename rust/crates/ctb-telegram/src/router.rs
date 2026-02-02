use std::{collections::HashMap, sync::Arc};

use teloxide::{dispatching::Dispatcher, dptree, prelude::*};

use tokio::sync::{Mutex, OwnedMutexGuard};

use ctb_core::messaging::throttled::{ThrottleConfig, ThrottledMessenger};
use ctb_core::{
    config::Config, messaging::port::MessagingPort, scheduler::CronScheduler,
    security::RateLimiter, session::ClaudeSession, usage::UsageService, utils::AuditLogger,
};

use crate::handlers;
use crate::TelegramMessenger;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub session: Arc<ClaudeSession>,
    pub messenger: Arc<dyn MessagingPort>,
    pub scheduler: Arc<CronScheduler>,
    pub usage: Arc<UsageService>,
    pub rate_limiter: Arc<Mutex<RateLimiter>>,
    pub chat_locks: Arc<ChatLocks>,
    pub audit: Arc<AuditLogger>,
}

#[derive(Default)]
pub struct ChatLocks {
    inner: Mutex<HashMap<i64, Arc<Mutex<()>>>>,
}

impl ChatLocks {
    pub async fn lock_chat(&self, chat_id: i64) -> OwnedMutexGuard<()> {
        let lock = {
            let mut map = self.inner.lock().await;
            map.entry(chat_id)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }
}

pub async fn run_polling(cfg: Arc<Config>, session: Arc<ClaudeSession>) -> anyhow::Result<()> {
    let bot = Bot::new(cfg.telegram_bot_token.clone());

    // Basic startup info.
    if let Ok(me) = bot.get_me().await {
        println!("ctb (Rust) started: @{}", me.username());
    }
    println!("Working directory: {}", cfg.claude_working_dir.display());
    println!("Allowed users: {}", cfg.telegram_allowed_users.len());

    // Auto-resume previous session if available (parity with TS).
    match session.resume_last().await {
        Ok((true, msg)) => println!("Auto-resumed: {msg}"),
        Ok((false, _)) => println!("No previous session to resume"),
        Err(e) => eprintln!("Failed to resume previous session: {e}"),
    }

    // If we were restarted via a command, update the "restarting..." message.
    // Data format matches TS: { chat_id, message_id, timestamp }.
    if cfg.restart_file.exists() {
        #[derive(serde::Deserialize)]
        struct RestartData {
            chat_id: i64,
            message_id: i32,
            timestamp: u64,
        }

        let cleanup = || {
            let _ = std::fs::remove_file(&cfg.restart_file);
        };

        match std::fs::read_to_string(&cfg.restart_file)
            .ok()
            .and_then(|txt| serde_json::from_str::<RestartData>(&txt).ok())
        {
            Some(data) => {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let age = now_ms.saturating_sub(data.timestamp);
                if age < 30_000 {
                    let _ = bot
                        .edit_message_text(
                            teloxide::types::ChatId(data.chat_id),
                            teloxide::types::MessageId(data.message_id),
                            "✅ Bot restarted",
                        )
                        .await;
                }
                cleanup();
            }
            None => cleanup(),
        }
    }

    // Wrap the raw Telegram messenger with a throttling decorator to reduce 429s for streaming-heavy
    // workloads. We still keep a 429 RetryAfter retry at the Telegram adapter layer.
    let raw_messenger: Arc<dyn MessagingPort> = Arc::new(TelegramMessenger::new(bot.clone()));
    let messenger: Arc<dyn MessagingPort> = Arc::new(ThrottledMessenger::new(
        raw_messenger,
        ThrottleConfig::default(),
    ));
    let scheduler = Arc::new(CronScheduler::new(
        cfg.clone(),
        session.clone(),
        messenger.clone(),
    ));
    if let Err(e) = scheduler.start().await {
        eprintln!("[CRON] Failed to start scheduler: {e}");
    }
    scheduler.ensure_watcher().await;
    let usage = Arc::new(UsageService::new());
    let state = Arc::new(AppState {
        cfg: cfg.clone(),
        session,
        messenger,
        scheduler,
        usage,
        rate_limiter: Arc::new(Mutex::new(RateLimiter::new(
            cfg.rate_limit_enabled,
            cfg.rate_limit_requests,
            cfg.rate_limit_window,
        ))),
        chat_locks: Arc::new(ChatLocks::default()),
        audit: Arc::new(AuditLogger::new(
            cfg.audit_log_path.clone(),
            cfg.audit_log_json,
        )),
    });

    let handler = dptree::entry()
        .branch(Update::filter_callback_query().endpoint(handlers::handle_callback))
        .branch(Update::filter_message().endpoint(handlers::handle_message));

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state])
        .build()
        .dispatch()
        .await;

    Ok(())
}
