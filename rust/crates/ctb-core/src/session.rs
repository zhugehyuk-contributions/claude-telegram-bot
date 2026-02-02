use std::sync::Arc;

use chrono::Local;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio::time::{interval, Duration, Instant};

use crate::{
    config::Config,
    errors::Error,
    formatting::{escape_html, format_tool_status},
    messaging::{port::MessagingPort, types::InlineKeyboard},
    model::{
        client::ModelClient,
        types::{ModelEvent, ProviderKind, RunRequest, RunResult, SessionRef, TokenUsage},
    },
    security::{check_command_safety, PathPolicy},
    streaming::{StatusType, StreamingState},
    utils::iso_timestamp_utc,
    Result,
};

#[derive(Debug, Default)]
struct SessionState {
    session: Option<SessionRef>,
    is_running: bool,
    stop_requested: bool,
    interrupted_by_new_message: bool,
    last_message: Option<String>,

    // Token usage parity with TS (cumulative across turns).
    session_start_time: Option<String>,
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_cache_read_tokens: u64,
    total_cache_create_tokens: u64,
    total_queries: u64,
    last_usage: Option<TokenUsage>,
}

/// High-level session manager (provider-agnostic).
///
/// Mirrors TS semantics:
/// - persists session id for `/resume`
/// - supports `/stop` and `!` interrupts
pub struct ClaudeSession {
    cfg: Arc<Config>,
    model: Arc<dyn ModelClient>,
    state: Mutex<SessionState>,
}

#[derive(Clone, Debug)]
pub struct TurnOutput {
    pub text: String,
    pub waiting_for_user: bool,
    pub usage: Option<TokenUsage>,
    pub session: Option<SessionRef>,
}

#[derive(Clone, Debug)]
pub struct SessionStats {
    pub session: Option<SessionRef>,
    pub is_running: bool,
    pub last_message: Option<String>,

    pub session_start_time: Option<String>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    pub total_cache_create_tokens: u64,
    pub total_queries: u64,
    pub last_usage: Option<TokenUsage>,
}

impl ClaudeSession {
    pub fn new(cfg: Arc<Config>, model: Arc<dyn ModelClient>) -> Self {
        Self {
            cfg,
            model,
            state: Mutex::new(SessionState::default()),
        }
    }

    pub async fn is_active(&self) -> bool {
        self.state.lock().await.session.is_some()
    }

    pub async fn is_running(&self) -> bool {
        self.state.lock().await.is_running
    }

    pub async fn mark_interrupt(&self) {
        let mut st = self.state.lock().await;
        st.interrupted_by_new_message = true;
    }

    /// Clear the stop flag without consuming the interrupt marker.
    ///
    /// Parity with TS `clearStopRequested()` used after `!` interrupts so the new
    /// message can proceed, while still suppressing the "Query stopped" message.
    pub async fn clear_stop_requested(&self) {
        let mut st = self.state.lock().await;
        st.stop_requested = false;
    }

    pub async fn consume_interrupt_flag(&self) -> bool {
        let mut st = self.state.lock().await;
        let was = st.interrupted_by_new_message;
        st.interrupted_by_new_message = false;
        if was {
            st.stop_requested = false;
        }
        was
    }

    pub async fn stop(&self) -> Result<bool> {
        let mut st = self.state.lock().await;
        if !st.is_running {
            return Ok(false);
        }
        st.stop_requested = true;
        drop(st);

        self.model.cancel().await?;
        Ok(true)
    }

    pub async fn kill(&self) -> Result<()> {
        let mut st = self.state.lock().await;
        st.session = None;
        st.is_running = false;
        st.stop_requested = false;
        st.interrupted_by_new_message = false;
        st.last_message = None;
        st.session_start_time = None;
        st.total_input_tokens = 0;
        st.total_output_tokens = 0;
        st.total_cache_read_tokens = 0;
        st.total_cache_create_tokens = 0;
        st.total_queries = 0;
        st.last_usage = None;
        Ok(())
    }

    pub async fn set_last_message(&self, message: String) {
        let mut st = self.state.lock().await;
        st.last_message = Some(message);
    }

    pub async fn last_message(&self) -> Option<String> {
        self.state.lock().await.last_message.clone()
    }

    pub async fn resume_last(&self) -> Result<(bool, String)> {
        let Some(data) = load_session_file(&self.cfg.session_file)? else {
            return Ok((false, "No saved session found".to_string()));
        };

        // Working dir check (parity with TS).
        if data.working_dir != self.cfg.claude_working_dir.to_string_lossy() {
            return Ok((
                false,
                format!("Session was for different directory: {}", data.working_dir),
            ));
        }

        let provider = match data.provider.as_str() {
            "claude_cli" => ProviderKind::ClaudeCli,
            _ => ProviderKind::ClaudeCli, // default for now
        };

        let mut st = self.state.lock().await;
        st.session = Some(SessionRef {
            provider,
            id: data.session_id.clone(),
        });
        Ok((
            true,
            format!(
                "Resumed session `{}` (saved at {})",
                short_id(&data.session_id),
                data.saved_at
            ),
        ))
    }

    pub async fn stats(&self) -> SessionStats {
        let st = self.state.lock().await;
        SessionStats {
            session: st.session.clone(),
            is_running: st.is_running,
            last_message: st.last_message.clone(),
            session_start_time: st.session_start_time.clone(),
            total_input_tokens: st.total_input_tokens,
            total_output_tokens: st.total_output_tokens,
            total_cache_read_tokens: st.total_cache_read_tokens,
            total_cache_create_tokens: st.total_cache_create_tokens,
            total_queries: st.total_queries,
            last_usage: st.last_usage.clone(),
        }
    }

    pub async fn send_message_streaming(
        &self,
        chat_id: crate::domain::ChatId,
        prompt: &str,
        on_event: &mut (dyn FnMut(ModelEvent) -> Result<()> + Send),
    ) -> Result<RunResult> {
        let (resume, is_new_session) = {
            let st = self.state.lock().await;
            (st.session.clone(), st.session.is_none())
        };

        // Inject date/time at session start (parity with TS).
        let mut prompt_to_send = prompt.to_string();
        if is_new_session {
            let now = Local::now().format("%A, %B %d, %Y, %H:%M %Z").to_string();
            prompt_to_send = format!("[Current date/time: {now}]\n\n{prompt_to_send}");
        }

        // Thinking token selection (keyword triggers parity).
        let max_thinking_tokens = thinking_tokens_for_prompt(&self.cfg, &prompt_to_send);

        // MCP config is optional; if present we materialize an interpolated JSON file and inject
        // the current chat context so `ask_user` can target the right conversation.
        let mcp_config_path = prepare_mcp_config_for_chat(&self.cfg, chat_id)?;

        let req = RunRequest {
            prompt: prompt_to_send,
            cwd: self.cfg.claude_working_dir.clone(),
            add_dirs: self.cfg.allowed_paths.clone(),
            mcp_config_path,
            system_prompt: Some(self.cfg.safety_prompt.clone()),
            append_system_prompt: None,
            resume,
            fork_session: false,
            max_thinking_tokens: Some(max_thinking_tokens),
        };

        {
            let mut st = self.state.lock().await;
            if st.stop_requested {
                st.stop_requested = false;
                return Err(Error::External(
                    "Query cancelled before starting".to_string(),
                ));
            }
            st.is_running = true;
        }

        let result = self.model.run(req, on_event).await;

        {
            let mut st = self.state.lock().await;
            st.is_running = false;
            st.stop_requested = false;
        }

        let result = result?;
        if let Some(session) = &result.session {
            // Persist + keep in memory for subsequent resume.
            {
                let mut st = self.state.lock().await;
                st.session = Some(session.clone());
            }
            save_session_file(
                &self.cfg.session_file,
                &SessionFileData {
                    provider: "claude_cli".to_string(),
                    session_id: session.id.clone(),
                    saved_at: iso_timestamp_utc(),
                    working_dir: self.cfg.claude_working_dir.to_string_lossy().to_string(),
                },
            )?;
        }

        // Accumulate token usage (parity with TS).
        if let Some(u) = &result.usage {
            self.accumulate_usage(u).await;
        }

        Ok(result)
    }

    /// Higher-level helper: run a prompt and stream user-visible updates to a messenger.
    ///
    /// This implements the TS behavior of:
    /// - thinking/tool/text/segment_end/done events
    /// - tool safety checks for Bash + file ops
    /// - ask_user trigger hook (scans `/tmp/ask-user-*.json` and sends inline keyboard)
    pub async fn send_message_to_chat(
        &self,
        chat_id: crate::domain::ChatId,
        prompt: &str,
        messenger: Arc<dyn MessagingPort>,
    ) -> Result<TurnOutput> {
        let (tx, mut rx) = mpsc::unbounded_channel::<ModelEvent>();

        // Spawn event processor which owns the streaming state and ticks the spinner.
        let cfg = self.cfg.clone();
        let model = self.model.clone();
        let messenger_for_task = messenger.clone();
        let processor = tokio::spawn(async move {
            let mut pipeline = EventPipeline::new(cfg, model, messenger_for_task, chat_id);
            let mut tick = interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                  _ = tick.tick() => {
                    pipeline.tick_progress().await?;
                  }
                  maybe = rx.recv() => {
                    let Some(ev) = maybe else { break; };
                    pipeline.handle_event(ev).await?;
                    if pipeline.should_stop_early() {
                      break;
                    }
                  }
                }
            }
            pipeline.finish().await
        });

        let mut on_event = |ev: ModelEvent| -> Result<()> {
            tx.send(ev)
                .map_err(|_| Error::External("event processor stopped".to_string()))?;
            Ok(())
        };

        // Run the model while the processor consumes events.
        let model_result = self
            .send_message_streaming(chat_id, prompt, &mut on_event)
            .await;

        // Wait for processor completion and use its output as source-of-truth for streaming semantics.
        let pipeline_out = processor
            .await
            .map_err(|e| Error::External(format!("event processor task failed: {e}")))??;

        // Persist observed session even if the model was cancelled (parity with TS which saves
        // session_id as soon as it's seen).
        if let Some(session) = pipeline_out.session.clone() {
            self.persist_observed_session(&session).await?;
        }

        // If the model errored due to our own ask_user cancellation, suppress it.
        if pipeline_out.waiting_for_user {
            return Ok(pipeline_out);
        }

        // Otherwise propagate the model error if present.
        match model_result {
            Ok(_) => Ok(pipeline_out),
            Err(e) => Err(e),
        }
    }

    async fn persist_observed_session(&self, session: &SessionRef) -> Result<()> {
        // Keep in memory for subsequent `/resume`.
        {
            let mut st = self.state.lock().await;
            if st.session.is_none() {
                st.session = Some(session.clone());
            }
        }

        // Persist for process restarts.
        save_session_file(
            &self.cfg.session_file,
            &SessionFileData {
                provider: "claude_cli".to_string(),
                session_id: session.id.clone(),
                saved_at: iso_timestamp_utc(),
                working_dir: self.cfg.claude_working_dir.to_string_lossy().to_string(),
            },
        )?;
        Ok(())
    }

    async fn accumulate_usage(&self, u: &TokenUsage) {
        let mut st = self.state.lock().await;
        if st.session_start_time.is_none() {
            st.session_start_time = Some(iso_timestamp_utc());
        }

        st.total_input_tokens += u.input_tokens;
        st.total_output_tokens += u.output_tokens;
        st.total_cache_read_tokens += u.cache_read_input_tokens;
        st.total_cache_create_tokens += u.cache_creation_input_tokens;
        st.total_queries += 1;
        st.last_usage = Some(u.clone());
    }
}

fn thinking_tokens_for_prompt(cfg: &Config, prompt: &str) -> u32 {
    let lower = prompt.to_lowercase();
    if cfg
        .thinking_deep_keywords
        .iter()
        .any(|k| !k.is_empty() && lower.contains(k))
    {
        return 50_000;
    }
    if cfg
        .thinking_keywords
        .iter()
        .any(|k| !k.is_empty() && lower.contains(k))
    {
        return 10_000;
    }
    cfg.default_thinking_tokens
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn prepare_mcp_config_for_chat(
    cfg: &Config,
    chat_id: crate::domain::ChatId,
) -> Result<Option<std::path::PathBuf>> {
    let repo_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    // Provide a stable interpolation target for MCP configs (e.g. `${CTB_REPO_ROOT}/...`).
    // We set it once and keep it stable for the lifetime of the process.
    if std::env::var("CTB_REPO_ROOT")
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        std::env::set_var("CTB_REPO_ROOT", repo_root.to_string_lossy().to_string());
    }
    let base = repo_root.join("mcp-config.json");
    if !base.exists() {
        return Ok(None);
    }

    let mut servers = crate::mcp_config::load_mcp_servers(&base)?;
    if servers.is_empty() {
        return Ok(None);
    }

    // Provide chat context for ask_user MCP so it can target the correct Telegram conversation.
    if let Some(crate::mcp_config::McpServerConfig::Stdio { env, .. }) = servers.get_mut("ask-user")
    {
        env.insert("TELEGRAM_CHAT_ID".to_string(), chat_id.0.to_string());
    }

    let pid = std::process::id();
    let path = cfg
        .temp_dir
        .join(format!("mcp-config-{}-{pid}.json", chat_id.0));
    crate::mcp_config::write_mcp_servers_json(&path, &servers)?;
    Ok(Some(path))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SessionFileData {
    provider: String,
    session_id: String,
    saved_at: String,
    working_dir: String,
}

fn load_session_file(path: &std::path::Path) -> Result<Option<SessionFileData>> {
    if !path.exists() {
        return Ok(None);
    }
    let txt = std::fs::read_to_string(path)?;
    if txt.trim().is_empty() {
        return Ok(None);
    }
    let data: SessionFileData = serde_json::from_str(&txt)?;
    Ok(Some(data))
}

fn save_session_file(path: &std::path::Path, data: &SessionFileData) -> Result<()> {
    let txt = serde_json::to_string(data)?;
    std::fs::write(path, txt)?;
    Ok(())
}

struct EventPipeline {
    cfg: Arc<Config>,
    model: Arc<dyn ModelClient>,
    messenger: Arc<dyn MessagingPort>,
    stream: StreamingState,
    paths: PathPolicy,

    response_parts: Vec<String>,
    current_segment_id: u32,
    current_segment_text: String,
    last_snapshot_text: String,
    last_text_emit: Option<Instant>,

    observed_session: Option<SessionRef>,
    last_usage: Option<TokenUsage>,
    ask_user_triggered: bool,
    ask_user_buttons_sent: bool,
    final_result_text: Option<String>,
}

impl EventPipeline {
    fn new(
        cfg: Arc<Config>,
        model: Arc<dyn ModelClient>,
        messenger: Arc<dyn MessagingPort>,
        chat_id: crate::domain::ChatId,
    ) -> Self {
        let paths = PathPolicy {
            allowed_paths: cfg.allowed_paths.clone(),
            temp_paths: cfg.temp_paths.clone(),
            home_dir: std::env::var_os("HOME").map(std::path::PathBuf::from),
            base_dir: Some(cfg.claude_working_dir.clone()),
        };

        Self {
            cfg,
            model,
            messenger,
            stream: StreamingState::new(chat_id),
            paths,
            response_parts: Vec::new(),
            current_segment_id: 0,
            current_segment_text: String::new(),
            last_snapshot_text: String::new(),
            last_text_emit: None,
            observed_session: None,
            last_usage: None,
            ask_user_triggered: false,
            ask_user_buttons_sent: false,
            final_result_text: None,
        }
    }

    fn should_stop_early(&self) -> bool {
        self.ask_user_triggered
    }

    async fn tick_progress(&mut self) -> Result<()> {
        self.stream.tick_progress(self.messenger.as_ref()).await
    }

    async fn handle_event(&mut self, ev: ModelEvent) -> Result<()> {
        let raw = match &ev {
            ModelEvent::SystemInit { raw }
            | ModelEvent::Assistant { raw }
            | ModelEvent::Tool { raw }
            | ModelEvent::Result { raw }
            | ModelEvent::Unknown { raw } => raw,
        };
        self.observe_session_id(raw);

        match ev {
            ModelEvent::Assistant { raw } => self.handle_assistant_raw(&raw).await,
            ModelEvent::Result { raw } => {
                self.handle_result_raw(&raw);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn observe_session_id(&mut self, raw: &serde_json::Value) {
        if self.observed_session.is_some() {
            return;
        }
        let Some(id) = raw.get("session_id").and_then(|v| v.as_str()) else {
            return;
        };
        self.observed_session = Some(SessionRef {
            provider: ProviderKind::ClaudeCli,
            id: id.to_string(),
        });
    }

    fn handle_result_raw(&mut self, raw: &serde_json::Value) {
        if let Some(result) = raw.get("result").and_then(|v| v.as_str()) {
            self.final_result_text = Some(result.to_string());
        }
        if let Some(usage) = raw.get("usage") {
            self.last_usage = parse_usage(usage);
        }
    }

    async fn handle_assistant_raw(&mut self, raw: &serde_json::Value) -> Result<()> {
        let Some(content) = raw
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            return Ok(());
        };

        let all_text = content
            .iter()
            .all(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"));

        if all_text {
            let snapshot = content
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<String>();
            self.handle_text_snapshot(&snapshot).await?;
            return Ok(());
        }

        for block in content {
            let Some(ty) = block.get("type").and_then(|t| t.as_str()) else {
                continue;
            };
            match ty {
                "thinking" => {
                    if let Some(t) = block.get("thinking").and_then(|t| t.as_str()) {
                        self.stream
                            .on_status(
                                &self.cfg,
                                self.messenger.as_ref(),
                                StatusType::Thinking,
                                t,
                                None,
                            )
                            .await?;
                    }
                }
                "tool_use" => {
                    self.handle_tool_use(block).await?;
                }
                "text" => {
                    if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                        self.append_text_delta(t).await?;
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }

    async fn handle_text_snapshot(&mut self, snapshot: &str) -> Result<()> {
        if snapshot.starts_with(&self.last_snapshot_text) {
            let delta = &snapshot[self.last_snapshot_text.len()..];
            if !delta.is_empty() {
                self.append_text_delta(delta).await?;
            }
            self.last_snapshot_text = snapshot.to_string();
            return Ok(());
        }

        // Fallback: treat as delta-like (best-effort). Do not reset segment state mid-turn.
        if !snapshot.is_empty() {
            self.append_text_delta(snapshot).await?;
        }
        self.last_snapshot_text = self.current_segment_text.clone();
        Ok(())
    }

    async fn append_text_delta(&mut self, text: &str) -> Result<()> {
        self.response_parts.push(text.to_string());
        self.current_segment_text.push_str(text);
        self.last_snapshot_text.push_str(text);

        let now = Instant::now();
        let should_emit = self.current_segment_text.len() > 20
            && self
                .last_text_emit
                .map(|t| now.duration_since(t) > self.cfg.streaming_throttle)
                .unwrap_or(true);

        if should_emit {
            self.stream
                .on_status(
                    &self.cfg,
                    self.messenger.as_ref(),
                    StatusType::Text,
                    &self.current_segment_text,
                    Some(self.current_segment_id),
                )
                .await?;
            self.last_text_emit = Some(now);
        }

        Ok(())
    }

    async fn handle_tool_use(&mut self, block: &serde_json::Value) -> Result<()> {
        let tool_name = block.get("name").and_then(|v| v.as_str()).unwrap_or("Tool");
        let tool_input = block.get("input").unwrap_or(&serde_json::Value::Null);

        // Safety check for Bash.
        if tool_name.eq_ignore_ascii_case("Bash") {
            let cmd = tool_input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let (ok, reason) = check_command_safety(cmd, &self.cfg.blocked_patterns, &self.paths);
            if !ok {
                let _ = self.model.cancel().await;
                let msg = format!("BLOCKED: {}", escape_html(&reason));
                let _ = self
                    .stream
                    .on_status(
                        &self.cfg,
                        self.messenger.as_ref(),
                        StatusType::Tool,
                        &msg,
                        None,
                    )
                    .await;
                return Err(Error::Security(format!("Unsafe command blocked: {reason}")));
            }
        }

        // Safety check for file operations.
        if ["Read", "Write", "Edit"]
            .iter()
            .any(|t| tool_name.eq_ignore_ascii_case(t))
        {
            let file_path = tool_input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if !file_path.is_empty() {
                let is_tmp_or_claude_read = tool_name.eq_ignore_ascii_case("Read")
                    && (file_path.contains("/.claude/")
                        || self
                            .cfg
                            .temp_paths
                            .iter()
                            .any(|p| file_path.starts_with(&*p.to_string_lossy())));

                if !is_tmp_or_claude_read && !self.paths.is_path_allowed(file_path) {
                    let _ = self.model.cancel().await;
                    let msg = format!("Access denied: {}", escape_html(file_path));
                    let _ = self
                        .stream
                        .on_status(
                            &self.cfg,
                            self.messenger.as_ref(),
                            StatusType::Tool,
                            &msg,
                            None,
                        )
                        .await;
                    return Err(Error::Security(format!("File access blocked: {file_path}")));
                }
            }
        }

        // Segment ends when tool starts.
        if !self.current_segment_text.is_empty() {
            self.stream
                .on_status(
                    &self.cfg,
                    self.messenger.as_ref(),
                    StatusType::SegmentEnd,
                    &self.current_segment_text,
                    Some(self.current_segment_id),
                )
                .await?;
            self.current_segment_id += 1;
            self.current_segment_text.clear();
            self.last_snapshot_text.clear();
            self.last_text_emit = None;
        }

        // ask_user MCP tool: don't spam tool status; instead send inline keyboard if request file is present.
        if is_ask_user_tool(tool_name) {
            self.ask_user_triggered = true;

            // Give MCP server a moment to write the request file, then retry a few times.
            tokio::time::sleep(Duration::from_millis(200)).await;
            for attempt in 0..3 {
                if check_pending_ask_user_requests(&*self.messenger, &self.cfg, self.stream.chat_id)
                    .await?
                {
                    self.ask_user_buttons_sent = true;
                    break;
                }
                if attempt < 2 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }

            // Stop the current run so the bot can wait for the user's callback response.
            let _ = self.model.cancel().await;
            return Ok(());
        }

        let tool_display = format_tool_status(tool_name, tool_input);
        self.stream
            .on_status(
                &self.cfg,
                self.messenger.as_ref(),
                StatusType::Tool,
                &tool_display,
                None,
            )
            .await?;

        Ok(())
    }

    async fn finish(mut self) -> Result<TurnOutput> {
        // If ask_user was triggered, return early: user will respond via callback.
        if self.ask_user_triggered {
            self.stream
                .on_status(
                    &self.cfg,
                    self.messenger.as_ref(),
                    StatusType::Done,
                    "",
                    None,
                )
                .await?;
            return Ok(TurnOutput {
                text: if self.ask_user_buttons_sent {
                    "[Waiting for user selection]".to_string()
                } else {
                    "[Waiting for user selection (no request file found yet)]".to_string()
                },
                waiting_for_user: true,
                usage: self.last_usage,
                session: self.observed_session,
            });
        }

        if !self.current_segment_text.is_empty() {
            self.stream
                .on_status(
                    &self.cfg,
                    self.messenger.as_ref(),
                    StatusType::SegmentEnd,
                    &self.current_segment_text,
                    Some(self.current_segment_id),
                )
                .await?;
        }

        self.stream
            .on_status(
                &self.cfg,
                self.messenger.as_ref(),
                StatusType::Done,
                "",
                None,
            )
            .await?;

        let joined = if !self.response_parts.is_empty() {
            self.response_parts.join("")
        } else {
            self.final_result_text
                .unwrap_or_else(|| "No response from Claude.".to_string())
        };

        Ok(TurnOutput {
            text: joined,
            waiting_for_user: false,
            usage: self.last_usage,
            session: self.observed_session,
        })
    }
}

fn is_ask_user_tool(tool_name: &str) -> bool {
    tool_name.starts_with("mcp__ask-user") || tool_name == "AskUserQuestion"
}

async fn check_pending_ask_user_requests(
    messenger: &dyn MessagingPort,
    cfg: &Config,
    chat_id: crate::domain::ChatId,
) -> Result<bool> {
    let dir = std::path::Path::new("/tmp");
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Ok(false);
    };

    let mut any_sent = false;
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().to_string();
        if !name.starts_with("ask-user-") || !name.ends_with(".json") {
            continue;
        }

        let path = ent.path();
        let Ok(txt) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&txt) else {
            continue;
        };

        if v.get("status").and_then(|s| s.as_str()) != Some("pending") {
            continue;
        }
        let file_chat = v
            .get("chat_id")
            .and_then(|c| {
                if let Some(n) = c.as_i64() {
                    return Some(n);
                }
                c.as_str().and_then(|s| s.parse::<i64>().ok())
            })
            .unwrap_or_default();
        if file_chat != chat_id.0 {
            continue;
        }

        let question = v
            .get("question")
            .and_then(|q| q.as_str())
            .unwrap_or("Please choose:");
        let request_id = v.get("request_id").and_then(|r| r.as_str()).unwrap_or("");
        let options = v
            .get("options")
            .and_then(|o| o.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default();

        if request_id.is_empty() || options.is_empty() {
            continue;
        }

        let keyboard =
            InlineKeyboard::one_per_row(request_id, &options, cfg.button_label_max_length);
        let _ = messenger
            .send_inline_keyboard(chat_id, &format!("❓ {}", escape_html(question)), keyboard)
            .await?;

        // Mark as sent.
        v["status"] = serde_json::Value::String("sent".to_string());
        let _ = std::fs::write(&path, serde_json::to_string(&v)?);
        any_sent = true;
    }

    Ok(any_sent)
}

fn parse_usage(v: &serde_json::Value) -> Option<TokenUsage> {
    let get = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    Some(TokenUsage {
        input_tokens: get("input_tokens"),
        output_tokens: get("output_tokens"),
        cache_read_input_tokens: get("cache_read_input_tokens"),
        cache_creation_input_tokens: get("cache_creation_input_tokens"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::MessageRef;
    use crate::model::types::{ModelCapabilities, ProviderKind, RunRequest, RunResult};
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeModel {
        cancels: AtomicUsize,
    }

    impl FakeModel {
        fn cancel_calls(&self) -> usize {
            self.cancels.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ModelClient for FakeModel {
        fn provider(&self) -> ProviderKind {
            ProviderKind::ClaudeCli
        }

        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities {
                supports_streaming: true,
                supports_tools: true,
                supports_vision: true,
                supports_thinking: true,
                supports_mcp: true,
            }
        }

        async fn run(
            &self,
            _req: RunRequest,
            _on_event: &mut (dyn FnMut(ModelEvent) -> Result<()> + Send),
        ) -> Result<RunResult> {
            Err(Error::External(
                "FakeModel::run not implemented for tests".to_string(),
            ))
        }

        async fn cancel(&self) -> Result<()> {
            self.cancels.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeMessenger {
        next_id: Mutex<i32>,
        sends: Mutex<Vec<String>>,
        keyboards: Mutex<Vec<(crate::domain::ChatId, String, InlineKeyboard)>>,
    }

    impl FakeMessenger {
        fn alloc(&self, chat_id: crate::domain::ChatId) -> MessageRef {
            use crate::domain::MessageId;
            let mut guard = self.next_id.lock().unwrap();
            if *guard == 0 {
                *guard = 1;
            }
            let id = *guard;
            *guard += 1;
            MessageRef {
                chat_id,
                message_id: MessageId(id),
            }
        }

        fn sent_html(&self) -> Vec<String> {
            self.sends.lock().unwrap().clone()
        }

        fn keyboard_sends(&self) -> Vec<(crate::domain::ChatId, String, InlineKeyboard)> {
            self.keyboards.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl MessagingPort for FakeMessenger {
        fn capabilities(&self) -> crate::messaging::types::MessagingCapabilities {
            crate::messaging::types::MessagingCapabilities {
                supports_html: true,
                supports_edit: true,
                supports_reactions: true,
                supports_chat_actions: true,
                supports_inline_keyboards: true,
                max_message_len: 4096,
            }
        }

        async fn send_html(
            &self,
            chat_id: crate::domain::ChatId,
            html: &str,
        ) -> Result<MessageRef> {
            self.sends.lock().unwrap().push(html.to_string());
            Ok(self.alloc(chat_id))
        }

        async fn edit_html(&self, _msg: MessageRef, _html: &str) -> Result<()> {
            Ok(())
        }

        async fn delete_message(&self, _msg: MessageRef) -> Result<()> {
            Ok(())
        }

        async fn send_chat_action(
            &self,
            _chat_id: crate::domain::ChatId,
            _action: crate::messaging::types::ChatAction,
        ) -> Result<()> {
            Ok(())
        }

        async fn set_reaction(&self, _msg: MessageRef, _emoji: &str) -> Result<()> {
            Ok(())
        }

        async fn send_inline_keyboard(
            &self,
            chat_id: crate::domain::ChatId,
            text: &str,
            keyboard: InlineKeyboard,
        ) -> Result<MessageRef> {
            self.keyboards
                .lock()
                .unwrap()
                .push((chat_id, text.to_string(), keyboard));
            Ok(self.alloc(chat_id))
        }

        async fn answer_callback_query(
            &self,
            _callback_id: &str,
            _text: Option<&str>,
        ) -> Result<()> {
            Ok(())
        }
    }

    fn test_config() -> Arc<Config> {
        use std::time::Duration;
        Arc::new(Config {
            telegram_bot_token: "x".to_string(),
            telegram_allowed_users: vec![1],
            claude_working_dir: "/tmp".into(),
            openai_api_key: None,
            transcription_prompt: "x".to_string(),
            transcription_available: false,
            claude_cli_path: "/usr/bin/claude".into(),
            claude_config_dir: None,
            allowed_paths: vec!["/tmp".into()],
            temp_paths: vec!["/tmp/".into()],
            blocked_patterns: vec!["rm -rf /".to_string()],
            safety_prompt: "x".to_string(),
            query_timeout: Duration::from_secs(1),
            temp_dir: "/tmp".into(),
            session_file: "/tmp/claude-telegram-session.json".into(),
            restart_file: "/tmp/claude-telegram-restart.json".into(),
            telegram_message_limit: 4096,
            telegram_safe_limit: 4000,
            streaming_throttle: Duration::from_millis(0),
            button_label_max_length: 30,
            default_thinking_tokens: 0,
            thinking_keywords: vec![],
            thinking_deep_keywords: vec![],
            delete_thinking_messages: false,
            delete_tool_messages: false,
            audit_log_path: "/tmp/a.log".into(),
            audit_log_json: false,
            rate_limit_enabled: false,
            rate_limit_requests: 20,
            rate_limit_window: Duration::from_secs(60),
            media_group_timeout: Duration::from_millis(1000),
        })
    }

    fn assistant_raw(session_id: &str, blocks: Vec<serde_json::Value>) -> serde_json::Value {
        json!({
          "session_id": session_id,
          "message": { "content": blocks }
        })
    }

    #[tokio::test]
    async fn text_snapshot_prefix_diff_dedupes() {
        let cfg = test_config();
        let model = Arc::new(FakeModel::default());
        let messenger = Arc::new(FakeMessenger::default());
        let mut p = EventPipeline::new(cfg, model, messenger, crate::domain::ChatId(1));

        p.handle_event(ModelEvent::Assistant {
            raw: assistant_raw("s1", vec![json!({"type":"text","text":"hello"})]),
        })
        .await
        .unwrap();
        p.handle_event(ModelEvent::Assistant {
            raw: assistant_raw("s1", vec![json!({"type":"text","text":"hello world"})]),
        })
        .await
        .unwrap();

        assert_eq!(p.current_segment_text, "hello world");
        assert_eq!(p.response_parts.join(""), "hello world");
    }

    #[tokio::test]
    async fn tool_use_splits_segments_and_formats_status() {
        let cfg = test_config();
        let model = Arc::new(FakeModel::default());
        let messenger = Arc::new(FakeMessenger::default());
        let mut p = EventPipeline::new(cfg, model, messenger.clone(), crate::domain::ChatId(1));

        p.handle_event(ModelEvent::Assistant {
            raw: assistant_raw("s1", vec![json!({"type":"text","text":"hi"})]),
        })
        .await
        .unwrap();

        p.handle_event(ModelEvent::Assistant {
      raw: assistant_raw(
        "s1",
        vec![json!({"type":"tool_use","name":"Write","input":{"file_path":"/tmp/x.txt","content":"hello"}})],
      ),
    })
    .await
    .unwrap();

        assert_eq!(p.current_segment_id, 1);
        assert!(p.current_segment_text.is_empty());

        let sent = messenger.sent_html();
        assert!(
            sent.iter().any(|s| s.contains("hi")),
            "expected a segment_end message containing hi"
        );
        assert!(
            sent.iter().any(|s| s.contains("Writing")),
            "expected a tool status message for Write"
        );
    }

    #[tokio::test]
    async fn bash_unsafe_command_is_blocked_and_cancels() {
        let cfg = test_config();
        let model = Arc::new(FakeModel::default());
        let messenger = Arc::new(FakeMessenger::default());
        let mut p = EventPipeline::new(
            cfg,
            model.clone(),
            messenger.clone(),
            crate::domain::ChatId(1),
        );

        let err = p
      .handle_event(ModelEvent::Assistant {
        raw: assistant_raw(
          "s1",
          vec![json!({"type":"tool_use","name":"Bash","input":{"command":"rm /etc/passwd"}})],
        ),
      })
      .await
      .unwrap_err();

        assert!(matches!(err, Error::Security(_)));
        assert_eq!(model.cancel_calls(), 1);
        assert!(
            messenger.sent_html().iter().any(|s| s.contains("BLOCKED:")),
            "expected a BLOCKED tool message"
        );
    }

    #[tokio::test]
    async fn ask_user_scans_tmp_sends_keyboard_and_marks_sent() {
        let cfg = test_config();
        let model = Arc::new(FakeModel::default());
        let messenger = Arc::new(FakeMessenger::default());

        let path = std::path::Path::new("/tmp/ask-user-test.json");
        let payload = json!({
          "status": "pending",
          "chat_id": 1,
          "question": "Pick one",
          "options": ["a", "b"],
          "request_id": "req123"
        });
        std::fs::write(path, serde_json::to_string(&payload).unwrap()).unwrap();

        let mut p = EventPipeline::new(
            cfg,
            model.clone(),
            messenger.clone(),
            crate::domain::ChatId(1),
        );
        p.handle_event(ModelEvent::Assistant {
            raw: assistant_raw(
                "s1",
                vec![json!({"type":"tool_use","name":"mcp__ask-user__askUser","input":{}})],
            ),
        })
        .await
        .unwrap();

        let out = p.finish().await.unwrap();
        assert!(out.waiting_for_user);
        assert_eq!(model.cancel_calls(), 1);

        let keyboards = messenger.keyboard_sends();
        assert!(!keyboards.is_empty(), "expected an inline keyboard send");

        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(updated.get("status").and_then(|s| s.as_str()), Some("sent"));
    }

    #[tokio::test]
    async fn parses_doc_fixtures_into_pipeline_output() {
        let cfg = test_config();
        let model = Arc::new(FakeModel::default());
        let messenger = Arc::new(FakeMessenger::default());

        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../docs/rust-port/fixtures");

        for (fixture_name, expected) in [
            (
                "claude-stream-json.sample.jsonl",
                "API Error: Connection error.",
            ),
            (
                "claude-stream-json.invalid-api-key.jsonl",
                "Invalid API key · Fix external API key",
            ),
            ("claude-stream-json.synthetic-tool-use.jsonl", "done"),
        ] {
            let txt = std::fs::read_to_string(base.join(fixture_name)).unwrap();

            let mut p = EventPipeline::new(
                cfg.clone(),
                model.clone(),
                messenger.clone(),
                crate::domain::ChatId(1),
            );
            for line in txt.lines().filter(|l| !l.trim().is_empty()) {
                let raw: serde_json::Value = serde_json::from_str(line).unwrap();
                let ty = raw.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let ev = match ty {
                    "system" => ModelEvent::SystemInit { raw },
                    "assistant" => ModelEvent::Assistant { raw },
                    "result" => ModelEvent::Result { raw },
                    _ => ModelEvent::Unknown { raw },
                };
                p.handle_event(ev).await.unwrap();
            }
            let out = p.finish().await.unwrap();
            assert!(!out.waiting_for_user);
            assert!(
                out.text.contains(expected),
                "fixture {fixture_name} expected text to contain: {expected}, got: {}",
                out.text
            );
        }
    }
}
