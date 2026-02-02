use std::sync::Arc;

use teloxide::{prelude::*, types::ChatAction};

use ctb_core::{
    domain::{ChatId, UserId},
    errors::Error,
    messaging::port::MessagingPort,
    utils::{add_timestamp, AuditEvent},
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
            let _ = state
                .audit
                .write(AuditEvent::rate_limit(user_id, &username, retry));
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
                let _ = state.audit.write(AuditEvent::message(
                    user_id,
                    &username,
                    message_type,
                    &text,
                    Some(&out.text),
                ));
                if !out.waiting_for_user {
                    let _ = state.scheduler.process_queued_jobs().await;
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
                let _ = state.audit.write(AuditEvent::error(
                    user_id,
                    &username,
                    &truncated,
                    Some(message_type),
                ));
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
