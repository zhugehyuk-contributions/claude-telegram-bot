use std::sync::Arc;

use chrono::{DateTime, Utc};
use teloxide::prelude::*;

use ctb_core::{
    formatting::escape_html,
    messaging::port::MessagingPort,
    usage::{AllUsage, ClaudeUsage, CodexUsage, GeminiUsage},
};

use crate::router::AppState;

use super::prompt::{run_text_prompt, PromptContext};

fn parse_command(text: &str) -> (String, String) {
    // Telegram may send `/cmd@botname arg1 ...`
    let mut parts = text.trim().splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("").trim();
    let rest = parts.next().unwrap_or("").trim().to_string();

    let cmd = first
        .trim_start_matches('/')
        .split('@')
        .next()
        .unwrap_or("")
        .to_lowercase();

    (cmd, rest)
}

fn format_duration(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let hours = seconds / 3600;
    let mins = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        return format!("{hours}h {mins}m {secs}s");
    }
    if mins > 0 {
        return format!("{mins}m {secs}s");
    }
    format!("{secs}s")
}

fn format_time_remaining(reset_time: Option<&str>) -> String {
    let Some(reset_time) = reset_time else {
        return "".to_string();
    };

    let reset = DateTime::parse_from_rfc3339(reset_time)
        .map(|dt| dt.with_timezone(&Utc))
        .ok();
    let Some(reset) = reset else {
        return "".to_string();
    };

    let now = Utc::now();
    let diff = reset.signed_duration_since(now);
    if diff.num_seconds() <= 0 {
        return "now".to_string();
    }

    let diff_sec = diff.num_seconds();
    let days = diff_sec / 86400;
    let hours = (diff_sec % 86400) / 3600;
    let mins = (diff_sec % 3600) / 60;

    if days > 0 {
        return format!("{days}d {hours}h");
    }
    if hours > 0 {
        return format!("{hours}h {mins}m");
    }
    format!("{mins}m")
}

fn format_time_remaining_unix_seconds(reset_at: u64) -> String {
    if reset_at == 0 {
        return "".to_string();
    }
    let reset = DateTime::<Utc>::from_timestamp(reset_at as i64, 0);
    let Some(reset) = reset else {
        return "".to_string();
    };

    let now = Utc::now();
    let diff = reset.signed_duration_since(now);
    if diff.num_seconds() <= 0 {
        return "now".to_string();
    }

    let diff_sec = diff.num_seconds();
    let days = diff_sec / 86400;
    let hours = (diff_sec % 86400) / 3600;
    let mins = (diff_sec % 3600) / 60;

    if days > 0 {
        return format!("{days}d {hours}h");
    }
    if hours > 0 {
        return format!("{hours}h {mins}m");
    }
    format!("{mins}m")
}

async fn send_html_split(state: &AppState, chat_id: i64, html: &str) {
    let limit = state.cfg.telegram_safe_limit.max(200);
    if html.len() <= limit {
        let _ = state
            .messenger
            .send_html(ctb_core::domain::ChatId(chat_id), html)
            .await;
        return;
    }

    let mut buf = String::new();
    for line in html.split('\n') {
        let extra = if buf.is_empty() {
            line.len()
        } else {
            line.len() + 1
        };
        if !buf.is_empty() && buf.len() + extra > limit {
            let _ = state
                .messenger
                .send_html(ctb_core::domain::ChatId(chat_id), &buf)
                .await;
            buf.clear();
        }
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(line);
    }
    if !buf.is_empty() {
        let _ = state
            .messenger
            .send_html(ctb_core::domain::ChatId(chat_id), &buf)
            .await;
    }
}

fn format_claude_usage(usage: &ClaudeUsage) -> Vec<String> {
    let mut lines = vec!["<b>Claude Code:</b>".to_string()];

    if let Some(w) = &usage.five_hour {
        let reset = format_time_remaining(w.resets_at.as_deref());
        lines.push(format!(
            "   5h: {}%{}",
            w.utilization.round(),
            if reset.is_empty() {
                "".to_string()
            } else {
                format!(" (resets in {reset})")
            }
        ));
    }
    if let Some(w) = &usage.seven_day {
        let reset = format_time_remaining(w.resets_at.as_deref());
        lines.push(format!(
            "   7d: {}%{}",
            w.utilization.round(),
            if reset.is_empty() {
                "".to_string()
            } else {
                format!(" (resets in {reset})")
            }
        ));
    }
    if let Some(w) = &usage.seven_day_sonnet {
        let reset = format_time_remaining(w.resets_at.as_deref());
        lines.push(format!(
            "   7d Sonnet: {}%{}",
            w.utilization.round(),
            if reset.is_empty() {
                "".to_string()
            } else {
                format!(" (resets in {reset})")
            }
        ));
    }

    lines
}

fn format_codex_usage(usage: &CodexUsage) -> Vec<String> {
    let mut lines = vec![format!(
        "<b>OpenAI Codex</b> ({}):",
        escape_html(&usage.plan_type)
    )];

    if let Some(w) = &usage.primary {
        let reset = format_time_remaining_unix_seconds(w.reset_at);
        lines.push(format!(
            "   5h: {}%{}",
            w.used_percent.round(),
            if reset.is_empty() {
                "".to_string()
            } else {
                format!(" (resets in {reset})")
            }
        ));
    }
    if let Some(w) = &usage.secondary {
        let reset = format_time_remaining_unix_seconds(w.reset_at);
        lines.push(format!(
            "   7d: {}%{}",
            w.used_percent.round(),
            if reset.is_empty() {
                "".to_string()
            } else {
                format!(" (resets in {reset})")
            }
        ));
    }

    lines
}

fn format_gemini_usage(usage: &GeminiUsage) -> Vec<String> {
    let mut lines = vec![format!("<b>Gemini</b> ({}):", escape_html(&usage.model))];

    if let Some(pct) = usage.used_percent {
        let reset = format_time_remaining(usage.reset_at.as_deref());
        lines.push(format!(
            "   Usage: {pct}%{}",
            if reset.is_empty() {
                "".to_string()
            } else {
                format!(" (resets in {reset})")
            }
        ));
    }

    lines
}

fn format_provider_usage(all: &AllUsage) -> Vec<String> {
    let mut lines = vec!["\n🌐 <b>Provider Usage</b>".to_string()];
    if let Some(c) = &all.claude {
        lines.extend(format_claude_usage(c));
    }
    if let Some(c) = &all.codex {
        lines.extend(format_codex_usage(c));
    }
    if let Some(g) = &all.gemini {
        lines.extend(format_gemini_usage(g));
    }
    if all.claude.is_none() && all.codex.is_none() && all.gemini.is_none() {
        lines.push("   <i>No providers authenticated</i>".to_string());
    }
    lines
}

pub async fn handle_command(bot: Bot, msg: Message, state: Arc<AppState>) -> ResponseResult<()> {
    let Some(user) = msg.from() else {
        return Ok(());
    };
    let Some(text) = msg.text() else {
        return Ok(());
    };

    let user_id = user.id.0 as i64;
    let username = user
        .username
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let chat_id = msg.chat.id.0;

    let (cmd, arg) = parse_command(text);

    match cmd.as_str() {
        "start" | "help" => {
            let status = if state.session.is_active().await {
                "Active session"
            } else {
                "No active session"
            };
            let work_dir = escape_html(&state.cfg.claude_working_dir.display().to_string());

            let body = format!(
                "🤖 <b>Claude Telegram Bot (Rust)</b>\n\n\
Status: {status}\n\
Working directory: <code>{work_dir}</code>\n\n\
<b>📋 Commands:</b>\n\
/start - Show this help message\n\
/new - Start fresh session\n\
/stop - Stop current query (silent)\n\
/status - Show current session status\n\
/stats - Show token usage & cost stats\n\
/resume - Resume last saved session\n\
/retry - Retry last message\n\
/cron [reload] - Scheduled jobs status/reload\n\
/restart - Restart the bot process\n\n\
<b>💡 Tips:</b>\n\
• Prefix with <code>!</code> to interrupt current query\n\
• Use \"think\" keyword for extended reasoning\n\
• Use \"ultrathink\" for deep analysis\n\
• Send photos, voice messages, or documents\n\
• Multiple photos = album (auto-grouped)"
            );

            send_html_split(&state, chat_id, &body).await;
            Ok(())
        }

        "new" => {
            if state.session.is_running().await {
                let _ = state.session.stop().await;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                state.session.clear_stop_requested().await;
            }
            let _ = state.session.kill().await;
            send_html_split(
                &state,
                chat_id,
                "🆕 Session cleared. Next message starts fresh.",
            )
            .await;
            Ok(())
        }

        "stop" => {
            if state.session.is_running().await {
                let _ = state.session.stop().await;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                state.session.clear_stop_requested().await;
            }
            // Silent by design.
            Ok(())
        }

        "status" => {
            let st = state.session.stats().await;
            let mut lines: Vec<String> = vec!["📊 <b>Bot Status</b>\n".to_string()];

            if let Some(sref) = st.session.as_ref() {
                let short = if sref.id.len() > 8 {
                    &sref.id[..8]
                } else {
                    &sref.id
                };
                lines.push(format!("✅ Session: Active ({short}...)"));
                if let Some(start) = st.session_start_time.as_deref() {
                    if let Ok(dt) = DateTime::parse_from_rfc3339(start) {
                        let dur = (Utc::now() - dt.with_timezone(&Utc)).num_seconds();
                        lines.push(format!(
                            "   └─ Duration: {} | {} queries",
                            format_duration(dur),
                            st.total_queries
                        ));
                    }
                }
            } else {
                lines.push("⚪ Session: None".to_string());
            }

            if st.is_running {
                lines.push("🔄 Query: Running".to_string());
            } else {
                lines.push("⚪ Query: Idle".to_string());
            }

            if let Some(u) = st.last_usage.as_ref() {
                lines.push("\n📈 Last query usage:".to_string());
                lines.push(format!("   Input: {} tokens", u.input_tokens));
                lines.push(format!("   Output: {} tokens", u.output_tokens));
                if u.cache_read_input_tokens > 0 {
                    lines.push(format!("   Cache read: {}", u.cache_read_input_tokens));
                }
            }

            lines.push(format!(
                "\n📁 Working dir: <code>{}</code>",
                escape_html(&state.cfg.claude_working_dir.display().to_string())
            ));

            send_html_split(&state, chat_id, &lines.join("\n")).await;
            Ok(())
        }

        "resume" => {
            if state.session.is_active().await {
                send_html_split(
                    &state,
                    chat_id,
                    "Session already active. Use /new to start fresh first.",
                )
                .await;
                return Ok(());
            }
            match state.session.resume_last().await {
                Ok((true, msg)) => {
                    send_html_split(&state, chat_id, &format!("✅ {}", escape_html(&msg))).await
                }
                Ok((false, msg)) => {
                    send_html_split(&state, chat_id, &format!("❌ {}", escape_html(&msg))).await
                }
                Err(e) => {
                    send_html_split(
                        &state,
                        chat_id,
                        &format!("❌ {}", escape_html(&format!("{e}"))),
                    )
                    .await
                }
            };
            Ok(())
        }

        "cron" => {
            if arg.trim().eq_ignore_ascii_case("reload") {
                match state.scheduler.reload().await {
                    Ok(0) => {
                        send_html_split(&state, chat_id, "⚠️ No schedules found in cron.yaml").await
                    }
                    Ok(count) => {
                        send_html_split(
                            &state,
                            chat_id,
                            &format!(
                                "🔄 Reloaded {} scheduled job{}",
                                count,
                                if count == 1 { "" } else { "s" }
                            ),
                        )
                        .await
                    }
                    Err(e) => {
                        send_html_split(
                            &state,
                            chat_id,
                            &format!("❌ {}", escape_html(&format!("{e}"))),
                        )
                        .await
                    }
                }
                return Ok(());
            }

            let status = state.scheduler.status_html().await;
            let note = "\n\n<i>cron.yaml is auto-monitored for changes.\nYou can also use /cron reload to force reload.</i>";
            send_html_split(&state, chat_id, &format!("{status}{note}")).await;
            Ok(())
        }

        "stats" => {
            let st = state.session.stats().await;
            let mut lines: Vec<String> = vec!["📊 <b>Session Statistics</b>\n".to_string()];

            if let Some(start) = st.session_start_time.as_deref() {
                if let Ok(dt) = DateTime::parse_from_rfc3339(start) {
                    let dur = (Utc::now() - dt.with_timezone(&Utc)).num_seconds();
                    lines.push(format!("⏱️ Session duration: {}", format_duration(dur)));
                    lines.push(format!("🔢 Total queries: {}", st.total_queries));
                }
            } else {
                lines.push("⚪ No active session".to_string());
            }

            if st.total_queries > 0 {
                let total_in = st.total_input_tokens;
                let total_out = st.total_output_tokens;
                let total_cache = st.total_cache_read_tokens + st.total_cache_create_tokens;
                let total_tokens = total_in + total_out;

                lines.push("\n🧠 <b>Token Usage</b>".to_string());
                lines.push(format!("   Input: {total_in} tokens"));
                lines.push(format!("   Output: {total_out} tokens"));
                if total_cache > 0 {
                    lines.push(format!("   Cache: {total_cache} tokens"));
                    lines.push(format!("     └─ Read: {}", st.total_cache_read_tokens));
                    lines.push(format!("     └─ Create: {}", st.total_cache_create_tokens));
                }
                lines.push(format!("   <b>Total: {total_tokens} tokens</b>"));

                let cost_in = (total_in as f64 / 1_000_000.0) * 3.0;
                let cost_out = (total_out as f64 / 1_000_000.0) * 15.0;
                let cost_cache_read = (st.total_cache_read_tokens as f64 / 1_000_000.0) * 0.3;
                let cost_cache_write = (st.total_cache_create_tokens as f64 / 1_000_000.0) * 3.75;
                let total_cost = cost_in + cost_out + cost_cache_read + cost_cache_write;

                lines.push("\n💰 <b>Estimated Cost</b>".to_string());
                lines.push(format!("   Input: ${cost_in:.4}"));
                lines.push(format!("   Output: ${cost_out:.4}"));
                if total_cache > 0 {
                    lines.push(format!(
                        "   Cache: ${:.4}",
                        cost_cache_read + cost_cache_write
                    ));
                }
                lines.push(format!("   <b>Total: ${total_cost:.4}</b>"));

                if st.total_queries > 1 {
                    let avg_in = total_in / st.total_queries;
                    let avg_out = total_out / st.total_queries;
                    let avg_cost = total_cost / st.total_queries as f64;
                    lines.push("\n📈 <b>Per Query Average</b>".to_string());
                    lines.push(format!("   Input: {avg_in} tokens"));
                    lines.push(format!("   Output: {avg_out} tokens"));
                    lines.push(format!("   Cost: ${avg_cost:.4}"));
                }
            } else {
                lines.push("\n📭 No queries in this session yet".to_string());
            }

            if let Some(u) = st.last_usage.as_ref() {
                lines.push("\n🔍 <b>Last Query</b>".to_string());
                lines.push(format!("   Input: {} tokens", u.input_tokens));
                lines.push(format!("   Output: {} tokens", u.output_tokens));
                if u.cache_read_input_tokens > 0 {
                    lines.push(format!("   Cache read: {}", u.cache_read_input_tokens));
                }
            }

            let all = state.usage.fetch_all(None).await;
            lines.extend(format_provider_usage(&all));
            lines.push("\n<i>Pricing: Claude Sonnet 4 rates</i>".to_string());

            send_html_split(&state, chat_id, &lines.join("\n")).await;
            Ok(())
        }

        "retry" => {
            let last = state.session.last_message().await;
            let Some(last) = last else {
                send_html_split(&state, chat_id, "❌ No message to retry.").await;
                return Ok(());
            };

            if state.session.is_running().await {
                send_html_split(
                    &state,
                    chat_id,
                    "⏳ A query is already running. Use /stop first.",
                )
                .await;
                return Ok(());
            }

            let preview = if last.len() > 50 {
                format!("{}...", last.chars().take(50).collect::<String>())
            } else {
                last.clone()
            };
            let _ = bot
                .send_message(msg.chat.id, format!("🔄 Retrying: \"{preview}\""))
                .await;

            run_text_prompt(
                PromptContext {
                    bot: bot.clone(),
                    state: state.clone(),
                    chat_id,
                    user_id,
                    username,
                },
                "RETRY",
                last,
            )
            .await
        }

        "restart" => {
            let sent = bot
                .send_message(msg.chat.id, "🔄 Restarting bot...")
                .await?;
            // Keep TS-compatible fields: chat_id/message_id/timestamp(ms).
            let payload = serde_json::json!({
              "chat_id": chat_id,
              "message_id": sent.id.0,
              "timestamp": (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64),
            });
            let _ = std::fs::write(
                &state.cfg.restart_file,
                serde_json::to_string(&payload).unwrap_or_default(),
            );

            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            std::process::exit(0);
        }

        _ => {
            let msg = format!("Unknown command: /{}", escape_html(&cmd));
            send_html_split(&state, chat_id, &msg).await;
            Ok(())
        }
    }
}
