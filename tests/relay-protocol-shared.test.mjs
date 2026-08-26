import { readFileSync } from "node:fs";
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  CORRELATED_KINDS,
  MESSAGE_KINDS,
  RELAY_PROTOCOL_VERSION,
  RELAY_PROTOCOL_VERSION_STRING,
  validateRelayMessage,
} from "../dist/relay/index.js";

const fixture = JSON.parse(
  readFileSync(new URL("./fixtures/relay-protocol-shared.json", import.meta.url), "utf8"),
);

test("relay protocol shared fixture matches runtime TypeScript constants", () => {
  assert.equal(RELAY_PROTOCOL_VERSION, fixture.protocol_version.numeric);
  assert.equal(RELAY_PROTOCOL_VERSION_STRING, fixture.protocol_version.string);
  assert.deepEqual([...MESSAGE_KINDS], fixture.message_kinds);
  assert.deepEqual([...CORRELATED_KINDS], fixture.correlated_kinds);
});

test("relay protocol shared representative messages pass TypeScript validation", () => {
  assert.equal(fixture.representative_messages.length, 15);
  for (const entry of fixture.representative_messages) {
    const result = validateRelayMessage(entry.value);
    assert.equal(result.ok, true, `${entry.name}: ${JSON.stringify(result)}`);
  }
});
