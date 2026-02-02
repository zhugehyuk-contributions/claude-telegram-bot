//! Core domain + application logic for the Claude Telegram Bot (Rust port).
//!
//! This crate is intentionally framework-agnostic. Telegram / Claude CLI / OpenAI
//! live behind ports (traits) implemented in adapter crates.

pub mod archive_security;
pub mod config;
pub mod domain;
pub mod errors;
pub mod formatting;
pub mod logging;
pub mod mcp_config;
pub mod messaging;
pub mod model;
pub mod ports;
pub mod scheduler;
pub mod security;
pub mod session;
pub mod streaming;
pub mod usage;
pub mod utils;

pub use errors::{Error, Result};
