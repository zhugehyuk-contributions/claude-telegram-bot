use std::{
    collections::HashMap,
    fs,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

use crate::{domain::UserId, errors::Error, Result};

// ============== Authorization ==============

pub fn is_authorized(user_id: Option<UserId>, allowed_users: &[i64]) -> bool {
    let Some(user_id) = user_id else {
        return false;
    };
    if allowed_users.is_empty() {
        return false;
    }
    allowed_users.contains(&user_id.0)
}

// ============== Rate Limiter (Token Bucket) ==============

#[derive(Clone, Debug)]
struct Bucket {
    tokens: f64,
    last_update: Instant,
}

#[derive(Clone, Debug)]
pub struct RateLimiter {
    enabled: bool,
    max_tokens: f64,
    refill_per_sec: f64,
    buckets: HashMap<UserId, Bucket>,
}

#[derive(Clone, Copy, Debug)]
pub struct RateLimitStatus {
    pub tokens: f64,
    pub max: f64,
    pub refill_per_sec: f64,
}

impl RateLimiter {
    pub fn new(enabled: bool, max_tokens: u32, window: Duration) -> Self {
        let max_tokens_f = max_tokens as f64;
        let window_secs = window.as_secs_f64().max(1e-9);

        Self {
            enabled,
            max_tokens: max_tokens_f,
            refill_per_sec: max_tokens_f / window_secs,
            buckets: HashMap::new(),
        }
    }

    pub fn check(&mut self, user_id: UserId) -> (bool, Option<Duration>) {
        self.check_at(user_id, Instant::now())
    }

    pub fn check_at(&mut self, user_id: UserId, now: Instant) -> (bool, Option<Duration>) {
        if !self.enabled {
            return (true, None);
        }

        let bucket = self.buckets.entry(user_id).or_insert_with(|| Bucket {
            tokens: self.max_tokens,
            last_update: now,
        });

        let elapsed = now.duration_since(bucket.last_update).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(self.max_tokens);
        bucket.last_update = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            return (true, None);
        }

        let secs = (1.0 - bucket.tokens) / self.refill_per_sec;
        (false, Some(Duration::from_secs_f64(secs.max(0.0))))
    }

    pub fn status(&self, user_id: UserId) -> RateLimitStatus {
        let tokens = self
            .buckets
            .get(&user_id)
            .map(|b| b.tokens)
            .unwrap_or(self.max_tokens);

        RateLimitStatus {
            tokens,
            max: self.max_tokens,
            refill_per_sec: self.refill_per_sec,
        }
    }
}

// ============== Path Validation ==============

#[derive(Clone, Debug)]
pub struct PathPolicy {
    pub allowed_paths: Vec<PathBuf>,
    pub temp_paths: Vec<PathBuf>,
    pub home_dir: Option<PathBuf>,
    /// Base directory for resolving relative paths (if `None`, uses `std::env::current_dir()`).
    pub base_dir: Option<PathBuf>,
}

impl PathPolicy {
    pub fn is_path_allowed(&self, raw: &str) -> bool {
        let Ok(resolved) = self.resolve_user_path(raw) else {
            return false;
        };

        // Always allow temp paths (bot-owned temp files).
        for tmp in &self.temp_paths {
            if resolved.starts_with(tmp) {
                return true;
            }
        }

        for allowed in &self.allowed_paths {
            let allowed = self.expand_tilde_path(allowed);
            if let Ok(allowed_resolved) =
                canonicalize_or_resolve(&allowed, self.base_dir.as_deref())
            {
                if resolved == allowed_resolved || resolved.starts_with(&allowed_resolved) {
                    return true;
                }
            }
        }

        false
    }

    fn resolve_user_path(&self, raw: &str) -> Result<PathBuf> {
        let expanded = match (&self.home_dir, raw) {
            (Some(home), "~") => home.clone(),
            (Some(home), s) if s.starts_with("~/") => home.join(&s[2..]),
            _ => PathBuf::from(raw),
        };

        canonicalize_or_resolve(&expanded, self.base_dir.as_deref())
    }

    fn expand_tilde_path(&self, p: &Path) -> PathBuf {
        let Some(home) = &self.home_dir else {
            return p.to_path_buf();
        };

        let mut comps = p.components();
        match comps.next() {
            Some(Component::Normal(os)) if os == "~" => {
                let mut out = home.clone();
                for c in comps {
                    out.push(c.as_os_str());
                }
                out
            }
            _ => p.to_path_buf(),
        }
    }
}

fn canonicalize_or_resolve(p: &Path, base_dir: Option<&Path>) -> Result<PathBuf> {
    // First try to resolve symlinks (only works if the path exists).
    if let Ok(canon) = fs::canonicalize(p) {
        return Ok(canon);
    }

    let resolved = if p.is_absolute() {
        p.to_path_buf()
    } else {
        let base = match base_dir {
            Some(b) => b.to_path_buf(),
            None => std::env::current_dir().map_err(Error::Io)?,
        };
        base.join(p)
    };

    Ok(normalize_path(&resolved))
}

fn normalize_path(p: &Path) -> PathBuf {
    // A minimal lexical normalization: remove `.` and process `..` without consulting FS.
    // This is only a fallback when `canonicalize` fails.
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ============== Command Safety ==============

pub fn check_command_safety(
    command: &str,
    blocked_patterns: &[String],
    paths: &PathPolicy,
) -> (bool, String) {
    let lower = command.to_lowercase();

    for pat in blocked_patterns {
        if lower.contains(&pat.to_lowercase()) {
            return (false, format!("Blocked pattern: {pat}"));
        }
    }

    // Special handling for rm: validate targets.
    let words = split_shell_words(command);
    if words.is_empty() {
        return (true, String::new());
    }

    if let Some((rm_idx, _)) = words
        .iter()
        .enumerate()
        .find(|(_, w)| w == &"rm" || w == &"/bin/rm")
    {
        for arg in words.iter().skip(rm_idx + 1) {
            if arg.starts_with('-') {
                continue;
            }
            if !paths.is_path_allowed(arg) {
                return (false, format!("rm target outside allowed paths: {arg}"));
            }
        }
    }

    (true, String::new())
}

fn split_shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();

    let mut in_single = false;
    let mut in_double = false;

    while let Some(ch) = chars.next() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
            }
            '"' if !in_single => {
                in_double = !in_double;
            }
            '\\' if !in_single => {
                // Basic escapes outside single quotes.
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            }
            c if c.is_whitespace() && !in_single && !in_double => {
                if !cur.is_empty() {
                    out.push(cur);
                    cur = String::new();
                }
            }
            other => {
                cur.push(other);
            }
        }
    }

    if !cur.is_empty() {
        out.push(cur);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(prefix: &str) -> PathBuf {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(Duration::from_secs(0))
            .as_millis();
        let pid = std::process::id();
        PathBuf::from(format!("/tmp/{prefix}-{pid}-{ts}"))
    }

    #[test]
    fn rate_limiter_basic_refill() {
        let start = Instant::now();
        let mut rl = RateLimiter::new(true, 2, Duration::from_secs(10));
        let u = UserId(1);

        assert!(rl.check_at(u, start).0);
        assert!(rl.check_at(u, start).0);
        assert!(!rl.check_at(u, start).0);

        // After 5 seconds, we should have refilled 1 token (2 tokens / 10s).
        let (ok, _) = rl.check_at(u, start + Duration::from_secs(5));
        assert!(ok);
    }

    #[test]
    fn path_policy_allows_temp_paths() {
        let p = PathPolicy {
            allowed_paths: vec![],
            temp_paths: vec![PathBuf::from("/tmp/")],
            home_dir: None,
            base_dir: None,
        };
        assert!(p.is_path_allowed("/tmp/some-file.txt"));
    }

    #[test]
    fn path_policy_blocks_traversal_outside_allowed() {
        let base = tmp("allowed");
        let outside = tmp("outside");
        fs::create_dir_all(&base).unwrap();
        fs::create_dir_all(&outside).unwrap();

        let p = PathPolicy {
            allowed_paths: vec![base.clone()],
            temp_paths: vec![],
            home_dir: None,
            base_dir: None,
        };

        // Traversal should resolve outside.
        let raw = format!(
            "{}/../{}",
            base.display(),
            outside.file_name().unwrap().to_string_lossy()
        );
        assert!(!p.is_path_allowed(&raw));
    }

    #[test]
    fn path_policy_blocks_symlink_escape() {
        let base = tmp("allowed");
        let outside = tmp("outside");
        fs::create_dir_all(&base).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(outside.join("secret")).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(&outside, base.join("link")).unwrap();
        }

        let p = PathPolicy {
            allowed_paths: vec![base.clone()],
            temp_paths: vec![],
            home_dir: None,
            base_dir: None,
        };

        #[cfg(unix)]
        {
            let raw = base.join("link/secret");
            assert!(!p.is_path_allowed(raw.to_str().unwrap()));
        }
    }

    #[test]
    fn rm_parsing_handles_quotes() {
        let base = tmp("allowed");
        fs::create_dir_all(&base).unwrap();

        let p = PathPolicy {
            allowed_paths: vec![base.clone()],
            temp_paths: vec![],
            home_dir: None,
            base_dir: None,
        };

        let blocked = vec![];
        let cmd = format!("rm \"{}/file with space.txt\"", base.display());
        let (ok, reason) = check_command_safety(&cmd, &blocked, &p);
        assert!(ok, "expected ok, got: {reason}");
    }

    #[test]
    fn rm_blocks_outside_allowed() {
        let base = tmp("allowed");
        fs::create_dir_all(&base).unwrap();

        let p = PathPolicy {
            allowed_paths: vec![base],
            temp_paths: vec![],
            home_dir: None,
            base_dir: None,
        };

        let blocked = vec![];
        let (ok, _) = check_command_safety("rm /etc/passwd", &blocked, &p);
        assert!(!ok);
    }

    #[test]
    fn tilde_in_allowed_paths_is_respected() {
        let home = tmp("home");
        let allowed_rel = PathBuf::from("~/allowed");
        let allowed_abs = home.join("allowed");
        fs::create_dir_all(&allowed_abs).unwrap();
        fs::write(allowed_abs.join("file.txt"), "x").unwrap();

        let p = PathPolicy {
            allowed_paths: vec![allowed_rel],
            temp_paths: vec![],
            home_dir: Some(home.clone()),
            base_dir: None,
        };

        assert!(p.is_path_allowed("~/allowed/file.txt"));
    }
}
