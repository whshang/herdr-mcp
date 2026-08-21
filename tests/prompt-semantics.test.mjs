/**
 * Unit tests for prompt / status-wait error classification (no live herdr).
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  isAgentStatusWaitTimeout,
  isTrueTransportFailure,
  buildStateObservation,
} from "../dist/prompt-semantics.js";

test("status-wait message is not a true transport failure", () => {
  const msg = "timeout: timed out waiting for agent status";
  assert.equal(isAgentStatusWaitTimeout(msg), true);
  assert.equal(isTrueTransportFailure("timeout", msg), false);
});

test("connect timeout remains transport", () => {
  assert.equal(isTrueTransportFailure("timeout", "connect /tmp/herdr.sock"), true);
  assert.equal(isTrueTransportFailure("connection_refused", "ECONNREFUSED"), true);
});

test("state_observation: without wait, unchanged → unknown", () => {
  const before = { agent_status: "idle", state_change_seq: 1 };
  const after = { agent_status: "idle", state_change_seq: 1 };
  const o = buildStateObservation({ before, after, waited: false });
  assert.equal(o.state_observation.changed, "unknown");
  assert.equal(o.state_changed, false);
});

test("state_observation: status moved → true", () => {
  const before = { agent_status: "idle", state_change_seq: 1 };
  const after = { agent_status: "working", state_change_seq: 2 };
  const o = buildStateObservation({ before, after, waited: false });
  assert.equal(o.state_observation.changed, true);
  assert.equal(o.state_changed, true);
  assert.equal(o.state_observation.fresh, true);
});

test("state_observation: with wait, unchanged → false", () => {
  const before = { agent_status: "idle", state_change_seq: 1 };
  const after = { agent_status: "idle", state_change_seq: 1 };
  const o = buildStateObservation({ before, after, waited: true });
  assert.equal(o.state_observation.changed, false);
});
