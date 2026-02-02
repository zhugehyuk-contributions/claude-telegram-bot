use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use teloxide::{dispatching::Dispatcher, dptree, prelude::*};

use tokio::sync::{Mutex, OwnedMutexGuard};

use ctb_core::messaging::throttled::{ThrottleConfig, ThrottledMessenger};
use ctb_core::{
    config::Config, messaging::port::MessagingPort, scheduler::CronScheduler,
    security::RateLimiter, session::ClaudeSession, usage::UsageService, utils::AuditLogger,
};
use ctb_core::{
    domain::ChatId,
    formatting::{convert_markdown_to_html, escape_html},
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
    let resumed = match session.resume_last().await {
        Ok((true, msg)) => {
            println!("Auto-resumed: {msg}");
            true
        }
        Ok((false, _)) => {
            println!("No previous session to resume");
            false
        }
        Err(e) => {
            eprintln!("Failed to resume previous session: {e}");
            false
        }
    };

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

    // Send startup notification (best-effort) to the first allowed user (parity with TS).
    if !cfg.telegram_allowed_users.is_empty() {
        let cfg = cfg.clone();
        let session = session.clone();
        let messenger = messenger.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if let Err(e) = send_startup_notification(cfg, session, messenger, resumed).await {
                eprintln!("Startup notification failed: {e}");
            }
        });
    }

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

async fn send_startup_notification(
    cfg: Arc<Config>,
    session: Arc<ClaudeSession>,
    messenger: Arc<dyn MessagingPort>,
    resumed: bool,
) -> anyhow::Result<()> {
    let Some(&user_id) = cfg.telegram_allowed_users.first() else {
        return Ok(());
    };
    let chat_id = ChatId(user_id);

    // PRIORITY 1: .last-save-id auto-load mechanism.
    let save_id_file = cfg.claude_working_dir.join(".last-save-id");
    if save_id_file.exists() {
        match try_auto_load(session.clone(), messenger.clone(), chat_id, &save_id_file).await {
            Ok(true) => return Ok(()), // Successfully restored; skip normal startup message.
            Ok(false) => {}            // Not restored; fall through.
            Err(e) => {
                let msg = format!(
                    "🚨 <b>Auto-load Failed</b>\n\nError: <code>{}</code>\n\n⚠️ Starting fresh session. Check logs for recovery.",
                    escape_html(&sanitize_error(&cfg, &e.to_string()))
                );
                let _ = messenger.send_html(chat_id, &msg).await;
            }
        }
    }

    // PRIORITY 2: saved restart context (manual save-and-restart.sh / SIGTERM handler parity).
    let save_dir = cfg.claude_working_dir.join("docs/tasks/save");
    let mut context_message = String::new();
    if let Some((name, content)) = latest_restart_context(&save_dir) {
        context_message = format!("\n\n📋 **Saved Context Found:**\n{name}\n\n{content}");
    }

    // Determine startup type label (visible to user).
    let startup_type_md = if !context_message.is_empty() {
        "🔄 **SIGTERM Restart** (graceful shutdown via make up)"
    } else if resumed {
        "♻️ **Session Resumed** (no saved context found)"
    } else {
        "🆕 **Fresh Start** (new session)"
    };

    let mut header_md = startup_type_md.to_string();
    if let Some(s) = session.stats().await.session {
        header_md.push_str(&format!(
            "\nSession: `{}`",
            s.id.chars().take(8).collect::<String>()
        ));
    }
    let _ = messenger
        .send_html(chat_id, &convert_markdown_to_html(&header_md))
        .await;

    let prompt = if resumed {
        format!("{startup_type_md}\n\nBot restarted.\n\n현재 시간과 함께 간단히 상태를 알려주세요.{context_message}")
    } else {
        format!("{startup_type_md}\n\nBot restarted. New session starting.\n\n현재 시간과 함께 간단한 인사말을 써주세요.{context_message}")
    };

    let _ = session
        .send_message_to_chat(chat_id, &prompt, messenger.clone())
        .await;

    Ok(())
}

async fn try_auto_load(
    session: Arc<ClaudeSession>,
    messenger: Arc<dyn MessagingPort>,
    chat_id: ChatId,
    save_id_file: &Path,
) -> anyhow::Result<bool> {
    let save_id = std::fs::read_to_string(save_id_file)
        .unwrap_or_default()
        .trim()
        .to_string();

    if !is_valid_save_id(&save_id) {
        let _ = std::fs::remove_file(save_id_file);
        return Err(anyhow::anyhow!("Invalid save ID format: {save_id}"));
    }

    let notice = format!(
        "🔄 <b>Auto-restoring context</b>\n\nSave ID: <code>{}</code>\n\nExecuting /load...",
        escape_html(&save_id)
    );
    let _ = messenger.send_html(chat_id, &notice).await;

    let load_prompt = format!("Skill tool with skill='oh-my-claude:load' and args='{save_id}'");
    let out = session
        .send_message_to_chat(chat_id, &load_prompt, messenger.clone())
        .await
        .map_err(|e| anyhow::anyhow!("load failed: {e}"))?;

    if !out.text.contains("Loaded Context:") {
        return Err(anyhow::anyhow!(
            "load validation failed for save ID {save_id}: missing 'Loaded Context:'"
        ));
    }

    session.mark_restored().await;
    let _ = std::fs::remove_file(save_id_file);

    let ok_msg = format!(
        "✅ <b>Context Restored</b>\n\nResumed from save: <code>{}</code>",
        escape_html(&save_id)
    );
    let _ = messenger.send_html(chat_id, &ok_msg).await;

    Ok(true)
}

fn is_valid_save_id(s: &str) -> bool {
    if s.len() != 15 {
        return false;
    }
    let bytes = s.as_bytes();
    if bytes[8] != b'_' {
        return false;
    }
    bytes[..8].iter().all(|b| b.is_ascii_digit()) && bytes[9..].iter().all(|b| b.is_ascii_digit())
}

fn latest_restart_context(dir: &Path) -> Option<(String, String)> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut best_name: Option<String> = None;
    let mut best_path: Option<PathBuf> = None;

    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().to_string();
        if !name.starts_with("restart-context-") || !name.ends_with(".md") {
            continue;
        }
        if best_name.as_ref().map(|b| &name > b).unwrap_or(true) {
            best_name = Some(name);
            best_path = Some(ent.path());
        }
    }

    let name = best_name?;
    let path = best_path?;
    let content = std::fs::read_to_string(path).ok()?;
    Some((name, content))
}

fn sanitize_error(cfg: &Config, s: &str) -> String {
    let mut out = s.to_string();
    if let Ok(home) = std::env::var("HOME") {
        out = out.replace(&home, "~");
    }
    if let Some(wd) = cfg.claude_working_dir.to_str() {
        out = out.replace(wd, "~");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_id_validation() {
        assert!(is_valid_save_id("20260202_123456"));
        assert!(!is_valid_save_id("20260202-123456"));
        assert!(!is_valid_save_id("2026020_123456"));
        assert!(!is_valid_save_id("aaaaaaaa_bbbbbb"));
    }

    #[test]
    fn picks_latest_restart_context_by_filename() {
        let root = std::path::PathBuf::from(format!("/tmp/ctb-rc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let a = root.join("restart-context-2026-01-01T00-00-00.md");
        let b = root.join("restart-context-2026-02-01T00-00-00.md");
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();

        let (name, content) = latest_restart_context(&root).unwrap();
        assert_eq!(name, "restart-context-2026-02-01T00-00-00.md");
        assert_eq!(content, "b");

        let _ = std::fs::remove_dir_all(&root);
    }
}
