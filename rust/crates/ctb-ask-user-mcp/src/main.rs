//! ask_user MCP server (Rust).
//!
//! This mirrors `ask_user_mcp/server.ts`:
//! - JSON-RPC over stdio (newline-delimited)
//! - Exposes a single tool: `ask_user`
//! - Writes request files to `/tmp/ask-user-<id>.json` for the Telegram bot to pick up

use std::{
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};

static COUNTER: AtomicUsize = AtomicUsize::new(1);

#[derive(Debug, Deserialize)]
struct RpcRequest {
    #[allow(dead_code)]
    jsonrpc: Option<String>,
    id: Option<serde_json::Value>,
    method: String,
    params: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct RpcResponse<'a> {
    jsonrpc: &'a str,
    id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<serde_json::Value>,
}

fn respond_ok(id: serde_json::Value, result: serde_json::Value) -> RpcResponse<'static> {
    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(result),
        error: None,
    }
}

fn respond_err(id: serde_json::Value, code: i64, message: &str) -> RpcResponse<'static> {
    RpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(json!({ "code": code, "message": message })),
    }
}

fn next_request_id() -> String {
    // Stable, dependency-free 8-char id.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst) as u128;
    let pid = std::process::id() as u128;
    let x = ts ^ (n << 17) ^ (pid << 5);
    let hex = format!("{x:016x}");
    hex.chars().take(8).collect()
}

#[derive(Debug, Serialize)]
struct AskUserFile {
    request_id: String,
    question: String,
    options: Vec<String>,
    status: String,
    chat_id: String,
    created_at: String,
}

fn write_request_file(
    chat_id: &str,
    question: &str,
    options: Vec<String>,
) -> anyhow::Result<String> {
    let request_id = next_request_id();
    let path = PathBuf::from(format!("/tmp/ask-user-{request_id}.json"));

    let data = AskUserFile {
        request_id: request_id.clone(),
        question: question.to_string(),
        options,
        status: "pending".to_string(),
        chat_id: chat_id.to_string(),
        created_at: ctb_core::utils::iso_timestamp_utc(),
    };

    let txt = serde_json::to_string_pretty(&data)?;
    std::fs::write(&path, txt).with_context(|| format!("write {}", path.display()))?;
    Ok(request_id)
}

fn handle_rpc(req: RpcRequest) -> Option<RpcResponse<'static>> {
    let id = req.id?;

    match req.method.as_str() {
        "initialize" => {
            let proto = req
                .params
                .as_ref()
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            Some(respond_ok(
                id,
                json!({
                  "protocolVersion": proto,
                  "serverInfo": { "name": "ask-user", "version": "1.0.0" },
                  "capabilities": { "tools": {} }
                }),
            ))
        }

        "tools/list" => Some(respond_ok(
            id,
            json!({
              "tools": [
                {
                  "name": "ask_user",
                  "description": "Present options to the user as tappable inline buttons in Telegram. IMPORTANT: After calling this tool, STOP and wait. Do NOT add any text after calling this tool - the user will tap a button and their choice becomes their next message. Just call the tool and end your turn.",
                  "inputSchema": {
                    "type": "object",
                    "properties": {
                      "question": { "type": "string", "description": "The question to ask the user" },
                      "options": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "List of options for the user to choose from (2-6 options recommended)",
                        "minItems": 2,
                        "maxItems": 10
                      }
                    },
                    "required": ["question", "options"]
                  }
                }
              ]
            }),
        )),

        "tools/call" => {
            let Some(params) = req.params.as_ref() else {
                return Some(respond_err(id, -32602, "Missing params"));
            };

            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name != "ask_user" {
                return Some(respond_err(id, -32602, "Unknown tool"));
            }

            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let question = args
                .get("question")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let options = args
                .get("options")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();

            if question.trim().is_empty() || options.len() < 2 {
                return Some(respond_err(
                    id,
                    -32602,
                    "question and at least 2 options required",
                ));
            }

            let chat_id = std::env::var("TELEGRAM_CHAT_ID").unwrap_or_default();
            if chat_id.trim().is_empty() {
                return Some(respond_err(
                    id,
                    -32602,
                    "TELEGRAM_CHAT_ID env var is required",
                ));
            }

            match write_request_file(&chat_id, &question, options) {
                Ok(_request_id) => Some(respond_ok(
                    id,
                    json!({
                      "content": [
                        {
                          "type": "text",
                          "text": "[Buttons sent to user. STOP HERE - do not output any more text. Wait for user to tap a button.]"
                        }
                      ]
                    }),
                )),
                Err(e) => Some(respond_err(
                    id,
                    -32000,
                    &format!("failed to write request file: {e}"),
                )),
            }
        }

        _ => Some(respond_err(id, -32601, "Method not found")),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    eprintln!("ask-user MCP server (Rust) running on stdio");

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();

    let mut stdout = std::io::stdout();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        let req = match serde_json::from_str::<RpcRequest>(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Notifications have no id => no response.
        let Some(resp) = handle_rpc(req) else {
            continue;
        };

        let out = serde_json::to_string(&resp)?;
        stdout.write_all(out.as_bytes())?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_is_8_chars() {
        let id = next_request_id();
        assert_eq!(id.len(), 8);
    }

    #[test]
    fn tools_list_contains_ask_user() {
        let req = RpcRequest {
            jsonrpc: Some("2.0".to_string()),
            id: Some(json!(1)),
            method: "tools/list".to_string(),
            params: None,
        };
        let resp = handle_rpc(req).unwrap();
        let tools = resp
            .result
            .unwrap()
            .get("tools")
            .unwrap()
            .as_array()
            .unwrap()
            .clone();
        assert!(tools
            .iter()
            .any(|t| t.get("name").and_then(|n| n.as_str()) == Some("ask_user")));
    }

    #[test]
    fn writes_ask_user_file_schema() {
        let id = write_request_file("123", "Q?", vec!["a".to_string(), "b".to_string()]).unwrap();
        let path = format!("/tmp/ask-user-{id}.json");
        let txt = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&txt).unwrap();
        assert_eq!(
            v.get("request_id").and_then(|x| x.as_str()),
            Some(id.as_str())
        );
        assert_eq!(v.get("question").and_then(|x| x.as_str()), Some("Q?"));
        assert_eq!(v.get("status").and_then(|x| x.as_str()), Some("pending"));
        assert_eq!(v.get("chat_id").and_then(|x| x.as_str()), Some("123"));
        assert!(v.get("options").and_then(|x| x.as_array()).unwrap().len() == 2);
        let _ = std::fs::remove_file(&path);
    }
}
