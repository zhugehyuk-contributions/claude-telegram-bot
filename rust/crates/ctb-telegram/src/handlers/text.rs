use std::sync::Arc;

use teloxide::prelude::*;

use ctb_core::utils::strip_interrupt_prefix;

use crate::handlers::prompt::{run_text_prompt, PromptContext};
use crate::router::AppState;

pub async fn handle_text(bot: Bot, msg: Message, state: Arc<AppState>) -> ResponseResult<()> {
    let Some(user) = msg.from() else {
        return Ok(());
    };
    let Some(mut text) = msg.text().map(|s| s.to_string()) else {
        return Ok(());
    };

    let user_id = user.id.0 as i64;
    let username = user
        .username
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let chat_id = msg.chat.id.0;

    // Interrupt prefix handling (`!`): stop current run, then proceed with stripped text.
    let (is_interrupt, stripped) = strip_interrupt_prefix(&text);
    text = stripped;
    if is_interrupt && state.session.is_running().await {
        state.session.mark_interrupt().await;
        let _ = state.session.stop().await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        state.session.clear_stop_requested().await;
    }

    if text.trim().is_empty() {
        return Ok(());
    }

    run_text_prompt(
        PromptContext {
            bot,
            state,
            chat_id,
            user_id,
            username,
        },
        "TEXT",
        text,
    )
    .await
}
