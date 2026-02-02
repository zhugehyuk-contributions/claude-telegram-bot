//! Claude CLI adapter (planned primary model backend).
//!
//! Streaming implementation for `claude -p --output-format stream-json`.

use async_trait::async_trait;

use std::process::Stdio;

use std::collections::VecDeque;

use ctb_core::{
    errors::Error,
    model::{
        client::{ClaudeCliPromptAdapter, ModelClient},
        types::{
            ClaudeCliConfig, ModelCapabilities, ModelEvent, ProviderKind, RunRequest, RunResult,
            SessionRef, TokenUsage,
        },
    },
    Result,
};

use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;

const STDERR_TAIL_MAX_BYTES: usize = 16 * 1024;
const STDERR_TAIL_MAX_LINES: usize = 200;

#[derive(Clone, Debug)]
pub struct ClaudeCliClient {
    cfg: ClaudeCliConfig,
    child: std::sync::Arc<Mutex<Option<tokio::process::Child>>>,
    cancel: std::sync::Arc<Mutex<Option<CancellationToken>>>,
}

#[derive(Clone, Debug, Default)]
struct StderrTail {
    lines: VecDeque<String>,
    bytes: usize,
}

impl StderrTail {
    fn push_line(&mut self, line: String) {
        // +1 for the '\n' we join with later.
        self.bytes = self.bytes.saturating_add(line.len() + 1);
        self.lines.push_back(line);

        while self.lines.len() > STDERR_TAIL_MAX_LINES || self.bytes > STDERR_TAIL_MAX_BYTES {
            if let Some(front) = self.lines.pop_front() {
                self.bytes = self.bytes.saturating_sub(front.len() + 1);
            } else {
                break;
            }
        }
    }

    fn snapshot(&self) -> String {
        self.lines.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

impl ClaudeCliClient {
    pub fn new(cfg: ClaudeCliConfig) -> Self {
        Self {
            cfg,
            child: std::sync::Arc::new(Mutex::new(None)),
            cancel: std::sync::Arc::new(Mutex::new(None)),
        }
    }
}

async fn clear_cancel_token(cancel: &std::sync::Arc<Mutex<Option<CancellationToken>>>) {
    let mut guard = cancel.lock().await;
    *guard = None;
}

#[async_trait]
impl ModelClient for ClaudeCliClient {
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
        req: RunRequest,
        on_event: &mut (dyn FnMut(ModelEvent) -> Result<()> + Send),
    ) -> Result<RunResult> {
        // Cancel any existing run first. If we can't kill/reap it, fail fast rather than
        // spawning a second long-running CLI process.
        self.cancel().await?;

        let token = CancellationToken::new();
        {
            let mut guard = self.cancel.lock().await;
            *guard = Some(token.clone());
        }

        let adapter = ClaudeCliPromptAdapter {
            cfg: self.cfg.clone(),
        };
        let inv = adapter.build_invocation(&req);

        let mut cmd = Command::new(&inv.program);
        cmd.args(&inv.args)
            .current_dir(&inv.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &inv.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn()?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::External("claude stdout was not captured".to_string()))?;
        let stderr = child.stderr.take();
        let stderr_tail: std::sync::Arc<Mutex<StderrTail>> =
            std::sync::Arc::new(Mutex::new(StderrTail::default()));

        // Store child so `cancel()` can kill it.
        {
            let mut guard = self.child.lock().await;
            *guard = Some(child);
        }

        // Drain stderr in background to avoid blocking on a full pipe.
        if let Some(stderr) = stderr {
            let tail = stderr_tail.clone();
            tokio::spawn(async move {
                let mut r = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = r.next_line().await {
                    tail.lock().await.push_line(line);
                }
            });
        }

        let mut session: Option<SessionRef> = None;
        let mut final_text: Option<String> = None;
        let mut final_is_error: Option<bool> = None;
        let mut final_usage: Option<TokenUsage> = None;

        let mut reader = BufReader::new(stdout).lines();
        loop {
            tokio::select! {
              _ = token.cancelled() => {
                if let Err(e) = self.kill_child().await {
                  return Err(Error::External(format!("Cancelled (failed to kill claude process: {e})")));
                }
                return Err(Error::External("Cancelled".to_string()));
              }
              line = reader.next_line() => {
                let line = match line {
                  Ok(v) => v,
                  Err(e) => {
                    let kill = self.kill_child().await;
                    if let Err(kill_e) = kill {
                      return Err(Error::External(format!("claude stdout read failed: {e} (also failed to kill claude process: {kill_e})")));
                    }
                    return Err(Error::Io(e));
                  }
                };
                let Some(line) = line else { break; };

                let value: serde_json::Value = match serde_json::from_str(&line) {
                  Ok(v) => v,
                  Err(e) => {
                    let stderr = stderr_tail.lock().await.snapshot();
                    let line_preview = truncate_text(&line, 500);
                    let kill = self.kill_child().await;
                    let mut msg = format!(
                      "claude stream-json parse failed: {e}\nstdout line: {line_preview}"
                    );
                    if !stderr.trim().is_empty() {
                      msg.push_str("\nstderr (tail):\n");
                      msg.push_str(&stderr);
                    }
                    if let Err(kill_e) = kill {
                      msg.push_str(&format!("\nfailed to kill claude process: {kill_e}"));
                    }
                    return Err(Error::External(msg));
                  }
                };

                // Extract session id opportunistically.
                if session.is_none() {
                  if let Some(id) = value.get("session_id").and_then(|v| v.as_str()) {
                    session = Some(SessionRef { provider: ProviderKind::ClaudeCli, id: id.to_string() });
                  }
                }

                // Track final result fields.
                if value.get("type").and_then(|v| v.as_str()) == Some("result") {
                  if let Some(text) = value.get("result").and_then(|v| v.as_str()) {
                    final_text = Some(text.to_string());
                  }
                  if let Some(is_error) = value.get("is_error").and_then(|v| v.as_bool()) {
                    final_is_error = Some(is_error);
                  }
                  if let Some(usage) = value.get("usage") {
                    final_usage = parse_usage(usage);
                  }
                }

                let ev = classify_event(value);
                if let Err(e) = on_event(ev) {
                  if let Err(kill_e) = self.kill_child().await {
                    return Err(Error::External(format!("{e} (also failed to kill claude process: {kill_e})")));
                  }
                  return Err(e);
                }
              }
            }
        }

        // Wait for the process to exit.
        let status = {
            let mut guard = self.child.lock().await;
            if let Some(mut child) = guard.take() {
                child.wait().await?
            } else {
                // Process already removed (cancelled).
                // Avoid returning a confusing error if the caller requested cancellation.
                if token.is_cancelled() {
                    return Err(Error::External("Cancelled".to_string()));
                }
                return Err(Error::External("claude process missing".to_string()));
            }
        };

        // Clear cancellation token.
        clear_cancel_token(&self.cancel).await;

        if !status.success() && final_text.is_none() {
            let stderr = stderr_tail.lock().await.snapshot();
            if !stderr.trim().is_empty() {
                return Err(Error::External(format!(
                    "claude exited with status {status}\nstderr (tail):\n{stderr}"
                )));
            }
            return Err(Error::External(format!(
                "claude exited with status {status}"
            )));
        }

        Ok(RunResult {
            session,
            is_error: final_is_error.unwrap_or(!status.success()),
            text: final_text.unwrap_or_default(),
            usage: final_usage,
        })
    }

    async fn cancel(&self) -> Result<()> {
        // Signal cancellation first (for cooperative shutdown paths).
        if let Some(token) = self.cancel.lock().await.as_ref() {
            token.cancel();
        }
        self.kill_child().await?;
        Ok(())
    }
}

impl ClaudeCliClient {
    async fn kill_child(&self) -> Result<()> {
        clear_cancel_token(&self.cancel).await;

        let child = {
            let mut guard = self.child.lock().await;
            guard.take()
        };

        let Some(mut child) = child else {
            return Ok(());
        };

        // If it's already exited, `try_wait` reaps it.
        if child.try_wait()?.is_some() {
            return Ok(());
        }

        // Best-effort kill + reap. If kill fails and the process is still alive, keep
        // the handle so callers can retry instead of losing track of the child.
        match child.kill().await {
            Ok(()) => {
                let _ = child.wait().await?;
            }
            Err(e) => {
                // If it exited between `try_wait` and `kill`, `wait` will reap it.
                if child.try_wait()?.is_none() {
                    let mut guard = self.child.lock().await;
                    *guard = Some(child);
                    return Err(Error::Io(e));
                }
            }
        }

        Ok(())
    }
}

fn classify_event(raw: serde_json::Value) -> ModelEvent {
    match raw.get("type").and_then(|v| v.as_str()) {
        Some("system") => ModelEvent::SystemInit { raw },
        Some("assistant") => ModelEvent::Assistant { raw },
        Some("result") => ModelEvent::Result { raw },
        Some("tool_progress") | Some("tool_use_summary") => ModelEvent::Tool { raw },
        _ => ModelEvent::Unknown { raw },
    }
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

fn truncate_text(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let mut out = s.chars().take(max_len).collect::<String>();
    out.push_str("...");
    out
}
