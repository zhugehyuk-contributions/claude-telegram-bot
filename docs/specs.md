# Claude Telegram Bot - Technical Specification

**Version**: 1.0
**Last Updated**: 2026-01-14
**Author**: Codebase Analysis

---

## 1. Executive Summary

Claude Telegram Bot은 **Anthropic Claude Code를 Telegram에서 원격으로 사용**할 수 있게 하는 TypeScript 기반 봇입니다. 텍스트, 음성, 사진, 문서를 통해 Claude와 상호작용하며, 실시간 스트리밍 응답과 세션 지속성을 제공합니다.

### Key Features
- 💬 Multi-modal 입력: 텍스트, 음성(OpenAI Whisper), 사진, 문서(PDF/ZIP 지원)
- 🔄 Session persistence: 재시작 후에도 대화 이어가기 가능
- 📨 Message queuing: Claude 실행 중에도 메시지 큐잉
- 🧠 Extended thinking: "think", "reason" 키워드로 추론 과정 표시
- 🔘 Interactive buttons: MCP ask_user 툴로 인라인 버튼 제공
- 🔐 Defense-in-depth security: 6단계 보안 계층

### Tech Stack
| Layer | Technology |
|-------|-----------|
| Runtime | Bun 1.0+ |
| Language | TypeScript 5.x |
| Bot Framework | grammY 1.38+ |
| Claude Integration | @anthropic-ai/claude-agent-sdk 0.1.76+ |
| Voice Transcription | OpenAI API (Whisper) |
| MCP Protocol | @modelcontextprotocol/sdk 1.25+ |

### Metrics
- **Total Lines**: ~3,300 TypeScript (excluding dependencies)
- **Main Modules**: 8 core modules + 8 handlers
- **Architecture**: Event-driven async handlers with streaming
- **Security Layers**: 6 (allowlist, rate limiting, path validation, command safety, system prompt, audit logging)

---

## 2. Architecture Overview

### 2.1 High-Level Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        Telegram Bot API                         │
└───────────────────────┬─────────────────────────────────────────┘
                        │
                        ▼
┌─────────────────────────────────────────────────────────────────┐
│                     grammY Bot Instance                         │
│  - Sequentialization (per chat)                                 │
│  - Command routing                                               │
│  - Error handling                                                │
└───────────────────────┬─────────────────────────────────────────┘
                        │
        ┌───────────────┼────────────────┐
        ▼               ▼                ▼
   ┌─────────┐    ┌─────────┐    ┌──────────┐
   │ Commands│    │ Messages│    │ Callbacks│
   │ Handler │    │ Handler │    │ Handler  │
   └────┬────┘    └────┬────┘    └────┬─────┘
        │              │               │
        └──────────────┼───────────────┘
                       ▼
        ┌──────────────────────────────┐
        │   Security Layer             │
        │  - Authorization             │
        │  - Rate Limiting             │
        │  - Path Validation           │
        │  - Command Safety            │
        └──────────┬───────────────────┘
                   ▼
        ┌──────────────────────────────┐
        │   ClaudeSession              │
        │  - SDK V1 query()            │
        │  - Streaming events          │
        │  - Session persistence       │
        │  - Abort control             │
        └──────────┬───────────────────┘
                   ▼
        ┌──────────────────────────────┐
        │  Claude Agent SDK V1         │
        │  - CLI auth / API key auth   │
        │  - MCP server integration    │
        │  - Tool execution            │
        │  - Extended thinking         │
        └──────────┬───────────────────┘
                   ▼
        ┌──────────────────────────────┐
        │  Claude Code                 │
        │  (Sonnet-4.5)                │
        └──────────────────────────────┘
```

### 2.2 Message Flow

```
User Message (Telegram)
    ↓
grammY Middleware (sequentialization)
    ↓
Handler Selection (command | text | voice | photo | document)
    ↓
Authorization Check (TELEGRAM_ALLOWED_USERS)
    ↓
Rate Limiting (token bucket)
    ↓
Message Processing (voice→text, photo download, PDF extraction)
    ↓
ClaudeSession.sendMessageStreaming()
    ├─ Safety checks (command patterns, file paths)
    ├─ SDK query() with streaming
    ├─ Real-time updates (thinking, tool use, text)
    └─ Session persistence
    ↓
StreamingState (message updates)
    ↓
Telegram Response (formatted HTML)
    ↓
Audit Log (file write)
```

---

## 3. Core Modules

### 3.1 `src/index.ts` - Entry Point
**Purpose**: Bot initialization, handler registration, graceful shutdown

**Key Responsibilities**:
- Bot instance creation with grammY
- Sequentialize messages per chat (prevent race conditions)
- Command routing: `/start`, `/new`, `/stop`, `/status`, `/resume`, `/restart`, `/retry`
- Message routing: text, voice, photo, document
- Callback query routing (inline buttons)
- Error handling and logging
- Restart file check (update "Bot restarted" message after restart)

**Key Code**:
```typescript
bot.use(sequentialize((ctx) => {
  if (ctx.message?.text?.startsWith("/")) return undefined; // Commands bypass
  if (ctx.message?.text?.startsWith("!")) return undefined; // Interrupt prefix
  return ctx.chat?.id.toString(); // Sequentialize per chat
}));
```

### 3.2 `src/config.ts` - Configuration Management
**Purpose**: Environment variable parsing, MCP loading, safety prompt generation

**Key Exports**:
- `TELEGRAM_TOKEN`, `ALLOWED_USERS`: Core authentication
- `WORKING_DIR`, `ALLOWED_PATHS`: File access control
- `OPENAI_API_KEY`: Voice transcription
- `MCP_SERVERS`: Dynamically loaded from `mcp-config.ts`
- `SAFETY_PROMPT`: Generated from `ALLOWED_PATHS`
- `BLOCKED_PATTERNS`: Dangerous command patterns
- `THINKING_KEYWORDS`, `THINKING_DEEP_KEYWORDS`: Trigger words
- `RATE_LIMIT_*`: Rate limiting config
- `AUDIT_LOG_PATH`: Logging configuration

**Security Features**:
- PATH enhancement (adds Homebrew, .local/bin)
- Default allowed paths with override support
- Command pattern blocklist
- Audit logging config

**Example**:
```typescript
export const SAFETY_PROMPT = buildSafetyPrompt(ALLOWED_PATHS);
// Generates dynamic prompt with allowed directories
```

### 3.3 `src/session.ts` - ClaudeSession Class
**Purpose**: Claude Agent SDK V1 integration with session management

**Key Features**:
- **Session persistence**: Saves `session_id` to `/tmp/claude-telegram-session.json`
- **Streaming response**: Yields `thinking`, `tool_use`, `text`, `result` events
- **Extended thinking**: Dynamic token budget (0, 10K, 50K) based on keywords
- **Abort control**: Graceful cancellation with `AbortController`
- **Safety checks**: Real-time validation of Bash commands and file paths
- **MCP integration**: ask_user tool with inline button support
- **Retry logic**: Auto-retry on Claude Code crashes

**State Machine**:
```
Idle
  ↓ sendMessageStreaming()
Processing
  ↓ query()
Query Running
  ├─ Tool use → Safety check → Status callback
  ├─ Text → Streaming update
  └─ Thinking → Display reasoning
  ↓
Done / Error / Aborted
```

**Critical Code**:
```typescript
const options: Options = {
  model: "claude-sonnet-4-5",
  cwd: WORKING_DIR,
  permissionMode: "bypassPermissions", // ⚠️ No permission prompts
  allowDangerouslySkipPermissions: true,
  systemPrompt: SAFETY_PROMPT,
  mcpServers: MCP_SERVERS,
  maxThinkingTokens: thinkingTokens,
  additionalDirectories: ALLOWED_PATHS,
  resume: this.sessionId || undefined
};
```

**Methods**:
| Method | Purpose |
|--------|---------|
| `sendMessageStreaming()` | Main query method with streaming |
| `stop()` | Abort current query |
| `kill()` | Clear session |
| `resumeLast()` | Restore from disk |
| `consumeInterruptFlag()` | Check if stopped by new message |

### 3.4 `src/security.ts` - Security Module
**Purpose**: Rate limiting, path validation, command safety

#### RateLimiter Class
- **Algorithm**: Token bucket (refill over time)
- **Default**: 20 requests per 60 seconds
- **Per-user tracking**: Separate buckets for each Telegram user ID
- **Methods**: `check(userId)`, `getStatus(userId)`

#### Path Validation
```typescript
isPathAllowed(path: string): boolean
```
- Expands `~` to home directory
- Resolves symlinks with `realpathSync()`
- Checks containment (exact match or subdirectory)
- Always allows temp paths (`/tmp/`, `/var/folders/`)

#### Command Safety
```typescript
checkCommandSafety(command: string): [safe: boolean, reason: string]
```
- **Blocked patterns**: `rm -rf /`, `sudo rm`, fork bomb, `dd if=`, etc.
- **rm validation**: Checks each path argument against `ALLOWED_PATHS`
- **Returns**: `[true, ""]` if safe, `[false, "reason"]` if blocked

### 3.5 `src/formatting.ts` - Message Formatting
**Purpose**: Convert Claude's Markdown to Telegram HTML

**Key Functions**:
- `markdownToTelegramHtml()`: Converts code blocks, bold, italic, links
- `formatToolStatus()`: Emoji-based tool display
  - `Bash` → 💻
  - `Edit` → ✏️
  - `Write` → 📝
  - `Read` → 📖
  - `Grep` → 🔍
  - `Glob` → 📂
- `splitLongMessage()`: Respects Telegram 4096 char limit
- `truncateText()`: Ellipsis truncation

**Example**:
```typescript
formatToolStatus("Bash", { command: "ls -la" })
// Returns: "💻 ls -la"
```

### 3.6 `src/utils.ts` - Utility Functions
**Purpose**: Audit logging, voice transcription, typing indicators

#### Audit Logging
```typescript
auditLog(userId, username, type, input, output)
```
- Writes to `AUDIT_LOG_PATH` (default: `/tmp/claude-telegram-audit.log`)
- Supports plaintext and JSON formats
- Logs: message, auth, tool_use, error, rate_limit

#### Voice Transcription
```typescript
transcribeVoice(filePath: string): Promise<string>
```
- Uses OpenAI Whisper API
- Converts OGG (Telegram) to MP3
- Returns transcribed text

#### Typing Indicator
```typescript
startTypingIndicator(ctx: Context)
```
- Shows "typing..." every 5 seconds
- Auto-stops when done
- Returns object with `stop()` method

#### Interrupt Check
```typescript
checkInterrupt(message: string): Promise<string>
```
- Detects `!` prefix
- Stops current query
- Returns cleaned message

### 3.7 `src/types.ts` - Type Definitions
**Purpose**: Shared TypeScript interfaces

**Key Types**:
```typescript
interface SessionData {
  session_id: string;
  saved_at: string;
  working_dir: string;
}

interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
  cache_read_input_tokens?: number;
  cache_creation_input_tokens?: number;
}

type StatusType = "thinking" | "tool" | "text" | "segment_end" | "done";
type StatusCallback = (type: StatusType, content: string, segmentId?: number) => Promise<void>;

interface McpServerConfig {
  command: string;
  args?: string[];
  env?: Record<string, string>;
}

interface RateLimitBucket {
  tokens: number;
  lastUpdate: number;
}
```

### 3.8 Built-in MCP Server - ask_user
**Location**: `ask_user_mcp/server.ts`

**Purpose**: Let Claude present options as Telegram inline keyboard buttons

**How it works**:
1. Claude calls `mcp__ask-user__ask_question` tool
2. MCP server writes question to `/tmp/ask_user_pending_{chatId}.json`
3. `streaming.ts` detects file and sends inline keyboard
4. User clicks button → `callback.ts` handles response
5. Response sent back to Claude in next message

**JSON Format**:
```json
{
  "chat_id": 123456789,
  "question": "Choose an option:",
  "options": ["Option A", "Option B", "Option C"]
}
```

---

## 4. Handler Modules

### 4.1 `handlers/commands.ts` - Command Handlers
**Commands**:
| Command | Description | Implementation |
|---------|-------------|----------------|
| `/start` | Show status and user ID | Displays bot info, session status, rate limit |
| `/new` | Start fresh session | Kills current session |
| `/stop` | Interrupt current query | Calls `session.stop()` |
| `/status` | Check what Claude is doing | Shows last tool, error, usage |
| `/resume` | Resume last session | Loads from `/tmp/claude-telegram-session.json` |
| `/restart` | Restart bot process | Exits with code 0 (LaunchAgent restarts) |
| `/retry` | Retry last message | Resends `session.lastMessage` |

**Authorization**: All commands check `isAuthorized(userId, ALLOWED_USERS)`

### 4.2 `handlers/text.ts` - Text Message Handler
**Flow**:
1. Authorization check
2. Interrupt detection (`!` prefix)
3. Rate limiting
4. Store message for `/retry`
5. Mark processing started
6. Start typing indicator
7. Create streaming state
8. Send to Claude with retry (1 retry on crash)
9. Audit log
10. Cleanup

**Retry Logic**:
- Max 1 retry
- Only retries on Claude Code crashes (`exited with code`)
- Cleans up partial messages
- Kills corrupted session before retry

### 4.3 `handlers/voice.ts` - Voice Message Handler
**Flow**:
1. Download OGG file from Telegram
2. Transcribe with OpenAI Whisper
3. Show transcription to user
4. Process as text message

**Features**:
- Supports multi-language transcription
- Shows "🎤 Transcribing..." status
- Falls back to error message if transcription fails

### 4.4 `handlers/photo.ts` - Photo Message Handler
**Features**:
- **Media group buffering**: Waits 1 second for additional photos
- **Highest resolution**: Selects largest photo size
- **Caption support**: Includes photo caption
- Downloads to `/tmp/telegram-bot/`

**Example Message**:
```
[User uploads 3 photos with caption "Analyze these charts"]
→ Bot downloads all 3 photos
→ Sends to Claude with caption
→ Claude analyzes all photos in context
```

### 4.5 `handlers/document.ts` - Document Handler
**Supported Formats**:
| Format | Processing |
|--------|-----------|
| `.txt`, `.md`, `.json`, `.yaml`, `.yml` | Direct text read |
| `.pdf` | Extracted with `pdftotext` CLI |
| `.zip`, `.tar.gz` | Extracted recursively |
| Code files (`.py`, `.js`, `.ts`, etc.) | Read as text |

**PDF Processing**:
```bash
pdftotext -layout document.pdf - # Streams to stdout
```

**Archive Processing**:
- Extracts to temp directory
- Recursively reads all text files
- Truncates to 100KB per file
- Combines into single message

**Size Limits**:
- Individual file: 100KB
- Total ZIP content: 1MB

### 4.6 `handlers/callback.ts` - Inline Button Handler
**Purpose**: Handle ask_user button clicks

**Flow**:
1. User clicks button
2. Answer callback query (remove loading state)
3. Delete inline keyboard message
4. Send user's selection back to Claude
5. Process response with streaming

**Example**:
```
Claude: "Choose database: PostgreSQL | MySQL | MongoDB"
[Inline buttons appear]
User clicks "PostgreSQL"
→ Message: "PostgreSQL"
→ Claude receives and continues
```

### 4.7 `handlers/streaming.ts` - Streaming State Management
**Purpose**: Track and update live messages during Claude execution

#### StreamingState Class
**State**:
```typescript
{
  currentSegment: number;
  segmentMessages: Map<number, Message>; // Segment ID → Telegram Message
  toolMessages: Message[]; // Tool status messages
  lastToolMessageText: string;
  lastTextMessageText: string;
}
```

**Methods**:
- `updateToolMessage()`: Update/create tool status message
- `updateTextMessage()`: Update/create text segment message
- `finalizeSegment()`: Mark segment complete, create new for next

#### createStatusCallback Factory
**Returns**: `StatusCallback` function that:
- `"thinking"` → Show thinking block
- `"tool"` → Update tool status message
- `"text"` → Stream text updates
- `"segment_end"` → Finalize current segment
- `"done"` → Cleanup

**Throttling**: Text updates max once per 500ms

#### checkPendingAskUserRequests
**Purpose**: Check for ask_user MCP pending files

**Flow**:
1. Read `/tmp/ask_user_pending_{chatId}.json`
2. Parse question and options
3. Create inline keyboard
4. Send to Telegram
5. Delete pending file

### 4.8 `handlers/media-group.ts` - Media Group Buffer
**Purpose**: Group multiple photos sent together (albums)

**Implementation**:
- Uses `Map<string, Photo[]>` keyed by `media_group_id`
- Waits 1 second after last photo
- Returns all photos in group

**Usage**:
```typescript
const photos = await bufferMediaGroup(ctx);
// Returns all photos in album
```

---

## 5. Security Architecture

### 5.1 Permission Mode

**⚠️ CRITICAL**: Bot runs in `bypassPermissions` mode

```typescript
permissionMode: "bypassPermissions",
allowDangerouslySkipPermissions: true
```

**Implications**:
- No permission prompts for file reads/writes
- No permission prompts for command execution
- All tools run autonomously

**Rationale**: Mobile UX - confirmation prompts impractical on phone

### 5.2 Defense in Depth - 6 Layers

#### Layer 1: User Allowlist
```typescript
TELEGRAM_ALLOWED_USERS=123456789,987654321
```
- Numeric Telegram user IDs (unspoofable)
- Checked on every message/command
- Unauthorized attempts logged

#### Layer 2: Rate Limiting
```typescript
Default: 20 requests / 60 seconds per user
```
- Token bucket algorithm
- Per-user tracking
- Configurable via env vars

#### Layer 3: Path Validation
```typescript
ALLOWED_PATHS=/project,/home/user/Documents,~/.claude
```
- Whitelist-based
- Symlink resolution
- Subdirectory containment check
- Temp paths always allowed

#### Layer 4: Command Safety
**Blocked Patterns**:
- `rm -rf /`, `rm -rf ~`, `rm -rf $HOME`
- `sudo rm`
- Fork bomb: `:(){ :|:& };:`
- Disk operations: `> /dev/sd`, `mkfs.`, `dd if=`

**rm Path Validation**:
```bash
rm file.txt              # ✓ if in ALLOWED_PATHS
rm /etc/passwd           # ✗ blocked
rm -rf ./node_modules    # ✓ if cwd in ALLOWED_PATHS
```

#### Layer 5: System Prompt
```typescript
SAFETY_PROMPT = `
CRITICAL SAFETY RULES FOR TELEGRAM BOT:
1. NEVER delete files without EXPLICIT confirmation
2. You can ONLY access files in these directories: [list]
3. NEVER run dangerous commands
4. For destructive actions, ALWAYS ask confirmation
`
```

#### Layer 6: Audit Logging
```typescript
/tmp/claude-telegram-audit.log
```
**Logged Events**:
- `message`: User input and Claude output
- `auth`: Authorization attempts (allowed/denied)
- `tool_use`: Tool calls with arguments
- `error`: Errors during processing
- `rate_limit`: Rate limit triggers

**Format Options**:
- Plaintext (default)
- JSON (`AUDIT_LOG_JSON=true`)

### 5.3 What This Doesn't Protect Against

1. **Malicious authorized users**: Full access if in allowlist
2. **Zero-day vulnerabilities**: Unknown bugs in SDK/dependencies
3. **Physical access**: Machine compromise
4. **Prompt injection in documents**: Malicious content in PDFs/images
5. **Social engineering**: User approving dangerous actions

---

## 6. Configuration

### 6.1 Environment Variables

**Required**:
| Variable | Purpose | Example |
|----------|---------|---------|
| `TELEGRAM_BOT_TOKEN` | Bot token from @BotFather | `1234567890:ABC-DEF...` |
| `TELEGRAM_ALLOWED_USERS` | Comma-separated user IDs | `123456789,987654321` |

**Recommended**:
| Variable | Purpose | Default |
|----------|---------|---------|
| `CLAUDE_WORKING_DIR` | Working directory | `$HOME` |
| `OPENAI_API_KEY` | Voice transcription | None |
| `ALLOWED_PATHS` | File access whitelist | See below |

**Optional**:
| Variable | Purpose | Default |
|----------|---------|---------|
| `CLAUDE_CLI_PATH` | Path to `claude` binary | Auto-detected |
| `ANTHROPIC_API_KEY` | API auth (instead of CLI) | None |
| `RATE_LIMIT_ENABLED` | Enable rate limiting | `true` |
| `RATE_LIMIT_REQUESTS` | Requests per window | `20` |
| `RATE_LIMIT_WINDOW` | Window in seconds | `60` |
| `AUDIT_LOG_PATH` | Audit log location | `/tmp/claude-telegram-audit.log` |
| `AUDIT_LOG_JSON` | JSON format logging | `false` |
| `THINKING_KEYWORDS` | Normal thinking triggers | `think,pensa,ragiona` |
| `THINKING_DEEP_KEYWORDS` | Deep thinking triggers | `ultrathink,think hard` |
| `TRANSCRIPTION_CONTEXT` | Voice transcription context | None |

**Default Allowed Paths**:
```typescript
[
  CLAUDE_WORKING_DIR,
  "~/Documents",
  "~/Downloads",
  "~/Desktop",
  "~/.claude"
]
```

**Override**: Set `ALLOWED_PATHS` to replace all defaults
```bash
ALLOWED_PATHS=/project,/data,~/.claude
```

### 6.2 MCP Configuration

**File**: `mcp-config.ts` (or `mcp-config.local.ts`)

**Format**:
```typescript
export const MCP_SERVERS: Record<string, McpServerConfig> = {
  "server-name": {
    command: "/path/to/mcp-server",
    args: ["--arg1", "value1"],
    env: {
      API_KEY: process.env.SERVER_API_KEY
    }
  }
};
```

**Built-in Server**:
```typescript
"ask-user": {
  command: "bun",
  args: ["run", "./ask_user_mcp/server.ts"],
  env: { TELEGRAM_CHAT_ID: process.env.TELEGRAM_CHAT_ID }
}
```

---

## 7. Runtime Behavior

### 7.1 Session Lifecycle

```
Bot Start
  ↓
No active session
  ↓
User sends message
  ↓
New session created
  ├─ session_id generated by SDK
  └─ Saved to /tmp/claude-telegram-session.json
  ↓
Conversation continues (session_id reused)
  ↓
Bot restart
  ↓
/resume command
  ↓
Session restored from file
  ↓
Conversation continues
```

### 7.2 Thinking Modes

**Detection**: Keyword-based (case-insensitive)

| Keywords | Token Budget | Use Case |
|----------|--------------|----------|
| None | 0 | Normal queries |
| `think`, `pensa`, `ragiona` | 10,000 | Standard reasoning |
| `ultrathink`, `think hard`, `pensa bene` | 50,000 | Deep reasoning |

**Display**: Thinking blocks streamed as separate messages

### 7.3 Message Queuing

**Behavior**:
- Messages sent while Claude is working queue automatically
- Processed sequentially (grammY sequentialize middleware)
- `/stop` command always works immediately

**Interrupt Mechanism**:
- Prefix message with `!` to interrupt current query
- Example: `!new question` stops current, starts new

### 7.4 File Persistence

| File | Purpose | Format |
|------|---------|--------|
| `/tmp/claude-telegram-session.json` | Session persistence | JSON |
| `/tmp/claude-telegram-restart.json` | Restart message update | JSON |
| `/tmp/claude-telegram-audit.log` | Audit log | Text/JSON |
| `/tmp/telegram-bot/*.{jpg,png,pdf}` | Downloaded media | Binary |
| `/tmp/ask_user_pending_{chatId}.json` | Pending button questions | JSON |

---

## 8. Deployment

### 8.1 Running Modes

#### Development
```bash
bun --watch run src/index.ts
```
- Auto-reload on file changes
- Terminal output

#### Production
```bash
bun run src/index.ts
```
- No auto-reload
- Can be run in tmux/screen

#### macOS Service (LaunchAgent)
```bash
cp launchagent/com.claude-telegram-ts.plist.template \
   ~/Library/LaunchAgents/com.claude-telegram-ts.plist
# Edit plist with paths and env vars
launchctl load ~/Library/LaunchAgents/com.claude-telegram-ts.plist
```
**Features**:
- Auto-start on login
- Auto-restart on crash
- Logs to `/tmp/claude-telegram-bot-ts.{log,err}`

**Useful Aliases**:
```bash
alias cbot='launchctl list | grep com.claude-telegram-ts'
alias cbot-stop='launchctl bootout gui/$(id -u)/com.claude-telegram-ts'
alias cbot-start='launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/com.claude-telegram-ts.plist'
alias cbot-restart='launchctl kickstart -k gui/$(id -u)/com.claude-telegram-ts'
alias cbot-logs='tail -f /tmp/claude-telegram-bot-ts.log'
```

### 8.2 Standalone Binary

**Compilation**:
```bash
bun build --compile src/index.ts --outfile claude-telegram-bot
```

**Requirements**:
- `pdftotext` in PATH (install via `brew install poppler`)
- PATH must include Homebrew bins

**Use Case**: Packaged macOS app wrapper

### 8.3 System Requirements

- **OS**: macOS, Linux (Bun supported platforms)
- **Runtime**: Bun 1.0+
- **Dependencies**:
  - `pdftotext` (for PDF extraction)
  - `claude` CLI (for CLI auth mode)
- **Network**: Internet access for Telegram API, Claude API, OpenAI API

---

## 9. Performance Characteristics

### 9.1 Response Times

| Operation | Typical Time | Notes |
|-----------|--------------|-------|
| Text message → First response | 2-5s | Claude processing time |
| Voice transcription | 1-3s | OpenAI Whisper |
| Photo download | 0.5-2s | Depends on size |
| PDF extraction | 0.5-5s | Depends on page count |
| Session resume | <0.1s | Disk read |

### 9.2 Streaming Update Frequency

- **Text updates**: Max 1 per 500ms (throttled)
- **Tool status**: Immediate on tool start
- **Thinking blocks**: Immediate when complete

### 9.3 Token Usage

**Typical Query**:
- Input: 500-2000 tokens (context + message)
- Output: 100-1000 tokens (response)
- Cache read: 1000-5000 tokens (session context)

**With Thinking**:
- Normal: +10K tokens max
- Deep: +50K tokens max

**Cost Optimization**:
- Session persistence reduces input tokens (cache hit)
- Extended thinking only when keywords detected

---

## 10. Error Handling

### 10.1 Error Types and Recovery

| Error | Detection | Recovery |
|-------|-----------|----------|
| Claude Code crash | `exited with code` | Auto-retry once, kill session |
| User cancellation | `abort`, `cancel` | Silent (no error message) |
| Rate limit | Token bucket check | Show retry-after time |
| Unauthorized | User ID not in allowlist | Reject with message |
| Path access denied | Path validation | Block and log |
| Command blocked | Pattern match | Block and log |
| Voice transcription fail | OpenAI API error | Show error, skip transcription |
| PDF extraction fail | pdftotext error | Show error, treat as text |

### 10.2 Logging

**Console Output**:
```
STARTING new Claude session (thinking=off)
GOT session_id: 12abc34d...
Tool: 💻 ls -la
THINKING BLOCK: Let me analyze...
Response complete
Usage: in=523 out=147 cache_read=0 cache_create=0
```

**Audit Log Format** (plaintext):
```
[2025-01-14T19:03:42+09:00] USER=123456789 @username TYPE=message
INPUT: What is the weather today?
OUTPUT: Based on current data...

[2025-01-14T19:03:45+09:00] USER=987654321 @unknown TYPE=auth
RESULT: denied (not in allowlist)
```

**Audit Log Format** (JSON):
```json
{
  "timestamp": "2025-01-14T19:03:42+09:00",
  "user_id": 123456789,
  "username": "username",
  "type": "message",
  "input": "What is the weather today?",
  "output": "Based on current data..."
}
```

---

## 11. Testing and Debugging

### 11.1 Type Checking
```bash
bun run typecheck
# Runs: tsc --noEmit
```

### 11.2 Manual Testing

**Test Checklist**:
- [ ] Authorization (allowed vs blocked users)
- [ ] Rate limiting (send 21 messages in 60s)
- [ ] Text message processing
- [ ] Voice message transcription
- [ ] Photo upload and analysis
- [ ] PDF document extraction
- [ ] ZIP archive extraction
- [ ] Command execution and path validation
- [ ] Session persistence (`/new`, `/resume`)
- [ ] Interrupt mechanism (`!` prefix, `/stop`)
- [ ] Extended thinking keywords
- [ ] ask_user inline buttons
- [ ] Bot restart (`/restart`)

### 11.3 Debugging Tips

**Check logs**:
```bash
tail -f /tmp/claude-telegram-bot-ts.log
tail -f /tmp/claude-telegram-bot-ts.err
```

**Check audit log**:
```bash
tail -f /tmp/claude-telegram-audit.log
```

**Check session file**:
```bash
cat /tmp/claude-telegram-session.json
```

**Check pending ask_user**:
```bash
ls -la /tmp/ask_user_pending_*.json
cat /tmp/ask_user_pending_123456789.json
```

**Test rate limiter**:
```bash
for i in {1..25}; do curl ...; done
```

**Test path validation**:
```typescript
isPathAllowed("/tmp/test.txt") // Should be true
isPathAllowed("/etc/passwd")   // Should be false
```

---

## 12. Known Limitations

### 12.1 Technical Limitations

1. **Telegram Message Length**: 4096 chars max (handled by splitting)
2. **Voice OGG Format**: Must convert to MP3 for OpenAI
3. **PDF Extraction**: Requires external `pdftotext` binary
4. **No Streaming Edit**: Telegram API doesn't support streaming edits, only replace
5. **Session Resume**: Only works if working directory matches
6. **MCP Timing**: ask_user buttons may take 200-300ms to appear

### 12.2 Security Limitations

1. **Bypass Mode**: No per-action permission prompts
2. **Prompt Injection**: Malicious content in documents could manipulate Claude
3. **API Key Exposure**: Claude might read files containing keys (mitigated by system prompt)
4. **Rate Limiting**: Can be exhausted by authorized users
5. **Audit Log**: Can be deleted/modified (file permissions)

### 12.3 UX Limitations

1. **No Message Editing**: Can't edit existing messages (Telegram limitation)
2. **Long Responses**: Multiple messages for long content
3. **Thinking Display**: Separate messages (not inline)
4. **No Syntax Highlighting**: Telegram HTML doesn't support custom CSS

---

## 13. Future Enhancements (Potential)

### 13.1 High Priority

- [ ] Message editing support (if Telegram API allows)
- [ ] User-specific rate limits (per-user config)
- [ ] Better prompt injection detection (content filtering)
- [ ] Conversation history export
- [ ] Multi-language UI (i18n)

### 13.2 Medium Priority

- [ ] Voice output (TTS for responses)
- [ ] Image generation (DALL-E integration)
- [ ] Code execution sandboxing
- [ ] PostgreSQL audit logging (instead of file)
- [ ] Web dashboard (analytics)

### 13.3 Low Priority

- [ ] Multi-bot support (one process, multiple bots)
- [ ] Plugin system (custom handlers)
- [ ] Metrics endpoint (Prometheus)
- [ ] Docker deployment
- [ ] Kubernetes support

---

## 14. Code Quality Metrics

### 14.1 Complexity Analysis

| Module | Lines | Complexity | Maintainability |
|--------|-------|------------|-----------------|
| `index.ts` | 140 | Low | High |
| `config.ts` | 239 | Medium | Medium |
| `session.ts` | 534 | High | Medium |
| `security.ts` | 168 | Medium | High |
| `handlers/` | 1,842 | Medium | High |
| **Total** | **~3,300** | **Medium** | **High** |

### 14.2 Design Patterns

- **Factory Pattern**: `createStatusCallback()` in streaming.ts
- **Singleton Pattern**: Global `session` instance
- **Strategy Pattern**: Handler selection based on message type
- **Observer Pattern**: Streaming events from SDK
- **Command Pattern**: Bot command handlers
- **State Machine**: Session lifecycle management

### 14.3 TypeScript Usage

- **Strict Mode**: `tsconfig.json` with strict checks
- **Type Coverage**: ~95% (most code fully typed)
- **Any Usage**: Minimal (mostly in SDK integration)
- **External Types**: Uses `@types/bun`, `@types/node`

---

## 15. References

### 15.1 Documentation

- [Claude Agent SDK](https://github.com/anthropic-ai/claude-agent-sdk)
- [grammY Bot Framework](https://grammy.dev/)
- [Telegram Bot API](https://core.telegram.org/bots/api)
- [Model Context Protocol](https://modelcontextprotocol.io/)
- [OpenAI Whisper API](https://platform.openai.com/docs/guides/speech-to-text)

### 15.2 Project Files

- `README.md`: User documentation
- `SECURITY.md`: Security model details
- `CLAUDE.md`: Developer guide
- `.env.example`: Configuration template
- `docs/personal-assistant-guide.md`: Personal assistant setup

### 15.3 Related Projects

- [Claude Code](https://claude.com/product/claude-code)
- [Claude Desktop App](https://claude.ai/download)
- [MCP Servers](https://github.com/modelcontextprotocol/servers)

---

## 16. Changelog

### Version 1.0 (Initial Release)
- Core bot functionality
- Multi-modal inputs (text, voice, photo, document)
- Session persistence
- Streaming responses
- Defense-in-depth security
- MCP integration
- ask_user inline buttons
- Extended thinking support
- macOS LaunchAgent support
- Audit logging

---

## 17. Glossary

| Term | Definition |
|------|------------|
| **MCP** | Model Context Protocol - standard for connecting AI tools |
| **grammY** | Modern Telegram bot framework for Node.js/Bun |
| **Session** | Persistent conversation context with Claude |
| **Streaming** | Real-time response updates as Claude generates |
| **Thinking** | Extended reasoning mode with token budget |
| **ask_user** | MCP tool for presenting options as buttons |
| **Sequentialize** | Process messages one at a time per chat |
| **Token Bucket** | Rate limiting algorithm |
| **Bypass Mode** | No permission prompts for tool execution |
| **Audit Log** | Security log of all interactions |

---

**End of Technical Specification**

*This document describes claude-telegram-bot as of commit hash from 2026-01-14.*
*For updates, see the GitHub repository README and commit history.*
