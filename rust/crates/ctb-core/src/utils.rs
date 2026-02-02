use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use chrono::{Local, Utc};
use serde::Serialize;

use crate::{errors::Error, Result};

// ============== Timestamp Helpers ==============

/// RFC3339 timestamp in UTC (for logs/telemetry).
pub fn iso_timestamp_utc() -> String {
    Utc::now().to_rfc3339()
}

/// Human timestamp appended to a message (parity with TS `addTimestamp()`).
pub fn add_timestamp(message: &str) -> String {
    let ts = Local::now().format("%a %b %d %H:%M %Z").to_string();
    format!("{message}\n\n<timestamp>{ts}</timestamp>")
}

// ============== Interrupt Helpers ==============

/// Telegram convention: `!` prefix means "interrupt" (stop current run and handle this message).
///
/// This helper only strips the prefix; the handler/session layer decides what to do with it.
pub fn strip_interrupt_prefix(text: &str) -> (bool, String) {
    let Some(rest) = text.strip_prefix('!') else {
        return (false, text.to_string());
    };
    (true, rest.trim_start().to_string())
}

// ============== Typing Indicator Loop ==============

pub struct IntervalController {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl IntervalController {
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Start a cancellable background loop that calls `tick()` every `interval`.
///
/// This is the generic building block; the Telegram adapter will supply `tick()`
/// that sends `typing` chat actions.
pub fn start_interval_loop(
    interval: Duration,
    mut tick: impl FnMut() + Send + 'static,
) -> IntervalController {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = Arc::clone(&stop);

    let handle = thread::spawn(move || {
        while !stop2.load(Ordering::SeqCst) {
            tick();
            thread::sleep(interval);
        }
    });

    IntervalController {
        stop,
        handle: Some(handle),
    }
}

// ============== Audit Logging ==============

const AUDIT_MAX_TEXT: usize = 500;

#[derive(Clone, Debug, Serialize)]
pub struct AuditEvent {
    pub timestamp: String,
    pub event: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorized: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<f64>,
}

impl AuditEvent {
    pub fn message(
        user_id: i64,
        username: &str,
        message_type: &str,
        content: &str,
        response: Option<&str>,
    ) -> Self {
        Self {
            timestamp: iso_timestamp_utc(),
            event: "message".to_string(),
            user_id: Some(user_id),
            username: Some(username.to_string()),
            message_type: Some(message_type.to_string()),
            content: Some(content.to_string()),
            response: response.map(|s| s.to_string()),
            authorized: None,
            tool_name: None,
            tool_input: None,
            blocked: None,
            reason: None,
            error: None,
            context: None,
            retry_after: None,
        }
    }

    pub fn auth(user_id: i64, username: &str, authorized: bool) -> Self {
        Self {
            timestamp: iso_timestamp_utc(),
            event: "auth".to_string(),
            user_id: Some(user_id),
            username: Some(username.to_string()),
            message_type: None,
            content: None,
            response: None,
            authorized: Some(authorized),
            tool_name: None,
            tool_input: None,
            blocked: None,
            reason: None,
            error: None,
            context: None,
            retry_after: None,
        }
    }

    pub fn tool_use(
        user_id: i64,
        username: &str,
        tool_name: &str,
        tool_input: serde_json::Value,
        blocked: bool,
        reason: Option<&str>,
    ) -> Self {
        Self {
            timestamp: iso_timestamp_utc(),
            event: "tool_use".to_string(),
            user_id: Some(user_id),
            username: Some(username.to_string()),
            message_type: None,
            content: None,
            response: None,
            authorized: None,
            tool_name: Some(tool_name.to_string()),
            tool_input: Some(tool_input),
            blocked: Some(blocked),
            reason: reason.map(|s| s.to_string()),
            error: None,
            context: None,
            retry_after: None,
        }
    }

    pub fn error(user_id: i64, username: &str, error: &str, context: Option<&str>) -> Self {
        Self {
            timestamp: iso_timestamp_utc(),
            event: "error".to_string(),
            user_id: Some(user_id),
            username: Some(username.to_string()),
            message_type: None,
            content: None,
            response: None,
            authorized: None,
            tool_name: None,
            tool_input: None,
            blocked: None,
            reason: None,
            error: Some(error.to_string()),
            context: context.map(|s| s.to_string()),
            retry_after: None,
        }
    }

    pub fn rate_limit(user_id: i64, username: &str, retry_after: f64) -> Self {
        Self {
            timestamp: iso_timestamp_utc(),
            event: "rate_limit".to_string(),
            user_id: Some(user_id),
            username: Some(username.to_string()),
            message_type: None,
            content: None,
            response: None,
            authorized: None,
            tool_name: None,
            tool_input: None,
            blocked: None,
            reason: None,
            error: None,
            context: None,
            retry_after: Some(retry_after),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AuditLogger {
    path: PathBuf,
    json: bool,
}

impl AuditLogger {
    pub fn new(path: impl Into<PathBuf>, json: bool) -> Self {
        Self {
            path: path.into(),
            json,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&self, mut event: AuditEvent) -> Result<()> {
        // Truncate potentially large payloads (parity with TS default 500 chars).
        if let Some(s) = &event.content {
            event.content = Some(truncate_text(s, AUDIT_MAX_TEXT));
        }
        if let Some(s) = &event.response {
            event.response = Some(truncate_text(s, AUDIT_MAX_TEXT));
        }
        if let Some(v) = &event.tool_input {
            event.tool_input = Some(truncate_json_strings(v, AUDIT_MAX_TEXT));
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;

        if self.json {
            let line = serde_json::to_string(&event)?;
            writeln!(file, "{line}")?;
            return Ok(());
        }

        // Plain text format for readability.
        let mut out = String::new();
        out.push('\n');
        out.push_str(&"=".repeat(60));

        let value = serde_json::to_value(&event)?;
        let Some(obj) = value.as_object() else {
            return Err(Error::External(
                "audit event is not a JSON object".to_string(),
            ));
        };
        for (k, v) in obj {
            out.push('\n');
            out.push_str(k);
            out.push_str(": ");
            out.push_str(&json_value_to_display(v));
        }
        out.push('\n');

        file.write_all(out.as_bytes())?;
        Ok(())
    }
}

pub fn truncate_text(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    let mut out = s.chars().take(max_len).collect::<String>();
    out.push_str("...");
    out
}

fn truncate_json_strings(v: &serde_json::Value, max_str_len: usize) -> serde_json::Value {
    match v {
        serde_json::Value::String(s) => serde_json::Value::String(truncate_text(s, max_str_len)),
        serde_json::Value::Array(xs) => serde_json::Value::Array(
            xs.iter()
                .map(|x| truncate_json_strings(x, max_str_len))
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), truncate_json_strings(v, max_str_len)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn json_value_to_display(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => s.to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "<unprintable>".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_file(prefix: &str) -> PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(Duration::from_secs(0))
            .as_millis();
        let pid = std::process::id();
        PathBuf::from(format!("/tmp/{prefix}-{pid}-{ts}.log"))
    }

    #[test]
    fn truncate_text_adds_ellipsis() {
        let s = "a".repeat(AUDIT_MAX_TEXT + 10);
        let t = truncate_text(&s, AUDIT_MAX_TEXT);
        assert!(t.ends_with("..."));
        assert!(t.len() >= AUDIT_MAX_TEXT);
    }

    #[test]
    fn audit_truncates_content_and_response() {
        let log = AuditLogger::new(tmp_file("ctb-audit-test"), true);
        let content = "x".repeat(AUDIT_MAX_TEXT + 1);
        let response = "y".repeat(AUDIT_MAX_TEXT + 50);
        let ev = AuditEvent::message(1, "u", "text", &content, Some(&response));
        let line = serde_json::to_string(&ev).unwrap();
        assert!(line.contains(&content)); // raw event not truncated yet

        // Truncation happens during write()
        log.write(ev).unwrap();
        let written = std::fs::read_to_string(log.path()).unwrap();
        assert!(written.contains("..."));
    }

    #[test]
    fn audit_truncates_tool_input_strings_recursively() {
        let log = AuditLogger::new(tmp_file("ctb-audit-tool-test"), true);
        let long = "z".repeat(AUDIT_MAX_TEXT + 10);
        let tool_input = serde_json::json!({
          "command": long,
          "nested": { "a": ["b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s", "t", "u", "v", "w"] }
        });
        let ev = AuditEvent::tool_use(1, "u", "Bash", tool_input, false, None);
        log.write(ev).unwrap();
        let written = std::fs::read_to_string(log.path()).unwrap();
        assert!(written.contains("..."));
    }
}
