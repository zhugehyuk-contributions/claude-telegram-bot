//! Streaming state machine for Telegram updates (provider-agnostic).
//!
//! This mirrors the TS `createStatusCallback()` behavior:
//! - throttled edits for streaming text
//! - per-segment message tracking
//! - progress spinner + completion message
//! - optional deletion of thinking/tool messages

use std::{collections::HashMap, time::Instant};

use chrono::Local;

use crate::{
    config::Config,
    domain::{ChatId, MessageRef},
    formatting::convert_markdown_to_html,
    messaging::port::MessagingPort,
    Result,
};

/// Status callback event types (parity with TS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusType {
    Thinking,
    Tool,
    Text,
    SegmentEnd,
    Done,
}

#[derive(Clone, Debug)]
pub struct StreamingState {
    pub chat_id: ChatId,

    pub text_messages: HashMap<u32, MessageRef>, // segment_id -> message
    pub thinking_messages: Vec<MessageRef>,
    pub tool_messages: Vec<MessageRef>,

    last_edit_times: HashMap<u32, Instant>,
    last_content: HashMap<u32, String>,

    progress_message: Option<MessageRef>,
    start_time: Option<ProgressStart>,
    frame_index: usize,
}

#[derive(Clone, Debug)]
struct ProgressStart {
    instant: Instant,
    wallclock: chrono::DateTime<Local>,
}

impl StreamingState {
    pub fn new(chat_id: ChatId) -> Self {
        Self {
            chat_id,
            text_messages: HashMap::new(),
            thinking_messages: Vec::new(),
            tool_messages: Vec::new(),
            last_edit_times: HashMap::new(),
            last_content: HashMap::new(),
            progress_message: None,
            start_time: None,
            frame_index: 0,
        }
    }

    pub async fn on_status(
        &mut self,
        cfg: &Config,
        api: &dyn MessagingPort,
        status_type: StatusType,
        content: &str,
        segment_id: Option<u32>,
    ) -> Result<()> {
        self.on_status_at(cfg, api, status_type, content, segment_id, Instant::now())
            .await
    }

    pub async fn on_status_at(
        &mut self,
        cfg: &Config,
        api: &dyn MessagingPort,
        status_type: StatusType,
        content: &str,
        segment_id: Option<u32>,
        now: Instant,
    ) -> Result<()> {
        // Initialize progress tracking on first event.
        if self.start_time.is_none() {
            self.start_time = Some(ProgressStart {
                instant: now,
                wallclock: Local::now(),
            });
            self.recreate_progress(api).await?;
        }

        match status_type {
            StatusType::Thinking => {
                let preview = truncate_with_ellipsis(content, 500);
                let msg = api
                    .send_html(
                        self.chat_id,
                        &format!("🧠 <i>{}</i>", crate::formatting::escape_html(&preview)),
                    )
                    .await?;
                self.thinking_messages.push(msg);
                self.recreate_progress(api).await?;
            }
            StatusType::Tool => {
                let msg = api.send_html(self.chat_id, content).await?;
                self.tool_messages.push(msg);
                self.recreate_progress(api).await?;
            }
            StatusType::Text => {
                let Some(seg) = segment_id else {
                    return Ok(());
                };
                self.handle_text_stream(cfg, api, seg, content, now).await?;
            }
            StatusType::SegmentEnd => {
                let Some(seg) = segment_id else {
                    return Ok(());
                };
                self.handle_segment_end(cfg, api, seg, content).await?;
            }
            StatusType::Done => {
                self.handle_done(cfg, api).await?;
            }
        }

        Ok(())
    }

    /// Tick the progress spinner (call from an interval timer).
    pub async fn tick_progress(&mut self, api: &dyn MessagingPort) -> Result<()> {
        let Some(start) = self.start_time.as_ref() else {
            return Ok(());
        };
        let Some(msg) = self.progress_message else {
            return Ok(());
        };

        self.frame_index = self.frame_index.wrapping_add(1);
        let spinner = SPINNER_FRAMES[self.frame_index % SPINNER_FRAMES.len()];
        let elapsed = format_elapsed(start.instant);
        let text = format!("{spinner} Working... ({elapsed})");
        // Best-effort; ignore edit errors.
        let _ = api.edit_html(msg, &text).await;
        Ok(())
    }

    async fn handle_text_stream(
        &mut self,
        cfg: &Config,
        api: &dyn MessagingPort,
        segment_id: u32,
        content: &str,
        now: Instant,
    ) -> Result<()> {
        let last_edit = self.last_edit_times.get(&segment_id).copied();

        if !self.text_messages.contains_key(&segment_id) {
            // New segment: create message.
            let display = truncate_with_ellipsis(content, cfg.telegram_safe_limit);
            let formatted = convert_markdown_to_html(&display);
            let msg = api.send_html(self.chat_id, &formatted).await?;
            self.text_messages.insert(segment_id, msg);
            self.last_content.insert(segment_id, formatted);
            self.last_edit_times.insert(segment_id, now);
            self.recreate_progress(api).await?;
            return Ok(());
        }

        if let Some(last) = last_edit {
            if now.duration_since(last) <= cfg.streaming_throttle {
                return Ok(());
            }
        }

        let msg = self.text_messages[&segment_id];
        let display = truncate_with_ellipsis(content, cfg.telegram_safe_limit);
        let formatted = convert_markdown_to_html(&display);

        if self
            .last_content
            .get(&segment_id)
            .map(|s| s == &formatted)
            .unwrap_or(false)
        {
            return Ok(());
        }

        let _ = api.edit_html(msg, &formatted).await;
        self.last_content.insert(segment_id, formatted);
        self.last_edit_times.insert(segment_id, now);
        Ok(())
    }

    async fn handle_segment_end(
        &mut self,
        cfg: &Config,
        api: &dyn MessagingPort,
        segment_id: u32,
        content: &str,
    ) -> Result<()> {
        if content.is_empty() {
            return Ok(());
        }

        // If short response and no message exists yet, send now.
        if !self.text_messages.contains_key(&segment_id) {
            let formatted = convert_markdown_to_html(content);
            let msg = api.send_html(self.chat_id, &formatted).await?;
            self.text_messages.insert(segment_id, msg);
            self.recreate_progress(api).await?;
            return Ok(());
        }

        let msg = self.text_messages[&segment_id];
        let formatted = convert_markdown_to_html(content);
        if self
            .last_content
            .get(&segment_id)
            .map(|s| s == &formatted)
            .unwrap_or(false)
        {
            return Ok(());
        }

        if formatted.len() <= cfg.telegram_message_limit {
            let _ = api.edit_html(msg, &formatted).await;
            self.last_content.insert(segment_id, formatted);
            return Ok(());
        }

        // Too long: delete and split into safe chunks (convert per chunk to keep HTML well-formed).
        let _ = api.delete_message(msg).await;
        self.text_messages.remove(&segment_id);
        self.last_content.remove(&segment_id);
        self.last_edit_times.remove(&segment_id);

        for chunk in split_text(content, cfg.telegram_safe_limit) {
            let html = convert_markdown_to_html(&chunk);
            let _ = api.send_html(self.chat_id, &html).await;
        }

        self.recreate_progress(api).await?;
        Ok(())
    }

    async fn handle_done(&mut self, cfg: &Config, api: &dyn MessagingPort) -> Result<()> {
        // Update progress message with completion info.
        if let (Some(start), Some(progress_msg)) = (self.start_time.as_ref(), self.progress_message)
        {
            let duration = format_elapsed(start.instant);
            let start_str = start.wallclock.format("%H:%M:%S").to_string();
            let end_str = Local::now().format("%H:%M:%S").to_string();

            let completion = format!("✅ Completed\n⏰ {start_str} → {end_str} ({duration})");
            let _ = api.edit_html(progress_msg, &completion).await;
        }

        // Delete thinking/tool messages if configured.
        if cfg.delete_thinking_messages {
            for m in &self.thinking_messages {
                let _ = api.delete_message(*m).await;
            }
        }
        if cfg.delete_tool_messages {
            for m in &self.tool_messages {
                let _ = api.delete_message(*m).await;
            }
        }

        // Add completion reaction to the last segment message.
        if let Some((_, &last_msg)) = self.text_messages.iter().max_by_key(|(k, _)| *k) {
            let _ = api.set_reaction(last_msg, "👍").await;
        }

        Ok(())
    }

    async fn recreate_progress(&mut self, api: &dyn MessagingPort) -> Result<()> {
        let Some(start) = self.start_time.as_ref() else {
            return Ok(());
        };

        // Delete old progress message (best-effort).
        if let Some(old) = self.progress_message {
            let _ = api.delete_message(old).await;
        }

        let spinner = SPINNER_FRAMES[self.frame_index % SPINNER_FRAMES.len()];
        let elapsed = format_elapsed(start.instant);
        let text = format!("{spinner} Working... ({elapsed})");
        let msg = api.send_html(self.chat_id, &text).await?;
        self.progress_message = Some(msg);
        Ok(())
    }
}

const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn format_elapsed(start: Instant) -> String {
    let elapsed = start.elapsed().as_secs();
    let minutes = elapsed / 60;
    let seconds = elapsed % 60;
    format!("{minutes}:{seconds:02}")
}

fn truncate_with_ellipsis(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    format!("{}...", s.chars().take(max_len).collect::<String>())
}

fn split_text(s: &str, max_len: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();

    for ch in s.chars() {
        if cur.len() >= max_len {
            out.push(cur);
            cur = String::new();
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::MessageId;
    use crate::messaging::types::{ChatAction, InlineKeyboard, MessagingCapabilities};
    use async_trait::async_trait;
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Default)]
    struct FakeMessenger {
        next_id: Mutex<i32>,
        sends: Mutex<Vec<String>>,
        edits: Mutex<Vec<(MessageRef, String)>>,
        deletes: Mutex<Vec<MessageRef>>,
        reactions: Mutex<Vec<(MessageRef, String)>>,
    }

    impl FakeMessenger {
        fn new() -> Self {
            Self {
                next_id: Mutex::new(1),
                ..Default::default()
            }
        }

        fn alloc(&self, chat_id: ChatId) -> MessageRef {
            let mut guard = self.next_id.lock().unwrap();
            let id = *guard;
            *guard += 1;
            MessageRef {
                chat_id,
                message_id: MessageId(id),
            }
        }
    }

    #[async_trait]
    impl MessagingPort for FakeMessenger {
        fn capabilities(&self) -> MessagingCapabilities {
            MessagingCapabilities {
                supports_html: true,
                supports_edit: true,
                supports_reactions: true,
                supports_chat_actions: false,
                supports_inline_keyboards: false,
                max_message_len: 4096,
            }
        }

        async fn send_html(&self, chat_id: ChatId, html: &str) -> Result<MessageRef> {
            self.sends.lock().unwrap().push(html.to_string());
            Ok(self.alloc(chat_id))
        }

        async fn edit_html(&self, msg: MessageRef, html: &str) -> Result<()> {
            self.edits.lock().unwrap().push((msg, html.to_string()));
            Ok(())
        }

        async fn delete_message(&self, msg: MessageRef) -> Result<()> {
            self.deletes.lock().unwrap().push(msg);
            Ok(())
        }

        async fn send_chat_action(&self, _chat_id: ChatId, _action: ChatAction) -> Result<()> {
            Ok(())
        }

        async fn set_reaction(&self, msg: MessageRef, emoji: &str) -> Result<()> {
            self.reactions
                .lock()
                .unwrap()
                .push((msg, emoji.to_string()));
            Ok(())
        }

        async fn send_inline_keyboard(
            &self,
            _chat_id: ChatId,
            _text: &str,
            _keyboard: InlineKeyboard,
        ) -> Result<MessageRef> {
            Ok(self.alloc(ChatId(0)))
        }

        async fn answer_callback_query(
            &self,
            _callback_id: &str,
            _text: Option<&str>,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn creates_and_throttles_segment_edits() {
        // Avoid Config::load() env dependency: hand-roll config.
        let cfg = Config {
            telegram_bot_token: "x".to_string(),
            telegram_allowed_users: vec![1],
            claude_working_dir: "/tmp".into(),
            openai_api_key: None,
            transcription_prompt: "x".to_string(),
            transcription_available: false,
            claude_cli_path: "/usr/bin/claude".into(),
            claude_config_dir: None,
            allowed_paths: vec!["/tmp".into()],
            temp_paths: vec!["/tmp".into()],
            blocked_patterns: vec![],
            safety_prompt: "x".to_string(),
            query_timeout: Duration::from_secs(1),
            temp_dir: "/tmp".into(),
            session_file: "/tmp/s.json".into(),
            restart_file: "/tmp/r.json".into(),
            telegram_message_limit: 4096,
            telegram_safe_limit: 50,
            streaming_throttle: Duration::from_millis(500),
            button_label_max_length: 30,
            default_thinking_tokens: 0,
            thinking_keywords: vec![],
            thinking_deep_keywords: vec![],
            delete_thinking_messages: true,
            delete_tool_messages: true,
            audit_log_path: "/tmp/a.log".into(),
            audit_log_json: false,
            rate_limit_enabled: true,
            rate_limit_requests: 20,
            rate_limit_window: Duration::from_secs(60),
            media_group_timeout: Duration::from_millis(1000),
        };

        let chat = ChatId(1);
        let mut st = StreamingState::new(chat);
        let api = FakeMessenger::new();
        let now = Instant::now();

        st.on_status_at(&cfg, &api, StatusType::Text, "hello world", Some(0), now)
            .await
            .unwrap();
        assert_eq!(
            api.sends.lock().unwrap().len(),
            3 /* progress + segment + recreated progress */
        );

        // Within throttle: no edit.
        st.on_status_at(
            &cfg,
            &api,
            StatusType::Text,
            "hello world!!!",
            Some(0),
            now + Duration::from_millis(100),
        )
        .await
        .unwrap();
        assert!(api.edits.lock().unwrap().is_empty());

        // After throttle: edit happens.
        st.on_status_at(
            &cfg,
            &api,
            StatusType::Text,
            "hello world!!!",
            Some(0),
            now + Duration::from_millis(600),
        )
        .await
        .unwrap();
        assert_eq!(api.edits.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn done_deletes_thinking_and_tool_and_sets_reaction() {
        let cfg = Config {
            telegram_bot_token: "x".to_string(),
            telegram_allowed_users: vec![1],
            claude_working_dir: "/tmp".into(),
            openai_api_key: None,
            transcription_prompt: "x".to_string(),
            transcription_available: false,
            claude_cli_path: "/usr/bin/claude".into(),
            claude_config_dir: None,
            allowed_paths: vec!["/tmp".into()],
            temp_paths: vec!["/tmp".into()],
            blocked_patterns: vec![],
            safety_prompt: "x".to_string(),
            query_timeout: Duration::from_secs(1),
            temp_dir: "/tmp".into(),
            session_file: "/tmp/s.json".into(),
            restart_file: "/tmp/r.json".into(),
            telegram_message_limit: 4096,
            telegram_safe_limit: 50,
            streaming_throttle: Duration::from_millis(500),
            button_label_max_length: 30,
            default_thinking_tokens: 0,
            thinking_keywords: vec![],
            thinking_deep_keywords: vec![],
            delete_thinking_messages: true,
            delete_tool_messages: true,
            audit_log_path: "/tmp/a.log".into(),
            audit_log_json: false,
            rate_limit_enabled: true,
            rate_limit_requests: 20,
            rate_limit_window: Duration::from_secs(60),
            media_group_timeout: Duration::from_millis(1000),
        };

        let chat = ChatId(1);
        let mut st = StreamingState::new(chat);
        let api = FakeMessenger::new();
        let now = Instant::now();

        st.on_status_at(&cfg, &api, StatusType::Thinking, "t", None, now)
            .await
            .unwrap();
        st.on_status_at(&cfg, &api, StatusType::Tool, "tool", None, now)
            .await
            .unwrap();
        st.on_status_at(&cfg, &api, StatusType::Text, "hi", Some(0), now)
            .await
            .unwrap();
        st.on_status_at(&cfg, &api, StatusType::Done, "", None, now)
            .await
            .unwrap();

        assert!(!api.deletes.lock().unwrap().is_empty());
        assert!(!api.reactions.lock().unwrap().is_empty());
    }
}
