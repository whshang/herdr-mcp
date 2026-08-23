import { test } from "node:test";
import assert from "node:assert/strict";
import { ExponentialBackoff, clampNonnegative } from "../dist/link/backoff.js";

test("clampNonnegative falls back and floors", () => {
  assert.equal(clampNonnegative(undefined, 42), 42);
  assert.equal(clampNonnegative(NaN, 7), 7);
  assert.equal(clampNonnegative(-3, 7), 7);
  assert.equal(clampNonnegative(9.9, 7), 9);
});

test("full jitter: delay is floor(rng * cap)", () => {
  const b = new ExponentialBackoff({ baseMs: 1000, maxMs: 60_000, factor: 2, jitter: 1, rng: () => 0.5 });
  // cap(0) = 1000 → 500
  assert.equal(b.peek(0), 500);
  // cap(1) = 2000 → 1000
  assert.equal(b.peek(1), 1000);
  // cap(10) = min(60000, 512000) = 60000 → 30000
  assert.equal(b.peek(10), 30_000);
});

test("jitter 0 means deterministic exact cap", () => {
  const b = new ExponentialBackoff({ baseMs: 1000, maxMs: 60_000, factor: 2, jitter: 0, rng: () => 0.25 });
  assert.equal(b.peek(0), 1000);
  assert.equal(b.peek(1), 2000);
  assert.equal(b.peek(5), 32_000);
});

test("rng=1 always hits the cap", () => {
  const b = new ExponentialBackoff({ baseMs: 1000, maxMs: 60_000, factor: 2, jitter: 1, rng: () => 1 });
  assert.equal(b.peek(0), 1000);
  assert.equal(b.peek(6), 60_000);
});

test("next() advances the attempt counter; reset() restores it", () => {
  const b = new ExponentialBackoff({ baseMs: 1000, factor: 2, rng: () => 0.5 });
  assert.equal(b.attempt, 0);
  assert.equal(b.next(), 500); // cap(0)=1000 * .5
  assert.equal(b.attempt, 1);
  assert.equal(b.next(), 1000); // cap(1)=2000 * .5
  assert.equal(b.attempt, 2);
  b.reset();
  assert.equal(b.attempt, 0);
  assert.equal(b.next(), 500);
});

test("defaults: base 1000 / cap 60000 / full jitter", () => {
  const b = new ExponentialBackoff({ rng: () => 0.5 });
  assert.equal(b.baseMs, 1_000);
  assert.equal(b.maxMs, 60_000);
  assert.equal(b.peek(0), 500);
  assert.equal(b.peek(5), 16_000); // cap(5)=32000, *0.5
});

test("option sanitization: negative/too big inputs are clamped", () => {
  const b = new ExponentialBackoff({ baseMs: -5, maxMs: 10, factor: 0, jitter: 3, rng: () => 1 });
  assert.equal(b.baseMs, 1000); // negative base falls back to the 1000 default
  assert.equal(b.maxMs, 1000); // max is pushed up to at least baseMs
  assert.equal(b.factor, 1); // factor 0 is floored at 1
  assert.equal(b.jitter, 1); // jitter > 1 is clamped to 1
  assert.equal(b.peek(0), 1000); // cap(0) = 1000 with rng=1
});