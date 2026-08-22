/** Hard cap for any herdr Unix-socket RPC (align with Cloudflare / connector yield budgets). */
export const HERDR_RPC_TIMEOUT_DEFAULT_MS = 30_000;
export const HERDR_RPC_TIMEOUT_MAX_MS = 60_000;

/** session.snapshot is heavy; never block bootstrap/tools longer than this. */
export const HERDR_SNAPSHOT_TIMEOUT_MS = 8_000;

export function clampHerdrTimeout(ms?: number): number {
  const fallback = HERDR_RPC_TIMEOUT_DEFAULT_MS;
  if (ms === undefined || ms === null) return fallback;
  const n = Number(ms);
  if (!Number.isFinite(n) || n < 1) return fallback;
  return Math.min(Math.floor(n), HERDR_RPC_TIMEOUT_MAX_MS);
}
