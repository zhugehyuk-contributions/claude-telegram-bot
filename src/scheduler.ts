/**
 * Cron scheduler for scheduled prompts.
 *
 * Loads schedules from cron.yaml and executes prompts at specified times.
 */

import { Cron } from "croner";
import { existsSync, readFileSync } from "fs";
import { resolve } from "path";
import { parse as parseYaml } from "yaml";
import type { Api } from "grammy";
import { WORKING_DIR, ALLOWED_USERS } from "./config";
import { session } from "./session";
import { escapeHtml } from "./formatting";
import { isPathAllowed } from "./security";
import type { CronConfig, CronSchedule } from "./types";

const CRON_CONFIG_PATH = resolve(WORKING_DIR, "cron.yaml");
const MAX_PROMPT_LENGTH = 10000;
const MAX_JOBS_PER_HOUR = 60;

const activeJobs: Map<string, Cron> = new Map();
let botApi: Api | null = null;
let cronExecutionLock = false;
const jobExecutions: number[] = [];

export function initScheduler(api: Api): void {
  botApi = api;
}

function validateCronConfig(config: unknown): config is CronConfig {
  if (!config || typeof config !== "object") return false;
  const c = config as Record<string, unknown>;

  if (!Array.isArray(c.schedules)) return false;

  for (const schedule of c.schedules) {
    if (typeof schedule !== "object" || !schedule) return false;
    const s = schedule as Record<string, unknown>;

    if (typeof s.name !== "string" || !s.name) return false;
    if (typeof s.cron !== "string" || !s.cron) return false;
    if (typeof s.prompt !== "string" || !s.prompt) return false;
    if (s.enabled !== undefined && typeof s.enabled !== "boolean") return false;
    if (s.notify !== undefined && typeof s.notify !== "boolean") return false;

    if (s.prompt.length > MAX_PROMPT_LENGTH) {
      console.error(`[CRON] Prompt too long in ${s.name}: ${s.prompt.length} chars`);
      return false;
    }
  }

  return true;
}

function loadCronConfig(): CronConfig | null {
  if (!isPathAllowed(CRON_CONFIG_PATH)) {
    console.error("[CRON] cron.yaml path not in allowed directories");
    return null;
  }

  if (!existsSync(CRON_CONFIG_PATH)) {
    console.log(`No cron.yaml found at ${CRON_CONFIG_PATH}`);
    return null;
  }

  try {
    const content = readFileSync(CRON_CONFIG_PATH, "utf-8");
    const config = parseYaml(content);

    if (!validateCronConfig(config)) {
      console.error("[CRON] Invalid cron.yaml structure");
      return null;
    }

    return config;
  } catch (error) {
    console.error(`Failed to parse cron.yaml: ${error}`);
    return null;
  }
}

function checkRateLimit(): boolean {
  const now = Date.now();
  const oneHourAgo = now - 3600000;

  while (jobExecutions.length > 0 && jobExecutions[0]! < oneHourAgo) {
    jobExecutions.shift();
  }

  return jobExecutions.length < MAX_JOBS_PER_HOUR;
}

async function executeScheduledPrompt(schedule: CronSchedule): Promise<void> {
  const { name, prompt, notify } = schedule;
  console.log(`[CRON] Executing scheduled job: ${name}`);

  if (cronExecutionLock || session.isRunning) {
    console.log(`[CRON] Skipping ${name} - session is busy`);
    return;
  }

  if (!checkRateLimit()) {
    console.log(`[CRON] Rate limit reached, skipping ${name}`);
    return;
  }

  cronExecutionLock = true;
  jobExecutions.push(Date.now());

  try {
    const statusCallback = async (
      type: "thinking" | "tool" | "text" | "segment_end" | "done",
      content: string,
      _segmentId?: number
    ) => {
      if (type === "tool") {
        console.log(`[CRON:${name}] Tool: ${content}`);
      }
    };

    const userId = ALLOWED_USERS[0] || 0;
    const result = await session.sendMessageStreaming(
      prompt,
      `cron:${name}`,
      userId,
      statusCallback
    );

    console.log(`[CRON] Job ${name} completed`);

    if (notify && botApi && ALLOWED_USERS.length > 0) {
      const notifyUserId = ALLOWED_USERS[0]!;
      const safeName = escapeHtml(name);
      const safeResult = escapeHtml(result.slice(0, 3500));
      const message = `🕐 <b>Scheduled: ${safeName}</b>\n\n${safeResult}`;
      try {
        await botApi.sendMessage(notifyUserId, message, { parse_mode: "HTML" });
      } catch (err) {
        console.error(`[CRON] Failed to send notification: ${err}`);
      }
    }
  } catch (error) {
    console.error(`[CRON] Job ${name} failed: ${error}`);

    if (notify && botApi && ALLOWED_USERS.length > 0) {
      const notifyUserId = ALLOWED_USERS[0]!;
      const safeName = escapeHtml(name);
      const safeError = escapeHtml(String(error).slice(0, 500));
      try {
        await botApi.sendMessage(
          notifyUserId,
          `❌ <b>Scheduled job failed: ${safeName}</b>\n\n${safeError}`,
          { parse_mode: "HTML" }
        );
      } catch {}
    }
  } finally {
    cronExecutionLock = false;
  }
}

function scheduleJobs(config: CronConfig, verbose: boolean): number {
  let loaded = 0;
  for (const schedule of config.schedules) {
    if (schedule.enabled === false) {
      if (verbose) console.log(`[CRON] Skipping disabled schedule: ${schedule.name}`);
      continue;
    }

    try {
      const job = new Cron(schedule.cron, async () => {
        await executeScheduledPrompt(schedule);
      });
      activeJobs.set(schedule.name, job);
      loaded++;

      if (verbose) {
        const nextRun = job.nextRun();
        console.log(
          `[CRON] Scheduled: ${schedule.name} (${schedule.cron}) - next: ${nextRun?.toLocaleString() || "never"}`
        );
      }
    } catch (error) {
      console.error(`[CRON] Failed to schedule ${schedule.name}: ${error}`);
    }
  }
  return loaded;
}

export function startScheduler(): void {
  stopScheduler();

  const config = loadCronConfig();
  if (!config || config.schedules.length === 0) {
    console.log("[CRON] No schedules configured");
    return;
  }

  console.log(`[CRON] Loading ${config.schedules.length} schedules`);
  const loaded = scheduleJobs(config, true);
  console.log(`[CRON] Started ${loaded} jobs`);
}

export function stopScheduler(): void {
  if (activeJobs.size === 0) return;

  console.log(`[CRON] Stopping ${activeJobs.size} jobs`);
  for (const [, job] of activeJobs) {
    job.stop();
  }
  activeJobs.clear();
}

export function reloadScheduler(): number {
  console.log("[CRON] Reloading configuration");
  stopScheduler();

  const config = loadCronConfig();
  if (!config || config.schedules.length === 0) {
    return 0;
  }

  const loaded = scheduleJobs(config, false);
  console.log(`[CRON] Reloaded ${loaded} jobs`);
  return loaded;
}

export function getSchedulerStatus(): string {
  if (activeJobs.size === 0) {
    return "No scheduled jobs";
  }

  const lines: string[] = [`📅 <b>Scheduled Jobs (${activeJobs.size})</b>`];

  for (const [name, job] of activeJobs) {
    const nextRun = job.nextRun();
    const nextStr = nextRun
      ? nextRun.toLocaleString("en-US", {
          hour: "2-digit",
          minute: "2-digit",
          hour12: false,
        })
      : "never";
    lines.push(`• ${name}: next at ${nextStr}`);
  }

  return lines.join("\n");
}
