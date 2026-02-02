use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::Result;

/// MCP server configuration (matches Claude's MCP schema).
///
/// This is intentionally JSON-friendly so we can pass the file path directly to
/// `claude --mcp-config <path>`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpServerConfig {
    /// stdio/command server (default)
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
    },

    /// HTTP server
    Http {
        #[serde(rename = "type")]
        kind: McpHttpKind,
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum McpHttpKind {
    #[serde(rename = "http")]
    Http,
}

pub type McpServers = HashMap<String, McpServerConfig>;

/// Load MCP servers from a JSON file and interpolate `${ENV_VAR}` placeholders.
///
/// If the file does not exist, returns an empty map.
pub fn load_mcp_servers(path: &Path) -> Result<McpServers> {
    if !path.exists() {
        return Ok(HashMap::new());
    }

    let raw = std::fs::read_to_string(path)?;
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    let interpolated = interpolate_env(value);
    let servers: McpServers = serde_json::from_value(interpolated)?;
    Ok(servers)
}

/// Recursively interpolate `${VAR}` placeholders in all JSON strings.
fn interpolate_env(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::String(s) => serde_json::Value::String(interpolate_env_str(&s)),
        serde_json::Value::Array(xs) => {
            serde_json::Value::Array(xs.into_iter().map(interpolate_env).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, interpolate_env(v)))
                .collect(),
        ),
        other => other,
    }
}

fn interpolate_env_str(s: &str) -> String {
    // Minimal `${VAR}` expansion (no defaults). Unset vars become empty string.
    let mut out = String::new();
    let bytes = s.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if let Some(end) = s[i + 2..].find('}') {
                let name = &s[i + 2..i + 2 + end];
                let val = env::var(name).unwrap_or_default();
                out.push_str(&val);
                i = i + 2 + end + 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }

    out
}

/// Convenience: write MCP servers to a temp file for passing to `claude --mcp-config`.
pub fn write_mcp_servers_json(path: &Path, servers: &McpServers) -> Result<()> {
    let data = serde_json::to_string_pretty(servers)?;
    std::fs::write(path, data)?;
    Ok(())
}

pub fn default_example_path(repo_root: &Path) -> PathBuf {
    repo_root.join("mcp-config.example.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_env_vars_in_strings() {
        let key = format!("CTB_TEST_TOKEN_{}", std::process::id());
        let prev = env::var(&key).ok();
        env::set_var(&key, "abc123");

        let s = format!("https://x.test/?k=${{{key}}}");
        assert_eq!(interpolate_env_str(&s), "https://x.test/?k=abc123");

        match prev {
            Some(v) => env::set_var(&key, v),
            None => env::remove_var(&key),
        }
    }

    #[test]
    fn loads_and_interpolates_json() {
        let tmp = PathBuf::from(format!("/tmp/ctb-mcp-{}.json", std::process::id()));
        env::set_var("CTB_TEST_MCP_KEY", "k1");

        let raw = r#"
{
  "typefully": {
    "type": "http",
    "url": "https://mcp.typefully.com/mcp?TYPEFULLY_API_KEY=${CTB_TEST_MCP_KEY}"
  }
}
"#;
        std::fs::write(&tmp, raw).unwrap();

        let servers = load_mcp_servers(&tmp).unwrap();
        let cfg = servers.get("typefully").unwrap();
        match cfg {
            McpServerConfig::Http { url, .. } => {
                assert!(url.contains("k1"));
            }
            _ => panic!("expected http config"),
        }
    }
}
