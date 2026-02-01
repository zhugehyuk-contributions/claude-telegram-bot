# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
# Development
bun run start      # Run the bot
bun run dev        # Run with auto-reload (--watch)
bun run typecheck  # Type check
bun install        # Install dependencies

# Build & Quality
make up            # Deploy: install → build → stop → start
make lint          # Lint code
make fmt           # Format code
make test          # Run tests

# Service Management (macOS)
make start         # Start service
make stop          # Stop service
make restart       # Restart service
make logs          # View logs
make status        # Check status
```

## Architecture

This is a Telegram bot (~3,300 lines TypeScript) that lets you control Claude Code from your phone via text, voice, photos, and documents. Built with Bun and grammY.

### Message Flow

```
Telegram message → Handler → Auth check → Rate limit → Claude session → Streaming response → Audit log
```

### Key Modules

- **`src/index.ts`** - Entry point, registers handlers, starts polling
- **`src/config.ts`** - Environment parsing, MCP loading, safety prompts
- **`src/session.ts`** - `ClaudeSession` class wrapping Agent SDK V1 with streaming, session persistence, cumulative token tracking (input/output/cache), and defense-in-depth safety checks
- **`src/security.ts`** - `RateLimiter` (token bucket), path validation, command safety checks
- **`src/formatting.ts`** - Markdown→HTML conversion for Telegram, tool status emoji formatting
- **`src/utils.ts`** - Audit logging, voice transcription (OpenAI), typing indicators
- **`src/types.ts`** - Shared TypeScript types
- **`src/scheduler.ts`** - Cron scheduler for scheduled prompts (loads `cron.yaml`, auto-reloads on file changes)

### Handlers (`src/handlers/`)

Each message type has a dedicated async handler:
- **`commands.ts`** - `/start`, `/help`, `/new`, `/stop`, `/status`, `/stats`, `/resume`, `/cron`, `/restart`, `/retry`
- **`text.ts`** - Text messages with intent filtering
- **`voice.ts`** - Voice→text via OpenAI, then same flow as text
- **`photo.ts`** - Image analysis with media group buffering (1s timeout for albums)
- **`document.ts`** - PDF extraction (pdftotext CLI) and text file processing
- **`callback.ts`** - Inline keyboard button handling for ask_user MCP
- **`streaming.ts`** - Shared `StreamingState` and status callback factory

### Security Layers

1. User allowlist (`TELEGRAM_ALLOWED_USERS`)
2. Rate limiting (token bucket, configurable)
3. Path validation (`ALLOWED_PATHS`)
4. Command safety (blocked patterns)
5. System prompt constraints
6. Audit logging

### Configuration

All config via `.env` (copy from `.env.example`). Key variables:
- `TELEGRAM_BOT_TOKEN`, `TELEGRAM_ALLOWED_USERS` (required)
- `CLAUDE_WORKING_DIR` - Working directory for Claude
- `ALLOWED_PATHS` - Directories Claude can access
- `OPENAI_API_KEY` - For voice transcription

MCP servers defined in `mcp-config.ts`.

### Runtime Files

- `/tmp/claude-telegram-session.json` - Session persistence for `/resume`
- `/tmp/telegram-bot/` - Downloaded photos/documents
- `/tmp/claude-telegram-audit.log` - Audit log
- `cron.yaml` - Cron scheduler config (in working directory)

## Patterns

**Adding a command**: Create handler in `commands.ts`, register in `index.ts` with `bot.command("name", handler)`

**Adding a message handler**: Create in `handlers/`, export from `index.ts`, register in `index.ts` with appropriate filter

**Streaming pattern**: All handlers use `createStatusCallback()` from `streaming.ts` and `session.sendMessageStreaming()` for live updates.

**Type checking**: Run `bun run typecheck` periodically while editing TypeScript files. Fix any type errors before committing.

**After code changes**: Restart the bot so changes can be tested. Use `launchctl kickstart -k gui/$(id -u)/com.claude-telegram-ts` if running as a service, or `bun run start` for manual runs.

## Standalone Build

The bot can be compiled to a standalone binary with `bun build --compile`. This is used by the ClaudeBot macOS app wrapper.

### External Dependencies

PDF extraction uses `pdftotext` CLI instead of an npm package (to avoid bundling issues):

```bash
brew install poppler  # Provides pdftotext
```

### PATH Requirements

When running as a standalone binary (especially from a macOS app), the PATH may not include Homebrew. The launcher must ensure PATH includes:
- `/opt/homebrew/bin` (Apple Silicon Homebrew)
- `/usr/local/bin` (Intel Homebrew)

Without this, `pdftotext` won't be found and PDF parsing will fail silently with an error message.

## Development Workflow

**After modifying code**, follow this workflow to ensure quality:

```bash
# 1. Run PR review (checks code quality, errors, types)
/pr-review-toolkit:review-pr

# 2. Fix any critical/important issues found
# (Edit files to address review findings)

# 3. Simplify code for clarity
# (Uses code-simplifier agent)

# 4. Lint and format
make lint
make fmt

# 5. Commit and push
git add -A
git commit -m "feat: description

- Detailed changes
- Bug fixes
- Improvements

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Elon Musk (AI) <elon@2lab.ai>"
git push
```

### Make Targets

```bash
make up          # Full deployment: install → build → stop → start
make install     # Install dependencies (bun install)
make build       # Type check (bun run typecheck)
make lint        # Run ESLint
make fmt         # Format with Prettier
make test        # Run tests
make stop        # Stop launchd service
make start       # Start launchd service
make restart     # Restart service
make logs        # Tail service logs
make errors      # Tail error logs
make status      # Check service status
```

## Commit Style

Commits should include Claude Code footer and Co-Authored-By trailer as shown in the workflow above.

## Running as Service (macOS)

```bash
cp launchagent/com.claude-telegram-ts.plist.template ~/Library/LaunchAgents/com.claude-telegram-ts.plist
# Edit plist with your paths
launchctl load ~/Library/LaunchAgents/com.claude-telegram-ts.plist

# Logs
tail -f /tmp/claude-telegram-bot-ts.log
tail -f /tmp/claude-telegram-bot-ts.err
```
