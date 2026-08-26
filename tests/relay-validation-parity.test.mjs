import { readFileSync } from "node:fs";
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  parseRelayFrame,
  validateRelayMessage,
} from "../dist/relay/index.js";

const fixture = JSON.parse(
  readFileSync(new URL("./fixtures/relay-validation-parity.json", import.meta.url), "utf8"),
);

function nodeOptions(raw = {}) {
  const out = {};
  if (raw.max_frame_bytes !== undefined) out.maxFrameBytes = raw.max_frame_bytes;
  if (raw.max_args_bytes !== undefined) out.maxArgsBytes = raw.max_args_bytes;
  if (raw.max_result_bytes !== undefined) out.maxResultBytes = raw.max_result_bytes;
  if (raw.max_details_bytes !== undefined) out.maxDetailsBytes = raw.max_details_bytes;
  if (raw.max_trace_bytes !== undefined) out.maxTraceBytes = raw.max_trace_bytes;
  if (raw.strict_unknown_fields !== undefined) out.strictUnknownFields = raw.strict_unknown_fields;
  return out;
}

function assertOutcome(actual, expected, name) {
  assert.equal(actual.ok, expected.ok, `${name}: ok`);
  if (!expected.ok) {
    assert.equal(actual.code, expected.code, `${name}: code`);
  }
}

test("relay validation shared frame fixtures match TypeScript oracle", () => {
  for (const entry of fixture.frame_cases) {
    assertOutcome(parseRelayFrame(entry.raw, nodeOptions(entry.options)), entry, entry.name);
  }
});

test("relay validation shared message fixtures match TypeScript oracle", () => {
  for (const entry of fixture.message_cases) {
    assertOutcome(validateRelayMessage(entry.value, nodeOptions(entry.options)), entry, entry.name);
  }
});
