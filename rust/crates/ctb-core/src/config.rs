use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{errors::Error, Result};

/// Typed configuration for the Rust port.
///
/// This mirrors the TS defaults in `src/config.ts` as closely as possible.
#[derive(Clone, Debug)]
pub struct Config {
    // Core
    pub telegram_bot_token: String,
    pub telegram_allowed_users: Vec<i64>,
    pub claude_working_dir: PathBuf,
    pub openai_api_key: Option<String>,
    pub transcription_prompt: String,
    pub transcription_available: bool,

    // Claude CLI
    pub claude_cli_path: PathBuf,
    pub claude_config_dir: Option<PathBuf>,

    // Security / safety
    pub allowed_paths: Vec<PathBuf>,
    pub temp_paths: Vec<PathBuf>,
    pub blocked_patterns: Vec<String>,
    pub safety_prompt: String,

    // Runtime constants
    pub query_timeout: Duration,
    pub temp_dir: PathBuf,
    pub session_file: PathBuf,
    pub restart_file: PathBuf,

    // Telegram limits
    pub telegram_message_limit: usize,
    pub telegram_safe_limit: usize,
    pub streaming_throttle: Duration,
    pub button_label_max_length: usize,

    // Behavior flags
    pub default_thinking_tokens: u32,
    pub thinking_keywords: Vec<String>,
    pub thinking_deep_keywords: Vec<String>,
    pub delete_thinking_messages: bool,
    pub delete_tool_messages: bool,

    // Audit
    pub audit_log_path: PathBuf,
    pub audit_log_json: bool,

    // Rate limiting
    pub rate_limit_enabled: bool,
    pub rate_limit_requests: u32,
    pub rate_limit_window: Duration,

    // Media groups
    pub media_group_timeout: Duration,
}

impl Config {
    pub fn load() -> Result<Self> {
        load_dotenv_if_present(Path::new(".env"));
        inject_extra_paths();

        // Required env vars
        let telegram_bot_token = env_str("TELEGRAM_BOT_TOKEN").unwrap_or_default();
        let telegram_allowed_users = parse_csv_i64(env_str("TELEGRAM_ALLOWED_USERS"));

        if telegram_bot_token.trim().is_empty() {
            return Err(Error::Config(
                "TELEGRAM_BOT_TOKEN environment variable is required".to_string(),
            ));
        }
        if telegram_allowed_users.is_empty() {
            return Err(Error::Config(
                "TELEGRAM_ALLOWED_USERS environment variable is required".to_string(),
            ));
        }

        // Working dir defaults to $HOME (parity with TS)
        let home = home_dir().ok_or_else(|| Error::Config("HOME is not set".to_string()))?;
        let claude_working_dir = env_path("CLAUDE_WORKING_DIR").unwrap_or_else(|| home.clone());

        // Optional providers
        let openai_api_key = env_str("OPENAI_API_KEY").and_then(non_empty);
        let transcription_prompt = build_transcription_prompt();
        let transcription_available = openai_api_key.is_some();

        // Claude CLI path
        let claude_cli_path = env_path("CLAUDE_CLI_PATH")
            .or_else(|| which_in_path("claude"))
            .unwrap_or_else(|| PathBuf::from("/usr/local/bin/claude"));
        let claude_config_dir = env_path("CLAUDE_CONFIG_DIR");

        // Allowed paths (ALLOWED_PATHS overrides defaults)
        let default_allowed_paths = vec![
            claude_working_dir.clone(),
            home.join("Documents"),
            home.join("Downloads"),
            home.join("Desktop"),
            home.join(".claude"),
        ];
        let allowed_paths =
            parse_csv_paths(env_str("ALLOWED_PATHS")).unwrap_or(default_allowed_paths);

        // Temp paths always allowed for bot-owned files (parity with TS)
        let temp_paths = vec![
            PathBuf::from("/tmp/"),
            PathBuf::from("/private/tmp/"),
            PathBuf::from("/var/folders/"),
        ];

        let safety_prompt = build_safety_prompt(&allowed_paths);

        let blocked_patterns = vec![
            "rm -rf /",
            "rm -rf ~",
            "rm -rf $HOME",
            "sudo rm",
            ":(){ :|:& };:",
            "> /dev/sd",
            "mkfs.",
            "dd if=",
        ]
        .into_iter()
        .map(|s| s.to_string())
        .collect();

        // Timeouts and constants
        let query_timeout = Duration::from_millis(env_u64("QUERY_TIMEOUT_MS").unwrap_or(180_000));
        let temp_dir =
            PathBuf::from(env_str("TEMP_DIR").unwrap_or("/tmp/telegram-bot".to_string()));
        let session_file = PathBuf::from(
            env_str("SESSION_FILE").unwrap_or("/tmp/claude-telegram-session.json".to_string()),
        );
        let restart_file = PathBuf::from(
            env_str("RESTART_FILE").unwrap_or("/tmp/claude-telegram-restart.json".to_string()),
        );

        // Ensure temp dir exists (parity with TS which writes `.keep`)
        fs::create_dir_all(&temp_dir)?;

        // Telegram message limits
        let telegram_message_limit = env_usize("TELEGRAM_MESSAGE_LIMIT").unwrap_or(4096);
        let telegram_safe_limit = env_usize("TELEGRAM_SAFE_LIMIT").unwrap_or(4000);
        let streaming_throttle =
            Duration::from_millis(env_u64("STREAMING_THROTTLE_MS").unwrap_or(500));
        let button_label_max_length = env_usize("BUTTON_LABEL_MAX_LENGTH").unwrap_or(30);

        // Thinking config
        let default_thinking_tokens = env_u32("DEFAULT_THINKING_TOKENS").unwrap_or(0).min(128_000);
        let thinking_keywords = parse_csv_lower(
            env_str("THINKING_KEYWORDS").or_else(|| Some("think,pensa,ragiona".to_string())),
        );
        let thinking_deep_keywords = parse_csv_lower(
            env_str("THINKING_DEEP_KEYWORDS")
                .or_else(|| Some("ultrathink,think hard,pensa bene".to_string())),
        );

        // Message deletion flags
        let delete_thinking_messages =
            env_bool("DEFAULT_DELETE_THINKING_MESSAGES").unwrap_or(false);
        let delete_tool_messages = env_bool("DEFAULT_DELETE_TOOL_MESSAGES").unwrap_or(true);

        // Audit logging
        let audit_log_path = PathBuf::from(
            env_str("AUDIT_LOG_PATH").unwrap_or("/tmp/claude-telegram-audit.log".to_string()),
        );
        let audit_log_json = env_bool("AUDIT_LOG_JSON").unwrap_or(false);

        // Rate limiting
        let rate_limit_enabled = env_bool("RATE_LIMIT_ENABLED").unwrap_or(true);
        let rate_limit_requests = env_u32("RATE_LIMIT_REQUESTS").unwrap_or(20);
        let rate_limit_window = Duration::from_secs(env_u64("RATE_LIMIT_WINDOW").unwrap_or(60));

        // Media groups
        let media_group_timeout =
            Duration::from_millis(env_u64("MEDIA_GROUP_TIMEOUT").unwrap_or(1000));

        Ok(Self {
            telegram_bot_token,
            telegram_allowed_users,
            claude_working_dir,
            openai_api_key,
            transcription_prompt,
            transcription_available,
            claude_cli_path,
            claude_config_dir,
            allowed_paths,
            temp_paths,
            blocked_patterns,
            safety_prompt,
            query_timeout,
            temp_dir,
            session_file,
            restart_file,
            telegram_message_limit,
            telegram_safe_limit,
            streaming_throttle,
            button_label_max_length,
            default_thinking_tokens,
            thinking_keywords,
            thinking_deep_keywords,
            delete_thinking_messages,
            delete_tool_messages,
            audit_log_path,
            audit_log_json,
            rate_limit_enabled,
            rate_limit_requests,
            rate_limit_window,
            media_group_timeout,
        })
    }
}

fn inject_extra_paths() {
    let Some(home) = home_dir() else {
        return;
    };

    let extras = [
        home.join(".local/bin"),
        home.join(".bun/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/homebrew/sbin"),
        PathBuf::from("/usr/local/bin"),
    ];

    let current = env::var_os("PATH").unwrap_or_else(|| OsString::from(""));
    let mut parts: Vec<OsString> = env::split_paths(&current)
        .map(|p| p.into_os_string())
        .collect();

    for extra in extras.into_iter().rev() {
        let extra_os = extra.into_os_string();
        if !parts.iter().any(|p| p == &extra_os) {
            parts.insert(0, extra_os);
        }
    }

    let joined = env::join_paths(parts.into_iter().map(PathBuf::from).collect::<Vec<_>>())
        .unwrap_or(current);
    env::set_var("PATH", joined);
}

fn build_safety_prompt(allowed_paths: &[PathBuf]) -> String {
    let paths_list = allowed_paths
        .iter()
        .map(|p| format!("   - {} (and subdirectories)", p.display()))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"
CRITICAL SAFETY RULES FOR TELEGRAM BOT:

1. NEVER delete, remove, or overwrite files without EXPLICIT confirmation from the user.
   - If user asks to delete something, respond: "Are you sure you want to delete [file]? Reply 'yes delete it' to confirm."
   - Only proceed with deletion if user replies with explicit confirmation like "yes delete it", "confirm delete"
   - This applies to: rm, trash, unlink, shred, or any file deletion

2. You can ONLY access files in these directories:
{paths_list}
   - REFUSE any file operations outside these paths

3. NEVER run dangerous commands like:
   - rm -rf (recursive force delete)
   - Any command that affects files outside allowed directories
   - Commands that could damage the system

4. For any destructive or irreversible action, ALWAYS ask for confirmation first.

You are running via Telegram, so the user cannot easily undo mistakes. Be extra careful!
"#
    )
}

fn build_transcription_prompt() -> String {
    const BASE: &str = "Transcribe this voice message accurately.\n\
The speaker may use multiple languages (English, and possibly others).\n\
Focus on accuracy for proper nouns, technical terms, and commands.";

    let Some(ctx) = env_str("TRANSCRIPTION_CONTEXT").and_then(non_empty) else {
        return BASE.to_string();
    };

    format!("{BASE}\n\nAdditional context:\n{ctx}")
}

fn env_str(key: &str) -> Option<String> {
    env::var(key).ok()
}

fn load_dotenv_if_present(path: &Path) {
    let Ok(contents) = fs::read_to_string(path) else {
        return;
    };

    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((k, v)) = line.split_once('=') else {
            continue;
        };

        let key = k.trim();
        if key.is_empty() {
            continue;
        }
        if env::var_os(key).is_some() {
            continue; // do not override existing env
        }

        let mut val = v.trim().to_string();
        // Strip optional surrounding quotes.
        if val.len() >= 2
            && ((val.starts_with('"') && val.ends_with('"'))
                || (val.starts_with('\'') && val.ends_with('\'')))
        {
            val = val[1..val.len() - 1].to_string();
        }

        env::set_var(key, val);
    }
}

fn env_bool(key: &str) -> Option<bool> {
    env_str(key).map(|s| {
        matches!(
            s.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn env_u64(key: &str) -> Option<u64> {
    env_str(key).and_then(|s| s.trim().parse::<u64>().ok())
}

fn env_u32(key: &str) -> Option<u32> {
    env_str(key).and_then(|s| s.trim().parse::<u32>().ok())
}

fn env_usize(key: &str) -> Option<usize> {
    env_str(key).and_then(|s| s.trim().parse::<usize>().ok())
}

fn env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key).map(PathBuf::from)
}

fn parse_csv_i64(v: Option<String>) -> Vec<i64> {
    v.unwrap_or_default()
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<i64>().ok())
        .collect()
}

fn parse_csv_lower(v: Option<String>) -> Vec<String> {
    v.unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_csv_paths(v: Option<String>) -> Option<Vec<PathBuf>> {
    let v = v?;
    let out = v
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn which_in_path(binary: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(binary);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable_file(p: &Path) -> bool {
    if !p.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(md) = fs::metadata(p) {
            return (md.permissions().mode() & 0o111) != 0;
        }
    }
    true
}

fn non_empty(s: String) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}
