import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../extension/content/webmcp/json-bridge-core.js", import.meta.url), "utf8");
const context = vm.createContext({ globalThis: {} });
vm.runInContext(source, context);
const CORE = context.globalThis.H2W_JSON_BRIDGE_CORE;

test("pending bridge recovery requires assistant tool JSON as the last conversation entry", () => {
  const entries = [
    { role: "user", text: `${CORE.MARKER}\nUSER_TASK:\ninspect` },
    { role: "assistant", text: '{"tool":"herdr_inspect","args":{}}' },
  ];
  assert.equal(CORE.hasPendingToolReply(entries), true);
  assert.equal(CORE.hasPendingToolReply([...entries, { role: "user", text: "TOOL_RESULT:\n[]" }]), false);
  assert.equal(CORE.hasPendingToolReply([...entries, { role: "assistant", text: "Done." }]), false);
});

test("pending bridge recovery accepts tool-result context and rejects unrelated JSON", () => {
  assert.equal(CORE.hasPendingToolReply([
    { role: "user", text: "TOOL_RESULT:\n[]" },
    { role: "assistant", text: '{"tool":"herdr_skill","args":{}}' },
  ]), true);
  assert.equal(CORE.hasPendingToolReply([
    { role: "user", text: "Show me JSON." },
    { role: "assistant", text: '{"tool":"not-a-bridge-request","args":{}}' },
  ]), false);
});

test("tool reply state distinguishes complete, incomplete, malformed and normal replies", () => {
  assert.equal(CORE.toolReplyState('{"tool":"herdr_inspect","args":{}}'), "complete");
  assert.equal(CORE.toolReplyState('{  "tool" : "herdr_inspect", "args": {} }'), "complete");
  assert.equal(CORE.toolReplyState('{"tool":"herdr_fs_read","args":{"path":"/tmp/x"'), "incomplete");
  assert.equal(CORE.toolReplyState('{"tool":oops}'), "malformed");
  assert.equal(CORE.toolReplyState('{"status":"still-json"}'), "malformed");
  assert.equal(CORE.toolReplyState("Finished normally."), "none");
  assert.equal(CORE.toolReplyState('{"tool":"a","args":{}}\n{"tool":"b"'), "incomplete");
});

test("pending bridge recovery treats incomplete tool JSON as unfinished", () => {
  assert.equal(CORE.hasPendingToolReply([
    { role: "user", text: `${CORE.MARKER}\nUSER_TASK:\ninspect` },
    { role: "assistant", text: '{"tool":"herdr_fs_read","args":{"path":"/tmp/x"' },
  ]), true);
});

test("bridge context treats a final JSON object without a tool as unfinished", () => {
  assert.equal(CORE.hasPendingToolReply([
    { role: "user", text: `${CORE.MARKER}\nUSER_TASK:\ninspect` },
    { role: "assistant", text: '{"status":"still-working"}' },
  ]), true);
});
