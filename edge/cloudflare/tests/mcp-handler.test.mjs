import { test } from "node:test";
import assert from "node:assert/strict";

import { EPOCH2_CONTRACT } from "../dist/contracts/epoch2.js";
import { EPOCH3_CONTRACT } from "../dist/contracts/epoch3.js";
import { makeLimits } from "../dist/limits.js";
import { handleMcp } from "../dist/mcp-handler.js";

function deps(over = {}) {
  const calls = [];
  const targets = [];
  return {
    calls,
    targets,
    value: {
      limits: makeLimits(),
      logger: { warn() {} },
      getStub: (workstationId) => {
        targets.push(workstationId);
        return { workstationId };
      },
      listDevices: async () => over.devices ?? [],
      createPairing: over.createPairing,
      resolveDevice: over.resolveDevice,
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
const DEVICE_A = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYZ";

test("initialize advertises legacy wire protocol and device-aware public identity", async () => {
  const d = deps();
  const r = await handleMcp(req(1, "initialize", {}), "w1", d.value);
  assert.equal(r.status, 200);
  assert.equal(r.body.id, 1);
  assert.equal(r.body.result.protocolVersion, "2025-11-25");
  assert.equal(r.body.result.capabilities.tools.listChanged, false);
  assert.equal(r.body.result._meta.herdr.contract_epoch, 3);
  assert.equal(r.body.result._meta.herdr.contract_hash, EPOCH3_CONTRACT.contract_hash);
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

test("tools/list exposes runtime tools plus edge-local herdr_devices", async () => {
  const d = deps();
  const r = await handleMcp(req(2, "tools/list", {}), "w1", d.value);
  assert.equal(r.body.result.tools.length, 19);
  assert.deepEqual(r.body.result.tools, EPOCH3_CONTRACT.tools);
  assert.equal(r.body.result.tools.some((tool) => tool.name === "herdr_skill"), true);
  assert.equal(r.body.result.tools.some((tool) => tool.name === "herdr_devices"), true);
  assert.equal(r.body.result._meta.herdr.contract_hash, EPOCH3_CONTRACT.contract_hash);
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

test("read-only call retries once after generation supersede proved not delivered", async () => {
  let forwards = 0;
  const d = deps({
    forward: async () => {
      forwards += 1;
      if (forwards === 1) {
        return new Response(JSON.stringify({
          status: "ok",
          completion: {
            status: "error",
            error: {
              ok: false,
              code: "runtime_generation_superseded_before_dispatch",
              retryable: true,
              delivery_state: "not_delivered",
              message: "runtime/current changed before dispatch",
            },
          },
        }));
      }
      return new Response(JSON.stringify({
        status: "ok",
        completion: { status: "ok", result: { served: true, generation: "new" } },
      }));
    },
  });

  const r = await handleMcp(req(701, "tools/call", { name: "herdr_inspect", arguments: {} }), "w1", d.value);
  assert.equal(r.body.result.isError, undefined);
  assert.equal(r.body.result.structuredContent.served, true);
  assert.equal(r.body.result.structuredContent.generation, "new");
  assert.equal(d.calls.length, 2);
  assert.notEqual(d.calls[0].requestId, d.calls[1].requestId);
  assert.equal(d.calls[0].op, "herdr_inspect");
  assert.equal(d.calls[1].op, "herdr_inspect");
  assert.equal(d.calls[0].deadlineMs, d.calls[1].deadlineMs);
});

test("mutating call never retries generation supersede even when not delivered", async () => {
  const d = deps({
    forward: async () => new Response(JSON.stringify({
      status: "ok",
      completion: {
        status: "error",
        error: {
          ok: false,
          code: "runtime_generation_superseded_before_dispatch",
          retryable: true,
          delivery_state: "not_delivered",
          message: "runtime/current changed before dispatch",
        },
      },
    })),
  });

  const r = await handleMcp(
    req(702, "tools/call", { name: "herdr_prompt", arguments: { target: "w1:p1", text: "test" } }),
    "w1",
    d.value,
  );
  assert.equal(r.body.result.isError, true);
  assert.equal(r.body.result.structuredContent.code, "runtime_generation_superseded_before_dispatch");
  assert.equal(r.body.result.structuredContent.delivery_state, "not_delivered");
  assert.equal(d.calls.length, 1);
});

test("herdr_devices executes at Edge and never forwards to a workstation", async () => {
  const devices = [{ device_id: DEVICE_A, name: "macbook" }];
  const d = deps({ devices });
  const r = await handleMcp(req(72, "tools/call", { name: "herdr_devices", arguments: {} }), "legacy-default", d.value);
  assert.equal(r.body.result.isError, undefined);
  assert.deepEqual(r.body.result.structuredContent, { ok: true, devices });
  assert.equal(d.calls.length, 0);
});

test("herdr_call herdr_mcp.device.pair executes at Edge and creates pairing without workstation", async () => {
  let pairingInput = null;
  const d = deps({
    createPairing: async (input) => {
      pairingInput = input;
      return {
        ok: true,
        pairing_id: "pair_" + "11".repeat(32),
        code: "654321",
        expires_at_ms: 600_000,
        pairing_address: "https://edge.example/pair#pair_" + "11".repeat(32),
        worker_origin: "https://edge.example",
      };
    },
  });

  // Call with stringified JSON params
  const r = await handleMcp(
    req(720, "tools/call", {
      name: "herdr_call",
      arguments: {
        method: "herdr_mcp.device.pair",
        params: JSON.stringify({ ttl_seconds: 300, name: "new-workstation" }),
      },
    }),
    "legacy-default",
    d.value,
  );
  assert.equal(r.body.id, 720);
  assert.equal(r.body.result.isError, undefined);
  assert.equal(r.body.result.structuredContent.ok, true);
  assert.equal(r.body.result.structuredContent.code, "654321");
  assert.match(r.body.result.structuredContent.pairing_id, /^pair_[0-9a-f]{64}$/);
  assert.equal(r.body.result.structuredContent.pairing_address, "https://edge.example/pair#pair_" + "11".repeat(32));
  assert.ok(r.body.result.structuredContent.instructions.includes("herdr-mcp worker connect"));
  assert.deepEqual(pairingInput, { ttl_seconds: 300, name: "new-workstation" });
  assert.equal(d.calls.length, 0, "must never forward to a workstation");
  assert.equal(d.targets.length, 0);
});

test("herdr_call herdr_mcp.device.pair handles object params and validation errors", async () => {
  let pairingInput = null;
  const d = deps({
    createPairing: async (input) => {
      pairingInput = input;
      return {
        ok: true,
        pairing_id: "pair_" + "22".repeat(32),
        code: "123456",
        expires_at_ms: 600_000,
        pairing_address: "https://edge.example/pair#pair_" + "22".repeat(32),
      };
    },
  });

  // Call with object params
  const r1 = await handleMcp(
    req(721, "tools/call", {
      name: "herdr_call",
      arguments: {
        method: "herdr_mcp.device.pair",
        params: { ttl_seconds: 600 },
      },
    }),
    "legacy-default",
    d.value,
  );
  assert.equal(r1.body.result.structuredContent.ok, true);
  assert.deepEqual(pairingInput, { ttl_seconds: 600, name: undefined });

  // Call with invalid JSON params string
  const rBadJson = await handleMcp(
    req(722, "tools/call", {
      name: "herdr_call",
      arguments: {
        method: "herdr_mcp.device.pair",
        params: "{not-json",
      },
    }),
    "legacy-default",
    d.value,
  );
  assert.equal(rBadJson.body.result.isError, true);
  assert.equal(rBadJson.body.result.structuredContent.code, "invalid_params");

  // Call with invalid name
  const rBadName = await handleMcp(
    req(723, "tools/call", {
      name: "herdr_call",
      arguments: {
        method: "herdr_mcp.device.pair",
        params: { name: "   " },
      },
    }),
    "legacy-default",
    d.value,
  );
  assert.equal(rBadName.body.result.isError, true);
  assert.equal(rBadName.body.result.structuredContent.code, "invalid_device_name");

  // Unavailable pairing dependency
  const dNoPair = deps({});
  const rNoPair = await handleMcp(
    req(724, "tools/call", {
      name: "herdr_call",
      arguments: { method: "herdr_mcp.device.pair" },
    }),
    "legacy-default",
    dNoPair.value,
  );
  assert.equal(rNoPair.body.result.isError, true);
  assert.equal(rNoPair.body.result.structuredContent.code, "pairing_unavailable");

  // Rejects explicit top-level device selector
  const rDevice = await handleMcp(
    req(725, "tools/call", {
      name: "herdr_call",
      arguments: {
        method: "herdr_mcp.device.pair",
        device: DEVICE_A,
      },
    }),
    "legacy-default",
    d.value,
  );
  assert.equal(rDevice.body.result.isError, true);
  assert.equal(rDevice.body.result.structuredContent.code, "device_selector_not_allowed");

  // Rejects device refs in params
  const rDeviceRef = await handleMcp(
    req(726, "tools/call", {
      name: "herdr_call",
      arguments: {
        method: "herdr_mcp.device.pair",
        params: { pane_id: `herdr_ref_${DEVICE_A}_pane1` },
      },
    }),
    "legacy-default",
    d.value,
  );
  assert.equal(rDeviceRef.body.result.isError, true);
  assert.equal(rDeviceRef.body.result.structuredContent.code, "device_ref_not_allowed");

  // Rejects array or scalar params
  for (const badParams of [[1, 2, 3], 42, true]) {
    const rBadType = await handleMcp(
      req(727, "tools/call", {
        name: "herdr_call",
        arguments: {
          method: "herdr_mcp.device.pair",
          params: badParams,
        },
      }),
      "legacy-default",
      d.value,
    );
    assert.equal(rBadType.body.result.isError, true);
    assert.equal(rBadType.body.result.structuredContent.code, "invalid_params");
  }

  // Rejects invalid ttl_seconds (too low, too high, float, string)
  for (const badTtl of [59, 601, 0, -10, 300.5, "300"]) {
    const rBadTtl = await handleMcp(
      req(728, "tools/call", {
        name: "herdr_call",
        arguments: {
          method: "herdr_mcp.device.pair",
          params: { ttl_seconds: badTtl },
        },
      }),
      "legacy-default",
      d.value,
    );
    assert.equal(rBadTtl.body.result.isError, true);
    assert.equal(rBadTtl.body.result.structuredContent.code, "invalid_pairing_ttl");
  }

  // Rejects invalid name types and lengths (>128 chars)
  for (const badName of [123, true, "", "a".repeat(129)]) {
    const rBadNameType = await handleMcp(
      req(729, "tools/call", {
        name: "herdr_call",
        arguments: {
          method: "herdr_mcp.device.pair",
          params: { name: badName },
        },
      }),
      "legacy-default",
      d.value,
    );
    assert.equal(rBadNameType.body.result.isError, true);
    assert.equal(rBadNameType.body.result.structuredContent.code, "invalid_device_name");
  }

  // Rejects unknown keys in params (e.g. camelCase ttlSeconds typo)
  const rUnknownKey = await handleMcp(
    req(730, "tools/call", {
      name: "herdr_call",
      arguments: {
        method: "herdr_mcp.device.pair",
        params: { ttlSeconds: 300 },
      },
    }),
    "legacy-default",
    d.value,
  );
  assert.equal(rUnknownKey.body.result.isError, true);
  assert.equal(rUnknownKey.body.result.structuredContent.code, "invalid_params");
});

test("explicit device routing selects one workstation and strips Edge-only device metadata", async () => {
  let routeCalls = 0;
  const d = deps({
    resolveDevice: async (selector) => {
      routeCalls += 1;
      assert.equal(selector, DEVICE_A);
      return {
        ok: true,
        device_id: DEVICE_A,
        workstation_id: "prod-real-runtime",
        routing_reason: "explicit_device",
      };
    },
  });
  const r = await handleMcp(
    req(73, "tools/call", { name: "herdr_inspect", arguments: { device: DEVICE_A } }),
    "legacy-default",
    d.value,
  );
  assert.equal(r.body.result.isError, undefined);
  assert.equal(routeCalls, 1);
  assert.deepEqual(d.targets, ["prod-real-runtime"]);
  assert.equal(Object.hasOwn(d.calls[0].args, "device"), false);
  assert.equal(d.calls[0].contractEpoch, EPOCH2_CONTRACT.contract_epoch);
  assert.equal(d.calls[0].contractHash, EPOCH2_CONTRACT.contract_hash);
});

test("ambiguous device routing fails closed before workstation delivery", async () => {
  const d = deps({ resolveDevice: async () => ({ ok: false, code: "device_ambiguous" }) });
  const r = await handleMcp(
    req(74, "tools/call", { name: "herdr_prompt", arguments: { target: "w1:p1", text: "test" } }),
    "legacy-default",
    d.value,
  );
  assert.equal(r.body.result.isError, true);
  assert.equal(r.body.result.structuredContent.code, "device_ambiguous");
  assert.equal(d.targets.length, 0);
  assert.equal(d.calls.length, 0);
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
        retry_after_ms: 5000,
        recovery: {
          action: "retry_read_only_probe",
          probe_tool: "herdr_inspect",
          max_attempts: 3,
          backoff_ms: [5000, 10000, 20000],
          mutation_replay: "only_after_not_delivered_or_verified_not_applied",
        },
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
  assert.equal(r.body.result.structuredContent.retry_after_ms, 5000);
  assert.deepEqual(r.body.result.structuredContent.recovery, {
    action: "retry_read_only_probe",
    probe_tool: "herdr_inspect",
    max_attempts: 3,
    backoff_ms: [5000, 10000, 20000],
    mutation_replay: "only_after_not_delivered_or_verified_not_applied",
  });
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
