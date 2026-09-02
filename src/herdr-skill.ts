/**
 * Dynamic herdr-mcp remote-planner skill.
 *
 * The project skill is authoritative for a web/remote planner. It is refreshed
 * from the herdr-mcp repository when allowed and falls back to the bundled
 * release copy. The installed `herdr --skill` output is appended only as
 * release-matched native reference material; its pane-local HERDR_ENV rules do
 * not override the remote-planner policy.
 */

import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { SERVER_VERSION } from "./version.js";
import { enrichedUserEnv } from "./user-path.js";

const execFileAsync = promisify(execFile);

const DEFAULT_PROJECT_SKILL_URL =
  process.env.HERDR_MCP_SKILL_URL
  ?? "https://whshang.github.io/herdr-mcp/herdr-mcp-SKILL.md";

const BUNDLED_PROJECT_SKILL_PATH = fileURLToPath(
  new URL("../assets/herdr-mcp-SKILL.md", import.meta.url),
);
const BUNDLED_NATIVE_SKILL_PATH = fileURLToPath(
  new URL("../assets/herdr-agent-SKILL.md", import.meta.url),
);

const CACHE_TTL_MS = Math.max(
  60_000,
  Number(process.env.HERDR_SKILL_CACHE_SEC ?? "3600") * 1000,
);
const FETCH_TIMEOUT_MS = Math.min(
  60_000,
  Math.max(3_000, Number(process.env.HERDR_SKILL_FETCH_TIMEOUT_MS ?? "15000")),
);
const NATIVE_TIMEOUT_MS = Math.min(
  15_000,
  Math.max(1_000, Number(process.env.HERDR_NATIVE_SKILL_TIMEOUT_MS ?? "5000")),
);

function networkOff(): boolean {
  return process.env.HERDR_SKILL_NETWORK === "0";
}

export const HERDR_MCP_SKILL_UPSTREAM = DEFAULT_PROJECT_SKILL_URL.replace(/^https?:\/\//, "");
export const HERDR_MCP_SKILL_BUNDLED = "bundled:assets/herdr-mcp-SKILL.md";
export const HERDR_NATIVE_SKILL_LOCAL = "local:herdr --skill";
export const HERDR_NATIVE_SKILL_BUNDLED = "bundled:assets/herdr-agent-SKILL.md";
// Backward-compatible export names used by older tests/callers.
export const HERDR_SKILL_UPSTREAM = HERDR_MCP_SKILL_UPSTREAM;
export const HERDR_SKILL_BUNDLED = HERDR_MCP_SKILL_BUNDLED;

type ProjectCache = {
  content: string;
  source: string;
  fetchedAt: number;
};

let projectCache: ProjectCache | null = null;
let bundledProjectCache: string | null = null;
let bundledNativeCache: string | null = null;

type ProjectSkill = {
  content: string;
  source: string;
  origin: "network" | "cache" | "bundled";
  fetchedAt: number;
  cached: boolean;
  stale?: boolean;
};

type NativeSkill = {
  content: string;
  source: string;
  origin: "local" | "bundled";
  sha256: string;
};

export type HerdrSkillResult =
  | {
    ok: true;
    content: string;
    project_skill: {
      source: string;
      origin: ProjectSkill["origin"];
      cached: boolean;
      stale?: boolean;
      fetched_at: string;
    };
    native_reference?: {
      source: string;
      origin: NativeSkill["origin"];
      sha256: string;
      bytes: number;
    };
    runtime: Record<string, unknown>;
    refreshed_at: string;
    cache_ttl_sec: number;
    bytes: number;
  }
  | {
    ok: false;
    reason: "project_skill_unavailable";
    message: string;
    source: string;
  };

function sha256(content: string): string {
  return createHash("sha256").update(content, "utf8").digest("hex");
}

async function readBundledProjectSkill(): Promise<string> {
  if (bundledProjectCache) return bundledProjectCache;
  const content = (await readFile(BUNDLED_PROJECT_SKILL_PATH, "utf8")).trim();
  if (!content) throw new Error("bundled herdr-mcp skill is empty");
  bundledProjectCache = content;
  return content;
}

async function readBundledNativeSkill(): Promise<string> {
  if (bundledNativeCache) return bundledNativeCache;
  const content = (await readFile(BUNDLED_NATIVE_SKILL_PATH, "utf8")).trim();
  if (!content) throw new Error("bundled native Herdr skill is empty");
  bundledNativeCache = content;
  return content;
}

async function fetchText(source: string): Promise<string> {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), FETCH_TIMEOUT_MS);
  try {
    const resp = await fetch(source, {
      signal: ctrl.signal,
      headers: {
        Accept: "text/plain, text/markdown, */*",
        "User-Agent": `herdr-mcp/${SERVER_VERSION} skill-refresh`,
      },
      redirect: "follow",
    });
    if (!resp.ok) throw new Error(`HTTP ${resp.status} ${resp.statusText}`.trim());
    return await resp.text();
  } finally {
    clearTimeout(timer);
  }
}

async function projectSkill(refresh: boolean): Promise<ProjectSkill> {
  const now = Date.now();
  if (!refresh && projectCache && now - projectCache.fetchedAt < CACHE_TTL_MS) {
    return {
      content: projectCache.content,
      source: projectCache.source,
      origin: "cache",
      fetchedAt: projectCache.fetchedAt,
      cached: true,
    };
  }

  if (!networkOff()) {
    try {
      const content = (await fetchText(DEFAULT_PROJECT_SKILL_URL)).trim();
      if (!content) throw new Error("project skill document was empty");
      projectCache = { content, source: DEFAULT_PROJECT_SKILL_URL, fetchedAt: now };
      return { content, source: DEFAULT_PROJECT_SKILL_URL, origin: "network", fetchedAt: now, cached: false };
    } catch {
      if (projectCache) {
        return {
          content: projectCache.content,
          source: projectCache.source,
          origin: "cache",
          fetchedAt: projectCache.fetchedAt,
          cached: true,
          stale: true,
        };
      }
    }
  }

  const content = await readBundledProjectSkill();
  return {
    content,
    source: HERDR_MCP_SKILL_BUNDLED,
    origin: "bundled",
    fetchedAt: now,
    cached: false,
  };
}

async function nativeSkill(): Promise<NativeSkill> {
  try {
    const result = await execFileAsync("herdr", ["--skill"], {
      encoding: "utf8",
      timeout: NATIVE_TIMEOUT_MS,
      maxBuffer: 512 * 1024,
      env: process.env,
    });
    const content = String(result.stdout || "").trim();
    if (!content) throw new Error("herdr --skill returned empty output");
    return { content, source: HERDR_NATIVE_SKILL_LOCAL, origin: "local", sha256: sha256(content) };
  } catch {
    const content = await readBundledNativeSkill();
    return { content, source: HERDR_NATIVE_SKILL_BUNDLED, origin: "bundled", sha256: sha256(content) };
  }
}

async function readOptionalJson(path: string): Promise<Record<string, unknown> | null> {
  try {
    const raw = await readFile(path, "utf8");
    if (raw.length > 256 * 1024) return null;
    const value = JSON.parse(raw);
    return value && typeof value === "object" && !Array.isArray(value)
      ? value as Record<string, unknown>
      : null;
  } catch {
    return null;
  }
}

function compactRuntimeStatus(value: Record<string, unknown> | null): Record<string, unknown> | null {
  const manager = value?.manager && typeof value.manager === "object"
    ? value.manager as Record<string, unknown>
    : null;
  if (!manager) return null;
  return {
    active_generation: manager.active_generation ?? null,
    previous_generation: manager.previous_generation ?? null,
    last_good_generation: manager.last_good_generation ?? null,
    transition_seq: manager.transition_seq ?? null,
  };
}

async function cliVersion(command: string, args: string[]): Promise<string | null> {
  try {
    const result = await execFileAsync(command, args, {
      encoding: "utf8",
      timeout: 2_000,
      maxBuffer: 64 * 1024,
      env: enrichedUserEnv(process.env),
    });
    return String(result.stdout || result.stderr || "").trim().split(/\r?\n/, 1)[0] || null;
  } catch {
    return null;
  }
}

async function dshTuiProfileVersion(): Promise<string | null> {
  const dshHome = process.env.DSH_HOME || join(homedir(), ".dsh");
  try {
    const raw = await readFile(join(dshHome, "profiles", "dsh-tui", "package.json"), "utf8");
    const pkg = JSON.parse(raw) as { dependencies?: Record<string, string> };
    return pkg.dependencies?.["@deepseek-harness-tui/dsh-tui"] ?? null;
  } catch {
    return null;
  }
}

async function workerFallbackContext(): Promise<Record<string, unknown>> {
  const [dshVersion, tuiProfileVersion] = await Promise.all([
    cliVersion("dsh", ["--version"]),
    dshTuiProfileVersion(),
  ]);
  return {
    dsh_headless: {
      available: dshVersion !== null,
      version: dshVersion,
      invocation: "herdr_exec_start -> dsh --profile headless <task>",
      note: "long-running fallback; inspect mutations before retrying after timeout",
    },
    dsh_tui: {
      available: tuiProfileVersion !== null,
      profile_package: tuiProfileVersion,
      role: "human-interactive fallback",
    },
  };
}

async function runtimeContext(): Promise<Record<string, unknown>> {
  const stateDir = process.env.HERDR_MCP_STATE_DIR || join(homedir(), ".config", "herdr-mcp");
  const runtimeStatusPath = process.env.HERDR_RUNTIME_STATUS_PATH || join(stateDir, "runtime-status-prod.json");
  const selfUpdatePath = process.env.HERDR_SELF_UPDATE_STATUS_PATH || join(stateDir, "self-update-status.json");
  const [runtimeStatus, updateStatus, workerFallbacks] = await Promise.all([
    readOptionalJson(runtimeStatusPath),
    readOptionalJson(selfUpdatePath),
    workerFallbackContext(),
  ]);
  const runtimeGeneration = compactRuntimeStatus(runtimeStatus);
  const buildCommit = process.env.HERDR_MCP_BUILD_COMMIT || null;
  const runtimeChannel = process.env.HERDR_MCP_BUILD_CHANNEL || "prod";
  return {
    server_version: SERVER_VERSION,
    runtime_channel: runtimeChannel,
    build_commit: buildCommit,
    active_runtime: {
      version: SERVER_VERSION,
      channel: runtimeChannel,
      source_commit: buildCommit,
      generation: runtimeGeneration?.active_generation ?? null,
      truth_source: "active_binary+runtime_generation_manager",
    },
    contract_profile: process.env.HERDR_MCP_CONTRACT_PROFILE || "current",
    network_skill_refresh: !networkOff(),
    worker_fallbacks: workerFallbacks,
    runtime_generation: runtimeGeneration,
    self_update: updateStatus
      ? {
          state: updateStatus.state ?? updateStatus.status ?? null,
          target_version: updateStatus.target_version ?? null,
          source: updateStatus.source ?? null,
          updated_at: updateStatus.updated_at ?? null,
          semantics: "historical_operation",
          active_runtime_authority: false,
        }
      : null,
  };
}

/** Read the project policy, live runtime context, and optional native Herdr reference. */
export async function fetchHerdrSkill(options?: {
  refresh?: boolean;
  includeNativeReference?: boolean;
}): Promise<HerdrSkillResult> {
  const refresh = options?.refresh === true;
  const includeNativeReference = options?.includeNativeReference !== false;
  const now = Date.now();
  try {
    const [project, runtime, native] = await Promise.all([
      projectSkill(refresh),
      runtimeContext(),
      includeNativeReference ? nativeSkill() : Promise.resolve(null),
    ]);

    const runtimeBlock = JSON.stringify(runtime, null, 2);
    const nativeBlock = native
      ? `\n\n---\n\n## Appendix: release-matched native Herdr reference\n\n` +
        `The block below comes from \`${native.source}\`. It documents pane-local Herdr usage. ` +
        `Its \`HERDR_ENV=1\` stop rule does **not** override the remote-planner policy above. ` +
        `For remote native calls, \`herdr_methods\` is the live schema authority.\n\n` +
        `\`\`\`text\n${native.content}\n\`\`\``
      : "";

    const content = `${project.content}\n\n---\n\n## Live herdr-mcp runtime context\n\n` +
      `This block is generated at call time and is status, not policy.\n\n` +
      `\`\`\`json\n${runtimeBlock}\n\`\`\`${nativeBlock}`;

    return {
      ok: true,
      content,
      project_skill: {
        source: project.source,
        origin: project.origin,
        cached: project.cached,
        ...(project.stale ? { stale: true } : {}),
        fetched_at: new Date(project.fetchedAt).toISOString(),
      },
      ...(native ? {
        native_reference: {
          source: native.source,
          origin: native.origin,
          sha256: native.sha256,
          bytes: Buffer.byteLength(native.content, "utf8"),
        },
      } : {}),
      runtime,
      refreshed_at: new Date(now).toISOString(),
      cache_ttl_sec: Math.round(CACHE_TTL_MS / 1000),
      bytes: Buffer.byteLength(content, "utf8"),
    };
  } catch (error) {
    return {
      ok: false,
      reason: "project_skill_unavailable",
      message: String(error instanceof Error ? error.message : error).slice(0, 240),
      source: DEFAULT_PROJECT_SKILL_URL,
    };
  }
}

/** Short pointer for herdr_inspect.workstation_info (no network). */
export function herdrSkillPointer(): Record<string, string> {
  return {
    tool: "herdr_skill",
    project_upstream: HERDR_MCP_SKILL_UPSTREAM,
    project_bundled: HERDR_MCP_SKILL_BUNDLED,
    native_reference: HERDR_NATIVE_SKILL_LOCAL,
    self_update: "herdr-self-update",
    hint: "Remote-planner policy first; live runtime/update context is generated per call; release-matched native Herdr skill is appended as scoped reference. refresh=true rechecks project policy upstream.",
  };
}
