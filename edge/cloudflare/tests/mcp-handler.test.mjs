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
      revokeDevice: over.revokeDevice,
      approveConnector: over.approveConnector,
      revokeConnector: over.revokeConnector,
      resolveDevice: over.resolveDevice,
      client: over.client,
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
  const d = deps({
    client: {
      webchatControlGrants: [{
        device_id: DEVICE_A,
        endpoint_ref: `be_${"a".repeat(64)}`,
        provider: "chatgpt",
        account_ref: `br_${"b".repeat(64)}`,
      }],
    },
  });
  const r = await handleMcp(req(7, "tools/call", { name: "herdr_inspect", arguments: {} }), "w1", d.value);
  assert.equal(r.body.id, 7);
  assert.equal(r.body.result.isError, undefined);
  assert.equal(r.body.result.structuredContent.served, true);
  assert.equal(d.calls.length, 1);
  assert.equal(d.calls[0].op, "herdr_inspect");
  assert.equal(d.calls[0].contractEpoch, 2);
  assert.equal(d.calls[0].contractHash, EPOCH2_CONTRACT.contract_hash);
  assert.equal(d.calls[0].deadlineMs, 31_000);
  assert.equal(d.calls[0].trace, undefined, "browser grants must not alter non-browser MCP forwarding");
});

test("read-only call retries across a stale generation window after supersede proved not delivered", async () => {
  let forwards = 0;
  const d = deps({
    forward: async () => {
      forwards += 1;
      if (forwards <= 3) {
        return new Response(JSON.stringify({
          status: "ok",
          completion: {
            status: "error",
            error: {
              ok: false,
              code: "runtime_generation_superseded_before_dispatch",
              retryable: true,
              delivery_state: "not_delivered",
              retry_after_ms: 0,
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
  assert.equal(d.calls.length, 4);
  assert.equal(new Set(d.calls.map((call) => call.requestId)).size, 4);
  assert.equal(d.calls.every((call) => call.op === "herdr_inspect"), true);
  assert.equal(new Set(d.calls.map((call) => call.deadlineMs)).size, 1);
});

test("mutating call retries generation supersede only after the runtime proves it was not delivered", async () => {
  let forwards = 0;
  const d = deps({
    forward: async () => {
      forwards += 1;
      if (forwards <= 2) {
        return new Response(JSON.stringify({
          status: "ok",
          completion: {
            status: "error",
            error: {
              ok: false,
              code: "runtime_generation_superseded_before_dispatch",
              retryable: true,
              delivery_state: "not_delivered",
              retry_after_ms: 0,
              message: "runtime/current changed before dispatch",
            },
          },
        }));
      }
      return new Response(JSON.stringify({
        status: "ok",
        completion: { status: "ok", result: { session_id: "es-new-generation" } },
      }));
    },
  });

  const r = await handleMcp(
    req(702, "tools/call", { name: "herdr_exec_start", arguments: { root: "/tmp/project", command: "pytest" } }),
    "w1",
    d.value,
  );
  assert.equal(r.body.result.isError, undefined);
  assert.equal(r.body.result.structuredContent.session_id, "es-new-generation");
  assert.equal(d.calls.length, 3);
  assert.equal(new Set(d.calls.map((call) => call.requestId)).size, 3);
  assert.equal(d.calls.every((call) => call.op === "herdr_exec_start"), true);
  assert.equal(new Set(d.calls.map((call) => call.deadlineMs)).size, 1);
});

test("herdr_devices executes at Edge and exposes pairing hint without tools/list contract drift", async () => {
  const devices = [{ device_id: DEVICE_A, name: "macbook" }];
  const d = deps({ devices });

  // 1. tools/list schema and contract hash remain completely untouched
  const listResp = await handleMcp(req(71, "tools/list", {}), "legacy-default", d.value);
  assert.equal(listResp.body.result._meta.herdr.contract_hash, EPOCH3_CONTRACT.contract_hash);
  const devicesTool = listResp.body.result.tools.find((t) => t.name === "herdr_devices");
  assert.deepEqual(devicesTool.inputSchema, {
    "$schema": "http://json-schema.org/draft-07/schema#",
    type: "object",
    properties: {},
    additionalProperties: false,
  });

  // 2. herdr_devices response exposes the devices and non-secret action hint
  const r = await handleMcp(req(72, "tools/call", { name: "herdr_devices", arguments: {} }), "legacy-default", d.value);
  assert.equal(r.body.result.isError, undefined);
  assert.equal(r.body.result.structuredContent.ok, true);
  assert.deepEqual(r.body.result.structuredContent.devices, devices);
  assert.ok(typeof r.body.result.structuredContent.pairing_hint === "string");
  assert.ok(r.body.result.structuredContent.pairing_hint.includes("herdr_mcp.device.pair"));
  assert.ok(r.body.result.structuredContent.pairing_hint.includes("params='{\"ttl_seconds\":600"));
  assert.ok(r.body.result.structuredContent.pairing_hint.includes("params is a JSON string"));
  assert.ok(r.body.result.structuredContent.pairing_hint.includes("exact expiry"));
  assert.ok(r.body.result.structuredContent.revoke_hint.includes("herdr_mcp.device.revoke"));
  assert.ok(r.body.result.structuredContent.revoke_hint.includes('"confirm":true'));
  assert.ok(r.body.result.structuredContent.revoke_hint.includes("Never revoke by display name"));
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
  assert.equal(r.body.result.structuredContent.expires_at, "1970-01-01T00:10:00.000Z");
  assert.equal(r.body.result.structuredContent.ttl_seconds, 300);
  assert.equal(
    r.body.result.structuredContent.new_device_command,
    `herdr-mcp worker connect "https://edge.example/pair#pair_${"11".repeat(32)}"`,
  );
  assert.ok(r.body.result.structuredContent.instructions.includes("herdr-mcp worker connect"));
  assert.ok(r.body.result.structuredContent.instructions.includes("1970-01-01T00:10:00.000Z"));
  assert.ok(r.body.result.structuredContent.instructions.includes("no-echo prompt"));
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

  // Rejects unknown top-level keys in arguments (e.g. ttl_seconds placed at top level or typo)
  let createCalled = false;
  const dTrack = deps({
    createPairing: async () => {
      createCalled = true;
      return { ok: true, pairing_id: "pair_1", code: "123456", expires_at_ms: 100, pairing_address: "https://edge.example/pair#pair_1" };
    },
  });
  for (const badArgs of [
    { method: "herdr_mcp.device.pair", ttl_seconds: 300 },
    { method: "herdr_mcp.device.pair", target: "pane1" },
    { method: "herdr_mcp.device.pair", extra: "field" },
  ]) {
    createCalled = false;
    const rTopLevelKey = await handleMcp(
      req(731, "tools/call", {
        name: "herdr_call",
        arguments: badArgs,
      }),
      "legacy-default",
      dTrack.value,
    );
    assert.equal(rTopLevelKey.body.result.isError, true);
    assert.equal(rTopLevelKey.body.result.structuredContent.code, "invalid_params");
    assert.equal(createCalled, false, "no pairing session must be created on invalid top-level arguments");
  }
});

test("herdr_call herdr_mcp.device.revoke executes at Edge only with immutable id and explicit confirmation", async () => {
  let revoked = null;
  const d = deps({
    revokeDevice: async (deviceId) => {
      revoked = deviceId;
      return { ok: true, device_id: deviceId, revoked_at_ms: 1_234_567 };
    },
  });

  const r = await handleMcp(
    req(732, "tools/call", {
      name: "herdr_call",
      arguments: {
        method: "herdr_mcp.device.revoke",
        params: JSON.stringify({ device_id: DEVICE_A, confirm: true }),
      },
    }),
    "legacy-default",
    d.value,
  );
  assert.equal(r.body.result.isError, undefined);
  assert.equal(r.body.result.structuredContent.ok, true);
  assert.equal(r.body.result.structuredContent.revoked, true);
  assert.equal(r.body.result.structuredContent.device_id, DEVICE_A);
  assert.equal(r.body.result.structuredContent.revoked_at_ms, 1_234_567);
  assert.equal(revoked, DEVICE_A);
  assert.equal(d.calls.length, 0, "Edge-local revoke must never forward to a workstation");
  assert.equal(d.targets.length, 0);

  const noConfirm = await handleMcp(
    req(733, "tools/call", {
      name: "herdr_call",
      arguments: { method: "herdr_mcp.device.revoke", params: { device_id: DEVICE_A } },
    }),
    "legacy-default",
    d.value,
  );
  assert.equal(noConfirm.body.result.isError, true);
  assert.equal(noConfirm.body.result.structuredContent.code, "confirmation_required");

  const badName = await handleMcp(
    req(734, "tools/call", {
      name: "herdr_call",
      arguments: { method: "herdr_mcp.device.revoke", params: { device_id: "macbook-air", confirm: true } },
    }),
    "legacy-default",
    d.value,
  );
  assert.equal(badName.body.result.isError, true);
  assert.equal(badName.body.result.structuredContent.code, "invalid_device_id");

  const nameSelector = await handleMcp(
    req(735, "tools/call", {
      name: "herdr_call",
      arguments: { method: "herdr_mcp.device.revoke", params: { device: DEVICE_A, confirm: true } },
    }),
    "legacy-default",
    d.value,
  );
  assert.equal(nameSelector.body.result.isError, true);
  assert.equal(nameSelector.body.result.structuredContent.code, "invalid_params");
});

test("connector approve/revoke private methods are Edge-local, schema-bounded, and add no public tool", async () => {
  let approved = null;
  let revoked = null;
  const d = deps({
    approveConnector: async (input) => {
      approved = input;
      return { ok: true, client_id: "dcr-abc", approved_at_ms: 1234 };
    },
    revokeConnector: async (clientId) => {
      revoked = clientId;
      return { ok: true };
    },
  });
  const approve = await handleMcp(
    req(736, "tools/call", {
      name: "herdr_call",
      arguments: {
        method: "herdr_mcp.connector.approve",
        params: { request_id: "req-abc", code: "123456" },
      },
    }),
    "legacy-default",
    d.value,
  );
  assert.deepEqual(approved, { request_id: "req-abc", code: "123456" });
  assert.equal(approve.body.result.structuredContent.client_id, "dcr-abc");
  assert.equal(d.calls.length, 0);
  assert.equal(d.targets.length, 0);

  const argvSecret = await handleMcp(
    req(737, "tools/call", {
      name: "herdr_call",
      arguments: {
        method: "herdr_mcp.connector.approve",
        params: { request_id: "req-abc", code: "123456", extra: true },
      },
    }),
    "legacy-default",
    d.value,
  );
  assert.equal(argvSecret.body.result.isError, true);
  assert.equal(argvSecret.body.result.structuredContent.code, "invalid_params");

  const noConfirm = await handleMcp(
    req(738, "tools/call", {
      name: "herdr_call",
      arguments: {
        method: "herdr_mcp.connector.revoke",
        params: { client_id: "dcr-abc" },
      },
    }),
    "legacy-default",
    d.value,
  );
  assert.equal(noConfirm.body.result.structuredContent.code, "confirmation_required");

  const revoke = await handleMcp(
    req(739, "tools/call", {
      name: "herdr_call",
      arguments: {
        method: "herdr_mcp.connector.revoke",
        params: { client_id: "dcr-abc", confirm: true },
      },
    }),
    "legacy-default",
    d.value,
  );
  assert.equal(revoke.body.result.structuredContent.revoked, true);
  assert.equal(revoked, "dcr-abc");

  const remoteGrant = await handleMcp(
    req(7391, "tools/call", {
      name: "herdr_call",
      arguments: {
        method: "herdr_mcp.connector.webchat_control.set",
        device: DEVICE_A,
        params: {
          client_id: "dcr-abc",
          endpoint_ref: `be_${"a".repeat(64)}`,
          provider: "chatgpt",
          account_ref: `br_${"b".repeat(64)}`,
          allowed: true,
          confirm: true,
        },
      },
    }),
    "legacy-default",
    d.value,
  );
  assert.equal(remoteGrant.body.result.isError, true);
  assert.equal(remoteGrant.body.result.structuredContent.code, "connector_owner_device_required");
  assert.equal(d.calls.length, 0, "OAuth MCP callers cannot self-authorize WebChat Control");

  const listed = await handleMcp(req(740, "tools/list", {}), "legacy-default", d.value);
  assert.equal(listed.body.result.tools.some((tool) => tool.name.includes("connector")), false);
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
        details: { source: "edge-test" },
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
  assert.deepEqual(r.body.result.structuredContent.details, { source: "edge-test" });
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

test("browser private methods require explicit enrolled device selector before forwarding", async () => {
  const d = deps({
    client: {
      webchatControlGrants: [
        {
          device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
          endpoint_ref: "be_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          provider: "chatgpt",
          account_ref: "br_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        },
        {
          device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAW",
          endpoint_ref: "be_cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
          provider: "chatgpt",
          account_ref: "br_dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        },
      ],
    },
    resolveDevice: async (selector) => {
      if (selector === "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV") {
        return { ok: true, device_id: selector, workstation_id: "w-target", routing_reason: "explicit_device_selector" };
      }
      return { ok: false, code: "device_not_found" };
    },
  });

  // Missing device selector fails before forward
  for (const method of ["herdr_mcp.browser_endpoint.list", "herdr_mcp.browser_resource.resolve"]) {
    const missing = await handleMcp(
      req(1, "tools/call", { name: "herdr_call", arguments: { method, params: JSON.stringify({ limit: 10 }) } }),
      "w1",
      d.value,
    );
    assert.equal(missing.body.result.isError, true);
    assert.equal(missing.body.result.structuredContent.ok, false);
    assert.equal(missing.body.result.structuredContent.code, "device_required");
    assert.equal(missing.body.result.structuredContent.delivery_state, "not_delivered");
    assert.equal(missing.body.result.structuredContent.failure_layer, "edge_routing");
    assert.equal(d.calls.length, 0, "no request forwarded to workstation when device is missing");
  }

  // Empty string device selector fails before forward
  const emptyDevice = await handleMcp(
    req(2, "tools/call", { name: "herdr_call", arguments: { method: "herdr_mcp.browser_endpoint.list", device: "  " } }),
    "w1",
    d.value,
  );
  assert.equal(emptyDevice.body.result.isError, true);
  assert.equal(emptyDevice.body.result.structuredContent.code, "device_required");
  assert.equal(d.calls.length, 0);

  // Explicit non-empty device selector succeeds and forwards to the resolved workstation
  const explicit = await handleMcp(
    req(3, "tools/call", {
      name: "herdr_call",
      arguments: {
        method: "herdr_mcp.browser_endpoint.list",
        device: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
        params: JSON.stringify({ limit: 5 }),
      },
    }),
    "w1",
    d.value,
  );
  assert.equal(explicit.body.result.isError, undefined);
  assert.equal(d.calls.length, 1, "request forwarded to workstation");
  assert.equal(d.targets[0], "w-target");
  assert.equal(d.calls[0].op, "herdr_call");
  assert.equal(d.calls[0].args.method, "herdr_mcp.browser_endpoint.list");
  assert.equal(d.calls[0].args.device, undefined, "device selector stripped by unwrapDeviceRefs");
  assert.deepEqual(d.calls[0].trace, {
    webchat_control_grants: [{
      endpoint_ref: "be_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      provider: "chatgpt",
      account_ref: "br_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    }],
  }, "only grants for the routed device cross the Edge -> Link handoff");
});
