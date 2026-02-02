use std::path::PathBuf;

use async_trait::async_trait;

use crate::Result;

use super::types::*;

/// A provider-specific prompt adapter.
///
/// This exists to keep provider quirks out of application logic:
/// - CLI flags vs HTTP payloads
/// - how system prompts are applied (replace vs append)
/// - thinking/tool toggles
pub trait PromptAdapter {
    fn provider(&self) -> ProviderKind;
}

/// A concrete CLI invocation (used by `claude` runner).
#[derive(Clone, Debug)]
pub struct CliInvocation {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

/// Prompt adapter for Claude CLI.
#[derive(Clone, Debug)]
pub struct ClaudeCliPromptAdapter {
    pub cfg: ClaudeCliConfig,
}

impl PromptAdapter for ClaudeCliPromptAdapter {
    fn provider(&self) -> ProviderKind {
        ProviderKind::ClaudeCli
    }
}

impl ClaudeCliPromptAdapter {
    /// Build `claude` CLI args for a run.
    ///
    /// This uses the flags documented in `docs/rust-port/claude-cli-stream-json.md`.
    pub fn build_invocation(&self, req: &RunRequest) -> CliInvocation {
        let mut args: Vec<String> = vec![
            // Non-interactive streaming NDJSON.
            "-p".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
            // Permissions / tools
            "--permission-mode".to_string(),
            self.cfg.permission_mode.as_claude_cli_flag().to_string(),
        ];
        if self.cfg.dangerously_skip_permissions {
            args.push("--dangerously-skip-permissions".to_string());
        }

        // Streaming: optionally include partial chunks.
        if self.cfg.include_partial_messages {
            args.push("--include-partial-messages".to_string());
        }

        // Model selection
        if let Some(model) = &self.cfg.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }

        // Append system prompt (safety prompt).
        if let Some(sys) = &req.system_prompt {
            args.push("--append-system-prompt".to_string());
            args.push(sys.clone());
        }
        if let Some(sys) = &req.append_system_prompt {
            args.push("--append-system-prompt".to_string());
            args.push(sys.clone());
        }

        // Allowed dirs (tools).
        if !req.add_dirs.is_empty() {
            args.push("--add-dir".to_string());
            for d in &req.add_dirs {
                args.push(d.display().to_string());
            }
        }

        // Resume / fork
        if let Some(s) = &req.resume {
            if s.provider == ProviderKind::ClaudeCli {
                args.push("--resume".to_string());
                args.push(s.id.clone());
                if req.fork_session {
                    args.push("--fork-session".to_string());
                }
            }
        }

        // MCP servers
        if let Some(p) = &req.mcp_config_path {
            args.push("--mcp-config".to_string());
            args.push(p.display().to_string());
        }

        // Prompt as the final positional argument.
        args.push(req.prompt.clone());

        CliInvocation {
            program: self.cfg.claude_path.clone(),
            args,
            cwd: req.cwd.clone(),
            env: Vec::new(),
        }
    }
}

/// Model client interface used by the session runner.
///
/// We prefer a callback-based streaming interface over `Stream<Item=...>` to keep
/// dependencies light and allow provider implementations to drive their own loops.
#[async_trait]
pub trait ModelClient: Send + Sync {
    fn provider(&self) -> ProviderKind;
    fn capabilities(&self) -> ModelCapabilities;

    async fn run(
        &self,
        req: RunRequest,
        on_event: &mut (dyn FnMut(ModelEvent) -> Result<()> + Send),
    ) -> Result<RunResult>;

    async fn cancel(&self) -> Result<()>;
}
