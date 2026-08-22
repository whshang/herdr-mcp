import { test } from "node:test";
import assert from "node:assert/strict";
import {
  clampHerdrTimeout,
  HERDR_RPC_TIMEOUT_DEFAULT_MS,
  HERDR_RPC_TIMEOUT_MAX_MS,
} from "../dist/timeouts.js";

test("clampHerdrTimeout defaults and caps at 60s", () => {
  assert.equal(clampHerdrTimeout(), HERDR_RPC_TIMEOUT_DEFAULT_MS);
  assert.equal(clampHerdrTimeout(undefined), HERDR_RPC_TIMEOUT_DEFAULT_MS);
  assert.equal(clampHerdrTimeout(5000), 5000);
  assert.equal(clampHerdrTimeout(HERDR_RPC_TIMEOUT_MAX_MS), HERDR_RPC_TIMEOUT_MAX_MS);
  assert.equal(clampHerdrTimeout(999_999), HERDR_RPC_TIMEOUT_MAX_MS);
  assert.equal(clampHerdrTimeout(NaN), HERDR_RPC_TIMEOUT_DEFAULT_MS);
});
