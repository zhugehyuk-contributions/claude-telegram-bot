//! Telegram adapter (teloxide).
//!
//! This crate implements the `ctb-core` MessagingPort over Telegram Bot API.

use async_trait::async_trait;

use teloxide::{
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup, ParseMode},
};

pub mod handlers;
pub mod router;

use ctb_core::{
    domain::{ChatId, MessageId, MessageRef},
    errors::Error,
    messaging::{
        port::MessagingPort,
        types::{ChatAction, InlineKeyboard, MessagingCapabilities},
    },
    Result,
};

#[derive(Clone)]
pub struct TelegramMessenger {
    bot: Bot,
}

impl TelegramMessenger {
    pub fn new(bot: Bot) -> Self {
        Self { bot }
    }

    pub fn bot(&self) -> Bot {
        self.bot.clone()
    }

    fn tg_chat(chat_id: ChatId) -> teloxide::types::ChatId {
        teloxide::types::ChatId(chat_id.0)
    }

    fn tg_msg_id(message_id: MessageId) -> teloxide::types::MessageId {
        teloxide::types::MessageId(message_id.0)
    }

    fn map_err(e: teloxide::RequestError) -> Error {
        Error::External(format!("telegram error: {e}"))
    }
}

#[async_trait]
impl MessagingPort for TelegramMessenger {
    fn capabilities(&self) -> MessagingCapabilities {
        MessagingCapabilities {
            supports_html: true,
            supports_edit: true,
            supports_reactions: true,
            supports_chat_actions: true,
            supports_inline_keyboards: true,
            max_message_len: 4096,
        }
    }

    async fn send_html(&self, chat_id: ChatId, html: &str) -> Result<MessageRef> {
        let msg = self
            .bot
            .send_message(Self::tg_chat(chat_id), html.to_string())
            .parse_mode(ParseMode::Html)
            .await
            .map_err(Self::map_err)?;

        Ok(MessageRef {
            chat_id,
            message_id: MessageId(msg.id.0),
        })
    }

    async fn edit_html(&self, msg: MessageRef, html: &str) -> Result<()> {
        self.bot
            .edit_message_text(
                Self::tg_chat(msg.chat_id),
                Self::tg_msg_id(msg.message_id),
                html.to_string(),
            )
            .parse_mode(ParseMode::Html)
            .await
            .map_err(Self::map_err)?;
        Ok(())
    }

    async fn delete_message(&self, msg: MessageRef) -> Result<()> {
        self.bot
            .delete_message(Self::tg_chat(msg.chat_id), Self::tg_msg_id(msg.message_id))
            .await
            .map_err(Self::map_err)?;
        Ok(())
    }

    async fn send_chat_action(&self, chat_id: ChatId, action: ChatAction) -> Result<()> {
        let tg_action = match action {
            ChatAction::Typing => teloxide::types::ChatAction::Typing,
            ChatAction::UploadPhoto => teloxide::types::ChatAction::UploadPhoto,
            ChatAction::UploadDocument => teloxide::types::ChatAction::UploadDocument,
        };
        self.bot
            .send_chat_action(Self::tg_chat(chat_id), tg_action)
            .await
            .map_err(Self::map_err)?;
        Ok(())
    }

    async fn set_reaction(&self, _msg: MessageRef, _emoji: &str) -> Result<()> {
        // Teloxide supports reactions via specific payloads; keep this best-effort and optional.
        Ok(())
    }

    async fn send_inline_keyboard(
        &self,
        chat_id: ChatId,
        text: &str,
        keyboard: InlineKeyboard,
    ) -> Result<MessageRef> {
        let rows: Vec<Vec<InlineKeyboardButton>> = keyboard
            .buttons
            .into_iter()
            .map(|b| vec![InlineKeyboardButton::callback(b.label, b.callback_data)])
            .collect();
        let markup = InlineKeyboardMarkup::new(rows);

        let msg = self
            .bot
            .send_message(Self::tg_chat(chat_id), text.to_string())
            .parse_mode(ParseMode::Html)
            .reply_markup(markup)
            .await
            .map_err(Self::map_err)?;

        Ok(MessageRef {
            chat_id,
            message_id: MessageId(msg.id.0),
        })
    }

    async fn answer_callback_query(&self, callback_id: &str, text: Option<&str>) -> Result<()> {
        let mut req = self.bot.answer_callback_query(callback_id.to_string());
        if let Some(t) = text {
            req = req.text(t.to_string());
        }
        req.await.map_err(Self::map_err)?;
        Ok(())
    }
}
