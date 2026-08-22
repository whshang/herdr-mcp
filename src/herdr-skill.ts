/**
 * Herdr agent SKILL.md: try upstream (herdr master), fall back to shipped bundle.
 * ChatGPT never fetches GitHub — only this MCP server process does.
 */

import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const DEFAULT_SKILL_URL =
  process.env.HERDR_SKILL_URL
  ?? "https://raw.githubusercontent.com/herdrdev/herdr/master/skills/herdr/SKILL.md";

const BUNDLED_SKILL_PATH = fileURLToPath(
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

/** When "1", skip network and serve bundled (or warm cache) only. */
function networkOff(): boolean {
  return process.env.HERDR_SKILL_NETWORK === "0";
}

export const HERDR_SKILL_UPSTREAM = DEFAULT_SKILL_URL.replace(/^https?:\/\//, "");
export const HERDR_SKILL_BUNDLED = "bundled:assets/herdr-agent-SKILL.md";

type SkillCache = {
  content: string;
  source: string;
  fetchedAt: number;
};

let skillCache: SkillCache | null = null;
let bundledCache: { content: string } | null = null;

export type HerdrSkillResult =
  | {
    ok: true;
    content: string;
    source: string;
    origin: "network" | "cache" | "bundled";
    fetched_at: string;
    cached: boolean;
    stale?: boolean;
    cache_ttl_sec: number;
    bytes: number;
  }
  | {
    ok: false;
    reason: "fetch_failed" | "empty_body" | "bundled_missing";
    message: string;
    source: string;
  };

function okResult(
  content: string,
  source: string,
  origin: "network" | "cache" | "bundled",
  fetchedAt: number,
  cached: boolean,
  stale = false,
): Extract<HerdrSkillResult, { ok: true }> {
  return {
    ok: true,
    content,
    source,
    origin,
    fetched_at: new Date(fetchedAt).toISOString(),
    cached,
    ...(stale ? { stale: true } : {}),
    cache_ttl_sec: Math.round(CACHE_TTL_MS / 1000),
    bytes: Buffer.byteLength(content, "utf8"),
  };
}

async function readBundledSkill(): Promise<string> {
  if (bundledCache) return bundledCache.content;
  const content = (await readFile(BUNDLED_SKILL_PATH, "utf8")).trim();
  if (!content) throw new Error("bundled skill file is empty");
  bundledCache = { content };
  return content;
}

async function fetchSkillBody(source: string): Promise<string> {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), FETCH_TIMEOUT_MS);
  try {
    const resp = await fetch(source, {
      signal: ctrl.signal,
      headers: {
        Accept: "text/plain, text/markdown, */*",
        "User-Agent": "herdr-mcp/skill-fetch",
      },
      redirect: "follow",
    });
    if (!resp.ok) {
      throw new Error(`HTTP ${resp.status} ${resp.statusText}`.trim());
    }
    return await resp.text();
  } finally {
    clearTimeout(timer);
  }
}

/** Read cached, upstream, or bundled Herdr SKILL.md. */
export async function fetchHerdrSkill(options?: { refresh?: boolean }): Promise<HerdrSkillResult> {
  const source = DEFAULT_SKILL_URL;
  const refresh = options?.refresh === true;
  const now = Date.now();

  if (!refresh && skillCache && now - skillCache.fetchedAt < CACHE_TTL_MS) {
    return okResult(skillCache.content, skillCache.source, "cache", skillCache.fetchedAt, true);
  }

  if (!networkOff()) {
    try {
      const content = (await fetchSkillBody(source)).trim();
      if (!content) {
        return { ok: false, reason: "empty_body", message: "skill document was empty", source };
      }
      skillCache = { content, source, fetchedAt: now };
      return okResult(content, source, "network", now, false);
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      if (skillCache) {
        return okResult(skillCache.content, skillCache.source, "cache", skillCache.fetchedAt, true, true);
      }
      try {
        const bundled = await readBundledSkill();
        return okResult(bundled, HERDR_SKILL_BUNDLED, "bundled", now, false);
      } catch (be) {
        const bmsg = be instanceof Error ? be.message : String(be);
        return {
          ok: false,
          reason: "fetch_failed",
          message: `${message}; bundled fallback: ${bmsg}`,
          source,
        };
      }
    }
  }

  try {
    const bundled = await readBundledSkill();
    return okResult(bundled, HERDR_SKILL_BUNDLED, "bundled", now, false);
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    return { ok: false, reason: "bundled_missing", message, source: HERDR_SKILL_BUNDLED };
  }
}

/** Short pointer for herdr_inspect.workstation_info (no network). */
export function herdrSkillPointer(): Record<string, string> {
  return {
    tool: "herdr_skill",
    upstream: HERDR_SKILL_UPSTREAM,
    bundled: HERDR_SKILL_BUNDLED,
    hint: "Call herdr_skill once per session before herdr_call. Tries upstream herdr master; falls back to bundled copy if fetch fails. Set HERDR_SKILL_NETWORK=0 for offline-only.",
  };
}
