/// Telegram user id (numeric).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UserId(pub i64);

/// Telegram chat id (numeric).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChatId(pub i64);

/// Telegram message id (numeric).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MessageId(pub i32);

/// A stable reference to a Telegram message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MessageRef {
    pub chat_id: ChatId,
    pub message_id: MessageId,
}

/// Claude/agent session id (string).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);
