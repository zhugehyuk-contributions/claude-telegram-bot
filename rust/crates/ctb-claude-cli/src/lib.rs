//! Claude CLI adapter (planned primary model backend).
//!
//! Streaming implementation for `claude -p --output-format stream-json`.

use async_trait::async_trait;

use std::process::Stdio;

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

#[derive(Clone, Debug)]
pub struct ClaudeCliClient {
    cfg: ClaudeCliConfig,
    child: std::sync::Arc<Mutex<Option<tokio::process::Child>>>,
    cancel: std::sync::Arc<Mutex<Option<CancellationToken>>>,
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
        // Cancel any existing run first (best-effort).
        let _ = self.cancel().await;

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

        // Store child so `cancel()` can kill it.
        {
            let mut guard = self.child.lock().await;
            *guard = Some(child);
        }

        // Drain stderr in background to avoid blocking on a full pipe.
        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let mut r = BufReader::new(stderr).lines();
                while let Ok(Some(_line)) = r.next_line().await {
                    // Intentionally drop; caller can enable debug logging in the adapter later.
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
                let _ = self.kill_child().await;
                return Err(Error::External("Cancelled".to_string()));
              }
              line = reader.next_line() => {
                let line = line?;
                let Some(line) = line else { break; };

                let value: serde_json::Value = match serde_json::from_str(&line) {
                  Ok(v) => v,
                  Err(_) => serde_json::json!({"type":"unknown_line","line":line}),
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
                  let _ = self.kill_child().await;
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
                return Err(Error::External("claude process missing".to_string()));
            }
        };

        // Clear cancellation token.
        {
            let mut guard = self.cancel.lock().await;
            *guard = None;
        }

        if !status.success() && final_text.is_none() {
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
        let mut guard = self.child.lock().await;
        if let Some(child) = guard.as_mut() {
            let _ = child.kill().await;
        }
        *guard = None;
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
