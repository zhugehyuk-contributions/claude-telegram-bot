/**
 * Text message handler for Claude Telegram Bot.
 */

import type { Context } from "grammy";
import { session } from "../session";
import { ALLOWED_USERS, WORKING_DIR } from "../config";
import { isAuthorized, rateLimiter } from "../security";
import { writeFileSync, existsSync, readFileSync } from "fs";
import {
  addTimestamp,
  auditLog,
  auditLogRateLimit,
  checkInterrupt,
  startTypingIndicator,
} from "../utils";
import { StreamingState, createStatusCallback } from "./streaming";

/**
 * Handle incoming text messages.
 */
export async function handleText(ctx: Context): Promise<void> {
  const userId = ctx.from?.id;
  const username = ctx.from?.username || "unknown";
  const chatId = ctx.chat?.id;
  let message = ctx.message?.text;

  if (!userId || !message || !chatId) {
    return;
  }

  // 1. Authorization check
  if (!isAuthorized(userId, ALLOWED_USERS)) {
    await ctx.reply("Unauthorized. Contact the bot owner for access.");
    return;
  }

  // 1.5. React to user message to show it's received
  try {
    await ctx.react("👀");
  } catch (error) {
    console.debug("Failed to add reaction to user message:", error);
  }

  // 2. Check for interrupt prefix
  const wasInterrupt = message.startsWith("!");
  message = await checkInterrupt(message);
  if (!message.trim()) {
    return;
  }

  // 2.5. Real-time steering: buffer message if Claude is currently executing
  if (session.isProcessing) {
    // Interrupt messages should never be buffered as steering, otherwise they can be cleared by
    // the prior request's stopProcessing() cleanup before being consumed.
    if (wasInterrupt) {
      const start = Date.now();
      while (session.isProcessing && Date.now() - start < 2000) {
        await Bun.sleep(50);
      }
    } else {
      session.addSteering(message, ctx.message?.message_id);
      console.log(`[STEERING] Buffered user message during execution`);
      try {
        await ctx.react("👌");
      } catch (error) {
        console.debug("Failed to add steering reaction:", error);
      }
      return;
    }
  }

  // 3. Rate limit check
  const [allowed, retryAfter] = rateLimiter.check(userId);
  if (!allowed) {
    await auditLogRateLimit(userId, username, retryAfter!);
    await ctx.reply(`⏳ Rate limited. Please wait ${retryAfter!.toFixed(1)} seconds.`);
    return;
  }

  // 4. Store message for retry
  session.lastMessage = message;

  // 4.5. Add timestamp to message
  const messageWithTimestamp = addTimestamp(message);

  // 5. Mark processing started
  const stopProcessing = session.startProcessing();

  // 6. Start typing indicator
  const typing = startTypingIndicator(ctx);

  // 7. Create streaming state and callback
  let state = new StreamingState();
  let statusCallback = await createStatusCallback(ctx, state);

  // 8. Send to Claude with retry logic for crashes
  const MAX_RETRIES = 1;

  for (let attempt = 0; attempt <= MAX_RETRIES; attempt++) {
    try {
      const response = await session.sendMessageStreaming(
        messageWithTimestamp,
        username,
        userId,
        statusCallback,
        chatId,
        ctx
      );

      // 9. Audit log
      await auditLog(userId, username, "TEXT", message, response);

      // 9.5. Check context limit and trigger auto-save
      if (session.needsSave) {
        const currentTokens = session.currentContextTokens;
        const percentage = ((currentTokens / 200_000) * 100).toFixed(1);
        await ctx.reply(
          `⚠️ **Context Limit Approaching**\n\n` +
            `Current: ${currentTokens.toLocaleString()} / 200,000 tokens (${percentage}%)\n\n` +
            `Initiating automatic save...`,
          { parse_mode: "Markdown" }
        );

        // Auto-trigger /save skill
        try {
          const saveResponse = await session.sendMessageStreaming(
            "Context limit reached. Execute: Skill tool with skill='oh-my-claude:save'",
            username,
            userId,
            async () => {}, // No streaming updates for auto-save
            chatId,
            ctx
          );

          // Parse save_id from response
          const saveIdMatch = saveResponse.match(
            /Saved to:.*?\/docs\/tasks\/save\/(\d{8}_\d{6})\//
          );
          if (saveIdMatch && saveIdMatch[1]) {
            const saveId = saveIdMatch[1];

            // C1 FIX: Validate save ID format
            if (!/^\d{8}_\d{6}$/.test(saveId)) {
              console.error(`Invalid save ID format: ${saveId}`);
              console.error(`Full response: ${saveResponse}`);
              await ctx.reply(
                `❌ Save ID validation failed: ${saveId}\n\nFull response logged.`
              );
              return;
            }

            const saveIdFile = `${WORKING_DIR}/.last-save-id`;
            writeFileSync(saveIdFile, saveId, "utf-8");

            // C2 FIX: Verify write succeeded
            if (
              !existsSync(saveIdFile) ||
              readFileSync(saveIdFile, "utf-8").trim() !== saveId
            ) {
              const error = "Failed to persist save ID - file not written correctly";
              console.error(error);
              await ctx.reply(`❌ ${error}`);
              throw new Error(error);
            }

            console.log(`✅ Save ID captured & verified: ${saveId} → ${saveIdFile}`);

            // ORACLE: Add telemetry
            console.log("[TELEMETRY] auto_save_success", {
              saveId,
              contextTokens: currentTokens,
              timestamp: new Date().toISOString(),
            });

            await ctx.reply(
              `✅ **Context Saved**\n\n` +
                `Save ID: \`${saveId}\`\n\n` +
                `Please run: \`make up\` to restart with restored context.`,
              { parse_mode: "Markdown" }
            );
          } else {
            console.warn(
              "Failed to parse save_id from response:",
              saveResponse.slice(0, 200)
            );
            await ctx.reply(
              `⚠️ Save completed but couldn't parse save ID. Response: ${saveResponse.slice(0, 200)}`
            );
          }
        } catch (error) {
          // S3 FIX: Critical error handling - prevent data loss
          console.error("CRITICAL: Auto-save failed:", error);
          console.error("Stack:", error instanceof Error ? error.stack : "N/A");

          // S2 FIX: Sanitize error message
          const errorStr = String(error);
          const sanitized = errorStr.replace(
            process.env.HOME || "/home/zhugehyuk",
            "~"
          );

          await ctx.reply(
            `🚨 **CRITICAL: Auto-Save Failed**\n\n` +
              `Error: ${sanitized.slice(0, 300)}\n\n` +
              `⚠️ **YOUR WORK IS NOT SAVED**\n\n` +
              `Do NOT restart. Try manual: /oh-my-claude:save`,
            { parse_mode: "Markdown" }
          );
        }
      }

      break; // Success - exit retry loop
    } catch (error) {
      const errorStr = String(error);
      const isClaudeCodeCrash = errorStr.includes("exited with code");

      // Clean up any partial messages from this attempt
      for (const toolMsg of state.toolMessages) {
        try {
          await ctx.api.deleteMessage(toolMsg.chat.id, toolMsg.message_id);
        } catch {
          // Ignore cleanup errors
        }
      }

      // Retry on Claude Code crash (not user cancellation)
      if (isClaudeCodeCrash && attempt < MAX_RETRIES) {
        console.log(
          `Claude Code crashed, retrying (attempt ${attempt + 2}/${MAX_RETRIES + 1})...`
        );
        await session.kill(); // Clear corrupted session
        await ctx.reply(`⚠️ Claude crashed, retrying...`);
        // Clean up old state before retry
        state.cleanup();
        // Reset state for retry
        state = new StreamingState();
        statusCallback = await createStatusCallback(ctx, state);
        continue;
      }

      // Final attempt failed or non-retryable error
      console.error("Error processing message:", error);

      // Check if it was a cancellation
      if (errorStr.includes("abort") || errorStr.includes("cancel")) {
        // Only show "Query stopped" if it was an explicit stop, not an interrupt from a new message
        const wasInterrupt = session.consumeInterruptFlag();
        if (!wasInterrupt) {
          await ctx.reply("🛑 Query stopped.");
        }
      } else {
        await ctx.reply(`❌ Error: ${errorStr.slice(0, 200)}`);
      }
      break; // Exit loop after handling error
    }
  }

  // 10. Cleanup
  state.cleanup();
  stopProcessing();
  typing.stop();
}
