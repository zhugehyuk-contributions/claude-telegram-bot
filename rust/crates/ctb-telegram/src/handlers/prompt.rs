use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;

use teloxide::{prelude::*, types::ChatAction};

use ctb_core::{
    domain::{ChatId, MessageId, MessageRef, UserId},
    errors::Error,
    formatting::convert_markdown_to_html,
    messaging::port::MessagingPort,
    messaging::types::{ChatAction as PortChatAction, InlineKeyboard, MessagingCapabilities},
    utils::{add_timestamp, AuditEvent},
    Result,
};

use crate::router::AppState;

#[derive(Clone)]
pub struct PromptContext {
    pub bot: Bot,
    pub state: Arc<AppState>,
    pub chat_id: i64,
    pub user_id: i64,
    pub username: String,
}

#[derive(Clone, Copy, Debug)]
pub struct PromptOptions {
    pub record_last_message: bool,
    pub skip_rate_limit: bool,
}

fn is_claude_crash(err: &ctb_core::Error) -> bool {
    match err {
        Error::External(s) => s.contains("exited with status") || s.contains("exited with code"),
        _ => false,
    }
}

fn is_cancel_error(err: &ctb_core::Error) -> bool {
    match err {
        Error::External(s) => {
            let lower = s.to_lowercase();
            lower.contains("cancel") || lower.contains("abort")
        }
        _ => false,
    }
}

pub async fn run_prompt(
    ctx: PromptContext,
    message_type: &str,
    text: String,
    opts: PromptOptions,
) -> ResponseResult<()> {
    let PromptContext {
        bot,
        state,
        chat_id,
        user_id,
        username,
    } = ctx;

    if text.trim().is_empty() {
        return Ok(());
    }

    if !opts.skip_rate_limit {
        // Rate limit before heavy work.
        let mut rl = state.rate_limiter.lock().await;
        let (ok, retry_after) = rl.check(UserId(user_id));
        if !ok {
            let retry = retry_after.unwrap_or_default().as_secs_f64();
            if let Err(e) = state
                .audit
                .write(AuditEvent::rate_limit(user_id, &username, retry))
            {
                eprintln!("[AUDIT] Failed to write rate_limit event: {e}");
            }
            let _ = bot
                .send_message(
                    teloxide::types::ChatId(chat_id),
                    format!("⏳ Rate limited. Please wait {:.1} seconds.", retry),
                )
                .await;
            return Ok(());
        }
    }

    if opts.record_last_message {
        state.session.set_last_message(text.clone()).await;
    }
    let prompt = add_timestamp(&text);

    // Typing loop (best-effort).
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    let bot_for_typing = bot.clone();
    let chat_for_typing = teloxide::types::ChatId(chat_id);
    let typing_task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3));
        loop {
            tokio::select! {
              _ = tick.tick() => {
                let _ = bot_for_typing.send_chat_action(chat_for_typing, ChatAction::Typing).await;
              }
              _ = &mut stop_rx => break,
            }
        }
    });

    let messenger: Arc<dyn MessagingPort> = state.messenger.clone();

    const MAX_RETRIES: usize = 1;
    for attempt in 0..=MAX_RETRIES {
        let result = state
            .session
            .send_message_to_chat(ChatId(chat_id), &prompt, messenger.clone())
            .await;

        match result {
            Ok(out) => {
                if let Err(e) = state.audit.write(AuditEvent::message(
                    user_id,
                    &username,
                    message_type,
                    &text,
                    Some(&out.text),
                )) {
                    eprintln!("[AUDIT] Failed to write message event: {e}");
                }
                if !out.waiting_for_user {
                    let _ = state.scheduler.process_queued_jobs().await;
                }

                // Context-limit warning + auto-save (parity with TS).
                if state.session.needs_save().await {
                    if let Err(e) = handle_context_limit_autosave(
                        state.clone(),
                        ChatId(chat_id),
                        user_id,
                        &username,
                        messenger.clone(),
                    )
                    .await
                    {
                        eprintln!("[AUTO_SAVE] failed: {e}");
                        let sanitized = sanitize_error(&state, &e.to_string());
                        let truncated = sanitized.chars().take(300).collect::<String>();
                        let msg = format!(
                            "🚨 **CRITICAL: Auto-Save Failed**\n\nError: `{}`\n\n⚠️ **YOUR WORK IS NOT SAVED**\n\nDo NOT restart. Try manual: /oh-my-claude:save",
                            truncated
                        );
                        let _ = messenger
                            .send_html(ChatId(chat_id), &convert_markdown_to_html(&msg))
                            .await;
                    }
                }
                break;
            }
            Err(err) => {
                if is_claude_crash(&err) && attempt < MAX_RETRIES {
                    let _ = state.session.kill().await;
                    let _ = bot
                        .send_message(
                            teloxide::types::ChatId(chat_id),
                            "⚠️ Claude crashed, retrying...",
                        )
                        .await;
                    continue;
                }

                if is_cancel_error(&err) {
                    let was_interrupt = state.session.consume_interrupt_flag().await;
                    if !was_interrupt {
                        let _ = bot
                            .send_message(teloxide::types::ChatId(chat_id), "🛑 Query stopped.")
                            .await;
                    }
                    break;
                }

                let msg_txt = format!("{err}");
                let truncated = if msg_txt.len() > 200 {
                    format!("{}...", msg_txt.chars().take(200).collect::<String>())
                } else {
                    msg_txt
                };
                let _ = bot
                    .send_message(
                        teloxide::types::ChatId(chat_id),
                        format!("❌ Error: {truncated}"),
                    )
                    .await;
                if let Err(e) = state.audit.write(AuditEvent::error(
                    user_id,
                    &username,
                    &truncated,
                    Some(message_type),
                )) {
                    eprintln!("[AUDIT] Failed to write error event: {e}");
                }
                break;
            }
        }
    }

    let _ = stop_tx.send(());
    let _ = typing_task.await;

    Ok(())
}

pub async fn run_text_prompt(
    ctx: PromptContext,
    message_type: &str,
    text: String,
) -> ResponseResult<()> {
    run_prompt(
        ctx,
        message_type,
        text,
        PromptOptions {
            record_last_message: true,
            skip_rate_limit: false,
        },
    )
    .await
}

async fn handle_context_limit_autosave(
    state: Arc<AppState>,
    chat_id: ChatId,
    user_id: i64,
    username: &str,
    messenger: Arc<dyn MessagingPort>,
) -> anyhow::Result<()> {
    // If a save id is already present, don't spam /save again.
    let save_id_file = state.cfg.claude_working_dir.join(".last-save-id");
    if let Ok(existing) = std::fs::read_to_string(&save_id_file) {
        let id = existing.trim();
        if crate::router::is_valid_save_id(id) {
            let msg = format!(
                "✅ **Context Already Saved**\n\nSave ID: `{}`\n\nPlease run: `make up` to restart with restored context.",
                id
            );
            let _ = messenger
                .send_html(chat_id, &convert_markdown_to_html(&msg))
                .await;
            return Ok(());
        }
        // Malformed file: remove so we can try saving again.
        let _ = std::fs::remove_file(&save_id_file);
    }

    let current = state.session.current_context_tokens().await;
    let percentage = ((current as f64 / 200_000f64) * 100.0).min(999.9);
    let warn = format!(
        "⚠️ **Context Limit Approaching**\n\nCurrent: {} / 200,000 tokens ({:.1}%)\n\nInitiating automatic save...",
        current,
        percentage
    );
    let _ = messenger
        .send_html(chat_id, &convert_markdown_to_html(&warn))
        .await;

    let silent: Arc<dyn MessagingPort> = Arc::new(SuppressedMessenger::new(messenger.clone()));
    let save_prompt = "Context limit reached. Execute: Skill tool with skill='oh-my-claude:save'";

    let out = state
        .session
        .send_message_to_chat(chat_id, save_prompt, silent)
        .await?;

    let Some(save_id) = parse_save_id(&out.text) else {
        let snippet = out.text.chars().take(200).collect::<String>();
        let msg = format!(
            "⚠️ Save completed but couldn't parse save ID.\n\nResponse: `{}`",
            snippet
        );
        let _ = messenger
            .send_html(chat_id, &convert_markdown_to_html(&msg))
            .await;
        return Ok(());
    };

    if !crate::router::is_valid_save_id(&save_id) {
        return Err(anyhow::anyhow!(
            "Invalid save ID format from /save: {save_id}"
        ));
    }

    std::fs::write(&save_id_file, &save_id)?;
    let written = std::fs::read_to_string(&save_id_file)
        .unwrap_or_default()
        .trim()
        .to_string();
    if written != save_id {
        return Err(anyhow::anyhow!(
            "Failed to persist save id - file not written correctly"
        ));
    }

    let ok = format!(
        "✅ **Context Saved**\n\nSave ID: `{}`\n\nPlease run: `make up` to restart with restored context.",
        save_id
    );
    let _ = messenger
        .send_html(chat_id, &convert_markdown_to_html(&ok))
        .await;

    if let Err(e) = state.audit.write(AuditEvent::message(
        user_id,
        username,
        "AUTO_SAVE",
        save_prompt,
        Some(&format!("save_id={save_id}")),
    )) {
        eprintln!("[AUDIT] Failed to write auto_save event: {e}");
    }

    Ok(())
}

fn parse_save_id(text: &str) -> Option<String> {
    for marker in ["/docs/tasks/save/", "docs/tasks/save/"] {
        if let Some(i) = text.find(marker) {
            let rest = &text[i + marker.len()..];
            let candidate = rest.chars().take(15).collect::<String>();
            if crate::router::is_valid_save_id(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn sanitize_error(state: &AppState, s: &str) -> String {
    let mut out = s.to_string();
    if let Ok(home) = std::env::var("HOME") {
        out = out.replace(&home, "~");
    }
    if let Some(wd) = state.cfg.claude_working_dir.to_str() {
        out = out.replace(wd, "~");
    }
    out
}

// === MessagingPort decorator used for auto-save (no streaming spam) ===

struct SuppressedMessenger {
    real: Arc<dyn MessagingPort>,
    next_id: AtomicI32,
}

impl SuppressedMessenger {
    fn new(real: Arc<dyn MessagingPort>) -> Self {
        Self {
            real,
            next_id: AtomicI32::new(1),
        }
    }

    fn alloc(&self, chat_id: ChatId) -> MessageRef {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        MessageRef {
            chat_id,
            message_id: MessageId(id),
        }
    }
}

#[async_trait::async_trait]
impl MessagingPort for SuppressedMessenger {
    fn capabilities(&self) -> MessagingCapabilities {
        self.real.capabilities()
    }

    async fn send_html(&self, chat_id: ChatId, _html: &str) -> Result<MessageRef> {
        Ok(self.alloc(chat_id))
    }

    async fn edit_html(&self, _msg: MessageRef, _html: &str) -> Result<()> {
        Ok(())
    }

    async fn delete_message(&self, _msg: MessageRef) -> Result<()> {
        Ok(())
    }

    async fn send_chat_action(&self, _chat_id: ChatId, _action: PortChatAction) -> Result<()> {
        Ok(())
    }

    async fn set_reaction(&self, _msg: MessageRef, _emoji: &str) -> Result<()> {
        Ok(())
    }

    async fn send_inline_keyboard(
        &self,
        chat_id: ChatId,
        text: &str,
        keyboard: InlineKeyboard,
    ) -> Result<MessageRef> {
        self.real
            .send_inline_keyboard(chat_id, text, keyboard)
            .await
    }

    async fn answer_callback_query(&self, callback_id: &str, text: Option<&str>) -> Result<()> {
        self.real.answer_callback_query(callback_id, text).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_save_id_from_response() {
        let txt = "Saved to: /docs/tasks/save/20260202_123456/";
        assert_eq!(parse_save_id(txt), Some("20260202_123456".to_string()));
    }
}
