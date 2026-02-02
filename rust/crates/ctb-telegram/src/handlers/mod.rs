//! Telegram update handlers (stubbed).
//!
//! Each handler is implemented as a small adapter that:
//! - validates auth + rate limits
//! - builds a prompt / downloads media if needed
//! - calls into `ctb-core` session runner + streaming
//!
//! Concrete message-type handlers are implemented in agi-cnf.13-17 and command
//! handlers in agi-cnf.22.

use std::sync::Arc;

use teloxide::{
    prelude::*,
    types::{CallbackQuery, Message},
};

use ctb_core::domain::UserId;
use ctb_core::security::is_authorized;

use crate::router::AppState;
mod callback;
mod commands;
mod document;
mod media_group;
mod photo;
mod prompt;
mod text;
mod voice;

pub async fn handle_callback(
    bot: Bot,
    q: CallbackQuery,
    state: Arc<AppState>,
) -> ResponseResult<()> {
    callback::handle_callback(bot, q, state).await
}

pub async fn handle_message(bot: Bot, msg: Message, state: Arc<AppState>) -> ResponseResult<()> {
    let chat_id = msg.chat.id.0;
    let user_id = msg.from().map(|u| u.id.0);

    if !is_authorized(
        user_id.map(|id| UserId(id as i64)),
        &state.cfg.telegram_allowed_users,
    ) {
        let _ = bot
            .send_message(
                msg.chat.id,
                "Unauthorized. Contact the bot owner for access.",
            )
            .await;
        return Ok(());
    }

    if let Some(text) = msg.text() {
        if text.starts_with('/') {
            return commands::handle_command(bot, msg, state).await;
        }
    }

    if msg.text().is_some() {
        // Interrupt (`!`) bypasses queue.
        if msg.text().unwrap_or("").starts_with('!') {
            return text::handle_text(bot, msg, state).await;
        }

        // Sequentialize normal text messages per chat.
        let _guard = state.chat_locks.lock_chat(chat_id).await;
        return text::handle_text(bot, msg, state).await;
    }

    // Photos (agi-cnf.15).
    if msg.photo().is_some() {
        // Only lock for single photos; media groups are buffered and processed later.
        if msg.media_group_id().is_none() {
            let _guard = state.chat_locks.lock_chat(chat_id).await;
            return photo::handle_photo(bot, msg, state).await;
        }
        return photo::handle_photo(bot, msg, state).await;
    }

    // Documents (agi-cnf.16).
    if msg.document().is_some() {
        if msg.media_group_id().is_none() {
            let _guard = state.chat_locks.lock_chat(chat_id).await;
            return document::handle_document(bot, msg, state).await;
        }
        return document::handle_document(bot, msg, state).await;
    }

    // Voice (agi-cnf.14).
    if msg.voice().is_some() {
        let _guard = state.chat_locks.lock_chat(chat_id).await;
        return voice::handle_voice(bot, msg, state).await;
    }

    // Other message types (voice/document) implemented in agi-cnf.14-16.
    let _ = bot
        .send_message(
            msg.chat.id,
            "Rust port: message handling not implemented yet.",
        )
        .await;

    Ok(())
}
