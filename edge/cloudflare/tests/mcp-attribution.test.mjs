import test from "node:test";
import assert from "node:assert/strict";
import { McpMinuteAttribution, classifyMcpAttributionOperation } from "../dist/mcp-attribution.js";

function call(name, args = {}) {
  return { jsonrpc: "2.0", id: 1, method: "tools/call", params: { name, arguments: args } };
}

test("mcp attribution classifies only bounded operation names", () => {
  assert.equal(classifyMcpAttributionOperation(call("herdr_inspect")), "tool:herdr_inspect");
  assert.equal(classifyMcpAttributionOperation(call("herdr_call", { method: "continuity.resume" })), "call:continuity");
  assert.equal(classifyMcpAttributionOperation(call("herdr_call", { method: "herdr_mcp.skill.load" })), "call:herdr_mcp.skill");
  assert.equal(classifyMcpAttributionOperation(call("herdr_call", { method: "herdr_mcp.not-a-real-method.user-secret" })), "call:herdr_mcp.other");
  assert.equal(classifyMcpAttributionOperation(call("definitely-not-public", { message: "do not log me" })), "tool:unknown");
  assert.equal(classifyMcpAttributionOperation({ method: "totally-user-controlled" }), "rpc:other");
});

test("mcp attribution emits one compact aggregate when the minute rolls over", () => {
  const events = [];
  const logger = {
    info(event, fields) { events.push({ event, fields }); },
    warn() {},
    error() {},
  };
  const attribution = new McpMinuteAttribution(logger);
  attribution.record(call("herdr_inspect"), 60_001);
  attribution.record(call("herdr_inspect"), 60_100);
  attribution.record(call("herdr_git"), 60_200);
  assert.equal(events.length, 0, "current minute stays in memory instead of logging every request");

  attribution.record(call("herdr_call", { method: "workspace.list" }), 120_001);
  assert.equal(events.length, 1);
  assert.equal(events[0].event, "mcp.minute_attribution");
  assert.deepEqual(events[0].fields, {
    minute_start_ms: 60_000,
    total: 3,
    operations: "tool:herdr_inspect=2,tool:herdr_git=1",
  });

  attribution.flush();
  assert.equal(events.length, 2);
  assert.deepEqual(events[1].fields, {
    minute_start_ms: 120_000,
    total: 1,
    operations: "call:workspace=1",
  });
});
