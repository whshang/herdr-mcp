/**
 * Exponential backoff with full-jitter for the link reconnect loop.
 *
 * Pure and injectable: pass `rng` (0..1) and `clock` in tests so reconnect
 * scheduling is deterministic. Formula (Truncated Exponential Backoff with
 * full jitter):
 *
 *   cap(attempt) = min(maxMs, baseMs * factor ** attempt)
 *   delay       = floor(rng() * cap(attempt))
 *
 * `next()` advances the internal attempt counter; `reset()` is called after a
 * successful handshake so the next failure starts from attempt 0.
 */

import type { BackoffOptions, RngFn } from "./types.js";

/** @internal */
export function clampNonnegative(value: number | undefined, fallback: number): number {
  const n = Number(value);
  if (!Number.isFinite(n) || n < 0) return fallback;
  return Math.floor(n);
}

export class ExponentialBackoff {
  readonly baseMs: number;
  readonly maxMs: number;
  readonly factor: number;
  readonly jitter: number;
  readonly rng: RngFn;

  private attemptCount = 0;

  constructor(opts?: BackoffOptions) {
    this.baseMs = Math.max(1, clampNonnegative(opts?.baseMs, 1_000));
    this.maxMs = Math.max(this.baseMs, clampNonnegative(opts?.maxMs, 60_000));
    this.factor = Math.max(1, clampNonnegative(opts?.factor, 2));
    this.jitter = Math.min(1, clampNonnegative(opts?.jitter, 1));
    this.rng = opts?.rng ?? Math.random;
  }

  get attempt(): number {
    return this.attemptCount;
  }

  /** Reset the backoff counter (called after a successful handshake). */
  reset(): void {
    this.attemptCount = 0;
  }

  /**
   * Delay for the NEXT retry, advancing the counter. Attempt 0 is the first
   * reconnect (so the first reconnect delay is `cap(0)`).
   */
  next(): number {
    const delay = this.peek(this.attemptCount);
    this.attemptCount += 1;
    return delay;
  }

  /** Delay that would be returned for `attempt` without advancing state. */
  peek(attempt: number): number {
    const cap = Math.min(this.maxMs, this.baseMs * this.factor ** Math.max(0, attempt));
    const raw = cap * (1 - this.jitter) + cap * this.jitter * this.rng();
    return Math.floor(raw);
  }
}