import { readFileSync } from "node:fs";
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  encodeHelloMessage,
  encodeHeartbeatMessage,
  encodeStatusReport,
  encodeToolResultMessage,
  encodeToolErrorMessage,
  encodeCancelAckMessage,
  encodeCompactOversizedError,
  toInternalRequest,
  toInternalCancel,
} from "../dist/link/relay-adapter.js";
import { validateRelayMessage } from "../dist/relay/index.js";

const fixture = JSON.parse(
  readFileSync(new URL("./fixtures/relay-adapter-builders.json", import.meta.url), "utf8"),
);

const RUNTIME = fixture.runtime_identity;

/** Substitute fixture sentinels with values JSON cannot represent directly. */
function resolveArgs(args) {
  return args.map((a) => {
    if (a === "__RUNTIME__") return RUNTIME;
    if (a === "__UNDEFINED__") return undefined;
    return a;
  });
}

function wireObject(value) {
  return JSON.parse(JSON.stringify(value));
}

const BUILDERS = {
  encodeHelloMessage,
  encodeHeartbeatMessage,
  encodeStatusReport,
  encodeToolResultMessage,
  encodeToolErrorMessage,
  encodeCancelAckMessage,
};

test("relay adapter builder wire outputs match fixture exactly", () => {
  assert.equal(fixture.builder_cases.length, 7);
  for (const entry of fixture.builder_cases) {
    const builder = BUILDERS[entry.builder];
    assert.ok(builder, `unknown builder ${entry.builder}`);
    const actual = wireObject(builder(...resolveArgs(entry.args)));
    assert.deepEqual(actual, entry.expected, `${entry.name}: exact wire output`);
  }
});

test("relay adapter builder wire outputs validate as canonical messages", () => {
  for (const entry of fixture.builder_cases) {
    const builder = BUILDERS[entry.builder];
    const actual = wireObject(builder(...resolveArgs(entry.args)));
    const result = validateRelayMessage(actual);
    assert.equal(result.ok, true, `${entry.name}: ${JSON.stringify(result)}`);
  }
});

test("inbound tool_request/cancel translate to internal frames", () => {
  assert.equal(fixture.inbound_cases.length, 2);
  for (const entry of fixture.inbound_cases) {
    const result = validateRelayMessage(entry.message);
    assert.equal(result.ok, true, `${entry.name}: inbound validates`);
    const internal =
      entry.message.kind === "tool_request"
        ? toInternalRequest(entry.message)
        : toInternalCancel(entry.message);
    assert.deepEqual(internal, entry.internal, `${entry.name}: internal frame`);
  }
});

test("encodeCompactOversizedError exposes stable fields and valid shape", () => {
  const stable = fixture.compact_oversized.stable_fields;

  // runtime_generation null as supplied
  const withNull = encodeCompactOversizedError("w1", "req_99", null);
  assert.equal(withNull.kind, "tool_error");
  assert.equal(withNull.protocol_version, 1);
  assert.equal(withNull.workstation_id, "w1");
  assert.equal(withNull.request_id, "req_99");
  assert.equal(withNull.code, stable.code);
  assert.equal(withNull.message, stable.message);
  assert.equal(withNull.retryable, stable.retryable);
  assert.equal(withNull.delivery_state, stable.delivery_state);
  assert.equal(withNull.runtime_generation, null);
  assert.equal(typeof withNull.served_at_ms, "number");
  assert.ok(Number.isFinite(withNull.served_at_ms), "served_at_ms is finite");
  assert.ok(withNull.served_at_ms > 0, "served_at_ms is a positive epoch ms");

  // runtime_generation value as supplied
  const withGen = encodeCompactOversizedError("w1", "req_99", "gen-7");
  assert.equal(withGen.runtime_generation, "gen-7");

  // shape is a valid canonical tool_error
  const result = validateRelayMessage(withNull);
  assert.equal(result.ok, true, `compact oversized validates: ${JSON.stringify(result)}`);
});
