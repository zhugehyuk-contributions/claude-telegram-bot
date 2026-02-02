use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use teloxide::{net::Download, prelude::*};

use ctb_core::{
    archive_security::{safe_extract_archive, ExtractLimits},
    utils::AuditEvent,
};

use crate::router::AppState;

use super::{
    media_group::{BoxFuture, MediaGroupBuffer, MediaGroupConfig},
    prompt::{run_prompt, PromptContext, PromptOptions},
};

static DOC_COUNTER: AtomicUsize = AtomicUsize::new(1);
static DOC_BUFFER: std::sync::OnceLock<Arc<MediaGroupBuffer>> = std::sync::OnceLock::new();

const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10MB
const MAX_ARCHIVE_CONTENT: usize = 50_000;

fn text_extensions() -> &'static [&'static str] {
    &[
        ".md", ".txt", ".json", ".yaml", ".yml", ".csv", ".xml", ".html", ".css", ".js", ".ts",
        ".py", ".sh", ".env", ".log", ".cfg", ".ini", ".toml",
    ]
}

fn is_text_file(name: &str, mime: Option<&str>) -> bool {
    let lower = name.to_lowercase();
    if let Some(m) = mime {
        if m.starts_with("text/") {
            return true;
        }
    }
    text_extensions().iter().any(|ext| lower.ends_with(ext))
}

fn is_pdf(name: &str, mime: Option<&str>) -> bool {
    if mime == Some("application/pdf") {
        return true;
    }
    name.to_lowercase().ends_with(".pdf")
}

fn is_archive(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".zip")
        || lower.ends_with(".tar")
        || lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
}

fn sanitize_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "document".to_string()
    } else {
        out
    }
}

fn uniquify_filename(name: &str, ts: u128, n: usize) -> String {
    let base = sanitize_filename(name);
    if let Some((stem, ext)) = base.rsplit_once('.') {
        if !stem.is_empty() && !ext.is_empty() {
            return format!("{stem}_{ts}_{n}.{ext}");
        }
    }
    format!("{base}_{ts}_{n}")
}

fn doc_buffer() -> &'static Arc<MediaGroupBuffer> {
    DOC_BUFFER.get_or_init(|| {
        let cfg = MediaGroupConfig {
            emoji: "📄",
            item_label_plural: "documents",
        };

        let process = std::sync::Arc::new(
            |ctx: PromptContext, items: Vec<String>, caption: Option<String>| {
                let fut: BoxFuture = Box::pin(async move {
                    let docs = extract_documents(&items).await;
                    if docs.is_empty() {
                        let _ = ctx
                            .bot
                            .send_message(
                                teloxide::types::ChatId(ctx.chat_id),
                                "❌ Failed to extract any documents.",
                            )
                            .await;
                        return;
                    }

                    let prompt = build_documents_prompt(&docs, caption.as_deref());
                    let _ = run_prompt(
                        ctx,
                        "DOCUMENT",
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

async fn download_document(
    bot: &Bot,
    state: &AppState,
    doc: &teloxide::types::Document,
) -> anyhow::Result<String> {
    let file = bot.get_file(doc.file.id.clone()).await?;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let n = DOC_COUNTER.fetch_add(1, Ordering::SeqCst);
    let file_name = doc
        .file_name
        .as_deref()
        .map(|s| uniquify_filename(s, ts, n))
        .unwrap_or_else(|| format!("doc_{ts}_{n}"));

    let path = state.cfg.temp_dir.join(file_name);
    let mut dst = tokio::fs::File::create(&path).await?;
    bot.download_file(&file.path, &mut dst).await?;
    Ok(path.to_string_lossy().to_string())
}

async fn extract_pdf(path: &str) -> String {
    use tokio::process::Command;

    let out = Command::new("pdftotext")
        .arg("-layout")
        .arg(path)
        .arg("-")
        .output()
        .await;

    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => {
            "[PDF parsing failed - ensure pdftotext is installed: brew install poppler]".to_string()
        }
    }
}

async fn extract_text_file(path: &str) -> Option<String> {
    let path = path.to_string();
    let raw = tokio::task::spawn_blocking(move || std::fs::read_to_string(path))
        .await
        .ok()?
        .ok()?;
    Some(raw.chars().take(100_000).collect::<String>())
}

async fn extract_documents(paths: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for p in paths {
        let name = p.rsplit('/').next().unwrap_or("document").to_string();
        if name.to_lowercase().ends_with(".pdf") {
            let text = extract_pdf(p).await;
            out.push((name, text));
            continue;
        }
        if let Some(txt) = extract_text_file(p).await {
            out.push((name, txt));
        }
    }
    out
}

fn build_documents_prompt(docs: &[(String, String)], caption: Option<&str>) -> String {
    if docs.len() == 1 {
        let (name, content) = &docs[0];
        return match caption {
            Some(c) if !c.trim().is_empty() => {
                format!("Document: {name}\n\nContent:\n{content}\n\n---\n\n{c}")
            }
            _ => format!("Please analyze this document ({name}):\n\n{content}"),
        };
    }

    let list = docs
        .iter()
        .enumerate()
        .map(|(i, (name, content))| format!("--- Document {}: {name} ---\n{content}", i + 1))
        .collect::<Vec<_>>()
        .join("\n\n");

    match caption {
        Some(c) if !c.trim().is_empty() => {
            format!("{} Documents:\n\n{list}\n\n---\n\n{c}", docs.len())
        }
        _ => format!("Please analyze these {} documents:\n\n{list}", docs.len()),
    }
}

async fn extract_archive_content(
    extract_dir: &std::path::Path,
) -> (Vec<String>, Vec<(String, String)>) {
    let mut tree: Vec<String> = Vec::new();
    let mut contents: Vec<(String, String)> = Vec::new();

    let Ok(rd) = std::fs::read_dir(extract_dir) else {
        return (tree, contents);
    };

    // Walk the extracted_files tree via filesystem (bounded).
    let mut stack: Vec<std::path::PathBuf> = rd.flatten().map(|e| e.path()).collect();
    while let Some(path) = stack.pop() {
        if tree.len() >= 100 {
            break;
        }
        let Ok(md) = std::fs::metadata(&path) else {
            continue;
        };
        if md.is_dir() {
            if let Ok(rd2) = std::fs::read_dir(&path) {
                for ent in rd2.flatten() {
                    stack.push(ent.path());
                }
            }
            continue;
        }

        let rel = path
            .strip_prefix(extract_dir)
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());
        tree.push(rel.clone());

        // Collect readable text content (bounded).
        let lower = rel.to_lowercase();
        if !text_extensions().iter().any(|ext| lower.ends_with(ext)) {
            continue;
        }
        if md.len() > 100_000 {
            continue;
        }
        if let Ok(txt) = std::fs::read_to_string(&path) {
            let truncated: String = txt.chars().take(10_000).collect();
            let total: usize = contents.iter().map(|(_, c)| c.len()).sum();
            if total + truncated.len() > MAX_ARCHIVE_CONTENT {
                break;
            }
            contents.push((rel, truncated));
        }
    }

    tree.sort();
    (tree, contents)
}

pub async fn handle_document(bot: Bot, msg: Message, state: Arc<AppState>) -> ResponseResult<()> {
    let Some(user) = msg.from() else {
        return Ok(());
    };
    let Some(doc) = msg.document() else {
        return Ok(());
    };

    let user_id = user.id.0 as i64;
    let username = user
        .username
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let chat_id = msg.chat.id.0;

    // File size gate.
    let size = doc.file.size as u64;
    if size > MAX_FILE_SIZE {
        let _ = bot
            .send_message(
                teloxide::types::ChatId(chat_id),
                "❌ File too large. Maximum size is 10MB.",
            )
            .await;
        return Ok(());
    }

    let file_name = doc
        .file_name
        .clone()
        .unwrap_or_else(|| "document".to_string());
    let mime = doc.mime_type.as_ref().map(|m| m.essence_str().to_string());
    let mime = mime.as_deref();

    let media_group_id = msg.media_group_id().map(|s| s.to_string());
    let caption = msg.caption().map(|s| s.to_string());

    // Archive files: process immediately (no media group support).
    if is_archive(&file_name) {
        // Rate limit.
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

        let status = bot
            .send_message(
                teloxide::types::ChatId(chat_id),
                format!(
                    "📦 Extracting <b>{}</b>...",
                    ctb_core::formatting::escape_html(&file_name)
                ),
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await
            .ok();

        let archive_path = match download_document(&bot, &state, doc).await {
            Ok(p) => p,
            Err(e) => {
                let _ = bot
                    .send_message(
                        teloxide::types::ChatId(chat_id),
                        format!(
                            "❌ Failed to download archive: {}",
                            e.to_string().chars().take(100).collect::<String>()
                        ),
                    )
                    .await;
                return Ok(());
            }
        };

        let extract_dir = state.cfg.temp_dir.join(format!(
            "archive_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));

        let res = tokio::task::spawn_blocking({
            let archive_path = std::path::PathBuf::from(&archive_path);
            let file_name = file_name.clone();
            let extract_dir = extract_dir.clone();
            move || {
                safe_extract_archive(
                    &archive_path,
                    &file_name,
                    &extract_dir,
                    ExtractLimits::default(),
                )
            }
        })
        .await;

        match res {
            Ok(Ok(report)) => {
                let (tree, contents) = extract_archive_content(&extract_dir).await;

                if let Some(st) = &status {
                    let _ = bot
                        .edit_message_text(
                            st.chat.id,
                            st.id,
                            format!(
                                "📦 Extracted <b>{}</b>: {} files",
                                ctb_core::formatting::escape_html(&file_name),
                                report.extracted_files.len()
                            ),
                        )
                        .parse_mode(teloxide::types::ParseMode::Html)
                        .await;
                }

                let tree_str = if tree.is_empty() {
                    "(empty)".to_string()
                } else {
                    tree.join("\n")
                };
                let contents_str = if contents.is_empty() {
                    "(no readable text files)".to_string()
                } else {
                    contents
                        .iter()
                        .map(|(n, c)| format!("--- {n} ---\n{c}"))
                        .collect::<Vec<_>>()
                        .join("\n\n")
                };

                let prompt = if let Some(c) = caption.as_deref().filter(|s| !s.trim().is_empty()) {
                    format!(
            "Archive: {file_name}\n\nFile tree ({} files):\n{tree_str}\n\nExtracted contents:\n{contents_str}\n\n---\n\n{c}",
            report.extracted_files.len()
          )
                } else {
                    format!(
            "Please analyze this archive ({file_name}):\n\nFile tree ({} files):\n{tree_str}\n\nExtracted contents:\n{contents_str}",
            report.extracted_files.len()
          )
                };

                let _ = run_prompt(
                    PromptContext {
                        bot: bot.clone(),
                        state: state.clone(),
                        chat_id,
                        user_id,
                        username: username.clone(),
                    },
                    "ARCHIVE",
                    prompt,
                    PromptOptions {
                        record_last_message: false,
                        skip_rate_limit: true,
                    },
                )
                .await;

                let _ = std::fs::remove_dir_all(&extract_dir);
            }
            Ok(Err(e)) => {
                let _ = bot
                    .send_message(
                        teloxide::types::ChatId(chat_id),
                        format!("❌ Failed to extract archive: {}", e),
                    )
                    .await;
            }
            Err(_) => {
                let _ = bot
                    .send_message(
                        teloxide::types::ChatId(chat_id),
                        "❌ Failed to extract archive.",
                    )
                    .await;
            }
        }

        if let Some(st) = status {
            let _ = bot.delete_message(st.chat.id, st.id).await;
        }

        if let Err(e) = state.audit.write(AuditEvent::message(
            user_id, &username, "ARCHIVE", &file_name, None,
        )) {
            eprintln!("[AUDIT] Failed to write message event: {e}");
        }

        return Ok(());
    }

    // Validate supported types.
    if !is_pdf(&file_name, mime) && !is_text_file(&file_name, mime) {
        let _ = bot
            .send_message(
                teloxide::types::ChatId(chat_id),
                format!(
          "❌ Unsupported file type.\n\nSupported: PDF, archives (.zip,.tar,.tar.gz,.tgz), {}",
          text_extensions().join(", ")
        ),
            )
            .await;
        return Ok(());
    }

    // Download document.
    let doc_path = match download_document(&bot, &state, doc).await {
        Ok(p) => p,
        Err(e) => {
            let _ = bot
                .send_message(
                    teloxide::types::ChatId(chat_id),
                    format!(
                        "❌ Failed to download document: {}",
                        e.to_string().chars().take(100).collect::<String>()
                    ),
                )
                .await;
            return Ok(());
        }
    };

    // Single document: process immediately.
    if media_group_id.is_none() {
        // Rate limit.
        {
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
        }

        let content = if is_pdf(&file_name, mime) {
            extract_pdf(&doc_path).await
        } else {
            extract_text_file(&doc_path).await.unwrap_or_default()
        };

        let prompt = build_documents_prompt(&[(file_name.clone(), content)], caption.as_deref());
        let _ = run_prompt(
            PromptContext {
                bot,
                state,
                chat_id,
                user_id,
                username,
            },
            "DOCUMENT",
            prompt,
            PromptOptions {
                record_last_message: false,
                skip_rate_limit: true,
            },
        )
        .await;

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
        let _ = doc_buffer()
            .add_to_group(ctx, group_id, doc_path, caption, timeout)
            .await;
    }

    Ok(())
}
