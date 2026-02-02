# Claude CLI `--output-format=stream-json` Notes

This captures the Rust-port requirements for driving Claude Code via the `claude` CLI in streaming JSON mode.

## Key CLI Flags (Rust Port)

Minimal non-interactive streaming:
- `-p, --print`: non-interactive mode (required for bot/service)
- `--output-format stream-json`: newline-delimited JSON events
- `--verbose`: required for `--print` + `--output-format=stream-json` (otherwise CLI errors)

Security / permissions:
- `--permission-mode bypassPermissions`: skip interactive permission prompts
- `--dangerously-skip-permissions`: bypass all permission checks (use only in sandboxed environments)
- `--add-dir <dir...>`: additional directories Claude tools can access (maps to TS `additionalDirectories`)

Session management:
- `-r, --resume [value]`: resume by session ID (or interactive picker)
- `-c, --continue`: continue the most recent conversation in the current directory
- `--fork-session`: when resuming, generate a new session ID
- `--no-session-persistence`: disables saving sessions to disk (not recommended for this bot)

MCP:
- `--mcp-config <configs...>`: load MCP servers from JSON files or JSON strings

Input/output:
- `--input-format text|stream-json`: input format (only with `--print`)
- `--include-partial-messages`: include partial chunks in stream-json (useful for fine-grained streaming)

Model selection:
- `--model <model>`: alias (`sonnet`, `opus`) or full model name

## Critical Env Var For Services/Sandboxes

The CLI writes config/state under `$CLAUDE_CONFIG_DIR` (default: `~/.claude`).

When running under a sandbox/service where `~` is not writable, set:
- `CLAUDE_CONFIG_DIR=/tmp/claude-config` (or another writable directory)

Without this, `claude` may fail early with `EACCES` trying to write `~/.claude.json`.

## Observed Stream-JSON Event Shapes

The output is NDJSON (one JSON object per line), each with a top-level `type`.

Captured in this environment (note: the sandbox cannot resolve DNS, so the run ends with a connection error; the shapes are still correct for parsing):
- `system` / `init` (first event)
- `assistant` (message container; contains `usage` and `content[]`)
- `result` (final summary; contains `duration_ms`, `total_cost_usd`, `usage`, etc.)

See sample fixture:
- `docs/rust-port/fixtures/claude-stream-json.sample.jsonl`

Additional fixtures:
- `docs/rust-port/fixtures/claude-stream-json.invalid-api-key.jsonl` (real output when ANTHROPIC_API_KEY is missing/invalid)
- `docs/rust-port/fixtures/claude-stream-json.synthetic-tool-use.jsonl` (synthetic shape used for parser regression tests when we cannot capture a live tool_use run)

Additional types we should handle (defensive parsing):
- `tool_progress`, `tool_use_summary`
- `auth_status`
- `keep_alive`
- `control_request` / `control_response` / `control_cancel_request`
- `stream_event`

Parser rule of thumb for Rust:
- Keep a strongly-typed model for the events we rely on (`system:init`, `assistant`, `result`).
- Parse everything else as `Unknown(Value)` and ignore (but log at `trace` with the event `type`).
