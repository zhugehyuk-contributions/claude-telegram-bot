# Rust Port Parity Checklist + Smoke Tests

This checklist is the “landing the plane” document for the Rust port.

## Build/Run

- [ ] `cd rust && cargo test --workspace` passes
- [ ] `cd rust && cargo build -p ctb -p ctb-ask-user-mcp --release` produces binaries
- [ ] Bot starts with `make start-rust` (or `./rust/target/release/ctb`)

## Core Telegram UX

- [ ] `/start` works
- [ ] `/help` works
- [ ] `/new` starts a fresh session
- [ ] `/status` shows current state
- [ ] `/stats` shows token usage + provider usage (where configured)
- [ ] `/stop` cancels a running request
- [ ] `!` prefix interrupts/queues correctly
- [ ] `/resume` restores the last session from disk
- [ ] `/retry` retries last prompt
- [ ] `/restart` writes the restart file and exits (service restarts)

## Streaming/Formatting

- [ ] Streaming updates appear (throttled edits, segments)
- [ ] Tool events are shown (and deleted if configured)
- [ ] Markdown → Telegram HTML conversion is safe (no broken HTML / injection)

## Media

- [ ] Voice: `.ogg` downloads, OpenAI transcription works (when `OPENAI_API_KEY` is set)
- [ ] Photo: single photo works
- [ ] Photo album: media group buffering works (1s window)
- [ ] Document: plain text works
- [ ] PDF: `pdftotext` extraction works
- [ ] Archive: ZIP/TAR extraction works and blocks traversal/bomb attempts

## Cron

- [ ] `cron.yaml` loads
- [ ] Scheduler executes prompts on schedule
- [ ] `/cron reload` reloads jobs
- [ ] File watcher detects `cron.yaml` changes and reloads

## MCP / ask_user

- [ ] `mcp-config.json` is picked up (if present) and passed to `claude --mcp-config ...`
- [ ] `ask_user` tool call results in an inline keyboard in Telegram
- [ ] Clicking a button injects the choice back into the next run
- [ ] Multi-chat safety: `TELEGRAM_CHAT_ID` is injected per run for the `ask-user` server

## Security/Controls

- [ ] Allowlist blocks unauthorized users
- [ ] Rate limiting works (and returns friendly “retry after”)
- [ ] Allowed paths enforced for Read/Write/Edit
- [ ] `rm` path validation blocks outside allowed dirs
- [ ] Dangerous patterns are blocked (`rm -rf /`, `mkfs`, etc.)
- [ ] Audit log written to `/tmp/claude-telegram-audit.log` (or configured path)

## Manual Smoke Test Plan

1. Start bot (Rust): `make build-rust && make start-rust`
2. In Telegram: send `/start` and verify allowlist/user id output
3. Send a basic text prompt and watch streaming updates
4. Send `!` prefixed message while it runs; verify queue/interrupt behavior
5. Send `/stop` mid-run; verify cancellation
6. Send `/status` and `/stats`
7. Send a voice message (requires `OPENAI_API_KEY`)
8. Send a photo (single) and an album (2+ images)
9. Send a small PDF (requires `pdftotext`)
10. Send a safe ZIP (and separately test a malicious traversal zip; ensure it’s blocked)
11. Configure MCP:
    - copy `mcp-config.example.json` → `mcp-config.json`
    - ensure `ctb-ask-user-mcp` exists at the configured path
12. Ask Claude a question that triggers `ask_user` and verify buttons show up
13. Click a button and verify the selection is processed
14. Configure a simple `cron.yaml` and verify scheduled messages

## Known Gaps (If Any)

Document any remaining parity gaps here before fully switching off the TS bot.

