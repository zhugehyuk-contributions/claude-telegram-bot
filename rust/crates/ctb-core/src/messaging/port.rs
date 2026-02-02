use async_trait::async_trait;

use crate::{
    domain::{ChatId, MessageRef},
    messaging::types::{ChatAction, InlineKeyboard, MessagingCapabilities},
    Result,
};

/// Cross-messenger port.
///
/// Telegram is the first implementation; the shape is designed so future adapters
/// (Slack/Discord/WhatsApp) can fit behind the same interface with capability flags.
#[async_trait]
pub trait MessagingPort: Send + Sync {
    fn capabilities(&self) -> MessagingCapabilities;

    async fn send_html(&self, chat_id: ChatId, html: &str) -> Result<MessageRef>;
    async fn edit_html(&self, msg: MessageRef, html: &str) -> Result<()>;
    async fn delete_message(&self, msg: MessageRef) -> Result<()>;

    async fn send_chat_action(&self, chat_id: ChatId, action: ChatAction) -> Result<()>;

    async fn set_reaction(&self, msg: MessageRef, emoji: &str) -> Result<()>;

    async fn send_inline_keyboard(
        &self,
        chat_id: ChatId,
        text: &str,
        keyboard: InlineKeyboard,
    ) -> Result<MessageRef>;

    async fn answer_callback_query(&self, callback_id: &str, text: Option<&str>) -> Result<()>;
}
