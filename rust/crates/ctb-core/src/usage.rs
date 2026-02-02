//! Multi-provider usage tracking (Rust port).
//!
//! Parity with `src/usage.ts`:
//! - Claude Code usage via oauth/usage endpoint (token from macOS keychain or ~/.claude/.credentials.json)
//! - OpenAI Codex usage via ChatGPT backend endpoint (token from ~/.codex/auth.json)
//! - Gemini usage via Code Assist API (token from macOS keychain or ~/.gemini/oauth_creds.json)
//! - In-memory caching keyed by a short token hash
//! - Best-effort: missing creds or API failures return `None` per provider

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const API_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaudeUsageWindow {
    pub utilization: f64,
    pub resets_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaudeUsage {
    pub five_hour: Option<ClaudeUsageWindow>,
    pub seven_day: Option<ClaudeUsageWindow>,
    pub seven_day_sonnet: Option<ClaudeUsageWindow>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodexWindow {
    pub used_percent: f64,
    pub reset_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodexUsage {
    pub model: String,
    pub plan_type: String,
    pub primary: Option<CodexWindow>,
    pub secondary: Option<CodexWindow>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeminiUsage {
    pub model: String,
    pub used_percent: Option<u32>,
    pub reset_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AllUsage {
    pub claude: Option<ClaudeUsage>,
    pub codex: Option<CodexUsage>,
    pub gemini: Option<GeminiUsage>,
    pub fetched_at_ms: u64,
}

#[derive(Clone)]
pub struct UsageService {
    http: reqwest::Client,
    claude_cache: Arc<tokio::sync::Mutex<HashMap<String, CacheEntry<ClaudeUsage>>>>,
    codex_cache: Arc<tokio::sync::Mutex<HashMap<String, CacheEntry<CodexUsage>>>>,
    gemini_cache: Arc<tokio::sync::Mutex<HashMap<String, CacheEntry<GeminiUsage>>>>,
}

#[derive(Clone)]
struct CacheEntry<T> {
    data: T,
    at: Instant,
}

impl Default for UsageService {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageService {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .timeout(API_TIMEOUT)
            .user_agent("ctb-rust/0.1")
            .build()
            .expect("reqwest client build");

        Self {
            http,
            claude_cache: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            codex_cache: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            gemini_cache: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    pub async fn fetch_all(&self, ttl: Option<Duration>) -> AllUsage {
        let ttl = ttl.unwrap_or(DEFAULT_CACHE_TTL);

        let (claude, codex, gemini) = tokio::join!(
            self.fetch_claude_usage(ttl),
            self.fetch_codex_usage(ttl),
            self.fetch_gemini_usage(ttl),
        );

        AllUsage {
            claude,
            codex,
            gemini,
            fetched_at_ms: now_ms(),
        }
    }

    pub async fn clear_cache(&self) {
        self.claude_cache.lock().await.clear();
        self.codex_cache.lock().await.clear();
        self.gemini_cache.lock().await.clear();
    }

    async fn fetch_claude_usage(&self, ttl: Duration) -> Option<ClaudeUsage> {
        let token = get_claude_access_token().await?;
        let token_hash = hash_token(&token);

        if let Some(v) = self.get_cached(&self.claude_cache, &token_hash, ttl).await {
            return Some(v);
        }

        let mut headers = HeaderMap::new();
        headers.insert("Accept", HeaderValue::from_static("application/json"));
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).ok()?,
        );
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static("oauth-2025-04-20"),
        );

        let resp = self
            .http
            .get("https://api.anthropic.com/api/oauth/usage")
            .headers(headers)
            .send()
            .await
            .ok()?;

        if !resp.status().is_success() {
            return None;
        }

        let v: serde_json::Value = resp.json().await.ok()?;
        let usage = ClaudeUsage {
            five_hour: parse_claude_window(v.get("five_hour")),
            seven_day: parse_claude_window(v.get("seven_day")),
            seven_day_sonnet: parse_claude_window(v.get("seven_day_sonnet")),
        };

        self.set_cached(&self.claude_cache, token_hash, usage.clone())
            .await;
        Some(usage)
    }

    async fn fetch_codex_usage(&self, ttl: Duration) -> Option<CodexUsage> {
        let auth = get_codex_auth().await?;
        let token_hash = hash_token(&auth.access_token);

        if let Some(v) = self.get_cached(&self.codex_cache, &token_hash, ttl).await {
            return Some(v);
        }

        let mut headers = HeaderMap::new();
        headers.insert("Accept", HeaderValue::from_static("application/json"));
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", auth.access_token)).ok()?,
        );
        headers.insert(
            "ChatGPT-Account-Id",
            HeaderValue::from_str(&auth.account_id).ok()?,
        );

        let resp = self
            .http
            .get("https://chatgpt.com/backend-api/wham/usage")
            .headers(headers)
            .send()
            .await
            .ok()?;

        if !resp.status().is_success() {
            return None;
        }

        let v: serde_json::Value = resp.json().await.ok()?;
        let plan_type = v.get("plan_type").and_then(|x| x.as_str())?.to_string();
        let rate = v.get("rate_limit")?;

        let model = get_codex_model()
            .await
            .unwrap_or_else(|| "unknown".to_string());
        let usage = CodexUsage {
            model,
            plan_type,
            primary: parse_codex_window(rate.get("primary_window")),
            secondary: parse_codex_window(rate.get("secondary_window")),
        };

        self.set_cached(&self.codex_cache, token_hash, usage.clone())
            .await;
        Some(usage)
    }

    async fn fetch_gemini_usage(&self, ttl: Duration) -> Option<GeminiUsage> {
        let creds = get_valid_gemini_credentials().await?;
        let token_hash = hash_token(&creds.access_token);

        if let Some(v) = self.get_cached(&self.gemini_cache, &token_hash, ttl).await {
            return Some(v);
        }

        let project_id = get_gemini_project_id(&self.http, &creds).await?;

        let settings = get_gemini_settings().await;
        let model = settings
            .as_ref()
            .and_then(|s| s.selected_model.clone())
            .unwrap_or_else(|| "unknown".to_string());

        let resp = self
            .http
            .post("https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota")
            .bearer_auth(&creds.access_token)
            .json(&serde_json::json!({ "project": project_id }))
            .send()
            .await
            .ok()?;

        if !resp.status().is_success() {
            return None;
        }

        let v: serde_json::Value = resp.json().await.ok()?;
        let buckets = v
            .get("buckets")
            .and_then(|b| b.as_array())
            .cloned()
            .unwrap_or_default();

        let mut active = buckets.first().cloned();
        if let (Some(sel), true) = (
            settings.as_ref().and_then(|s| s.selected_model.clone()),
            !buckets.is_empty(),
        ) {
            for b in &buckets {
                if b.get("modelId")
                    .and_then(|x| x.as_str())
                    .map(|id| id.contains(&sel))
                    .unwrap_or(false)
                {
                    active = Some(b.clone());
                    break;
                }
            }
        }

        let used_percent = active
            .as_ref()
            .and_then(|b| b.get("remainingFraction"))
            .and_then(|x| x.as_f64())
            .map(|frac| ((1.0 - frac) * 100.0).round().clamp(0.0, 100.0) as u32);

        let reset_at = active
            .as_ref()
            .and_then(|b| b.get("resetTime"))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());

        let usage = GeminiUsage {
            model,
            used_percent,
            reset_at,
        };

        self.set_cached(&self.gemini_cache, token_hash, usage.clone())
            .await;
        Some(usage)
    }

    async fn get_cached<T: Clone>(
        &self,
        cache: &tokio::sync::Mutex<HashMap<String, CacheEntry<T>>>,
        key: &str,
        ttl: Duration,
    ) -> Option<T> {
        let now = Instant::now();
        let map = cache.lock().await;
        map.get(key)
            .filter(|e| now.duration_since(e.at) < ttl)
            .map(|e| e.data.clone())
    }

    async fn set_cached<T>(
        &self,
        cache: &tokio::sync::Mutex<HashMap<String, CacheEntry<T>>>,
        key: String,
        value: T,
    ) {
        cache.lock().await.insert(
            key,
            CacheEntry {
                data: value,
                at: Instant::now(),
            },
        );
    }
}

fn parse_claude_window(v: Option<&serde_json::Value>) -> Option<ClaudeUsageWindow> {
    let v = v?;
    if v.is_null() {
        return None;
    }
    Some(ClaudeUsageWindow {
        utilization: v.get("utilization").and_then(|x| x.as_f64()).unwrap_or(0.0),
        resets_at: v
            .get("resets_at")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
    })
}

fn parse_codex_window(v: Option<&serde_json::Value>) -> Option<CodexWindow> {
    let v = v?;
    if v.is_null() {
        return None;
    }
    Some(CodexWindow {
        used_percent: v
            .get("used_percent")
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0),
        reset_at: v.get("reset_at").and_then(|x| x.as_u64()).unwrap_or(0),
    })
}

fn hash_token(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    let digest = h.finalize();
    hex_prefix(&digest, 16)
}

fn hex_prefix(bytes: &[u8], len: usize) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(len);
    for b in bytes {
        let _ = write!(&mut out, "{:02x}", b);
        if out.len() >= len {
            out.truncate(len);
            break;
        }
    }
    out
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

// === Claude credentials ===

async fn get_claude_access_token() -> Option<String> {
    if cfg!(target_os = "macos") {
        if let Some(raw) =
            security_find_generic_password("Claude Code-credentials", None, Duration::from_secs(3))
                .await
        {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(tok) = v
                    .get("claudeAiOauth")
                    .and_then(|x| x.get("accessToken"))
                    .and_then(|x| x.as_str())
                {
                    return Some(tok.to_string());
                }
            }
        }
    }

    let home = home_dir()?;
    let path = home.join(".claude").join(".credentials.json");
    let raw = read_file_to_string(path).await?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("claudeAiOauth")
        .and_then(|x| x.get("accessToken"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
}

// === Codex credentials ===

struct CodexAuth {
    access_token: String,
    account_id: String,
}

async fn get_codex_auth() -> Option<CodexAuth> {
    let home = home_dir()?;
    let path = home.join(".codex").join("auth.json");
    let raw = read_file_to_string(path).await?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;

    let access = v
        .get("tokens")
        .and_then(|x| x.get("access_token"))
        .and_then(|x| x.as_str())?;
    let account = v
        .get("tokens")
        .and_then(|x| x.get("account_id"))
        .and_then(|x| x.as_str())?;

    Some(CodexAuth {
        access_token: access.to_string(),
        account_id: account.to_string(),
    })
}

async fn get_codex_model() -> Option<String> {
    let home = home_dir()?;
    let path = home.join(".codex").join("config.toml");
    let raw = read_file_to_string(path).await?;
    let re = Regex::new(r#"^model\s*=\s*["']([^"']+)["']\s*(?:#.*)?$"#).ok()?;
    re.captures(&raw)
        .and_then(|cap| cap.get(1).map(|m| m.as_str().to_string()))
}

// === Gemini credentials ===

#[derive(Clone, Debug)]
struct GeminiCredentials {
    access_token: String,
    refresh_token: Option<String>,
    expiry_date_ms: Option<u64>,
}

#[derive(Clone, Debug)]
struct GeminiSettings {
    cloudaicompanion_project: Option<String>,
    selected_model: Option<String>,
}

async fn get_gemini_credentials() -> Option<GeminiCredentials> {
    if cfg!(target_os = "macos") {
        if let Some(raw) = security_find_generic_password(
            "gemini-cli-oauth",
            Some("main-account"),
            Duration::from_secs(3),
        )
        .await
        {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(tok) = v
                    .get("token")
                    .and_then(|x| x.get("accessToken"))
                    .and_then(|x| x.as_str())
                {
                    return Some(GeminiCredentials {
                        access_token: tok.to_string(),
                        refresh_token: v
                            .get("token")
                            .and_then(|x| x.get("refreshToken"))
                            .and_then(|x| x.as_str())
                            .map(|s| s.to_string()),
                        expiry_date_ms: v
                            .get("token")
                            .and_then(|x| x.get("expiresAt"))
                            .and_then(|x| x.as_u64()),
                    });
                }
            }
        }
    }

    let home = home_dir()?;
    let path = home.join(".gemini").join("oauth_creds.json");
    let raw = read_file_to_string(path).await?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let access = v.get("access_token").and_then(|x| x.as_str())?;
    Some(GeminiCredentials {
        access_token: access.to_string(),
        refresh_token: v
            .get("refresh_token")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        expiry_date_ms: v.get("expiry_date").and_then(|x| x.as_u64()),
    })
}

async fn refresh_gemini_token(
    http: &reqwest::Client,
    creds: &GeminiCredentials,
) -> Option<GeminiCredentials> {
    let refresh = creds.refresh_token.clone()?;

    let client_id = std::env::var("GOOGLE_OAUTH_CLIENT_ID")
        .ok()
        .unwrap_or_default();
    let client_secret = std::env::var("GOOGLE_OAUTH_CLIENT_SECRET")
        .ok()
        .unwrap_or_default();
    if client_id.trim().is_empty() || client_secret.trim().is_empty() {
        return None;
    }

    let resp = http
        .post("https://oauth2.googleapis.com/token")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
        ])
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let v: serde_json::Value = resp.json().await.ok()?;
    let access = v.get("access_token").and_then(|x| x.as_str())?;
    let expires_in = v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(0);

    Some(GeminiCredentials {
        access_token: access.to_string(),
        refresh_token: v
            .get("refresh_token")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .or(Some(refresh)),
        expiry_date_ms: Some(now_ms().saturating_add(expires_in.saturating_mul(1000))),
    })
}

async fn get_valid_gemini_credentials() -> Option<GeminiCredentials> {
    let creds = get_gemini_credentials().await?;
    let Some(expiry) = creds.expiry_date_ms else {
        return Some(creds);
    };

    // If expiring within 5 minutes, attempt refresh.
    if expiry < now_ms().saturating_add(5 * 60 * 1000) {
        return refresh_gemini_token(
            &reqwest::Client::builder()
                .timeout(API_TIMEOUT)
                .build()
                .ok()?,
            &creds,
        )
        .await;
    }

    Some(creds)
}

async fn get_gemini_settings() -> Option<GeminiSettings> {
    let home = home_dir()?;
    let path = home.join(".gemini").join("settings.json");
    let raw = read_file_to_string(path).await?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;

    Some(GeminiSettings {
        cloudaicompanion_project: v
            .get("cloudaicompanionProject")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        selected_model: v
            .get("selectedModel")
            .and_then(|x| x.as_str())
            .or_else(|| v.get("model").and_then(|x| x.as_str()))
            .map(|s| s.to_string()),
    })
}

async fn get_gemini_project_id(
    http: &reqwest::Client,
    creds: &GeminiCredentials,
) -> Option<String> {
    if let Ok(p) = std::env::var("GOOGLE_CLOUD_PROJECT") {
        if !p.trim().is_empty() {
            return Some(p);
        }
    }
    if let Ok(p) = std::env::var("GOOGLE_CLOUD_PROJECT_ID") {
        if !p.trim().is_empty() {
            return Some(p);
        }
    }

    if let Some(s) = get_gemini_settings().await {
        if let Some(p) = s.cloudaicompanion_project {
            return Some(p);
        }
    }

    // Fallback: call loadCodeAssist to discover project.
    let resp = http
        .post("https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist")
        .bearer_auth(&creds.access_token)
        .json(&serde_json::json!({
          "metadata": {
            "ideType": "GEMINI_CLI",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI"
          }
        }))
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    v.get("cloudaicompanionProject")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
}

// === IO helpers ===

async fn read_file_to_string(path: PathBuf) -> Option<String> {
    tokio::task::spawn_blocking(move || std::fs::read_to_string(path))
        .await
        .ok()?
        .ok()
}

async fn security_find_generic_password(
    service: &str,
    account: Option<&str>,
    timeout: Duration,
) -> Option<String> {
    use tokio::process::Command;

    let mut cmd = Command::new("security");
    cmd.arg("find-generic-password");
    cmd.arg("-s").arg(service);
    if let Some(a) = account {
        cmd.arg("-a").arg(a);
    }
    cmd.arg("-w");

    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout)
        .ok()
        .map(|s| s.trim().to_string())
}
