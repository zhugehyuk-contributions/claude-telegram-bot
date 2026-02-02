use std::sync::Arc;

use teloxide::{prelude::*, types::ChatAction};

use ctb_core::{
    domain::{ChatId, UserId},
    errors::Error,
    messaging::port::MessagingPort,
    utils::AuditEvent,
};

use crate::router::AppState;

#[derive(serde::Deserialize)]
struct AskUserRequestFile {
    chat_id: Option<serde_json::Value>,
    options: Option<Vec<String>>,
}

fn parse_chat_id(v: &serde_json::Value) -> Option<i64> {
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    v.as_str().and_then(|s| s.parse::<i64>().ok())
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

pub async fn handle_callback(
    bot: Bot,
    q: CallbackQuery,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    let cb_id = q.id.clone();
    let user = q.from.clone();
    let chat_id = q.message.as_ref().map(|m| m.chat.id);
    let data = q.data.clone().unwrap_or_default();

    // Always answer callback query eventually.
    if chat_id.is_none() || data.is_empty() {
        let _ = bot.answer_callback_query(cb_id).await;
        return Ok(());
    }

    let chat_id = chat_id.unwrap();
    let user_id = user.id.0 as i64;
    let username = user
        .username
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    // Auth check.
    if !ctb_core::security::is_authorized(Some(UserId(user_id)), &state.cfg.telegram_allowed_users)
    {
        let _ = bot
            .answer_callback_query(cb_id)
            .text("Unauthorized".to_string())
            .await;
        return Ok(());
    }

    // Parse callback data: askuser:{request_id}:{option_index}
    if !data.starts_with("askuser:") {
        let _ = bot.answer_callback_query(cb_id).await;
        return Ok(());
    }

    let parts: Vec<&str> = data.split(':').collect();
    if parts.len() != 3 {
        let _ = bot
            .answer_callback_query(cb_id)
            .text("Invalid callback data".to_string())
            .await;
        return Ok(());
    }
    let request_id = parts[1];
    let option_index: usize = match parts[2].parse::<usize>() {
        Ok(v) => v,
        Err(_) => {
            let _ = bot
                .answer_callback_query(cb_id)
                .text("Invalid option".to_string())
                .await;
            return Ok(());
        }
    };

    // Load request file
    let request_file = format!("/tmp/ask-user-{request_id}.json");
    let request: AskUserRequestFile = match std::fs::read_to_string(&request_file)
        .ok()
        .and_then(|txt| serde_json::from_str(&txt).ok())
    {
        Some(v) => v,
        None => {
            let _ = bot
                .answer_callback_query(cb_id)
                .text("Request expired or invalid".to_string())
                .await;
            return Ok(());
        }
    };

    if let Some(chat_val) = request.chat_id.as_ref().and_then(parse_chat_id) {
        if chat_val != chat_id.0 {
            let _ = bot
                .answer_callback_query(cb_id)
                .text("Request expired or invalid".to_string())
                .await;
            return Ok(());
        }
    }

    let options = request.options.unwrap_or_default();
    if option_index >= options.len() {
        let _ = bot
            .answer_callback_query(cb_id)
            .text("Invalid option".to_string())
            .await;
        return Ok(());
    }
    let selected = options[option_index].clone();

    // Update the keyboard message to show the selection.
    if let Some(msg) = &q.message {
        let _ = bot
            .edit_message_text(msg.chat.id, msg.id, format!("✓ {selected}"))
            .await;
    }

    // Answer callback.
    let preview = if selected.len() > 50 {
        format!("{}...", selected.chars().take(50).collect::<String>())
    } else {
        selected.clone()
    };
    let _ = bot
        .answer_callback_query(cb_id)
        .text(format!("Selected: {preview}"))
        .await;

    // Delete request file (best-effort).
    let _ = std::fs::remove_file(&request_file);

    // Interrupt any running query: button responses should be immediate.
    if state.session.is_running().await {
        let _ = state.session.stop().await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        state.session.clear_stop_requested().await;
    }

    // Typing loop (best-effort).
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    let bot_for_typing = bot.clone();
    let chat_for_typing = chat_id;
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

    let result = state
        .session
        .send_message_to_chat(ChatId(chat_id.0), &selected, messenger)
        .await;

    // Audit log (best-effort).
    let audit_res = match &result {
        Ok(out) => state.audit.write(AuditEvent::message(
            user_id,
            &username,
            "CALLBACK",
            &selected,
            Some(&out.text),
        )),
        Err(e) => state.audit.write(AuditEvent::error(
            user_id,
            &username,
            &format!("{e}"),
            Some("callback"),
        )),
    };
    if let Err(e) = audit_res {
        eprintln!("[AUDIT] Failed to write callback audit event: {e}");
    }

    if let Err(err) = result {
        if is_cancel_error(&err) {
            let was_interrupt = state.session.consume_interrupt_flag().await;
            if !was_interrupt {
                let _ = bot.send_message(chat_id, "🛑 Query stopped.").await;
            }
        } else {
            let msg_txt = format!("{err}");
            let truncated = if msg_txt.len() > 200 {
                format!("{}...", msg_txt.chars().take(200).collect::<String>())
            } else {
                msg_txt
            };
            let _ = bot
                .send_message(chat_id, format!("❌ Error: {truncated}"))
                .await;
        }
    }

    let _ = stop_tx.send(());
    let _ = typing_task.await;

    Ok(())
}
