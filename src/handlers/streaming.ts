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
}

/**
 * Spinner frames for progress indicator.
 */
const SPINNER_FRAMES = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/**
 * Format elapsed time as MM:SS.
 */
function formatElapsed(startTime: Date): string {
  const elapsed = Math.floor((Date.now() - startTime.getTime()) / 1000);
  const minutes = Math.floor(elapsed / 60);
  const seconds = elapsed % 60;
  return `${minutes}:${seconds.toString().padStart(2, '0')}`;
}

/**
 * Create a status callback for streaming updates.
 */
export function createStatusCallback(
  ctx: Context,
  state: StreamingState
): StatusCallback {
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
        console.debug('Failed to delete old progress message:', error);
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
        console.debug('Failed to create progress message:', error);
      }
    }
  };

  // Initialize progress tracking
  if (!state.startTime) {
    state.startTime = new Date();

    // Create initial progress message
    recreateProgressMessage();

    // Start update timer (1 second interval)
    state.progressTimer = setInterval(async () => {
      if (!state.startTime || !state.progressMessage) return;

      frameIndex++;

      // Update existing message (don't recreate on timer)
      const spinner = SPINNER_FRAMES[frameIndex % SPINNER_FRAMES.length];
      const elapsed = formatElapsed(state.startTime);
      const text = `${spinner} Working... (${elapsed})`;

      try {
        await ctx.api.editMessageText(
          state.progressMessage.chat.id,
          state.progressMessage.message_id,
          text
        );
      } catch (error) {
        console.debug('Failed to update progress message:', error);
      }
    }, 1000);
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

        // Recreate progress at bottom after new message
        await recreateProgressMessage();
      } else if (statusType === "tool") {
        const toolMsg = await ctx.reply(content, { parse_mode: "HTML" });
        state.toolMessages.push(toolMsg);

        // Recreate progress at bottom after new message
        await recreateProgressMessage();
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
            // HTML parse failed, fall back to plain text
            console.debug("HTML reply failed, using plain text:", htmlError);
            const msg = await ctx.reply(formatted);
            state.textMessages.set(segmentId, msg);
            state.lastContent.set(segmentId, formatted);
          }
          state.lastEditTimes.set(segmentId, now);

          // Recreate progress at bottom after new segment
          await recreateProgressMessage();
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
            console.debug("HTML edit failed, trying plain text:", htmlError);
            try {
              await ctx.api.editMessageText(msg.chat.id, msg.message_id, formatted);
              state.lastContent.set(segmentId, formatted);
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
          } catch {
            await ctx.reply(content);
          }

          // Recreate progress at bottom after new message
          await recreateProgressMessage();
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
          } catch (error) {
            console.debug("Failed to edit final message:", error);
          }
        } else {
          // Too long - delete and split
          try {
            await ctx.api.deleteMessage(msg.chat.id, msg.message_id);
          } catch (error) {
            console.debug("Failed to delete message for splitting:", error);
          }
          for (let i = 0; i < formatted.length; i += TELEGRAM_SAFE_LIMIT) {
            const chunk = formatted.slice(i, i + TELEGRAM_SAFE_LIMIT);
            try {
              await ctx.reply(chunk, { parse_mode: "HTML" });
            } catch (htmlError) {
              console.debug("HTML chunk failed, using plain text:", htmlError);
              await ctx.reply(chunk);
            }
          }

          // Recreate progress at bottom after split messages
          await recreateProgressMessage();
        }
      } else if (statusType === "done") {
        // Stop progress timer
        if (state.progressTimer) {
          clearInterval(state.progressTimer);
          state.progressTimer = null;
        }

        // Update progress message with completion info
        if (state.progressMessage && state.startTime) {
          const endTime = new Date();
          const duration = formatElapsed(state.startTime);
          const startStr = state.startTime.toLocaleTimeString('ko-KR', {
            hour: '2-digit',
            minute: '2-digit',
            second: '2-digit'
          });
          const endStr = endTime.toLocaleTimeString('ko-KR', {
            hour: '2-digit',
            minute: '2-digit',
            second: '2-digit'
          });

          const completionText = `✅ Completed\n⏰ ${startStr} → ${endStr} (${duration})`;

          try {
            await ctx.api.editMessageText(
              state.progressMessage.chat.id,
              state.progressMessage.message_id,
              completionText
            );
          } catch (error) {
            console.debug("Failed to update completion message:", error);
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
      console.error("Status callback error:", error);
    }
  };
}
