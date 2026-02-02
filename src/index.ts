/**
 * Claude Telegram Bot - TypeScript/Bun Edition
 *
 * Control Claude Code from your phone via Telegram.
 */

import { Bot } from "grammy";
import { run, sequentialize } from "@grammyjs/runner";
import { TELEGRAM_TOKEN, WORKING_DIR, ALLOWED_USERS, RESTART_FILE } from "./config";
import { unlinkSync, readFileSync, existsSync, readdirSync, statSync, writeFileSync, mkdirSync } from "fs";
import {
  handleStart,
  handleNew,
  handleStop,
  handleStatus,
  handleStats,
  handleResume,
  handleRestart,
  handleRetry,
  handleCron,
  handleText,
  handleVoice,
  handlePhoto,
  handleDocument,
  handleCallback,
} from "./handlers";
import { initScheduler, startScheduler, stopScheduler } from "./scheduler";
import { session } from "./session";
import { escapeHtml } from "./formatting";

// Create bot instance
const bot = new Bot(TELEGRAM_TOKEN);

// Sequentialize non-command messages per user (prevents race conditions)
// Commands bypass sequentialization so they work immediately
bot.use(
  sequentialize((ctx) => {
    // Commands are not sequentialized - they work immediately
    if (ctx.message?.text?.startsWith("/")) {
      return undefined;
    }
    // Messages with ! prefix bypass queue (interrupt)
    if (ctx.message?.text?.startsWith("!")) {
      return undefined;
    }
    // Callback queries (button clicks) are not sequentialized
    if (ctx.callbackQuery) {
      return undefined;
    }
    // Other messages are sequentialized per chat
    return ctx.chat?.id.toString();
  })
);

// ============== Command Handlers ==============

bot.command("start", handleStart);
bot.command("new", handleNew);
bot.command("stop", handleStop);
bot.command("status", handleStatus);
bot.command("stats", handleStats);
bot.command("resume", handleResume);
bot.command("restart", handleRestart);
bot.command("retry", handleRetry);
bot.command("cron", handleCron);

// ============== Message Handlers ==============

// Text messages
bot.on("message:text", handleText);

// Voice messages
bot.on("message:voice", handleVoice);

// Photo messages
bot.on("message:photo", handlePhoto);

// Document messages
bot.on("message:document", handleDocument);

// ============== Callback Queries ==============

bot.on("callback_query:data", handleCallback);

// ============== Error Handler ==============

bot.catch((err) => {
  console.error("Bot error:", err);
});

// ============== Startup ==============

console.log("=".repeat(50));
console.log("Claude Telegram Bot - TypeScript Edition");
console.log("=".repeat(50));
console.log(`Working directory: ${WORKING_DIR}`);
console.log(`Allowed users: ${ALLOWED_USERS.length}`);
console.log("Starting bot...");

// Get bot info first
const botInfo = await bot.api.getMe();
console.log(`Bot started: @${botInfo.username}`);

// Auto-resume previous session if available
const [resumed, resumeMsg] = session.resumeLast();
if (resumed) {
  console.log(`Auto-resumed: ${resumeMsg}`);
} else {
  console.log("No previous session to resume");
}

// Initialize and start cron scheduler
initScheduler(bot.api);
startScheduler();

// Check for pending restart message to update
if (existsSync(RESTART_FILE)) {
  try {
    const data = JSON.parse(readFileSync(RESTART_FILE, "utf-8"));
    const age = Date.now() - data.timestamp;

    // Only update if restart was recent (within 30 seconds)
    if (age < 30000 && data.chat_id && data.message_id) {
      await bot.api.editMessageText(data.chat_id, data.message_id, "✅ Bot restarted");
    }
    unlinkSync(RESTART_FILE);
  } catch (e) {
    console.warn("Failed to update restart message:", e);
    try {
      unlinkSync(RESTART_FILE);
    } catch {}
  }
}

// Start with concurrent runner (commands work immediately)
const runner = run(bot);

// Send startup notification to Claude and user
if (ALLOWED_USERS.length > 0) {
  const userId = ALLOWED_USERS[0]!;
  setTimeout(async () => {
    try {
      const statusCallback = async () => {};

      // PRIORITY 1: Check for .last-save-id (auto-load mechanism)
      const saveIdFile = `${WORKING_DIR}/.last-save-id`;
      if (existsSync(saveIdFile)) {
        try {
          const saveId = readFileSync(saveIdFile, "utf-8").trim();
          console.log(`📥 Found .last-save-id: ${saveId} - Triggering auto-load`);

          // Send /load command to Claude
          await bot.api.sendMessage(
            ALLOWED_USERS[0]!,
            `🔄 **Auto-restoring context**\n\nSave ID: \`${saveId}\`\n\nExecuting /load...`,
            { parse_mode: "Markdown" }
          );

          const loadResponse = await session.sendMessageStreaming(
            `Skill tool with skill='oh-my-claude:load' and args='${saveId}'`,
            "startup",
            userId,
            statusCallback
          );

          // C4 FIX: Validate /load succeeded
          if (!loadResponse.includes("Loaded Context:")) {
            console.error(`/load failed - response doesn't contain "Loaded Context:"`);
            console.error(`Response: ${loadResponse.slice(0, 500)}`);
            throw new Error(`/load validation failed for save ID: ${saveId}`);
          }

          console.log(`✅ Context restored from ${saveId}`);
          session.markRestored(); // Activate cooldown

          // C3 FIX: Delete .last-save-id AFTER verification
          unlinkSync(saveIdFile);

          await bot.api.sendMessage(
            ALLOWED_USERS[0]!,
            `✅ **Context Restored**\n\nResumed from save: \`${saveId}\``,
            { parse_mode: "Markdown" }
          );

          return; // Skip normal startup message
        } catch (err) {
          console.error("Failed to auto-load context:", err);
          await bot.api.sendMessage(
            ALLOWED_USERS[0]!,
            `❌ Auto-load failed: ${String(err).slice(0, 200)}`
          );
          // Fall through to normal startup
        }
      }

      // PRIORITY 2: Check for saved restart context (manual save-and-restart.sh)
      let contextMessage = "";
      const saveDir = `${WORKING_DIR}/docs/tasks/save`;
      if (existsSync(saveDir)) {
        try {
          const files = readdirSync(saveDir)
            .filter((f) => f.startsWith("restart-context-") && f.endsWith(".md"))
            .map((f) => ({
              name: f,
              path: `${saveDir}/${f}`,
              mtime: statSync(`${saveDir}/${f}`).mtimeMs,
            }))
            .sort((a, b) => b.mtime - a.mtime);

          if (files.length > 0) {
            const latestFile = files[0]!;
            const content = readFileSync(latestFile.path, "utf-8");
            contextMessage = `\n\n📋 **Saved Context Found:**\n${latestFile.name}\n\n${content}`;
          }
        } catch (err) {
          console.warn("Failed to read restart context:", err);
        }
      }

      const startupPrompt = resumed
        ? `봇이 재시작되었고 이전 세션이 복원되었습니다. 현재 시간과 함께 간단히 알려주세요. (세션 ID: ${session.sessionId?.slice(0, 8)}...)${contextMessage}`
        : `봇이 방금 재시작되었습니다. 새 세션이 시작됩니다. 현재 시간과 함께 간단한 인사말을 써주세요.${contextMessage}`;

      const response = await session.sendMessageStreaming(
        startupPrompt,
        "startup",
        userId,
        statusCallback
      );

      if (response && response !== "[Waiting for user selection]") {
        await bot.api.sendMessage(userId, escapeHtml(response), { parse_mode: "HTML" });
      }
    } catch (e) {
      console.error("Startup notification failed:", e);
      await bot.api.sendMessage(userId, "✅ Bot restarted").catch(() => {});
    }
  }, 2000);
}

// Graceful shutdown
const stopRunner = () => {
  stopScheduler();
  if (runner.isRunning()) {
    console.log("Stopping bot...");
    runner.stop();
  }
};

/**
 * Save graceful shutdown context to be restored on next startup.
 */
function saveShutdownContext(): void {
  try {
    const saveDir = `${WORKING_DIR}/docs/tasks/save`;
    mkdirSync(saveDir, { recursive: true });

    const timestamp = new Date().toISOString().replace(/[:.]/g, '-').slice(0, -5);
    const saveFile = `${saveDir}/restart-context-${timestamp}.md`;

    const now = new Date().toLocaleString('ko-KR', { timeZone: 'Asia/Seoul' });
    const sessionInfo = session.isActive && session.sessionId
      ? `Session ID: ${session.sessionId.slice(0, 8)}...`
      : 'Session ID: none';

    const content = [
      `# Restart Context - ${now}`,
      ``,
      `## Message from Previous Session`,
      ``,
      `Gracefully shut down via make up. Session will be restored automatically.`,
      ``,
      sessionInfo,
      ``,
      `---`,
      `*Auto-generated by SIGTERM handler*`,
    ].join('\n');

    writeFileSync(saveFile, content, 'utf-8');
    console.log(`✅ Context saved to ${saveFile}`);
  } catch (error) {
    console.warn(`Failed to save shutdown context: ${error}`);
  }
}

process.on("SIGINT", () => {
  console.log("Received SIGINT");
  stopRunner();
  process.exit(0);
});

process.on("SIGTERM", () => {
  console.log("Received SIGTERM");
  saveShutdownContext();
  stopRunner();
  process.exit(0);
});
