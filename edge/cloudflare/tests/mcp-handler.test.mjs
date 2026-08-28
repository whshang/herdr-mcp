import { test } from "node:test";
import assert from "node:assert/strict";

import { EPOCH2_CONTRACT } from "../dist/contracts/epoch2.js";
import { makeLimits } from "../dist/limits.js";
import { handleMcp } from "../dist/mcp-handler.js";

function deps(over = {}) {
  const calls = [];
  return {
    calls,
    value: {
      limits: makeLimits(),
      logger: { warn() {} },
      getStub: (workstationId) => ({ workstationId }),
      forward: async (_stub, body) => {
        calls.push(JSON.parse(body));
        if (over.forward) return over.forward(_stub, body);
        return new Response(JSON.stringify({
          status: "ok",
          completion: { status: "ok", result: { served: true } },
        }), { headers: { "content-type": "application/json" } });
      },
      now: () => 1000,
    },
  };
}

const req = (id, method, params = {}) => ({ jsonrpc: "2.0", id, method, params });

test("initialize advertises legacy wire protocol and frozen epoch-2 identity", async () => {
  const d = deps();
  const r = await handleMcp(req(1, "initialize", {}), "w1", d.value);
  assert.equal(r.status, 200);
  assert.equal(r.body.id, 1);
  assert.equal(r.body.result.protocolVersion, "2025-11-25");
  assert.equal(r.body.result.capabilities.tools.listChanged, false);
  assert.equal(r.body.result._meta.herdr.contract_epoch, 2);
  assert.equal(r.body.result._meta.herdr.contract_hash, EPOCH2_CONTRACT.contract_hash);
});

test("server/discover advertises supported versions without OAuth claims", async () => {
  const d = deps();
  const r = await handleMcp(req("d", "server/discover", {}), "w1", d.value);
  assert.equal(r.body.id, "d");
  assert.deepEqual(r.body.result.supportedVersions, [
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
  ]);
  assert.equal(r.body.result.capabilities.tools.listChanged, false);
  assert.equal(Object.hasOwn(r.body.result, "authorizationServers"), false);
});

test("server/discover for openai-mcp keeps SDK wire first and adds 2026-07-28", async () => {
  const d = deps();
  d.value.client = { userAgent: "openai-mcp/1.0.0", oauthClientId: null };
  const r = await handleMcp(req("d", "server/discover", {}), "w1", d.value);
  assert.equal(r.body.result.supportedVersions[0], "2025-11-25");
  assert.equal(r.body.result.supportedVersions.includes("2026-07-28"), true);
});

test("initialize negotiates unknown protocol versions down to SDK wire", async () => {
  const d = deps();
  const r = await handleMcp(
    req(11, "initialize", { protocolVersion: "2026-07-28", capabilities: {}, clientInfo: { name: "ChatGPT" } }),
    "w1",
    d.value,
  );
  assert.equal(r.body.result.protocolVersion, "2025-11-25");
});

test("tools/list is exactly the frozen 18-tool epoch-2 catalog", async () => {
  const d = deps();
  const r = await handleMcp(req(2, "tools/list", {}), "w1", d.value);
  assert.equal(r.body.result.tools.length, 18);
  assert.deepEqual(r.body.result.tools, EPOCH2_CONTRACT.tools);
  assert.equal(r.body.result.tools.some((tool) => tool.name === "herdr_skill"), true);
  assert.equal(r.body.result._meta.herdr.contract_hash, EPOCH2_CONTRACT.contract_hash);
});

test("tools/call forwards only frozen tools with epoch/hash and preserves id", async () => {
  const d = deps();
  const r = await handleMcp(req(7, "tools/call", { name: "herdr_inspect", arguments: {} }), "w1", d.value);
  assert.equal(r.body.id, 7);
  assert.equal(r.body.result.isError, undefined);
  assert.equal(r.body.result.structuredContent.served, true);
  assert.equal(d.calls.length, 1);
  assert.equal(d.calls[0].op, "herdr_inspect");
  assert.equal(d.calls[0].contractEpoch, 2);
  assert.equal(d.calls[0].contractHash, EPOCH2_CONTRACT.contract_hash);
  assert.equal(d.calls[0].deadlineMs, 31_000);
});

test("tools/call preserves an existing MCP CallToolResult including image content", async () => {
  const callToolResult = {
    content: [
      { type: "text", text: "image metadata" },
      { type: "image", data: "AA==", mimeType: "image/png" },
    ],
    structuredContent: { width: 1, height: 1 },
  };
  const d = deps({
    forward: async () => new Response(JSON.stringify({
      status: "ok",
      completion: { status: "ok", result: callToolResult },
    })),
  });
  const r = await handleMcp(
    req(71, "tools/call", { name: "herdr_fs_image", arguments: { path: "/tmp/x.png" } }),
    "w1",
    d.value,
  );
  assert.equal(r.body.id, 71);
  assert.deepEqual(r.body.result, callToolResult);
});

test("herdr_skill is public while unknown tools are rejected before forwarding", async () => {
  const d = deps();
  const skill = await handleMcp(req(8, "tools/call", { name: "herdr_skill", arguments: {} }), "w1", d.value);
  assert.equal(skill.body.error, undefined);
  assert.equal(d.calls.length, 1);
  assert.equal(d.calls[0].op, "herdr_skill");
  const unknown = await handleMcp(req(81, "tools/call", { name: "does_not_exist", arguments: {} }), "w1", d.value);
  assert.equal(unknown.body.error.code, -32602);
  assert.equal(d.calls.length, 1);
});

test("tools/call maps relay delivery errors to MCP isError tool results", async () => {
  const d = deps({
    forward: async () => new Response(JSON.stringify({
      status: "error",
      error: {
        code: "workstation_offline",
        retryable: true,
        delivery_state: "not_delivered",
        message: "offline",
      },
    })),
  });
  const r = await handleMcp(req(9, "tools/call", { name: "herdr_inspect", arguments: {} }), "w1", d.value);
  assert.equal(r.body.error, undefined);
  assert.equal(r.body.result.isError, true);
  assert.equal(r.body.result.structuredContent.code, "workstation_offline");
  assert.equal(r.body.result.structuredContent.retryable, true);
  assert.equal(r.body.result.structuredContent.delivery_state, "not_delivered");
});

test("JSON-RPC request validation and method errors preserve ids", async () => {
  const d = deps();
  const invalid = await handleMcp([], "w1", d.value);
  assert.equal(invalid.body.error.code, -32600);
  assert.equal(invalid.body.id, null);

  const badParams = await handleMcp(req(10, "tools/call", { name: "herdr_inspect", arguments: [] }), "w1", d.value);
  assert.equal(badParams.body.error.code, -32602);
  assert.equal(badParams.body.id, 10);

  const missing = await handleMcp(req("x", "unknown/method", {}), "w1", d.value);
  assert.equal(missing.body.error.code, -32601);
  assert.equal(missing.body.id, "x");

  const notification = await handleMcp({ jsonrpc: "2.0", method: "notifications/initialized", params: {} }, "w1", d.value);
  assert.equal(notification.status, 204);
  assert.equal(notification.body, null);
});
