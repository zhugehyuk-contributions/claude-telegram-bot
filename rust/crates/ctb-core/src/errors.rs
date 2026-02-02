use std::path::PathBuf;

/// Core error type for the Rust port.
///
/// Adapter crates should map their specific errors into this type so the bot
/// core can handle failures consistently (user-facing message vs retryable).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),

    #[error("security violation: {0}")]
    Security(String),

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid path: {path}: {reason}")]
    InvalidPath { path: PathBuf, reason: String },

    #[error("external error: {0}")]
    External(String),
}

pub type Result<T> = std::result::Result<T, Error>;
