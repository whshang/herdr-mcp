/**
 * limits.ts — edge bounds and knobs.
 *
 * All numeric bounds used by the edge live here so platform limits, capacity
 * decisions and timeouts are reviewable in one file. Values can be overridden
 * for local dev via Env vars (see workstation-do.ts Env); nothing here is a
 * production tuning exercise — it is the dev scaffold default.
 *
 * Cross-checked against Cloudflare platform limits recorded in
 * docs/_wip/HERDR_MCP_SELF_UPGRADE_PLAN.md §11 (DO message 32 MiB, memory
 * 128 MB, default CPU per request 30 s). Edge defaults stay well below those.
 */

/** Early "hello"/routing string guard lengths (log + storage hardening). */
export const MAX_WS_ID_LEN = 64;
export const MAX_HEADER_LEN = 64;
export const MAX_BOOT_ID_LEN = 64;
export const MAX_RESOURCE_TOKEN_LEN = 512;
export const MAX_CAPABILITIES = 32;
export const MAX_CAPABILITY_LEN = 128;

/** Bounded pending-request registry. */
export const DEFAULT_MAX_PENDING_REQUESTS = 256;
/** Bounded completed-request history (idempotent replay window). */
export const DEFAULT_MAX_COMPLETED_RECORDS = 512;
/** How long a completion remains replayable after settle. */
export const DEFAULT_COMPLETED_RECORD_TTL_MS = 600_000; // 10 min

/** Request timeout budget (clamped, mirrors local ≤60 s RPC convention). */
export const DEFAULT_REQUEST_TIMEOUT_MS = 30_000;
export const MIN_REQUEST_TIMEOUT_MS = 1_000;
export const MAX_REQUEST_TIMEOUT_MS = 60_000;

/** Link presence: after this long with no hello/heartbeat the link is stale. */
export const DEFAULT_LINK_STALE_AFTER_MS = 45_000;

/**
 * Persist `last_seen` only as a low-frequency recovery checkpoint. Live
 * presence is derived from the Hibernation WebSocket attachment, so heartbeat
 * traffic does not need to rewrite Durable Object storage every few seconds.
 */
export const HEARTBEAT_PERSIST_THROTTLE_MS = 300_000; // 5 min

/** Frame payload bound — well below the 32 MiB DO limit. */
export const DEFAULT_MAX_FRAME_BYTES = 1_048_576; // 1 MiB

/** Dead-letter / diagnostics caps. */
export const MAX_ARGS_SUMMARY_KEYS = 32;

export interface EdgeLimits {
  maxPendingRequests: number;
  maxCompletedRecords: number;
  completedRecordTtlMs: number;
  requestTimeoutMs: number;
  linkStaleAfterMs: number;
  maxFrameBytes: number;
}

function parsePositiveInt(value: string | undefined, fallback: number): number {
  if (value === undefined) return fallback;
  const n = Number.parseInt(value, 10);
  if (!Number.isFinite(n) || n <= 0) return fallback;
  return n;
}

/**
 * Build limits from Env string overrides (dev convenience). Kept pure so it
 * is unit-testable without a Workers runtime.
 */
export function makeLimits(env?: {
  EDGE_MAX_FRAME_BYTES?: string;
  DEFAULT_REQUEST_TIMEOUT_MS?: string;
  LINK_STALE_AFTER_MS?: string;
}): EdgeLimits {
  const requestTimeoutMs = Math.min(
    Math.max(
      parsePositiveInt(env?.DEFAULT_REQUEST_TIMEOUT_MS, DEFAULT_REQUEST_TIMEOUT_MS),
      MIN_REQUEST_TIMEOUT_MS,
    ),
    MAX_REQUEST_TIMEOUT_MS,
  );
  return {
    maxPendingRequests: DEFAULT_MAX_PENDING_REQUESTS,
    maxCompletedRecords: DEFAULT_MAX_COMPLETED_RECORDS,
    completedRecordTtlMs: DEFAULT_COMPLETED_RECORD_TTL_MS,
    requestTimeoutMs,
    linkStaleAfterMs: parsePositiveInt(env?.LINK_STALE_AFTER_MS, DEFAULT_LINK_STALE_AFTER_MS),
    maxFrameBytes: parsePositiveInt(env?.EDGE_MAX_FRAME_BYTES, DEFAULT_MAX_FRAME_BYTES),
  };
}

/** Categorize an operation for delivery retry semantics. */
export type OpClass = "read" | "mutating" | "unknown";

/**
 * Read-only ops may be retried after connection ambiguity; everything else is
 * treated conservatively (mutation-safe). This is a delivery-semantics
 * heuristic only — it is NOT a tool catalog. Frozen catalogs live under
 * edge/cloudflare/src/contracts/. Keep this set aligned with operations whose
 * handlers are provably read-only; unknown operations stay conservative.
 */
const READ_OPS: ReadonlySet<string> = new Set([
  "herdr_inspect",
  "herdr_since",
  "herdr_methods",
  "herdr_skill",
  "herdr_fs_image",
  "herdr_fs_list",
  "herdr_fs_grep",
  "herdr_fs_read",
  "herdr_git",
  "herdr_exec_read",
]);

export function classifyOp(op: string): OpClass {
  if (READ_OPS.has(op)) return "read";
  if (op === "") return "unknown";
  // Exec/prompt/git can mutate; unknown tool names are treated as mutating.
  return "mutating";
}