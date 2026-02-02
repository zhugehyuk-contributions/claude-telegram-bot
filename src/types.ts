/**
 * Shared TypeScript types for the Claude Telegram Bot.
 */

import type { Context } from "grammy";
import type { Message } from "grammy/types";

// Status callback for streaming updates
export type StatusCallback = (
  type: "thinking" | "tool" | "text" | "segment_end" | "done",
  content: string,
  segmentId?: number
) => Promise<void>;

// Rate limit bucket for token bucket algorithm
export interface RateLimitBucket {
  tokens: number;
  lastUpdate: number;
}

// Session persistence data
export interface SessionData {
  session_id: string;
  saved_at: string;
  working_dir: string;
  // Token tracking (for context window usage)
  totalInputTokens?: number;
  totalOutputTokens?: number;
  totalQueries?: number;
  sessionStartTime?: string;
}

// Token usage from Claude
export interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
  cache_read_input_tokens?: number;
  cache_creation_input_tokens?: number;
}

// MCP server configuration types
export type McpServerConfig = McpStdioConfig | McpHttpConfig;

export interface McpStdioConfig {
  command: string;
  args?: string[];
  env?: Record<string, string>;
}

export interface McpHttpConfig {
  type: "http";
  url: string;
  headers?: Record<string, string>;
}

// Audit log event types
export type AuditEventType = "message" | "auth" | "tool_use" | "error" | "rate_limit";

export interface AuditEvent {
  timestamp: string;
  event: AuditEventType;
  user_id: number;
  username?: string;
  [key: string]: unknown;
}

// Pending media group for buffering albums
export interface PendingMediaGroup {
  items: string[];
  ctx: Context;
  caption?: string;
  statusMsg?: Message;
  timeout: Timer;
}

// Bot context with optional message
export type BotContext = Context;

// Cron schedule configuration
export interface CronSchedule {
  name: string;
  cron: string;
  prompt: string;
  enabled?: boolean;
  notify?: boolean; // Send result to Telegram (default: false)
}

export interface CronConfig {
  schedules: CronSchedule[];
}

// Claude usage from oauth/usage endpoint
export interface ClaudeUsage {
  five_hour: { utilization: number; resets_at: string | null } | null;
  seven_day: { utilization: number; resets_at: string | null } | null;
  seven_day_sonnet: { utilization: number; resets_at: string | null } | null;
}

// Codex usage from ChatGPT backend
export interface CodexUsage {
  model: string;
  planType: string;
  primary: { usedPercent: number; resetAt: number } | null;
  secondary: { usedPercent: number; resetAt: number } | null;
}

// Gemini usage from Code Assist API
export interface GeminiUsage {
  model: string;
  usedPercent: number | null;
  resetAt: string | null;
}

// Combined usage result
export interface AllUsage {
  claude: ClaudeUsage | null;
  codex: CodexUsage | null;
  gemini: GeminiUsage | null;
  fetchedAt: number;
}
