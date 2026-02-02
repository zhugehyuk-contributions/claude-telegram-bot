use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
};

use teloxide::{net::Download, prelude::*};

use ctb_core::utils::AuditEvent;
use ctb_openai::OpenAiClient;

use crate::router::AppState;

use super::prompt::{run_prompt, PromptContext, PromptOptions};

static VOICE_COUNTER: AtomicUsize = AtomicUsize::new(1);

async fn download_voice(
    bot: &Bot,
    state: &AppState,
    voice: &teloxide::types::Voice,
) -> anyhow::Result<PathBuf> {
    let file = bot.get_file(voice.file.id.clone()).await?;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let n = VOICE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = state.cfg.temp_dir.join(format!("voice_{ts}_{n}.ogg"));

    let mut dst = tokio::fs::File::create(&path).await?;
    bot.download_file(&file.path, &mut dst).await?;
    Ok(path)
}

pub async fn handle_voice(bot: Bot, msg: Message, state: Arc<AppState>) -> ResponseResult<()> {
    let Some(user) = msg.from() else {
        return Ok(());
    };
    let Some(voice) = msg.voice() else {
        return Ok(());
    };

    let user_id = user.id.0 as i64;
    let username = user
        .username
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let chat_id = msg.chat.id.0;

    if !state.cfg.transcription_available {
        let _ = bot
            .send_message(
                teloxide::types::ChatId(chat_id),
                "Voice transcription is not configured. Set OPENAI_API_KEY in .env",
            )
            .await;
        return Ok(());
    }

    // Rate limit early.
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
            return Ok(());
        }
    }

    let status = bot
        .send_message(teloxide::types::ChatId(chat_id), "🎤 Transcribing...")
        .await
        .ok();

    let voice_path = match download_voice(&bot, &state, voice).await {
        Ok(p) => p,
        Err(e) => {
            let _ = bot
                .send_message(
                    teloxide::types::ChatId(chat_id),
                    format!(
                        "❌ Failed to download voice: {}",
                        e.to_string().chars().take(200).collect::<String>()
                    ),
                )
                .await;
            return Ok(());
        }
    };

    let transcript = match state
        .cfg
        .openai_api_key
        .as_ref()
        .map(|k| OpenAiClient::new(k.clone()))
    {
        Some(client) => client
            .transcribe_file(&voice_path, Some(&state.cfg.transcription_prompt))
            .await
            .ok(),
        None => None,
    };

    let Some(transcript) = transcript else {
        if let Some(st) = &status {
            let _ = bot
                .edit_message_text(st.chat.id, st.id, "❌ Transcription failed.")
                .await;
        } else {
            let _ = bot
                .send_message(teloxide::types::ChatId(chat_id), "❌ Transcription failed.")
                .await;
        }
        let _ = tokio::fs::remove_file(&voice_path).await;
        return Ok(());
    };

    // Show transcript.
    if let Some(st) = &status {
        let preview = if transcript.len() > 300 {
            format!("{}...", transcript.chars().take(300).collect::<String>())
        } else {
            transcript.clone()
        };
        let _ = bot
            .edit_message_text(st.chat.id, st.id, format!("🎤 \"{preview}\""))
            .await;
    }

    let _ = run_prompt(
        PromptContext {
            bot,
            state,
            chat_id,
            user_id,
            username,
        },
        "VOICE",
        transcript,
        PromptOptions {
            record_last_message: false,
            skip_rate_limit: true,
        },
    )
    .await;

    let _ = tokio::fs::remove_file(&voice_path).await;
    Ok(())
}
