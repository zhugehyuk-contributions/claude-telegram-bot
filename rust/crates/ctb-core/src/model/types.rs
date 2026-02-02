use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The provider backend used for a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    ClaudeCli,
    AnthropicHttp,
    OpenAi,
    Gemini,
    Local,
}

/// Provider-specific session reference (used for resume/restore).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionRef {
    pub provider: ProviderKind,
    pub id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    BypassPermissions,
    Delegate,
    DontAsk,
    Plan,
}

impl PermissionMode {
    pub fn as_claude_cli_flag(self) -> &'static str {
        match self {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::BypassPermissions => "bypassPermissions",
            PermissionMode::Delegate => "delegate",
            PermissionMode::DontAsk => "dontAsk",
            PermissionMode::Plan => "plan",
        }
    }
}

/// Model capabilities for routing + feature gating.
#[derive(Clone, Copy, Debug)]
pub struct ModelCapabilities {
    pub supports_streaming: bool,
    pub supports_tools: bool,
    pub supports_vision: bool,
    pub supports_thinking: bool,
    pub supports_mcp: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

/// Provider selection for the Rust port.
#[derive(Clone, Debug)]
pub enum ModelConfig {
    ClaudeCli(ClaudeCliConfig),
    // Future: AnthropicHttp/ OpenAI/ Gemini/ local backends.
}

#[derive(Clone, Debug)]
pub struct ClaudeCliConfig {
    pub claude_path: PathBuf,
    pub model: Option<String>,
    pub permission_mode: PermissionMode,
    pub dangerously_skip_permissions: bool,
    pub include_partial_messages: bool,
}

/// Normalized request for a single run.
#[derive(Clone, Debug)]
pub struct RunRequest {
    pub prompt: String,
    pub cwd: PathBuf,
    pub add_dirs: Vec<PathBuf>,
    pub mcp_config_path: Option<PathBuf>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,

    pub resume: Option<SessionRef>,
    pub fork_session: bool,

    pub max_thinking_tokens: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct RunResult {
    pub session: Option<SessionRef>,
    pub is_error: bool,
    pub text: String,
    pub usage: Option<TokenUsage>,
}

/// Provider-agnostic model events emitted during a run.
///
/// The Rust port keeps `raw` JSON for forward-compat as CLI schemas evolve.
#[derive(Clone, Debug)]
pub enum ModelEvent {
    SystemInit { raw: serde_json::Value },
    Assistant { raw: serde_json::Value },
    Tool { raw: serde_json::Value },
    Result { raw: serde_json::Value },
    Unknown { raw: serde_json::Value },
}
