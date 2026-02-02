use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use teloxide::{net::Download, prelude::*};

use ctb_core::utils::AuditEvent;

use crate::router::AppState;

use super::{
    media_group::{BoxFuture, MediaGroupBuffer, MediaGroupConfig},
    prompt::{run_prompt, PromptContext, PromptOptions},
};

static PHOTO_COUNTER: AtomicUsize = AtomicUsize::new(1);
static PHOTO_BUFFER: std::sync::OnceLock<Arc<MediaGroupBuffer>> = std::sync::OnceLock::new();

fn photo_buffer() -> &'static Arc<MediaGroupBuffer> {
    PHOTO_BUFFER.get_or_init(|| {
        let cfg = MediaGroupConfig {
            emoji: "📷",
            item_label_plural: "photos",
        };

        let process = std::sync::Arc::new(
            |ctx: PromptContext, items: Vec<String>, caption: Option<String>| {
                let fut: BoxFuture = Box::pin(async move {
                    let prompt = build_photo_prompt(&items, caption.as_deref());
                    let _ = run_prompt(
                        ctx,
                        "PHOTO",
                        prompt,
                        PromptOptions {
                            record_last_message: false,
                            skip_rate_limit: true,
                        },
                    )
                    .await;
                });
                fut
            },
        );

        MediaGroupBuffer::new(cfg, process)
    })
}

fn build_photo_prompt(photo_paths: &[String], caption: Option<&str>) -> String {
    if photo_paths.len() == 1 {
        let p = &photo_paths[0];
        return match caption {
            Some(c) if !c.trim().is_empty() => format!("[Photo: {p}]\n\n{c}"),
            _ => format!("Please analyze this image: {p}"),
        };
    }

    let list = photo_paths
        .iter()
        .enumerate()
        .map(|(i, p)| format!("{}. {}", i + 1, p))
        .collect::<Vec<_>>()
        .join("\n");

    match caption {
        Some(c) if !c.trim().is_empty() => format!("[Photos:\n{list}]\n\n{c}"),
        _ => format!("Please analyze these {} images:\n{list}", photo_paths.len()),
    }
}

async fn download_photo(
    bot: &Bot,
    state: &AppState,
    photos: &[teloxide::types::PhotoSize],
) -> anyhow::Result<String> {
    let best = photos
        .last()
        .ok_or_else(|| anyhow::anyhow!("no photo sizes"))?;
    let file = bot.get_file(best.file.id.clone()).await?;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let n = PHOTO_COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = state.cfg.temp_dir.join(format!("photo_{ts}_{n}.jpg"));

    let mut dst = tokio::fs::File::create(&path).await?;
    bot.download_file(&file.path, &mut dst).await?;

    Ok(path.to_string_lossy().to_string())
}

pub async fn handle_photo(bot: Bot, msg: Message, state: Arc<AppState>) -> ResponseResult<()> {
    let Some(user) = msg.from() else {
        return Ok(());
    };
    let Some(photos) = msg.photo() else {
        return Ok(());
    };

    let user_id = user.id.0 as i64;
    let username = user
        .username
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let chat_id = msg.chat.id.0;

    let media_group_id = msg.media_group_id().map(|s| s.to_string());
    let caption = msg.caption().map(|s| s.to_string());

    // For single photos, rate limit early and show status immediately (parity with TS).
    let mut status_msg: Option<Message> = None;
    if media_group_id.is_none() {
        let mut rl = state.rate_limiter.lock().await;
        let (ok, retry_after) = rl.check(ctb_core::domain::UserId(user_id));
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
        status_msg = bot
            .send_message(teloxide::types::ChatId(chat_id), "📷 Processing image...")
            .await
            .ok();
    }

    let photo_path = match download_photo(&bot, &state, photos).await {
        Ok(p) => p,
        Err(e) => {
            let _ = bot
                .send_message(
                    teloxide::types::ChatId(chat_id),
                    format!(
                        "❌ Failed to download photo: {}",
                        e.to_string().chars().take(100).collect::<String>()
                    ),
                )
                .await;
            return Ok(());
        }
    };

    // Single photo: process immediately.
    if media_group_id.is_none() {
        let prompt = build_photo_prompt(std::slice::from_ref(&photo_path), caption.as_deref());
        let _ = run_prompt(
            PromptContext {
                bot: bot.clone(),
                state: state.clone(),
                chat_id,
                user_id,
                username: username.clone(),
            },
            "PHOTO",
            prompt,
            PromptOptions {
                record_last_message: false,
                skip_rate_limit: true,
            },
        )
        .await;

        if let Some(st) = status_msg {
            let _ = bot.delete_message(st.chat.id, st.id).await;
        }

        return Ok(());
    }

    // Media group: buffer and process after timeout.
    if let Some(group_id) = media_group_id {
        let timeout = state.cfg.media_group_timeout;
        let ctx = PromptContext {
            bot,
            state: state.clone(),
            chat_id,
            user_id,
            username,
        };
        let _ = photo_buffer()
            .add_to_group(ctx, group_id, photo_path, caption, timeout)
            .await;
    }

    Ok(())
}
