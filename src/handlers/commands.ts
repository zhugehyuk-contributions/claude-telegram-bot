/**
 * Command handlers for Claude Telegram Bot.
 *
 * /start, /new, /stop, /status, /resume, /restart
 */

import type { Context } from "grammy";
import { session } from "../session";
import { WORKING_DIR, ALLOWED_USERS, RESTART_FILE } from "../config";
import { isAuthorized } from "../security";
import { getSchedulerStatus, reloadScheduler } from "../scheduler";
import { fetchAllUsage } from "../usage";
import type { ClaudeUsage, CodexUsage, GeminiUsage } from "../types";

function formatDuration(seconds: number): string {
  const hours = Math.floor(seconds / 3600);
  const mins = Math.floor((seconds % 3600) / 60);
  const secs = seconds % 60;

  if (hours > 0) return `${hours}h ${mins}m ${secs}s`;
  if (mins > 0) return `${mins}m ${secs}s`;
  return `${secs}s`;
}

function formatTimeRemaining(resetTime: string | number | null): string {
  if (!resetTime) return "";

  const resetMs =
    typeof resetTime === "number" ? resetTime * 1000 : new Date(resetTime).getTime();
  const diffMs = resetMs - Date.now();

  if (diffMs <= 0) return "now";

  const diffSec = Math.floor(diffMs / 1000);
  const days = Math.floor(diffSec / 86400);
  const hours = Math.floor((diffSec % 86400) / 3600);
  const mins = Math.floor((diffSec % 3600) / 60);

  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${mins}m`;
  return `${mins}m`;
}

function formatClaudeUsage(usage: ClaudeUsage): string[] {
  const lines: string[] = ["<b>Claude Code:</b>"];

  if (usage.five_hour) {
    const reset = formatTimeRemaining(usage.five_hour.resets_at);
    lines.push(
      `   5h: ${Math.round(usage.five_hour.utilization)}%${reset ? ` (resets in ${reset})` : ""}`
    );
  }
  if (usage.seven_day) {
    const reset = formatTimeRemaining(usage.seven_day.resets_at);
    lines.push(
      `   7d: ${Math.round(usage.seven_day.utilization)}%${reset ? ` (resets in ${reset})` : ""}`
    );
  }
  if (usage.seven_day_sonnet) {
    const reset = formatTimeRemaining(usage.seven_day_sonnet.resets_at);
    lines.push(
      `   7d Sonnet: ${Math.round(usage.seven_day_sonnet.utilization)}%${reset ? ` (resets in ${reset})` : ""}`
    );
  }

  return lines;
}

function formatCodexUsage(usage: CodexUsage): string[] {
  const lines: string[] = [`<b>OpenAI Codex</b> (${usage.planType}):`];

  if (usage.primary) {
    const reset = formatTimeRemaining(usage.primary.resetAt);
    lines.push(
      `   5h: ${Math.round(usage.primary.usedPercent)}%${reset ? ` (resets in ${reset})` : ""}`
    );
  }
  if (usage.secondary) {
    const reset = formatTimeRemaining(usage.secondary.resetAt);
    lines.push(
      `   7d: ${Math.round(usage.secondary.usedPercent)}%${reset ? ` (resets in ${reset})` : ""}`
    );
  }

  return lines;
}

function formatGeminiUsage(usage: GeminiUsage): string[] {
  const lines: string[] = [`<b>Gemini</b> (${usage.model}):`];

  if (usage.usedPercent !== null) {
    const reset = formatTimeRemaining(usage.resetAt);
    lines.push(
      `   Usage: ${usage.usedPercent}%${reset ? ` (resets in ${reset})` : ""}`
    );
  }

  return lines;
}

/**
 * /start - Show welcome message and status.
 */
export async function handleStart(ctx: Context): Promise<void> {
  const userId = ctx.from?.id;

  if (!isAuthorized(userId, ALLOWED_USERS)) {
    await ctx.reply("Unauthorized. Contact the bot owner for access.");
    return;
  }

  const status = session.isActive ? "Active session" : "No active session";
  const workDir = WORKING_DIR;

  await ctx.reply(
    `🤖 <b>Claude Telegram Bot</b>\n\n` +
      `Status: ${status}\n` +
      `Working directory: <code>${workDir}</code>\n\n` +
      `Type /help to see all available commands.`,
    { parse_mode: "HTML" }
  );
}

/**
 * /help - Show complete command list with descriptions, usage tips, and examples.
 */
export async function handleHelp(ctx: Context): Promise<void> {
  const userId = ctx.from?.id;

  if (!isAuthorized(userId, ALLOWED_USERS)) {
    await ctx.reply("Unauthorized.");
    return;
  }

  try {
    await ctx.reply(
      `⚙️ <b>Available Commands</b>\n\n` +
        `<b>Session Management:</b>\n` +
        `/start - Welcome message and status\n` +
        `/new - Start fresh Claude session\n` +
        `/resume - Resume last saved session\n` +
        `/stop - Stop current query (silent)\n` +
        `/restart - Restart the bot process\n\n` +
        `<b>Information:</b>\n` +
        `/status - Show current session details\n` +
        `/stats - Token usage & cost statistics\n` +
        `/context - Context window usage (200K limit)\n` +
        `/help - Show this command list\n\n` +
        `<b>Utilities:</b>\n` +
        `/retry - Retry last message\n` +
        `/cron [reload] - Scheduled jobs status/reload\n\n` +
        `<b>💡 Tips:</b>\n` +
        `• Prefix with <code>!</code> to interrupt current query\n` +
        `• Use "think" keyword for extended reasoning (10K tokens)\n` +
        `• Use "ultrathink" for deep analysis (50K tokens)\n` +
        `• Send photos, voice messages, or documents\n` +
        `• Multiple photos = album (auto-grouped)`,
      { parse_mode: "HTML" }
    );
  } catch (error) {
    console.error(
      "[ERROR:HELP_COMMAND_FAILED] Failed to send help message:",
      error instanceof Error ? error.message : String(error)
    );

    // Fallback: Try plain text version
    try {
      await ctx.reply(
        "Available commands:\n" +
          "/start, /new, /resume, /stop, /restart, /status, /stats, /context, /help, /retry, /cron\n\n" +
          "For details, contact the administrator."
      );
    } catch (fallbackError) {
      console.error(
        "[ERROR:HELP_FALLBACK_FAILED] Even plain text help failed:",
        fallbackError instanceof Error ? fallbackError.message : String(fallbackError)
      );
    }
  }
}

/**
 * /new - Start a fresh session.
 */
export async function handleNew(ctx: Context): Promise<void> {
  const userId = ctx.from?.id;

  if (!isAuthorized(userId, ALLOWED_USERS)) {
    await ctx.reply("Unauthorized.");
    return;
  }

  // Stop any running query
  if (session.isRunning) {
    const result = await session.stop();
    if (result) {
      await Bun.sleep(100);
      session.clearStopRequested();
    }
  }

  // Clear session
  await session.kill();

  await ctx.reply("🆕 Session cleared. Next message starts fresh.");
}

/**
 * /stop - Stop the current query (silently).
 */
export async function handleStop(ctx: Context): Promise<void> {
  const userId = ctx.from?.id;

  if (!isAuthorized(userId, ALLOWED_USERS)) {
    await ctx.reply("Unauthorized.");
    return;
  }

  if (session.isRunning) {
    const result = await session.stop();
    if (result) {
      // Wait for the abort to be processed, then clear stopRequested so next message can proceed
      await Bun.sleep(100);
      session.clearStopRequested();
    }
    // Silent stop - no message shown
  }
  // If nothing running, also stay silent
}

/**
 * /status - Show detailed status.
 */
export async function handleStatus(ctx: Context): Promise<void> {
  const userId = ctx.from?.id;

  if (!isAuthorized(userId, ALLOWED_USERS)) {
    await ctx.reply("Unauthorized.");
    return;
  }

  const lines: string[] = ["📊 <b>Bot Status</b>\n"];

  // Session status
  if (session.isActive) {
    lines.push(`✅ Session: Active (${session.sessionId?.slice(0, 8)}...)`);
    if (session.sessionStartTime) {
      const duration = Math.floor(
        (Date.now() - session.sessionStartTime.getTime()) / 1000
      );
      lines.push(
        `   └─ Duration: ${formatDuration(duration)} | ${session.totalQueries} queries`
      );
    }
  } else {
    lines.push("⚪ Session: None");
  }

  // Query status
  if (session.isRunning) {
    const elapsed = session.queryStarted
      ? Math.floor((Date.now() - session.queryStarted.getTime()) / 1000)
      : 0;
    lines.push(`🔄 Query: Running (${elapsed}s)`);
    if (session.currentTool) {
      lines.push(`   └─ ${session.currentTool}`);
    }
  } else {
    lines.push("⚪ Query: Idle");
    if (session.lastTool) {
      lines.push(`   └─ Last: ${session.lastTool}`);
    }
  }

  // Last activity
  if (session.lastActivity) {
    const ago = Math.floor((Date.now() - session.lastActivity.getTime()) / 1000);
    lines.push(`\n⏱️ Last activity: ${ago}s ago`);
  }

  // Usage stats
  if (session.lastUsage) {
    const usage = session.lastUsage;
    lines.push(
      `\n📈 Last query usage:`,
      `   Input: ${usage.input_tokens?.toLocaleString() || "?"} tokens`,
      `   Output: ${usage.output_tokens?.toLocaleString() || "?"} tokens`
    );
    if (usage.cache_read_input_tokens) {
      lines.push(`   Cache read: ${usage.cache_read_input_tokens.toLocaleString()}`);
    }
  }

  // Error status
  if (session.lastError) {
    const ago = session.lastErrorTime
      ? Math.floor((Date.now() - session.lastErrorTime.getTime()) / 1000)
      : "?";
    lines.push(`\n⚠️ Last error (${ago}s ago):`, `   ${session.lastError}`);
  }

  // Working directory
  lines.push(`\n📁 Working dir: <code>${WORKING_DIR}</code>`);

  await ctx.reply(lines.join("\n"), { parse_mode: "HTML" });
}

/**
 * /resume - Resume the last session.
 */
export async function handleResume(ctx: Context): Promise<void> {
  const userId = ctx.from?.id;

  if (!isAuthorized(userId, ALLOWED_USERS)) {
    await ctx.reply("Unauthorized.");
    return;
  }

  if (session.isActive) {
    await ctx.reply("Session already active. Use /new to start fresh first.");
    return;
  }

  const [success, message] = session.resumeLast();
  if (success) {
    await ctx.reply(`✅ ${message}`);
  } else {
    await ctx.reply(`❌ ${message}`);
  }
}

/**
 * /restart - Restart the bot process.
 */
export async function handleRestart(ctx: Context): Promise<void> {
  const userId = ctx.from?.id;
  const chatId = ctx.chat?.id;

  if (!isAuthorized(userId, ALLOWED_USERS)) {
    await ctx.reply("Unauthorized.");
    return;
  }

  const msg = await ctx.reply("🔄 Restarting bot...");

  // Save message info so we can update it after restart
  if (chatId && msg.message_id) {
    try {
      await Bun.write(
        RESTART_FILE,
        JSON.stringify({
          chat_id: chatId,
          message_id: msg.message_id,
          timestamp: Date.now(),
        })
      );
    } catch (e) {
      console.warn("Failed to save restart info:", e);
    }
  }

  // Give time for the message to send
  await Bun.sleep(500);

  // Exit - launchd will restart us
  process.exit(0);
}

/**
 * /cron - Show cron scheduler status or reload.
 */
export async function handleCron(ctx: Context): Promise<void> {
  const userId = ctx.from?.id;

  if (!isAuthorized(userId, ALLOWED_USERS)) {
    await ctx.reply("Unauthorized.");
    return;
  }

  const text = ctx.message?.text || "";
  const arg = text.replace("/cron", "").trim().toLowerCase();

  if (arg === "reload") {
    const count = reloadScheduler();
    if (count === 0) {
      await ctx.reply("⚠️ No schedules found in cron.yaml");
    } else {
      await ctx.reply(`🔄 Reloaded ${count} scheduled job${count > 1 ? "s" : ""}`);
    }
    return;
  }

  const status = getSchedulerStatus();
  await ctx.reply(
    `${status}\n\n<i>cron.yaml is auto-monitored for changes.\nYou can also use /cron reload to force reload.</i>`,
    { parse_mode: "HTML" }
  );
}

/**
 * /stats - Show comprehensive token usage and cost statistics.
 */
export async function handleStats(ctx: Context): Promise<void> {
  const userId = ctx.from?.id;

  if (!isAuthorized(userId, ALLOWED_USERS)) {
    await ctx.reply("Unauthorized.");
    return;
  }

  const lines: string[] = ["📊 <b>Session Statistics</b>\n"];

  // Session info
  if (session.sessionStartTime) {
    const duration = Math.floor(
      (Date.now() - session.sessionStartTime.getTime()) / 1000
    );
    lines.push(`⏱️ Session duration: ${formatDuration(duration)}`);
    lines.push(`🔢 Total queries: ${session.totalQueries}`);
  } else {
    lines.push("⚪ No active session");
  }

  // Token usage
  if (session.totalQueries > 0) {
    const totalIn = session.totalInputTokens;
    const totalOut = session.totalOutputTokens;
    const totalCache = session.totalCacheReadTokens + session.totalCacheCreateTokens;
    const totalTokens = totalIn + totalOut;

    lines.push(`\n🧠 <b>Token Usage</b>`);
    lines.push(`   Input: ${totalIn.toLocaleString()} tokens`);
    lines.push(`   Output: ${totalOut.toLocaleString()} tokens`);
    if (totalCache > 0) {
      lines.push(`   Cache: ${totalCache.toLocaleString()} tokens`);
      lines.push(`     └─ Read: ${session.totalCacheReadTokens.toLocaleString()}`);
      lines.push(`     └─ Create: ${session.totalCacheCreateTokens.toLocaleString()}`);
    }
    lines.push(`   <b>Total: ${totalTokens.toLocaleString()} tokens</b>`);

    // Cost estimation (Claude Sonnet 4 pricing)
    // $3 per MTok input, $15 per MTok output
    // Cache write: $3.75/MTok, Cache read: $0.30/MTok
    const costIn = (totalIn / 1000000) * 3.0;
    const costOut = (totalOut / 1000000) * 15.0;
    const costCacheRead = (session.totalCacheReadTokens / 1000000) * 0.3;
    const costCacheWrite = (session.totalCacheCreateTokens / 1000000) * 3.75;
    const totalCost = costIn + costOut + costCacheRead + costCacheWrite;

    lines.push(`\n💰 <b>Estimated Cost</b>`);
    lines.push(`   Input: $${costIn.toFixed(4)}`);
    lines.push(`   Output: $${costOut.toFixed(4)}`);
    if (totalCache > 0) {
      lines.push(`   Cache: $${(costCacheRead + costCacheWrite).toFixed(4)}`);
    }
    lines.push(`   <b>Total: $${totalCost.toFixed(4)}</b>`);

    // Efficiency metrics
    if (session.totalQueries > 1) {
      const avgIn = Math.floor(totalIn / session.totalQueries);
      const avgOut = Math.floor(totalOut / session.totalQueries);
      const avgCost = totalCost / session.totalQueries;

      lines.push(`\n📈 <b>Per Query Average</b>`);
      lines.push(`   Input: ${avgIn.toLocaleString()} tokens`);
      lines.push(`   Output: ${avgOut.toLocaleString()} tokens`);
      lines.push(`   Cost: $${avgCost.toFixed(4)}`);
    }
  } else {
    lines.push(`\n📭 No queries in this session yet`);
  }

  // Last query
  if (session.lastUsage) {
    const u = session.lastUsage;
    lines.push(`\n🔍 <b>Last Query</b>`);
    lines.push(`   Input: ${u.input_tokens.toLocaleString()} tokens`);
    lines.push(`   Output: ${u.output_tokens.toLocaleString()} tokens`);
    if (u.cache_read_input_tokens) {
      lines.push(`   Cache read: ${u.cache_read_input_tokens.toLocaleString()}`);
    }
  }

  // Fetch provider usage in parallel
  lines.push(`\n🌐 <b>Provider Usage</b>`);
  const allUsage = await fetchAllUsage();

  if (allUsage.claude) {
    lines.push(...formatClaudeUsage(allUsage.claude));
  }
  if (allUsage.codex) {
    lines.push(...formatCodexUsage(allUsage.codex));
  }
  if (allUsage.gemini) {
    lines.push(...formatGeminiUsage(allUsage.gemini));
  }

  if (!allUsage.claude && !allUsage.codex && !allUsage.gemini) {
    lines.push("   <i>No providers authenticated</i>");
  }

  lines.push(`\n<i>Pricing: Claude Sonnet 4 rates</i>`);

  await ctx.reply(lines.join("\n"), { parse_mode: "HTML" });
}

/**
 * /retry - Retry the last message (resume session and re-send).
 */
export async function handleRetry(ctx: Context): Promise<void> {
  const userId = ctx.from?.id;

  if (!isAuthorized(userId, ALLOWED_USERS)) {
    await ctx.reply("Unauthorized.");
    return;
  }

  // Check if there's a message to retry
  if (!session.lastMessage) {
    await ctx.reply("❌ No message to retry.");
    return;
  }

  // Check if something is already running
  if (session.isRunning) {
    await ctx.reply("⏳ A query is already running. Use /stop first.");
    return;
  }

  const message = session.lastMessage;
  await ctx.reply(
    `🔄 Retrying: "${message.slice(0, 50)}${message.length > 50 ? "..." : ""}"`
  );

  // Guard: ensure ctx.message exists before spreading
  if (!ctx.message) {
    await ctx.reply("❌ Could not retry: no message context.");
    return;
  }

  // Simulate sending the message again by emitting a fake text message event
  // We do this by directly calling the text handler logic
  const { handleText } = await import("./text");

  // Create a modified context with the last message
  const fakeCtx = {
    ...ctx,
    message: {
      ...ctx.message,
      text: message,
    },
  } as Context;

  await handleText(fakeCtx);
}

/**
 * /context - Display context window utilization against 200K input token limit.
 * Shows current input tokens (which count toward context) vs output tokens (which don't).
 */
export async function handleContext(ctx: Context): Promise<void> {
  const userId = ctx.from?.id;

  if (!isAuthorized(userId, ALLOWED_USERS)) {
    await ctx.reply("Unauthorized.");
    return;
  }

  try {
    const usage = session.lastUsage;

    if (!usage) {
      await ctx.reply("⚙️ No token usage data yet. Send a message first.");
      return;
    }

    // Validate usage data structure
    if (
      typeof usage.input_tokens !== "number" ||
      typeof usage.output_tokens !== "number"
    ) {
      console.error("[ERROR:CONTEXT_INVALID_DATA] Token usage data malformed:", usage);
      await ctx.reply(
        "⚠️ Token usage data is incomplete. Try sending a new message to refresh statistics."
      );
      return;
    }

    // Calculate current context window usage
    // Note: 200K limit is INPUT context only (system + history + user input)
    // Output tokens have separate limits and don't consume input context
    const CONTEXT_LIMIT = 200_000;
    const contextUsed = usage.input_tokens; // Only input counts toward context limit
    const percentage = ((contextUsed / CONTEXT_LIMIT) * 100).toFixed(1);

    // Format numbers with commas for readability
    const formatNumber = (n: number): string => n.toLocaleString("en-US");

    await ctx.reply(
      `⚙️ <b>Context Window Usage</b>\n\n` +
        `📊 <code>${formatNumber(contextUsed)} / ${formatNumber(CONTEXT_LIMIT)}</code> tokens (<b>${percentage}%</b>)\n\n` +
        `Input: ${formatNumber(usage.input_tokens)} (context)\n` +
        `Output: ${formatNumber(usage.output_tokens)} (generated)\n` +
        (usage.cache_read_input_tokens
          ? `Cache read: ${formatNumber(usage.cache_read_input_tokens)}\n`
          : "") +
        (usage.cache_creation_input_tokens
          ? `Cache created: ${formatNumber(usage.cache_creation_input_tokens)}\n`
          : ""),
      { parse_mode: "HTML" }
    );
  } catch (error) {
    console.error(
      "[ERROR:CONTEXT_COMMAND_FAILED] Failed to retrieve context usage:",
      error instanceof Error ? error.message : String(error)
    );
    await ctx.reply(
      "❌ Failed to retrieve context usage. Please try again.\n\n" +
        "If this persists, restart the session with /new"
    );
  }
}
