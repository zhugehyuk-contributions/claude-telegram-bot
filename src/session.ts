/**
 * Session management for Claude Telegram Bot.
 *
 * ClaudeSession class manages Claude Code sessions using the Agent SDK V1.
 * V1 supports full options (cwd, mcpServers, settingSources, etc.)
 */

import { query, type Options } from "@anthropic-ai/claude-agent-sdk";
import { readFileSync } from "fs";
import type { Context } from "grammy";
import {
  ALLOWED_PATHS,
  DEFAULT_THINKING_TOKENS,
  MCP_SERVERS,
  SAFETY_PROMPT,
  SESSION_FILE,
  STREAMING_THROTTLE_MS,
  TEMP_PATHS,
  THINKING_DEEP_KEYWORDS,
  THINKING_KEYWORDS,
  WORKING_DIR,
} from "./config";
import { formatToolStatus } from "./formatting";
import { checkPendingAskUserRequests } from "./handlers/streaming";
import { processQueuedJobs } from "./scheduler";
import { checkCommandSafety, isPathAllowed } from "./security";
import type { SessionData, StatusCallback, TokenUsage } from "./types";

/**
 * Determine thinking token budget based on message keywords.
 * Returns DEFAULT_THINKING_TOKENS if no keywords match.
 */
function getThinkingLevel(message: string): number {
  const msgLower = message.toLowerCase();

  // Check deep thinking triggers first (more specific)
  if (THINKING_DEEP_KEYWORDS.some((k) => msgLower.includes(k))) {
    return 50000;
  }

  // Check normal thinking triggers
  if (THINKING_KEYWORDS.some((k) => msgLower.includes(k))) {
    return 10000;
  }

  // Default: use configured default (0 if not set)
  return DEFAULT_THINKING_TOKENS;
}

/**
 * Manages Claude Code sessions using the Agent SDK V1.
 */
class ClaudeSession {
  sessionId: string | null = null;
  lastActivity: Date | null = null;
  queryStarted: Date | null = null;
  currentTool: string | null = null;
  lastTool: string | null = null;
  lastError: string | null = null;
  lastErrorTime: Date | null = null;
  lastUsage: TokenUsage | null = null;
  lastMessage: string | null = null;

  // Cumulative token tracking
  sessionStartTime: Date | null = null;
  totalInputTokens = 0;
  totalOutputTokens = 0;
  totalCacheReadTokens = 0;
  totalCacheCreateTokens = 0;
  totalQueries = 0;

  // Context limit tracking
  contextLimitWarned = false; // Only warn once per session (90% threshold)
  warned70 = false; // 70% threshold warning
  warned85 = false; // 85% threshold warning
  warned95 = false; // 95% threshold warning
  recentlyRestored = false; // Cooldown after /load
  messagesSinceRestore = 0; // Count messages since /load

  private abortController: AbortController | null = null;
  private isQueryRunning = false;
  private stopRequested = false;
  private _isProcessing = false;
  private _wasInterruptedByNewMessage = false;

  // Real-time steering buffer
  private steeringBuffer: string[] = [];
  private steeringMessageIds: number[] = [];

  get isActive(): boolean {
    return this.sessionId !== null;
  }

  get isRunning(): boolean {
    return this.isQueryRunning || this._isProcessing;
  }

  /**
   * Current cumulative context tokens (input + output) for this session.
   *
   * This is the same value used by accumulateUsage() to trigger context-limit warnings/auto-save.
   */
  get currentContextTokens(): number {
    return this.totalInputTokens + this.totalOutputTokens;
  }

  get needsSave(): boolean {
    return this.contextLimitWarned && !this.recentlyRestored;
  }

  get needsWarning70(): boolean {
    return this.warned70 && !this.recentlyRestored;
  }

  get needsWarning85(): boolean {
    return this.warned85 && !this.recentlyRestored;
  }

  get needsWarning95(): boolean {
    return this.warned95 && !this.recentlyRestored;
  }

  get isProcessing(): boolean {
    return this._isProcessing;
  }

  /**
   * Check if the last stop was triggered by a new message interrupt (! prefix).
   * Resets the flag when called. Also clears stopRequested so new messages can proceed.
   */
  consumeInterruptFlag(): boolean {
    const was = this._wasInterruptedByNewMessage;
    this._wasInterruptedByNewMessage = false;
    if (was) {
      // Clear stopRequested so the new message can proceed
      this.stopRequested = false;
    }
    return was;
  }

  /**
   * Mark that this stop is from a new message interrupt.
   */
  markInterrupt(): void {
    this._wasInterruptedByNewMessage = true;
  }

  /**
   * Clear the stopRequested flag (used after interrupt to allow new message to proceed).
   */
  clearStopRequested(): void {
    this.stopRequested = false;
  }

  /**
   * Clear warning flags after displaying warnings (one-time notification).
   */
  clearWarning70(): void {
    this.warned70 = false;
  }

  clearWarning85(): void {
    this.warned85 = false;
  }

  clearWarning95(): void {
    this.warned95 = false;
  }

  /**
   * Add a steering message to the buffer (user message sent during Claude execution).
   */
  addSteering(message: string, messageId?: number): void {
    this.steeringBuffer.push(message);
    if (messageId) {
      this.steeringMessageIds.push(messageId);
    }
  }

  /**
   * Consume all buffered steering messages and return as combined string.
   * Clears the buffer after consumption.
   */
  consumeSteering(): string | null {
    if (this.steeringBuffer.length === 0) {
      return null;
    }
    const combined = this.steeringBuffer.join("\n---\n");
    this.steeringBuffer = [];
    this.steeringMessageIds = [];
    return combined;
  }

  /**
   * Check if there are any buffered steering messages.
   */
  hasSteeringMessages(): boolean {
    return this.steeringBuffer.length > 0;
  }

  /**
   * Mark processing as started.
   * Returns a cleanup function to call when done.
   */
  startProcessing(): () => void {
    this._isProcessing = true;
    return () => {
      this._isProcessing = false;
      // Clear any unconsumed steering messages
      if (this.steeringBuffer.length > 0) {
        console.log(
          `[STEERING] Clearing ${this.steeringBuffer.length} unconsumed messages`
        );
        this.steeringBuffer = [];
        this.steeringMessageIds = [];
      }
    };
  }

  /**
   * Stop the currently running query or mark for cancellation.
   * Returns: "stopped" if query was aborted, "pending" if processing will be cancelled, false if nothing running
   */
  async stop(): Promise<"stopped" | "pending" | false> {
    // If a query is actively running, abort it
    if (this.isQueryRunning && this.abortController) {
      this.stopRequested = true;
      this.abortController.abort();
      console.log("Stop requested - aborting current query");
      return "stopped";
    }

    // If processing but query not started yet
    if (this._isProcessing) {
      this.stopRequested = true;
      console.log("Stop requested - will cancel before query starts");
      return "pending";
    }

    return false;
  }

  /**
   * Send a message to Claude with streaming updates via callback.
   *
   * @param ctx - grammY context for ask_user button display
   */
  async sendMessageStreaming(
    message: string,
    username: string,
    userId: number,
    statusCallback: StatusCallback,
    chatId?: number,
    ctx?: Context
  ): Promise<string> {
    // Set chat context for ask_user MCP tool
    if (chatId) {
      process.env.TELEGRAM_CHAT_ID = String(chatId);
    }

    const isNewSession = !this.isActive;
    const thinkingTokens = getThinkingLevel(message);
    const thinkingLabel =
      { 0: "off", 10000: "normal", 50000: "deep" }[thinkingTokens] ||
      String(thinkingTokens);

    // Inject current date/time at session start so Claude doesn't need to call a tool for it
    let messageToSend = message;
    if (isNewSession) {
      const now = new Date();
      const datePrefix = `[Current date/time: ${now.toLocaleDateString("en-US", {
        weekday: "long",
        year: "numeric",
        month: "long",
        day: "numeric",
        hour: "2-digit",
        minute: "2-digit",
        timeZoneName: "short",
      })}]\n\n`;
      messageToSend = datePrefix + message;
    }

    // Build SDK V1 options - supports all features
    const options: Options = {
      model: "claude-sonnet-4-5",
      cwd: WORKING_DIR,
      settingSources: ["user", "project"],
      permissionMode: "bypassPermissions",
      allowDangerouslySkipPermissions: true,
      systemPrompt: SAFETY_PROMPT,
      mcpServers: MCP_SERVERS,
      maxThinkingTokens: thinkingTokens,
      additionalDirectories: ALLOWED_PATHS,
      resume: this.sessionId || undefined,
      // Real-time steering: inject buffered user messages before tool execution
      hooks: {
        PreToolUse: [
          {
            hooks: [
              async (input) => {
                const steering = this.consumeSteering();
                if (!steering) {
                  return { continue: true };
                }

                console.log(`[STEERING] Injecting user message before tool execution`);
                return {
                  continue: true,
                  systemMessage: `[USER SENT MESSAGE DURING EXECUTION]\n${steering}\n[END USER MESSAGE]`,
                };
              },
            ],
          },
        ],
      },
    };

    // Add Claude Code executable path if set (required for standalone builds)
    if (process.env.CLAUDE_CODE_PATH) {
      options.pathToClaudeCodeExecutable = process.env.CLAUDE_CODE_PATH;
    }

    if (this.sessionId && !isNewSession) {
      console.log(
        `RESUMING session ${this.sessionId.slice(0, 8)}... (thinking=${thinkingLabel})`
      );
    } else {
      console.log(`STARTING new Claude session (thinking=${thinkingLabel})`);
      this.sessionId = null;
    }

    // Check if stop was requested during processing phase
    if (this.stopRequested) {
      console.log(
        "Query cancelled before starting (stop was requested during processing)"
      );
      this.stopRequested = false;
      throw new Error("Query cancelled");
    }

    // Create abort controller for cancellation
    this.abortController = new AbortController();
    this.isQueryRunning = true;
    this.stopRequested = false;
    this.queryStarted = new Date();
    this.currentTool = null;

    // Response tracking
    const responseParts: string[] = [];
    let currentSegmentId = 0;
    let currentSegmentText = "";
    let lastTextUpdate = 0;
    let queryCompleted = false;
    let askUserTriggered = false;

    try {
      // Use V1 query() API - supports all options including cwd, mcpServers, etc.
      const queryInstance = query({
        prompt: messageToSend,
        options: {
          ...options,
          abortController: this.abortController,
        },
      });

      // Process streaming response
      for await (const event of queryInstance) {
        // Check for abort
        if (this.stopRequested) {
          console.log("Query aborted by user");
          break;
        }

        // Capture session_id from first message
        if (!this.sessionId && event.session_id) {
          this.sessionId = event.session_id;
          console.log(`GOT session_id: ${this.sessionId!.slice(0, 8)}...`);
          this.saveSession();
        }

        // Handle different message types
        if (event.type === "assistant") {
          for (const block of event.message.content) {
            // Thinking blocks
            if (block.type === "thinking") {
              const thinkingText = block.thinking;
              if (thinkingText) {
                console.log(`THINKING BLOCK: ${thinkingText.slice(0, 100)}...`);
                await statusCallback("thinking", thinkingText);
              }
            }

            // Tool use blocks
            if (block.type === "tool_use") {
              const toolName = block.name;
              const toolInput = block.input as Record<string, unknown>;

              // Safety check for Bash commands
              if (toolName === "Bash") {
                const command = String(toolInput.command || "");
                const [isSafe, reason] = checkCommandSafety(command);
                if (!isSafe) {
                  console.warn(`BLOCKED: ${reason}`);
                  await statusCallback("tool", `BLOCKED: ${reason}`);
                  throw new Error(`Unsafe command blocked: ${reason}`);
                }
              }

              // Safety check for file operations
              if (["Read", "Write", "Edit"].includes(toolName)) {
                const filePath = String(toolInput.file_path || "");
                if (filePath) {
                  // Allow reads from temp paths and .claude directories
                  const isTmpRead =
                    toolName === "Read" &&
                    (TEMP_PATHS.some((p) => filePath.startsWith(p)) ||
                      filePath.includes("/.claude/"));

                  if (!isTmpRead && !isPathAllowed(filePath)) {
                    console.warn(
                      `BLOCKED: File access outside allowed paths: ${filePath}`
                    );
                    await statusCallback("tool", `Access denied: ${filePath}`);
                    throw new Error(`File access blocked: ${filePath}`);
                  }
                }
              }

              // Segment ends when tool starts
              if (currentSegmentText) {
                await statusCallback(
                  "segment_end",
                  currentSegmentText,
                  currentSegmentId
                );
                currentSegmentId++;
                currentSegmentText = "";
              }

              // Format and show tool status
              const toolDisplay = formatToolStatus(toolName, toolInput);
              this.currentTool = toolDisplay;
              this.lastTool = toolDisplay;
              console.log(`Tool: ${toolDisplay}`);

              // Don't show tool status for ask_user - the buttons are self-explanatory
              if (!toolName.startsWith("mcp__ask-user")) {
                await statusCallback("tool", toolDisplay);
              }

              // Check for pending ask_user requests after ask-user MCP tool
              if (toolName.startsWith("mcp__ask-user") && ctx && chatId) {
                // Small delay to let MCP server write the file
                await new Promise((resolve) => setTimeout(resolve, 200));

                // Retry a few times in case of timing issues
                for (let attempt = 0; attempt < 3; attempt++) {
                  const buttonsSent = await checkPendingAskUserRequests(ctx, chatId);
                  if (buttonsSent) {
                    askUserTriggered = true;
                    break;
                  }
                  if (attempt < 2) {
                    await new Promise((resolve) => setTimeout(resolve, 100));
                  }
                }
              }
            }

            // Text content
            if (block.type === "text") {
              responseParts.push(block.text);
              currentSegmentText += block.text;

              // Stream text updates (throttled)
              const now = Date.now();
              if (
                now - lastTextUpdate > STREAMING_THROTTLE_MS &&
                currentSegmentText.length > 20
              ) {
                await statusCallback("text", currentSegmentText, currentSegmentId);
                lastTextUpdate = now;
              }
            }
          }

          // Break out of event loop if ask_user was triggered
          if (askUserTriggered) {
            break;
          }
        }

        // Result message
        if (event.type === "result") {
          console.log("Response complete");
          queryCompleted = true;

          if ("usage" in event && event.usage) {
            this.lastUsage = event.usage as TokenUsage;
            this.accumulateUsage(this.lastUsage);
          }
        }
      }
    } catch (error) {
      const errorStr = String(error).toLowerCase();
      const isCleanupError = errorStr.includes("cancel") || errorStr.includes("abort");
      const shouldSuppress =
        isCleanupError && (queryCompleted || askUserTriggered || this.stopRequested);

      if (shouldSuppress) {
        console.warn(`Suppressed post-completion error: ${error}`);
      } else {
        console.error(`Error in query: ${error}`);
        this.lastError = String(error).slice(0, 100);
        this.lastErrorTime = new Date();
        throw error;
      }
    } finally {
      this.isQueryRunning = false;
      this.abortController = null;
      this.queryStarted = null;
      this.currentTool = null;
    }

    this.lastActivity = new Date();
    this.lastError = null;
    this.lastErrorTime = null;

    // If ask_user was triggered, return early - user will respond via button
    if (askUserTriggered) {
      await statusCallback("done", "");
      return "[Waiting for user selection]";
    }

    // Emit final segment
    if (currentSegmentText) {
      await statusCallback("segment_end", currentSegmentText, currentSegmentId);
    }

    await statusCallback("done", "");

    // Process any queued cron jobs now that session is complete
    processQueuedJobs().catch((err) => {
      console.error("[CRON] Failed to process queued jobs:", err);
    });

    return responseParts.join("") || "No response from Claude.";
  }

  /**
   * Kill the current session (clear session_id).
   */
  async kill(): Promise<void> {
    this.sessionId = null;
    this.lastActivity = null;

    // Reset cumulative stats
    this.sessionStartTime = null;
    this.totalInputTokens = 0;
    this.totalOutputTokens = 0;
    this.totalCacheReadTokens = 0;
    this.totalCacheCreateTokens = 0;
    this.totalQueries = 0;

    // Reset warning flags
    this.contextLimitWarned = false;
    this.warned70 = false;
    this.warned85 = false;
    this.warned95 = false;
    this.recentlyRestored = false;
    this.messagesSinceRestore = 0;

    console.log("Session cleared");
  }

  /**
   * Mark that context was just restored (activate cooldown).
   * Called after /load skill execution.
   */
  markRestored(): void {
    this.recentlyRestored = true;
    this.messagesSinceRestore = 0;
    this.contextLimitWarned = false;
    this.warned70 = false;
    this.warned85 = false;
    this.warned95 = false;
    console.log("Context restored - cooldown activated (50 messages)");
  }

  private accumulateUsage(u: TokenUsage): void {
    if (!this.sessionStartTime) this.sessionStartTime = new Date();

    this.totalInputTokens += u.input_tokens || 0;
    this.totalOutputTokens += u.output_tokens || 0;
    this.totalCacheReadTokens += u.cache_read_input_tokens || 0;
    this.totalCacheCreateTokens += u.cache_creation_input_tokens || 0;
    this.totalQueries++;

    console.log(
      `Usage: in=${u.input_tokens} out=${u.output_tokens} ` +
        `cache_read=${u.cache_read_input_tokens || 0} cache_create=${u.cache_creation_input_tokens || 0}`
    );

    // Context limit monitoring (Oracle critical)
    const CONTEXT_LIMIT = 200_000;
    const SAVE_THRESHOLD = 180_000; // Trigger at 90% (20k buffer)
    const COOLDOWN_MESSAGES = 50; // Don't re-trigger for 50 messages after /load

    // ORACLE FIX: Use cumulative context, not per-message
    const currentContext = this.totalInputTokens + this.totalOutputTokens;

    // Increment message counter
    if (this.recentlyRestored) {
      this.messagesSinceRestore++;
      if (this.messagesSinceRestore >= COOLDOWN_MESSAGES) {
        console.log("Cooldown period complete, re-enabling context limit monitoring");
        this.recentlyRestored = false;
        this.contextLimitWarned = false;
        this.warned70 = false;
        this.warned85 = false;
        this.warned95 = false;
      }
    }

    // Multi-threshold warning system (70%, 85%, 95%)
    const THRESHOLD_70 = 140_000;
    const THRESHOLD_85 = 170_000;
    const THRESHOLD_95 = 190_000;

    if (currentContext >= THRESHOLD_70 && !this.warned70 && !this.recentlyRestored) {
      this.warned70 = true;
      const percentage = ((currentContext / CONTEXT_LIMIT) * 100).toFixed(1);
      const tokensRemaining = CONTEXT_LIMIT - currentContext;
      console.log(`[TELEMETRY] context_threshold_70`, {
        sessionId: this.sessionId?.slice(0, 8),
        currentContext,
        threshold: THRESHOLD_70,
        tokensRemaining,
        percentage,
        timestamp: new Date().toISOString(),
      });
      console.warn(`⚠️  Context: ${currentContext}/${CONTEXT_LIMIT} (${percentage}%) - 70% reached`);
    }

    if (currentContext >= THRESHOLD_85 && !this.warned85 && !this.recentlyRestored) {
      this.warned85 = true;
      const percentage = ((currentContext / CONTEXT_LIMIT) * 100).toFixed(1);
      const tokensRemaining = CONTEXT_LIMIT - currentContext;
      console.log(`[TELEMETRY] context_threshold_85`, {
        sessionId: this.sessionId?.slice(0, 8),
        currentContext,
        threshold: THRESHOLD_85,
        tokensRemaining,
        percentage,
        timestamp: new Date().toISOString(),
      });
      console.warn(`⚠️  Context: ${currentContext}/${CONTEXT_LIMIT} (${percentage}%) - 85% reached`);
    }

    if (currentContext >= THRESHOLD_95 && !this.warned95 && !this.recentlyRestored) {
      this.warned95 = true;
      const percentage = ((currentContext / CONTEXT_LIMIT) * 100).toFixed(1);
      const tokensRemaining = CONTEXT_LIMIT - currentContext;
      console.log(`[TELEMETRY] context_threshold_95`, {
        sessionId: this.sessionId?.slice(0, 8),
        currentContext,
        threshold: THRESHOLD_95,
        tokensRemaining,
        percentage,
        timestamp: new Date().toISOString(),
      });
      console.warn(`⚠️  Context: ${currentContext}/${CONTEXT_LIMIT} (${percentage}%) - 95% CRITICAL`);
    }

    // Check if we should trigger save (180k threshold)
    if (
      currentContext >= SAVE_THRESHOLD &&
      !this.contextLimitWarned &&
      !this.recentlyRestored
    ) {
      this.contextLimitWarned = true;
      const percentage = ((currentContext / CONTEXT_LIMIT) * 100).toFixed(1);

      // ORACLE: Add telemetry
      console.log("[TELEMETRY] context_limit_approaching", {
        currentContext,
        threshold: SAVE_THRESHOLD,
        percentage,
        timestamp: new Date().toISOString(),
      });

      console.warn(
        `⚠️  CONTEXT LIMIT APPROACHING: ${currentContext}/${CONTEXT_LIMIT} tokens ` +
          `(${percentage}%) - SAVE REQUIRED`
      );
    }
  }

  private saveSession(): void {
    if (!this.sessionId) return;

    try {
      const data: SessionData = {
        session_id: this.sessionId,
        saved_at: new Date().toISOString(),
        working_dir: WORKING_DIR,
        // Save token counters for context tracking
        totalInputTokens: this.totalInputTokens,
        totalOutputTokens: this.totalOutputTokens,
        totalQueries: this.totalQueries,
        sessionStartTime: this.sessionStartTime?.toISOString(),
      };
      Bun.write(SESSION_FILE, JSON.stringify(data));
      console.log(`Session saved to ${SESSION_FILE} (context: ${this.totalInputTokens + this.totalOutputTokens} tokens)`);
    } catch (error) {
      console.warn(`Failed to save session: ${error}`);
    }
  }

  /**
   * Resume the last persisted session.
   */
  resumeLast(): [success: boolean, message: string] {
    try {
      const file = Bun.file(SESSION_FILE);
      if (!file.size) {
        return [false, "No saved session found"];
      }

      const text = readFileSync(SESSION_FILE, "utf-8");
      const data: SessionData = JSON.parse(text);

      if (!data.session_id) {
        return [false, "Saved session file is empty"];
      }

      if (data.working_dir && data.working_dir !== WORKING_DIR) {
        return [false, `Session was for different directory: ${data.working_dir}`];
      }

      this.sessionId = data.session_id;
      this.lastActivity = new Date();

      // Restore token counters for context tracking (backward compatible)
      this.totalInputTokens = data.totalInputTokens || 0;
      this.totalOutputTokens = data.totalOutputTokens || 0;
      this.totalQueries = data.totalQueries || 0;
      this.sessionStartTime = data.sessionStartTime ? new Date(data.sessionStartTime) : null;

      const contextTokens = this.totalInputTokens + this.totalOutputTokens;
      console.log(
        `Resumed session ${data.session_id.slice(0, 8)}... (saved at ${data.saved_at}, context: ${contextTokens} tokens)`
      );
      return [
        true,
        `Resumed session \`${data.session_id.slice(0, 8)}...\` (saved at ${
          data.saved_at
        })`,
      ];
    } catch (error) {
      console.error(`Failed to resume session: ${error}`);
      return [false, `Failed to load session: ${error}`];
    }
  }
}

// Global session instance
export const session = new ClaudeSession();
