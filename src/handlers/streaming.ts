/**
 * Shared streaming callback for Claude Telegram Bot handlers.
 *
 * Provides a reusable status callback for streaming Claude responses.
 */

import type { Context } from "grammy";
import type { Message } from "grammy/types";
import { InlineKeyboard } from "grammy";
import type { StatusCallback } from "../types";
import { convertMarkdownToHtml, escapeHtml } from "../formatting";
import {
  TELEGRAM_MESSAGE_LIMIT,
  TELEGRAM_SAFE_LIMIT,
  STREAMING_THROTTLE_MS,
  BUTTON_LABEL_MAX_LENGTH,
  DELETE_THINKING_MESSAGES,
  DELETE_TOOL_MESSAGES,
  PROGRESS_SPINNER_ENABLED,
  SHOW_ELAPSED_TIME,
  PROGRESS_REACTION_ENABLED,
} from "../config";

/**
 * Create inline keyboard for ask_user options.
 */
export function createAskUserKeyboard(
  requestId: string,
  options: string[]
): InlineKeyboard {
  const keyboard = new InlineKeyboard();
  for (let idx = 0; idx < options.length; idx++) {
    const option = options[idx]!;
    // Truncate long options for button display
    const display =
      option.length > BUTTON_LABEL_MAX_LENGTH
        ? option.slice(0, BUTTON_LABEL_MAX_LENGTH) + "..."
        : option;
    const callbackData = `askuser:${requestId}:${idx}`;
    keyboard.text(display, callbackData).row();
  }
  return keyboard;
}

/**
 * Check for pending ask-user requests and send inline keyboards.
 */
export async function checkPendingAskUserRequests(
  ctx: Context,
  chatId: number
): Promise<boolean> {
  const glob = new Bun.Glob("ask-user-*.json");
  let buttonsSent = false;

  for await (const filename of glob.scan({ cwd: "/tmp", absolute: false })) {
    const filepath = `/tmp/${filename}`;
    try {
      const file = Bun.file(filepath);
      const text = await file.text();
      const data = JSON.parse(text);

      // Only process pending requests for this chat
      if (data.status !== "pending") continue;
      if (String(data.chat_id) !== String(chatId)) continue;

      const question = data.question || "Please choose:";
      const options = data.options || [];
      const requestId = data.request_id || "";

      if (options.length > 0 && requestId) {
        const keyboard = createAskUserKeyboard(requestId, options);
        await ctx.reply(`❓ ${question}`, { reply_markup: keyboard });
        buttonsSent = true;

        // Mark as sent
        data.status = "sent";
        await Bun.write(filepath, JSON.stringify(data));
      }
    } catch (error) {
      console.warn(`Failed to process ask-user file ${filepath}:`, error);
    }
  }

  return buttonsSent;
}

/**
 * Tracks state for streaming message updates.
 */
export class StreamingState {
  textMessages = new Map<number, Message>(); // segment_id -> telegram message
  thinkingMessages: Message[] = []; // thinking status messages
  toolMessages: Message[] = []; // tool status messages
  lastEditTimes = new Map<number, number>(); // segment_id -> last edit time
  lastContent = new Map<number, string>(); // segment_id -> last sent content
  progressMessage: Message | null = null; // progress spinner message
  progressTimer: Timer | null = null; // timer for updating progress
  startTime: Date | null = null; // work start time
  rateLimitNotified = false; // whether we've already notified about rate limit

  cleanup() {
    if (this.progressTimer) {
      clearInterval(this.progressTimer);
      this.progressTimer = null;
    }
  }
}

/**
 * Check if error is a Telegram rate limit (429) and notify via reaction.
 * Returns true if rate limited.
 */
export async function handleRateLimitError(
  ctx: Context,
  error: unknown,
  state: StreamingState
): Promise<boolean> {
  const errorStr = String(error);
  if (!errorStr.includes("429") && !errorStr.includes("Too Many Requests")) {
    return false;
  }

  // Only notify once per request
  if (state.rateLimitNotified) {
    return true;
  }

  state.rateLimitNotified = true;

  // Extract retry_after if available
  let retryAfter = 60;
  const match = errorStr.match(/retry after (\d+)/i);
  if (match?.[1]) {
    retryAfter = parseInt(match[1], 10);
  }

  // React to original message with yawn emoji (waiting/rate limited)
  // Note: Telegram only allows specific emojis for reactions
  const msgId = ctx.message?.message_id;
  const chatId = ctx.chat?.id;
  if (msgId !== undefined && chatId !== undefined) {
    try {
      await ctx.api.setMessageReaction(chatId, msgId, [{ type: "emoji", emoji: "🥱" }]);
    } catch {
      // Reaction also rate limited, ignore
    }
  }

  console.warn(`[RATE LIMIT] Telegram 429 - retry after ${retryAfter}s`);
  return true;
}

/**
 * Spinner frames for progress indicator.
 */
const SPINNER_FRAMES = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/**
 * Format elapsed time as MM:SS.
 */
function formatElapsed(startTime: Date): string {
  const elapsed = Math.floor((Date.now() - startTime.getTime()) / 1000);
  const minutes = Math.floor(elapsed / 60);
  const seconds = elapsed % 60;
  return `${minutes}:${seconds.toString().padStart(2, "0")}`;
}

/**
 * Create a status callback for streaming updates.
 */
export async function createStatusCallback(
  ctx: Context,
  state: StreamingState
): Promise<StatusCallback> {
  let frameIndex = 0;

  // Helper to recreate progress message at bottom
  const recreateProgressMessage = async () => {
    // Delete old progress message
    if (state.progressMessage) {
      try {
        await ctx.api.deleteMessage(
          state.progressMessage.chat.id,
          state.progressMessage.message_id
        );
      } catch (error) {
        console.debug("Failed to delete old progress message:", error);
      }
    }

    // Create new progress message at bottom
    if (state.startTime) {
      const spinner = SPINNER_FRAMES[frameIndex % SPINNER_FRAMES.length];
      const elapsed = formatElapsed(state.startTime);
      const text = `${spinner} Working... (${elapsed})`;

      try {
        state.progressMessage = await ctx.reply(text);
      } catch (error) {
        console.debug("Failed to create progress message:", error);
      }
    }
  };

  // Initialize progress tracking (always track time, but spinner is optional)
  if (!state.startTime) {
    state.startTime = new Date();

    // React with 🔥 on user message to show work started (replaces 👀)
    if (PROGRESS_REACTION_ENABLED) {
      const msgId = ctx.message?.message_id;
      const chatId = ctx.chat?.id;
      if (msgId !== undefined && chatId !== undefined) {
        try {
          await ctx.api.setMessageReaction(chatId, msgId, [
            { type: "emoji", emoji: "🔥" },
          ]);
        } catch (error) {
          console.debug("Failed to set working reaction:", error);
        }
      }
    }

    // Only show spinner if enabled (default: OFF to avoid rate limits)
    if (PROGRESS_SPINNER_ENABLED) {
      // Create initial progress message and WAIT for it to prevent race condition
      await recreateProgressMessage();

      // Start update timer AFTER message is created (1 second interval)
      state.progressTimer = setInterval(async () => {
        if (!state.startTime) return;

        // Capture current reference to avoid race condition
        const currentProgressMsg = state.progressMessage;
        if (!currentProgressMsg) return;

        frameIndex++;

        // Update existing message (don't recreate on timer)
        const spinner = SPINNER_FRAMES[frameIndex % SPINNER_FRAMES.length];
        const elapsed = formatElapsed(state.startTime);
        const text = `${spinner} Working... (${elapsed})`;

        try {
          await ctx.api.editMessageText(
            currentProgressMsg.chat.id,
            currentProgressMsg.message_id,
            text
          );
        } catch (error) {
          console.debug("Failed to update progress message:", error);
        }
      }, 1000);
    }
  }

  return async (statusType: string, content: string, segmentId?: number) => {
    try {
      if (statusType === "thinking") {
        // Show thinking inline, compact (first 500 chars)
        const preview = content.length > 500 ? content.slice(0, 500) + "..." : content;
        const escaped = escapeHtml(preview);
        const thinkingMsg = await ctx.reply(`🧠 <i>${escaped}</i>`, {
          parse_mode: "HTML",
        });
        state.thinkingMessages.push(thinkingMsg);

        // Recreate progress at bottom after new message (only if spinner enabled)
        if (PROGRESS_SPINNER_ENABLED) {
          await recreateProgressMessage();
        }
      } else if (statusType === "tool") {
        const toolMsg = await ctx.reply(content, { parse_mode: "HTML" });
        state.toolMessages.push(toolMsg);

        // Recreate progress at bottom after new message (only if spinner enabled)
        if (PROGRESS_SPINNER_ENABLED) {
          await recreateProgressMessage();
        }
      } else if (statusType === "text" && segmentId !== undefined) {
        const now = Date.now();
        const lastEdit = state.lastEditTimes.get(segmentId) || 0;

        if (!state.textMessages.has(segmentId)) {
          // New segment - create message
          const display =
            content.length > TELEGRAM_SAFE_LIMIT
              ? content.slice(0, TELEGRAM_SAFE_LIMIT) + "..."
              : content;
          const formatted = convertMarkdownToHtml(display);
          try {
            const msg = await ctx.reply(formatted, { parse_mode: "HTML" });
            state.textMessages.set(segmentId, msg);
            state.lastContent.set(segmentId, formatted);
          } catch (htmlError) {
            // HTML parse failed, fall back to plain text (use display, not formatted)
            console.debug("HTML reply failed, using plain text:", htmlError);
            const msg = await ctx.reply(display);
            state.textMessages.set(segmentId, msg);
            state.lastContent.set(segmentId, display);
          }
          state.lastEditTimes.set(segmentId, now);

          // Recreate progress at bottom after new segment (only if spinner enabled)
          if (PROGRESS_SPINNER_ENABLED) {
            await recreateProgressMessage();
          }
        } else if (now - lastEdit > STREAMING_THROTTLE_MS) {
          // Update existing segment message (throttled)
          const msg = state.textMessages.get(segmentId)!;
          const display =
            content.length > TELEGRAM_SAFE_LIMIT
              ? content.slice(0, TELEGRAM_SAFE_LIMIT) + "..."
              : content;
          const formatted = convertMarkdownToHtml(display);

          // Skip if content unchanged
          if (formatted === state.lastContent.get(segmentId)) {
            return;
          }

          try {
            await ctx.api.editMessageText(msg.chat.id, msg.message_id, formatted, {
              parse_mode: "HTML",
            });
            state.lastContent.set(segmentId, formatted);
          } catch (htmlError) {
            // HTML edit failed, try plain text (use display, not formatted)
            console.debug("HTML edit failed, trying plain text:", htmlError);
            try {
              await ctx.api.editMessageText(msg.chat.id, msg.message_id, display);
              state.lastContent.set(segmentId, display);
            } catch (editError) {
              console.debug("Edit message failed:", editError);
            }
          }
          state.lastEditTimes.set(segmentId, now);
        }
      } else if (statusType === "segment_end" && segmentId !== undefined) {
        if (!content) return;

        // If no message exists yet (short response), create one
        if (!state.textMessages.has(segmentId)) {
          const formatted = convertMarkdownToHtml(content);
          try {
            const msg = await ctx.reply(formatted, { parse_mode: "HTML" });
            state.textMessages.set(segmentId, msg);
            state.lastContent.set(segmentId, formatted);
          } catch {
            const msg = await ctx.reply(content);
            state.textMessages.set(segmentId, msg);
            state.lastContent.set(segmentId, content);
          }

          // Recreate progress at bottom after new message (only if spinner enabled)
          if (PROGRESS_SPINNER_ENABLED) {
            await recreateProgressMessage();
          }
          return;
        }

        const msg = state.textMessages.get(segmentId)!;
        const formatted = convertMarkdownToHtml(content);

        // Skip if content unchanged
        if (formatted === state.lastContent.get(segmentId)) {
          return;
        }

        if (formatted.length <= TELEGRAM_MESSAGE_LIMIT) {
          try {
            await ctx.api.editMessageText(msg.chat.id, msg.message_id, formatted, {
              parse_mode: "HTML",
            });
            state.lastContent.set(segmentId, formatted);
          } catch (error) {
            console.debug("Failed to edit final message:", error);
            try {
              await ctx.api.editMessageText(msg.chat.id, msg.message_id, content);
              state.lastContent.set(segmentId, content);
            } catch (editError) {
              console.debug(
                "Failed to edit final message (plain text fallback):",
                editError
              );
            }
          }
        } else {
          // Too long - delete and split
          try {
            await ctx.api.deleteMessage(msg.chat.id, msg.message_id);
          } catch (error) {
            console.debug("Failed to delete message for splitting:", error);
          }

          // Replace the tracked message with the last chunk message so `done` can safely append
          // the elapsed-time footer without targeting a deleted message.
          state.textMessages.delete(segmentId);
          state.lastContent.delete(segmentId);

          let lastChunkMsg: Message | null = null;
          let lastChunkContent: string | null = null;
          for (let i = 0; i < formatted.length; i += TELEGRAM_SAFE_LIMIT) {
            const chunk = formatted.slice(i, i + TELEGRAM_SAFE_LIMIT);
            try {
              lastChunkMsg = await ctx.reply(chunk, { parse_mode: "HTML" });
              lastChunkContent = chunk;
            } catch (htmlError) {
              console.debug("HTML chunk failed, using plain text:", htmlError);
              lastChunkMsg = await ctx.reply(chunk);
              lastChunkContent = chunk;
            }
          }
          if (lastChunkMsg && lastChunkContent !== null) {
            state.textMessages.set(segmentId, lastChunkMsg);
            state.lastContent.set(segmentId, lastChunkContent);
          }

          // Recreate progress at bottom after split messages (only if spinner enabled)
          if (PROGRESS_SPINNER_ENABLED) {
            await recreateProgressMessage();
          }
        }
      } else if (statusType === "done") {
        // Stop progress timer
        if (state.progressTimer) {
          clearInterval(state.progressTimer);
          state.progressTimer = null;
        }

        // Delete progress message if exists
        if (state.progressMessage) {
          try {
            await ctx.api.deleteMessage(
              state.progressMessage.chat.id,
              state.progressMessage.message_id
            );
          } catch (error) {
            console.debug("Failed to delete progress message:", error);
          }
        }

        // Append elapsed time to the last bot message
        if (SHOW_ELAPSED_TIME && state.startTime && state.textMessages.size > 0) {
          const endTime = new Date();
          const duration = formatElapsed(state.startTime);
          const startStr = state.startTime.toLocaleTimeString("ko-KR", {
            hour: "2-digit",
            minute: "2-digit",
            second: "2-digit",
          });
          const endStr = endTime.toLocaleTimeString("ko-KR", {
            hour: "2-digit",
            minute: "2-digit",
            second: "2-digit",
          });

          const timeFooter = `\n\n<i>⏰ ${startStr} → ${endStr} (${duration})</i>`;

          // Find the last segment message
          const lastSegmentId = Math.max(...state.textMessages.keys());
          const lastMsg = state.textMessages.get(lastSegmentId);
          const lastContent = state.lastContent.get(lastSegmentId);

          if (lastMsg && lastContent) {
            const updatedContent = lastContent + timeFooter;
            try {
              await ctx.api.editMessageText(
                lastMsg.chat.id,
                lastMsg.message_id,
                updatedContent,
                { parse_mode: "HTML" }
              );
            } catch (error) {
              console.debug("Failed to append time footer to last message:", error);
            }
          }
        }

        // Delete thinking messages if configured
        if (DELETE_THINKING_MESSAGES) {
          for (const thinkingMsg of state.thinkingMessages) {
            try {
              await ctx.api.deleteMessage(thinkingMsg.chat.id, thinkingMsg.message_id);
            } catch (error) {
              console.debug("Failed to delete thinking message:", error);
            }
          }
        }

        // Delete tool messages if configured
        if (DELETE_TOOL_MESSAGES) {
          for (const toolMsg of state.toolMessages) {
            try {
              await ctx.api.deleteMessage(toolMsg.chat.id, toolMsg.message_id);
            } catch (error) {
              console.debug("Failed to delete tool message:", error);
            }
          }
        }

        // React with 🎉 on user message to show work completed (replaces 🔥)
        if (PROGRESS_REACTION_ENABLED) {
          const msgId = ctx.message?.message_id;
          const chatId = ctx.chat?.id;
          if (msgId !== undefined && chatId !== undefined) {
            try {
              await ctx.api.setMessageReaction(chatId, msgId, [
                { type: "emoji", emoji: "🎉" },
              ]);
            } catch (error) {
              console.debug("Failed to set completion reaction:", error);
            }
          }
        }

        // Add reaction to last message to indicate turn complete
        if (state.textMessages.size > 0) {
          // Find the last segment (highest segment ID)
          const lastSegmentId = Math.max(...state.textMessages.keys());
          const lastMsg = state.textMessages.get(lastSegmentId);

          if (lastMsg) {
            try {
              await ctx.api.setMessageReaction(lastMsg.chat.id, lastMsg.message_id, [
                { type: "emoji", emoji: "👍" },
              ]);
            } catch (error) {
              console.debug("Failed to add completion reaction to bot message:", error);
            }
          }
        }
      }
    } catch (error) {
      // Check if rate limited and notify via reaction
      const isRateLimited = await handleRateLimitError(ctx, error, state);
      if (!isRateLimited) {
        console.error("Status callback error:", error);
      }
    }
  };
}
