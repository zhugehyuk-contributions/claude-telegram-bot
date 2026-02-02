//! Telegram adapter (teloxide).
//!
//! This crate implements the `ctb-core` MessagingPort over Telegram Bot API.

use async_trait::async_trait;

use teloxide::{
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup, ParseMode},
};

use tokio::time::sleep;

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

    async fn with_retry<T, Fut>(&self, mut op: impl FnMut() -> Fut) -> Result<T>
    where
        Fut: std::future::IntoFuture<Output = std::result::Result<T, teloxide::RequestError>>,
        Fut::IntoFuture: Send,
    {
        const MAX_RETRIES: usize = 1;
        let mut attempts = 0usize;
        loop {
            match op().await {
                Ok(v) => return Ok(v),
                Err(e) => match e {
                    teloxide::RequestError::RetryAfter(d) if attempts < MAX_RETRIES => {
                        attempts += 1;
                        sleep(d).await;
                        continue;
                    }
                    other => return Err(Self::map_err(other)),
                },
            }
        }
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
            .with_retry(|| {
                self.bot
                    .send_message(Self::tg_chat(chat_id), html.to_string())
                    .parse_mode(ParseMode::Html)
            })
            .await?;

        Ok(MessageRef {
            chat_id,
            message_id: MessageId(msg.id.0),
        })
    }

    async fn edit_html(&self, msg: MessageRef, html: &str) -> Result<()> {
        self.with_retry(|| {
            self.bot
                .edit_message_text(
                    Self::tg_chat(msg.chat_id),
                    Self::tg_msg_id(msg.message_id),
                    html.to_string(),
                )
                .parse_mode(ParseMode::Html)
        })
        .await?;
        Ok(())
    }

    async fn delete_message(&self, msg: MessageRef) -> Result<()> {
        self.with_retry(|| {
            self.bot
                .delete_message(Self::tg_chat(msg.chat_id), Self::tg_msg_id(msg.message_id))
        })
        .await?;
        Ok(())
    }

    async fn send_chat_action(&self, chat_id: ChatId, action: ChatAction) -> Result<()> {
        let tg_action = match action {
            ChatAction::Typing => teloxide::types::ChatAction::Typing,
            ChatAction::UploadPhoto => teloxide::types::ChatAction::UploadPhoto,
            ChatAction::UploadDocument => teloxide::types::ChatAction::UploadDocument,
        };
        self.with_retry(|| self.bot.send_chat_action(Self::tg_chat(chat_id), tg_action))
            .await?;
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
            .with_retry(|| {
                self.bot
                    .send_message(Self::tg_chat(chat_id), text.to_string())
                    .parse_mode(ParseMode::Html)
                    .reply_markup(markup.clone())
            })
            .await?;

        Ok(MessageRef {
            chat_id,
            message_id: MessageId(msg.id.0),
        })
    }

    async fn answer_callback_query(&self, callback_id: &str, text: Option<&str>) -> Result<()> {
        self.with_retry(|| {
            let mut req = self.bot.answer_callback_query(callback_id.to_string());
            if let Some(t) = text {
                req = req.text(t.to_string());
            }
            req
        })
        .await?;
        Ok(())
    }
}
