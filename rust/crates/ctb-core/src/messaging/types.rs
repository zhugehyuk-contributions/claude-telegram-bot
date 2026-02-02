use crate::domain::{ChatId, MessageId, MessageRef, UserId};

/// Cross-messenger incoming update model.
///
/// Telegram-specific fields should live in the Telegram adapter.
#[derive(Clone, Debug)]
pub enum IncomingUpdate {
    Command(Command),
    Text(TextMessage),
    Voice(VoiceMessage),
    Photo(PhotoMessage),
    Document(DocumentMessage),
    Callback(CallbackQuery),
}

#[derive(Clone, Debug)]
pub struct Command {
    pub chat_id: ChatId,
    pub user_id: UserId,
    pub username: Option<String>,
    pub name: String,
    pub args: String,
}

#[derive(Clone, Debug)]
pub struct TextMessage {
    pub chat_id: ChatId,
    pub user_id: UserId,
    pub username: Option<String>,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct VoiceMessage {
    pub chat_id: ChatId,
    pub user_id: UserId,
    pub username: Option<String>,
    pub file_id: String,
    pub duration_seconds: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct PhotoMessage {
    pub chat_id: ChatId,
    pub user_id: UserId,
    pub username: Option<String>,
    pub file_id: String,
    pub caption: Option<String>,
    pub media_group_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DocumentMessage {
    pub chat_id: ChatId,
    pub user_id: UserId,
    pub username: Option<String>,
    pub file_id: String,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub caption: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CallbackQuery {
    pub chat_id: ChatId,
    pub user_id: UserId,
    pub username: Option<String>,
    pub callback_id: String,
    pub data: String,
    pub message: Option<MessageRef>,
}

/// Outgoing "chat action" (typing indicator, etc).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatAction {
    Typing,
    UploadPhoto,
    UploadDocument,
}

/// Inline keyboard (buttons) used for callbacks like `ask_user`.
#[derive(Clone, Debug)]
pub struct InlineKeyboard {
    pub buttons: Vec<InlineButton>,
}

#[derive(Clone, Debug)]
pub struct InlineButton {
    pub label: String,
    pub callback_data: String,
}

impl InlineKeyboard {
    pub fn new(buttons: Vec<InlineButton>) -> Self {
        Self { buttons }
    }

    /// Convenience for "one button per row" layouts.
    pub fn one_per_row(request_id: &str, options: &[String], max_label_len: usize) -> Self {
        let mut buttons = Vec::new();
        for (idx, opt) in options.iter().enumerate() {
            let label = if opt.len() > max_label_len {
                format!("{}...", opt.chars().take(max_label_len).collect::<String>())
            } else {
                opt.clone()
            };
            let callback_data = format!("askuser:{request_id}:{idx}");
            buttons.push(InlineButton {
                label,
                callback_data,
            });
        }
        Self { buttons }
    }
}

/// Capabilities / feature flags of a messenger implementation.
#[derive(Clone, Copy, Debug)]
pub struct MessagingCapabilities {
    pub supports_html: bool,
    pub supports_edit: bool,
    pub supports_reactions: bool,
    pub supports_chat_actions: bool,
    pub supports_inline_keyboards: bool,
    pub max_message_len: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct MessengerLimits {
    pub max_message_len: usize,
    pub safe_message_len: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct OutgoingMessageMeta {
    pub chat_id: ChatId,
    pub message_id: MessageId,
}

impl From<MessageRef> for OutgoingMessageMeta {
    fn from(m: MessageRef) -> Self {
        Self {
            chat_id: m.chat_id,
            message_id: m.message_id,
        }
    }
}
