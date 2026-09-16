// Node Link canary: exact Relay v1 inbound -> internal frame mapping.
//
// The protocol/wire parity suites for this frozen fixture were retired because
// Rust (relay/wire.rs) and Edge own those invariants. The still-supported Node
// Link canary keeps only the adapter translation oracle that has no other
// direct Node coverage: `toInternalRequest` / `toInternalCancel`.
import { readFileSync } from "node:fs";
import { test } from "node:test";
import assert from "node:assert/strict";

import { toInternalRequest, toInternalCancel } from "../dist/link/relay-adapter.js";

const fixture = JSON.parse(
  readFileSync(new URL("./fixtures/relay-adapter-builders.json", import.meta.url), "utf8"),
);

const TRANSLATE = {
  tool_request: toInternalRequest,
  cancel: toInternalCancel,
};

test("inbound tool_request/cancel translate to the fixture internal frames exactly", () => {
  assert.equal(fixture.inbound_cases.length, 2);
  for (const entry of fixture.inbound_cases) {
    const translate = TRANSLATE[entry.message.kind];
    assert.ok(translate, `${entry.name}: translator for ${entry.message.kind}`);
    assert.deepEqual(translate(entry.message), entry.internal, `${entry.name}: internal frame`);
  }
});