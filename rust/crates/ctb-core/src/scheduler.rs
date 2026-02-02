//! Cron scheduler for scheduled prompts (Rust port).
//!
//! Parity with `src/scheduler.ts`:
//! - Loads `cron.yaml` from `CLAUDE_WORKING_DIR`
//! - Schedules jobs with standard 5-field cron syntax (min hour dom mon dow)
//! - Queues jobs if a session is already running
//! - Rate limits job executions per hour
//! - Auto-reloads when `cron.yaml` changes (polling mtime)
//!
//! Notes:
//! - We intentionally avoid a YAML/cron dependency to keep offline builds working.

use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime},
};

use chrono::{DateTime, Datelike, Local, Timelike};
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::{
    config::Config,
    domain::{ChatId, MessageId, MessageRef},
    formatting::escape_html,
    messaging::{
        port::MessagingPort,
        types::{ChatAction, InlineKeyboard, MessagingCapabilities},
    },
    security::PathPolicy,
    session::ClaudeSession,
    Error, Result,
};

const MAX_PROMPT_LENGTH: usize = 10_000;
const MAX_JOBS_PER_HOUR: usize = 60;
const MAX_PENDING_QUEUE_SIZE: usize = 100;

#[derive(Clone, Debug)]
pub struct CronSchedule {
    pub name: String,
    pub cron: String,
    pub prompt: String,
    pub enabled: bool,
    pub notify: bool,
}

#[derive(Clone, Debug, Default)]
struct CronConfig {
    schedules: Vec<CronSchedule>,
}

#[derive(Clone)]
pub struct CronScheduler {
    inner: Arc<SchedulerInner>,
}

struct SchedulerInner {
    cfg: Arc<Config>,
    session: Arc<ClaudeSession>,
    messenger: Arc<dyn MessagingPort>,
    state: tokio::sync::Mutex<SchedulerState>,
}

#[derive(Default)]
struct SchedulerState {
    jobs: HashMap<String, JobEntry>,
    watcher: Option<JoinHandle<()>>,
    watcher_cancel: Option<CancellationToken>,
    last_modified: Option<SystemTime>,

    execution_lock: bool,
    executions: VecDeque<Instant>,
    pending: VecDeque<PendingJob>,
}

struct PendingJob {
    schedule: CronSchedule,
}

struct JobEntry {
    expr: CronExpr,
    cancel: CancellationToken,
    handle: JoinHandle<()>,
}

impl CronScheduler {
    pub fn new(
        cfg: Arc<Config>,
        session: Arc<ClaudeSession>,
        messenger: Arc<dyn MessagingPort>,
    ) -> Self {
        Self {
            inner: Arc::new(SchedulerInner {
                cfg,
                session,
                messenger,
                state: tokio::sync::Mutex::new(SchedulerState::default()),
            }),
        }
    }

    pub async fn start(&self) -> Result<usize> {
        self.stop_jobs_only().await;

        let config = match load_cron_config(&self.inner.cfg) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[CRON] Failed to load cron.yaml: {e}");
                None
            }
        };

        let Some(config) = config else {
            println!("[CRON] No schedules configured");
            return Ok(0);
        };
        if config.schedules.is_empty() {
            println!("[CRON] No schedules configured");
            return Ok(0);
        }

        println!("[CRON] Loading {} schedules", config.schedules.len());

        let mut loaded = 0usize;
        for schedule in config.schedules.into_iter() {
            if !schedule.enabled {
                println!("[CRON] Skipping disabled schedule: {}", schedule.name);
                continue;
            }

            let expr = match CronExpr::parse(&schedule.cron) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("[CRON] Invalid cron expression for {}: {e}", schedule.name);
                    continue;
                }
            };

            let cancel = CancellationToken::new();
            let scheduler = self.clone();
            let schedule_clone = schedule.clone();
            let cancel_clone = cancel.clone();
            let expr_for_task = expr.clone();
            let handle = tokio::spawn(async move {
                scheduler
                    .job_loop(schedule_clone, expr_for_task, cancel_clone)
                    .await;
            });

            let mut st = self.inner.state.lock().await;
            st.jobs.insert(
                schedule.name.clone(),
                JobEntry {
                    expr,
                    cancel,
                    handle,
                },
            );
            loaded += 1;
        }

        if loaded > 0 {
            println!("[CRON] Started {loaded} jobs");
        } else {
            println!("[CRON] No jobs started");
        }

        Ok(loaded)
    }

    /// Start the cron.yaml watcher (polling mtime), if not already running.
    ///
    /// This is intentionally separate from `start()` so we can reload jobs from
    /// within the watcher without creating a non-`Send` self-await chain.
    pub async fn ensure_watcher(&self) {
        self.start_file_watcher().await;
    }

    pub async fn stop(&self) {
        let mut st = self.inner.state.lock().await;

        if let Some(tok) = st.watcher_cancel.take() {
            tok.cancel();
        }
        st.watcher.take(); // let the task exit on cancellation

        for (_, job) in st.jobs.drain() {
            job.cancel.cancel();
            job.handle.abort(); // best-effort
        }
    }

    pub async fn reload(&self) -> Result<usize> {
        println!("[CRON] Reloading configuration");
        self.start().await
    }

    async fn stop_jobs_only(&self) {
        let mut st = self.inner.state.lock().await;
        for (_, job) in st.jobs.drain() {
            job.cancel.cancel();
            job.handle.abort();
        }
        st.execution_lock = false;
    }

    pub async fn status_html(&self) -> String {
        let st = self.inner.state.lock().await;
        if st.jobs.is_empty() {
            return "No scheduled jobs".to_string();
        }

        let mut lines = Vec::new();
        lines.push(format!("📅 <b>Scheduled Jobs ({})</b>", st.jobs.len()));

        let mut names: Vec<_> = st.jobs.keys().cloned().collect();
        names.sort();
        for name in names {
            let Some(job) = st.jobs.get(&name) else {
                continue;
            };
            let next = job.expr.next_after(Local::now());
            let next_str = next
                .map(|dt| format!("{:02}:{:02}", dt.hour(), dt.minute()))
                .unwrap_or_else(|| "never".to_string());
            lines.push(format!(
                "• {}: next at {}",
                escape_html(&name),
                escape_html(&next_str)
            ));
        }

        if !st.pending.is_empty() {
            lines.push(format!("\n⏳ <b>Queued Jobs ({})</b>", st.pending.len()));
            for pending in st.pending.iter() {
                lines.push(format!("• {}", escape_html(&pending.schedule.name)));
            }
        }

        lines.join("\n")
    }

    pub async fn process_queued_jobs(&self) -> Result<()> {
        // Mirror TS `processQueuedJobs()` semantics: process at most one job per call.
        if self.inner.session.is_running().await {
            return Ok(());
        }

        let schedule = {
            let mut st = self.inner.state.lock().await;
            if st.execution_lock {
                return Ok(());
            }
            st.pending.pop_front().map(|p| p.schedule)
        };

        let Some(schedule) = schedule else {
            return Ok(());
        };

        println!("[CRON] Processing queued job: {}", schedule.name);
        self.execute_scheduled_prompt(schedule).await?;

        Ok(())
    }

    async fn start_file_watcher(&self) {
        let cron_path = cron_config_path(&self.inner.cfg);

        let mut st = self.inner.state.lock().await;
        if st.watcher.is_some() {
            return;
        }

        let tok = CancellationToken::new();
        st.watcher_cancel = Some(tok.clone());
        let scheduler = self.clone();
        let handle = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            loop {
                tokio::select! {
                  _ = tok.cancelled() => break,
                  _ = tick.tick() => {
                    if !cron_path.exists() {
                      let _ = scheduler.process_queued_jobs().await;
                      continue;
                    }
                    let Ok(md) = fs::metadata(&cron_path) else {
                      continue;
                    };
                    let Ok(modified) = md.modified() else {
                      continue;
                    };
                    let should_reload = {
                      let mut st = scheduler.inner.state.lock().await;
                      match st.last_modified {
                        None => {
                          st.last_modified = Some(modified);
                          false
                        }
                        Some(prev) if modified > prev => {
                          st.last_modified = Some(modified);
                          true
                        }
                        _ => false,
                      }
                    };

                    if should_reload {
                      println!("[CRON] Detected cron.yaml change, auto-reloading...");
                      sleep(Duration::from_millis(100)).await;
                      let _ = scheduler.reload().await;
                    }

                    // Drain queued jobs opportunistically (parity with TS calling this on session completion).
                    let _ = scheduler.process_queued_jobs().await;
                  }
                }
            }
        });

        st.watcher = Some(handle);
        println!("[CRON] File watcher started");
    }

    async fn job_loop(&self, schedule: CronSchedule, expr: CronExpr, cancel: CancellationToken) {
        loop {
            let Some(next) = expr.next_after(Local::now()) else {
                eprintln!("[CRON] Job {} has no next run (stopping)", schedule.name);
                break;
            };

            let now = Local::now();
            let dur = match (next - now).to_std() {
                Ok(d) => d,
                Err(_) => Duration::from_secs(0),
            };

            tokio::select! {
              _ = cancel.cancelled() => break,
              _ = sleep(dur) => {
                let scheduler = self.clone();
                let schedule = schedule.clone();
                if let Err(e) = scheduler.execute_scheduled_prompt(schedule).await {
                  eprintln!("[CRON] Scheduled job failed: {e}");
                }
              }
            }
        }
    }

    async fn execute_scheduled_prompt(&self, schedule: CronSchedule) -> Result<()> {
        // If session is busy, queue.
        if self.inner.session.is_running().await {
            self.queue_job(schedule).await;
            return Ok(());
        }

        // Take execution lock + rate limit window.
        {
            let mut st = self.inner.state.lock().await;
            if st.execution_lock {
                drop(st);
                self.queue_job(schedule).await;
                return Ok(());
            }

            let now = Instant::now();
            let one_hour = Duration::from_secs(3600);
            while st
                .executions
                .front()
                .map(|t| now.duration_since(*t) > one_hour)
                .unwrap_or(false)
            {
                st.executions.pop_front();
            }
            if st.executions.len() >= MAX_JOBS_PER_HOUR {
                println!("[CRON] Rate limit reached, skipping {}", schedule.name);
                return Ok(());
            }

            st.execution_lock = true;
            st.executions.push_back(now);
        }

        let chat_id = self
            .inner
            .cfg
            .telegram_allowed_users
            .first()
            .copied()
            .unwrap_or_default();
        let chat_id = ChatId(chat_id);

        println!("[CRON] Executing scheduled job: {}", schedule.name);

        let cron_messenger: Arc<dyn MessagingPort> =
            Arc::new(CronMessenger::new(self.inner.messenger.clone()));
        let prompt = schedule.prompt.clone();

        let res = self
            .inner
            .session
            .send_message_to_chat(chat_id, &prompt, cron_messenger)
            .await;

        match res {
            Ok(out) => {
                println!("[CRON] Job {} completed", schedule.name);
                if schedule.notify {
                    let safe_name = escape_html(&schedule.name);
                    let mut snippet = out.text;
                    if snippet.len() > 3500 {
                        snippet.truncate(3500);
                    }
                    let msg = format!(
                        "🕐 <b>Scheduled: {safe_name}</b>\n\n{}",
                        escape_html(&snippet)
                    );
                    if let Err(e) = self.inner.messenger.send_html(chat_id, &msg).await {
                        eprintln!(
                            "[CRON] Failed to send completion notification for {}: {e}",
                            schedule.name
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("[CRON] Job {} failed: {e}", schedule.name);
                if schedule.notify {
                    let safe_name = escape_html(&schedule.name);
                    let mut err_txt = format!("{e}");
                    if err_txt.len() > 500 {
                        err_txt.truncate(500);
                    }
                    let msg = format!(
                        "❌ <b>Scheduled job failed: {safe_name}</b>\n\n{}",
                        escape_html(&err_txt)
                    );
                    if let Err(send_e) = self.inner.messenger.send_html(chat_id, &msg).await {
                        eprintln!(
                            "[CRON] Failed to send failure notification for {}: {send_e}",
                            schedule.name
                        );
                    }
                }
            }
        }

        // Release lock.
        {
            let mut st = self.inner.state.lock().await;
            st.execution_lock = false;
        }

        Ok(())
    }

    async fn queue_job(&self, schedule: CronSchedule) {
        let mut st = self.inner.state.lock().await;
        if st.pending.len() >= MAX_PENDING_QUEUE_SIZE {
            println!(
                "[CRON] Queue full ({}), dropping oldest job",
                MAX_PENDING_QUEUE_SIZE
            );
            st.pending.pop_front();
        }
        println!("[CRON] Session busy - queuing job: {}", schedule.name);
        st.pending.push_back(PendingJob { schedule });
    }
}

// === Messenger wrapper for cron runs ===

/// A "mostly silent" messenger for cron runs:
/// - suppresses streaming tool/thinking/text spam
/// - *does* forward `ask_user` keyboards so interactive flows still work.
struct CronMessenger {
    real: Arc<dyn MessagingPort>,
    next_id: AtomicI32,
}

impl CronMessenger {
    fn new(real: Arc<dyn MessagingPort>) -> Self {
        Self {
            real,
            next_id: AtomicI32::new(1),
        }
    }

    fn alloc(&self, chat_id: ChatId) -> MessageRef {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        MessageRef {
            chat_id,
            message_id: MessageId(id),
        }
    }
}

#[async_trait::async_trait]
impl MessagingPort for CronMessenger {
    fn capabilities(&self) -> MessagingCapabilities {
        self.real.capabilities()
    }

    async fn send_html(&self, chat_id: ChatId, _html: &str) -> Result<MessageRef> {
        Ok(self.alloc(chat_id))
    }

    async fn edit_html(&self, _msg: MessageRef, _html: &str) -> Result<()> {
        Ok(())
    }

    async fn delete_message(&self, _msg: MessageRef) -> Result<()> {
        Ok(())
    }

    async fn send_chat_action(&self, _chat_id: ChatId, _action: ChatAction) -> Result<()> {
        Ok(())
    }

    async fn set_reaction(&self, _msg: MessageRef, _emoji: &str) -> Result<()> {
        Ok(())
    }

    async fn send_inline_keyboard(
        &self,
        chat_id: ChatId,
        text_html: &str,
        keyboard: InlineKeyboard,
    ) -> Result<MessageRef> {
        self.real
            .send_inline_keyboard(chat_id, text_html, keyboard)
            .await
    }

    async fn answer_callback_query(&self, callback_id: &str, text: Option<&str>) -> Result<()> {
        self.real.answer_callback_query(callback_id, text).await
    }
}

// === cron.yaml loading ===

fn cron_config_path(cfg: &Config) -> PathBuf {
    cfg.claude_working_dir.join("cron.yaml")
}

fn load_cron_config(cfg: &Config) -> Result<Option<CronConfig>> {
    let path = cron_config_path(cfg);

    // Path allowlist parity with TS.
    let policy = PathPolicy {
        allowed_paths: cfg.allowed_paths.clone(),
        temp_paths: cfg.temp_paths.clone(),
        home_dir: std::env::var_os("HOME").map(PathBuf::from),
        base_dir: Some(cfg.claude_working_dir.clone()),
    };

    if !policy.is_path_allowed(&path.to_string_lossy()) {
        eprintln!("[CRON] cron.yaml path not in allowed directories");
        return Ok(None);
    }

    if !path.exists() {
        println!("[CRON] No cron.yaml found at {}", path.display());
        return Ok(None);
    }

    let content = fs::read_to_string(&path)?;
    let config = parse_cron_yaml(&content)?;
    Ok(Some(config))
}

fn parse_cron_yaml(input: &str) -> Result<CronConfig> {
    // A tiny YAML subset parser:
    // - top-level `schedules:`
    // - list items under schedules with `- name: ...` and indented key/value pairs
    // - `prompt: |` block scalars
    let mut lines: Vec<&str> = input.lines().collect();
    // Normalize Windows line endings if present.
    for l in lines.iter_mut() {
        if l.ends_with('\r') {
            *l = l.trim_end_matches('\r');
        }
    }

    let mut i = 0usize;
    let mut in_schedules = false;
    let mut schedules = Vec::new();

    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim_end();
        let trimmed = line.trim();

        i += 1;

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if !in_schedules {
            if trimmed == "schedules:" {
                in_schedules = true;
            }
            continue;
        }

        // Expect a list item starting with `-`.
        let indent = count_indent(line);
        if indent != 2 || !trimmed.starts_with('-') {
            // tolerate comments / extra top-level keys.
            continue;
        }

        // Parse the first line after `-`.
        let after_dash = trimmed.trim_start_matches('-').trim_start();
        let mut current = CronSchedule {
            name: String::new(),
            cron: String::new(),
            prompt: String::new(),
            enabled: true,
            notify: false,
        };

        if !after_dash.is_empty() {
            parse_schedule_kv(after_dash, &mut current, &mut i, &lines, 2)?;
        }

        // Parse subsequent indented fields (indent 4).
        while i < lines.len() {
            let raw2 = lines[i];
            let line2 = raw2.trim_end();
            let trimmed2 = line2.trim();
            if trimmed2.is_empty() || trimmed2.starts_with('#') {
                i += 1;
                continue;
            }

            let indent2 = count_indent(line2);
            if indent2 <= 2 {
                break; // next item or end
            }
            if indent2 != 4 {
                i += 1;
                continue;
            }

            // `key: value` at indent 4
            let kv = trimmed2;
            i += 1;
            parse_schedule_kv(kv, &mut current, &mut i, &lines, indent2)?;
        }

        validate_schedule(&current)?;
        schedules.push(current);
    }

    Ok(CronConfig { schedules })
}

fn validate_schedule(s: &CronSchedule) -> Result<()> {
    if s.name.trim().is_empty() {
        return Err(Error::Config("cron schedule missing name".to_string()));
    }
    if s.cron.trim().is_empty() {
        return Err(Error::Config(format!(
            "cron schedule {} missing cron",
            s.name
        )));
    }
    if s.prompt.trim().is_empty() {
        return Err(Error::Config(format!(
            "cron schedule {} missing prompt",
            s.name
        )));
    }
    if s.prompt.len() > MAX_PROMPT_LENGTH {
        return Err(Error::Config(format!(
            "cron schedule {} prompt too long: {} chars",
            s.name,
            s.prompt.len()
        )));
    }
    Ok(())
}

fn parse_schedule_kv(
    kv: &str,
    current: &mut CronSchedule,
    i: &mut usize,
    lines: &[&str],
    indent: usize,
) -> Result<()> {
    let Some((k, vraw)) = kv.split_once(':') else {
        return Ok(());
    };
    let key = k.trim();
    let value = vraw.trim();

    match key {
        "name" => current.name = strip_quotes(value).to_string(),
        "cron" => current.cron = strip_quotes(value).to_string(),
        "enabled" => current.enabled = parse_bool(value).unwrap_or(true),
        "notify" => current.notify = parse_bool(value).unwrap_or(false),
        "prompt" => {
            if value == "|" {
                // Block scalar. Capture until indent <= current indent.
                let mut block = Vec::new();
                // Determine indentation of block content from the first non-empty line.
                let mut block_indent: Option<usize> = None;

                while *i < lines.len() {
                    let raw = lines[*i];
                    let line = raw.trim_end_matches('\r');
                    let trimmed = line.trim_end();
                    let trimmed_ws = trimmed.trim();

                    let ind = count_indent(trimmed);
                    if !trimmed_ws.is_empty() {
                        if ind <= indent {
                            break;
                        }
                        if block_indent.is_none() {
                            block_indent = Some(ind);
                        }
                    } else {
                        // Empty line inside block is allowed.
                        if block_indent.is_none() {
                            // keep waiting for first content line
                        }
                    }

                    *i += 1;

                    // Inside the block, keep raw text (including leading spaces beyond the block indent).
                    let cut = block_indent.unwrap_or(indent + 2);
                    let out_line = if trimmed.len() >= cut {
                        &trimmed[cut..]
                    } else {
                        ""
                    };
                    block.push(out_line.to_string());
                }

                // YAML `|` preserves final newline, but TS prompt usage doesn't care.
                current.prompt = block.join("\n").trim_end_matches('\n').to_string();
            } else {
                current.prompt = strip_quotes(value).to_string();
            }
        }
        _ => {}
    }

    Ok(())
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

fn strip_quotes(s: &str) -> &str {
    let t = s.trim();
    if t.len() >= 2
        && ((t.starts_with('\"') && t.ends_with('\"'))
            || (t.starts_with('\'') && t.ends_with('\'')))
    {
        return &t[1..t.len() - 1];
    }
    t
}

fn count_indent(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

// === Cron expression engine ===

#[derive(Clone, Debug)]
struct CronExpr {
    min: Field,
    hour: Field,
    dom: Field,
    mon: Field,
    dow: Field,
}

#[derive(Clone, Debug)]
struct Field {
    min: u32,
    max: u32,
    any: bool,
    allowed: Vec<bool>, // index = value
}

impl CronExpr {
    fn parse(expr: &str) -> Result<Self> {
        let parts = expr
            .split_whitespace()
            .filter(|s| !s.trim().is_empty())
            .collect::<Vec<_>>();
        if parts.len() != 5 {
            return Err(Error::Config(format!(
                "expected 5 fields, got {}",
                parts.len()
            )));
        }

        let min = Field::parse(parts[0], 0, 59, false)?;
        let hour = Field::parse(parts[1], 0, 23, false)?;
        let dom = Field::parse(parts[2], 1, 31, false)?;
        let mon = Field::parse(parts[3], 1, 12, false)?;
        let dow = Field::parse(parts[4], 0, 6, true)?;

        Ok(Self {
            min,
            hour,
            dom,
            mon,
            dow,
        })
    }

    fn matches(&self, dt: DateTime<Local>) -> bool {
        let minute = dt.minute();
        let hour = dt.hour();
        let dom = dt.day();
        let mon = dt.month();
        let dow = dt.weekday().num_days_from_sunday();

        if !self.min.contains(minute) {
            return false;
        }
        if !self.hour.contains(hour) {
            return false;
        }
        if !self.mon.contains(mon) {
            return false;
        }

        // Standard cron semantics: if both DOM and DOW are restricted, match when EITHER matches.
        let dom_match = self.dom.contains(dom);
        let dow_match = self.dow.contains(dow);

        match (self.dom.any, self.dow.any) {
            (true, true) => true,
            (true, false) => dow_match,
            (false, true) => dom_match,
            (false, false) => dom_match || dow_match,
        }
    }

    fn next_after(&self, now: DateTime<Local>) -> Option<DateTime<Local>> {
        // Start at the next minute boundary.
        let mut t = now + chrono::Duration::minutes(1);
        t = t.with_second(0)?.with_nanosecond(0)?;

        // Hard cap to avoid infinite loops for impossible expressions.
        let max_iters = 366usize * 24 * 60;
        for _ in 0..max_iters {
            if self.matches(t) {
                return Some(t);
            }
            t += chrono::Duration::minutes(1);
        }
        None
    }
}

impl Field {
    fn parse(raw: &str, min: u32, max: u32, allow_7_as_0: bool) -> Result<Self> {
        let raw = raw.trim();
        if raw == "*" {
            return Ok(Self {
                min,
                max,
                any: true,
                allowed: vec![true; (max + 1) as usize],
            });
        }

        let mut allowed = vec![false; (max + 1) as usize];
        for part in raw.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }

            if part == "*" {
                for v in min..=max {
                    allowed[v as usize] = true;
                }
                continue;
            }

            let (base, step) = if let Some((a, b)) = part.split_once('/') {
                let step: u32 = b
                    .trim()
                    .parse()
                    .map_err(|_| Error::Config(format!("invalid step: {b}")))?;
                if step == 0 {
                    return Err(Error::Config("step must be > 0".to_string()));
                }
                (a.trim(), Some(step))
            } else {
                (part, None)
            };

            let (start, end) = if base == "*" {
                (min, max)
            } else if let Some((a, b)) = base.split_once('-') {
                let a = parse_u32(a.trim(), allow_7_as_0)?;
                let b = parse_u32(b.trim(), allow_7_as_0)?;
                (a, b)
            } else {
                let a = parse_u32(base.trim(), allow_7_as_0)?;
                if step.is_some() {
                    (a, max)
                } else {
                    (a, a)
                }
            };

            let start = start.max(min);
            let end = end.min(max);
            if start > end {
                return Err(Error::Config(format!("invalid range: {base}")));
            }

            let step = step.unwrap_or(1);
            let mut v = start;
            while v <= end {
                allowed[v as usize] = true;
                v = v.saturating_add(step);
                if step == 0 {
                    break;
                }
            }
        }

        // Determine "any" by checking if all values are allowed.
        let mut any = true;
        for v in min..=max {
            if !allowed[v as usize] {
                any = false;
                break;
            }
        }

        Ok(Self {
            min,
            max,
            any,
            allowed,
        })
    }

    fn contains(&self, v: u32) -> bool {
        if v < self.min || v > self.max {
            return false;
        }
        self.allowed.get(v as usize).copied().unwrap_or(false)
    }
}

fn parse_u32(s: &str, allow_7_as_0: bool) -> Result<u32> {
    let mut v: u32 = s
        .parse()
        .map_err(|_| Error::Config(format!("invalid number: {s}")))?;
    if allow_7_as_0 && v == 7 {
        v = 0;
    }
    Ok(v)
}

// === Tests ===

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn cron_expr_parses_and_matches_basic() {
        let expr = CronExpr::parse("0 * * * *").unwrap();
        let dt = Local.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap();
        assert!(expr.matches(dt));
        let dt2 = Local.with_ymd_and_hms(2026, 1, 1, 10, 1, 0).unwrap();
        assert!(!expr.matches(dt2));
    }

    #[test]
    fn cron_expr_next_after_finds_next_minute_boundary() {
        let expr = CronExpr::parse("*/5 * * * *").unwrap();
        let dt = Local.with_ymd_and_hms(2026, 1, 1, 10, 1, 30).unwrap();
        let next = expr.next_after(dt).unwrap();
        assert_eq!(next.minute(), 5);
        assert_eq!(next.second(), 0);
    }

    #[test]
    fn cron_yaml_parses_prompt_block() {
        let yaml = r#"
schedules:
  - name: heartbeat
    cron: "0 * * * *"
    prompt: |
      line1
      line2
    enabled: true
    notify: false
"#;
        let cfg = parse_cron_yaml(yaml).unwrap();
        assert_eq!(cfg.schedules.len(), 1);
        let s = &cfg.schedules[0];
        assert_eq!(s.name, "heartbeat");
        assert_eq!(s.cron, "0 * * * *");
        assert!(s.prompt.contains("line1"));
        assert!(s.prompt.contains("line2"));
        assert!(s.enabled);
        assert!(!s.notify);
    }
}
