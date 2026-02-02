# Rust Port Architecture (Plan + Module Map)

This document is the design/architecture plan for porting the existing TypeScript/Bun + grammY bot to Rust.

Goals:
- Feature parity with the current TS bot (text/voice/photo/document + streaming + cron + ask_user).
- Maintainable structure with clear boundaries (Ports & Adapters / Hexagonal).
- Keep security defense-in-depth (allowlist, rate limit, path validation, command safety, audit log).
- Make it easy to swap integrations later (Telegram library, model provider, transcription provider).

Non-goals:
- Rewriting Claude Code itself. We drive it via CLI (preferred) and keep a fallback path open.
- One-shot “big bang” replacement without parity checklist/tests.

## High-Level Flow

Telegram update
→ Router (commands vs message kinds)
→ Auth (allowlist)
→ Rate limit
→ Handler (text/voice/photo/document/callback/cron)
→ SessionRunner (model client + tool safety + persistence)
→ StreamingState (throttled editMessage updates)
→ Messenger (Telegram adapter)
→ Audit log

## Core Design Pattern: Ports & Adapters

We keep the application logic in a `core` crate, and treat Telegram / Claude CLI / OpenAI / filesystem as adapters.

Key Ports (traits):
- `MessagingPort`: send/edit messages, send media, callback answers, inline keyboards, capabilities.
- `ModelClient`: start/resume session, send prompt, stream events, cancel, usage accounting.
- `TranscriptionClient`: voice → text.
- `AuditSink`: append structured audit events (human-readable or JSON).
- `Clock` (optional): time source for deterministic tests.
- `SchedulerPort` (optional): schedule jobs, used by cron module.

Benefits:
- Easy to unit test app logic with fake ports.
- Telegram/teloxide concerns never leak into `core`.
- Claude CLI vs other providers can be swapped behind `ModelClient`.

## Proposed Cargo Workspace Layout

`rust/` (Cargo workspace root)
- `crates/ctb-core/` (library)
  - `config/` env + derived settings (including allowed paths + prompts)
  - `security/` auth + rate limit + path + command safety
  - `formatting/` markdown → Telegram HTML + tool status formatting
  - `streaming/` throttled streaming + message segmentation
  - `session/` session state + persistence + usage totals
  - `scheduler/` cron.yaml loader + job queueing
  - `usage/` token usage types + aggregation
  - `ports/` trait definitions + capability structs
  - `domain/` shared types (UserId, ChatId, MessageRef, etc.)
  - `errors.rs` (thiserror enums) + `Result<T>`
- `crates/ctb-telegram/` (library adapter)
  - teloxide wiring, update routing, inline keyboard, media downloads
- `crates/ctb-claude-cli/` (library adapter)
  - `tokio::process::Command` runner + `--stream-json` parser → `ModelEvent`
  - session resume/cancel + env/flags mapping
- `crates/ctb-openai/` (library adapter)
  - transcription via OpenAI (optional feature)
- `crates/ctb/` (binary)
  - dependency wiring + main loop + graceful shutdown

Notes:
- We can collapse this to fewer crates if it becomes friction, but the boundaries above match the bd tasks:
  - messenger abstraction ↔ `ctb-telegram` behind `MessagingPort`
  - model provider abstraction ↔ `ctb-claude-cli` behind `ModelClient`

## TS → Rust Module Map

Current TS modules:
- `src/index.ts` → `crates/ctb/src/main.rs` (bootstrap + router wiring)
- `src/config.ts` → `ctb-core::config`
- `src/security.ts` → `ctb-core::security`
- `src/session.ts` → `ctb-core::session` + `ctb-claude-cli` (provider-specific)
- `src/formatting.ts` → `ctb-core::formatting` (or `ctb-telegram::formatting`)
- `src/scheduler.ts` → `ctb-core::scheduler`
- `src/usage.ts` → `ctb-core::usage`
- `src/utils.ts` → `ctb-core::utils` + `ctb-core::audit`
- `src/handlers/*` → `ctb-telegram::handlers/*` (thin handlers that call `ctb-core`)
- `ask_user_mcp/*` → later `ctb-ask-user` (task agi-cnf.20) or keep Node for compatibility until parity is reached

## Model Provider Decision: Claude CLI vs SDK

Preferred: Claude CLI (`claude --stream-json ...`)
- Pros:
  - Closest to current “Claude Code in a working directory” behavior.
  - Keeps tooling semantics (Read/Write/Edit/Bash, MCP) aligned with the local Claude environment.
  - No need to reimplement tool execution ourselves.
- Cons / risks:
  - JSON event schema is not a stable public API.
  - Flags may differ across versions.

Fallback option (explicitly kept open):
- Implement `ModelClient` for Anthropic HTTP API (no local tool execution).
- Use it only for specific message types (e.g., voice transcription summaries) or as “degraded mode”.

Migration/rollback:
- Run Rust bot side-by-side with TS bot (different Telegram token) during parity testing.
- Keep TS as reference until parity checklist is green.

## Crate Selection (Initial)

Async/runtime:
- `tokio` + `tokio-util` (teloxide + process management)

Telegram:
- `teloxide` (mature bot framework, supports inline keyboards, media, editing)

Serialization/config:
- `serde`, `serde_json`, `serde_yaml`
- `dotenvy` (load `.env`)
- `config` or `figment` (optional) for layered config; otherwise plain env parsing

HTTP:
- `reqwest` (OpenAI transcription; optional for future providers)

Logging/errors:
- `tracing`, `tracing-subscriber` (structured logs; file + stdout)
- `anyhow` for top-level error context
- `thiserror` for typed errors in `ctb-core`

File watching / utilities:
- `notify` (cron.yaml auto-reload)
- `tempfile` (safe temp paths)
- `regex` (blocked patterns, markdown conversions if needed)

Testing:
- `insta` (snapshot tests for stream-json parsing + formatting)
- `proptest` (optional) for path validation edge cases

## Risks + Mitigations

1) Claude CLI event schema changes
- Mitigation:
  - Parse defensively: unknown event types are ignored with trace-level logs.
  - Maintain a small fixture corpus (recorded `--stream-json` logs) and snapshot tests.

2) Telegram rate limits / editMessage storms
- Mitigation:
  - Central `StreamingState` with throttle + coalescing.
  - Capability map includes `max_edit_rate_hz` and `max_message_len`.

3) Security regressions (paths/commands)
- Mitigation:
  - Single `security` module reused by all handlers.
  - Deny-by-default path access with canonicalization and temp-path allowlist.
  - “dangerous action confirmation” remains a system-prompt rule and a host-side guardrail.

4) Cron jobs overlapping with active session
- Mitigation:
  - Same queueing semantics as TS: enqueue while session is running; drain after completion.
  - Hard limits: jobs/hour, pending queue size.

5) Feature parity gaps (ask_user MCP, usage counters, photo albums)
- Mitigation:
  - Parity checklist + incremental rollout.
  - Keep ask_user Node MCP server temporarily if needed; port later behind a stable interface.

