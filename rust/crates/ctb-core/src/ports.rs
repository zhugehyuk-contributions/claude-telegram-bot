use crate::{domain::*, Result};

/// Capabilities of a messenger implementation (Telegram, Slack, etc).
#[derive(Clone, Copy, Debug)]
pub struct MessagingCapabilities {
    pub supports_html: bool,
    pub supports_edit: bool,
    pub max_message_len: usize,
}

/// Hexagonal port for messaging.
///
/// NOTE: This is synchronous in scaffolding to keep the crate buildable in offline sandboxes.
/// We'll migrate this to async once we pull in `tokio` + `teloxide`.
pub trait MessagingPort {
    fn capabilities(&self) -> MessagingCapabilities;

    fn send_html(&self, chat_id: ChatId, html: &str) -> Result<MessageRef>;
    fn edit_html(&self, message: MessageRef, html: &str) -> Result<()>;
}

/// Capabilities of a model client (Claude CLI, Anthropic HTTP, etc).
#[derive(Clone, Copy, Debug)]
pub struct ModelCapabilities {
    pub supports_streaming: bool,
    pub supports_tools: bool,
    pub supports_vision: bool,
}

/// Provider-agnostic model events.
#[derive(Clone, Debug)]
pub enum ModelEvent {
    SystemInit { raw: serde_json::Value },
    Assistant { raw: serde_json::Value },
    Tool { raw: serde_json::Value },
    Result { raw: serde_json::Value },
    Unknown { raw: serde_json::Value },
}

#[derive(Clone, Debug)]
pub struct ModelRunRequest {
    pub prompt: String,
    pub cwd: String,
    pub resume: Option<SessionId>,
}

#[derive(Clone, Debug)]
pub struct ModelRunResult {
    pub session_id: Option<SessionId>,
    pub text: String,
    pub is_error: bool,
}

/// Hexagonal port for driving a model/tooling backend.
///
/// The Rust port will primarily implement this via the `claude` CLI.
pub trait ModelClient {
    fn capabilities(&self) -> ModelCapabilities;

    /// Run a prompt and stream structured events via callback.
    fn run(
        &mut self,
        req: ModelRunRequest,
        on_event: &mut dyn FnMut(ModelEvent) -> Result<()>,
    ) -> Result<ModelRunResult>;
}
