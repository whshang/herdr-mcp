import { test } from "node:test";
import assert from "node:assert/strict";

import {
  encodeDeviceRef,
  decodeDeviceRef,
  isDeviceAwareRef,
  extractDeviceIdFromArgs,
  unwrapDeviceRefs,
  wrapResultWithDevice,
} from "../dist/device-refs.js";
import { resolveDeviceRouteWithContext } from "../dist/device-directory.js";
import { handleMcp } from "../dist/mcp-handler.js";
import { makeLimits } from "../dist/limits.js";

const DEV_A = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYZ";
const DEV_B = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYY";

function response(body, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}
function registryWith(devices) {
  return { fetch: async () => response({ ok: true, devices }) };
}

test("device refs encode and decode round-trip; legacy plain ids remain valid", async () => {
  const ref = encodeDeviceRef(DEV_A, "w1", undefined);
  const decoded = decodeDeviceRef(ref);
  assert.deepEqual(decoded, { v: 1, d: DEV_A, w: "w1", p: undefined });
  assert.equal(isDeviceAwareRef(ref), true);
  assert.equal(isDeviceAwareRef("w1"), false);
  assert.equal(isDeviceAwareRef("w1:p1"), false);
  assert.equal(decodeDeviceRef("w1"), null);
  assert.equal(decodeDeviceRef("herdr_ref_notbase64"), null);
});

test("device refs pane and workspace variants", async () => {
  const paneRef = encodeDeviceRef(DEV_B, undefined, "w2:p1");
  assert.equal(decodeDeviceRef(paneRef).p, "w2:p1");
  assert.equal(decodeDeviceRef(paneRef).d, DEV_B);
  const wsRef = encodeDeviceRef(DEV_A, "w3", undefined);
  assert.equal(decodeDeviceRef(wsRef).w, "w3");
});

test("strict ref payload: exactly one subject, grammar, reject /tmp and dual/device-only", async () => {
  // dual payload must be rejected
  assert.throws(() => encodeDeviceRef(DEV_A, "w1", "w1:p1"));
  assert.throws(() => encodeDeviceRef(DEV_A, undefined, undefined));
  // invalid grammar
  assert.throws(() => encodeDeviceRef(DEV_A, "/tmp", undefined));
  assert.throws(() => encodeDeviceRef(DEV_A, undefined, "/tmp/p1"));
  assert.throws(() => encodeDeviceRef(DEV_A, "bad id", undefined));
  // decode must reject dual and device-only and path-like
  const dualB64 = (() => {
    const payload = { v: 1, d: DEV_A, w: "w1", p: "w1:p1" };
    const b64 = Buffer.from(JSON.stringify(payload)).toString("base64url");
    return `herdr_ref_${b64}`;
  })();
  assert.equal(decodeDeviceRef(dualB64), null);
  const deviceOnlyB64 = (() => {
    const payload = { v: 1, d: DEV_A };
    const b64 = Buffer.from(JSON.stringify(payload)).toString("base64url");
    return `herdr_ref_${b64}`;
  })();
  assert.equal(decodeDeviceRef(deviceOnlyB64), null);
  const tmpPayload = (() => {
    const payload = { v: 1, d: DEV_A, w: "/tmp" };
    const b64 = Buffer.from(JSON.stringify(payload)).toString("base64url");
    return `herdr_ref_${b64}`;
  })();
  assert.equal(decodeDeviceRef(tmpPayload), null);
});

test("extractDeviceIdFromArgs: ignores path and untrusted binding fields, strict type per field", async () => {
  const refA = encodeDeviceRef(DEV_A, "w1");
  const paneA = encodeDeviceRef(DEV_A, undefined, "w1:p1");
  const args1 = { workspace: refA, path: "/tmp/foo" };
  const ext1 = extractDeviceIdFromArgs(args1);
  assert.equal(ext1.deviceId, DEV_A);
  assert.equal(ext1.kind, "ref");
  // path containing slash must not be treated as device
  const args2 = { path: "/home/user/project/file.txt" };
  assert.equal(extractDeviceIdFromArgs(args2), null);
  // binding fields are NOT trusted – must be ignored for routing
  const args3 = { binding_device_id: DEV_B };
  assert.equal(extractDeviceIdFromArgs(args3), null);
  const args3b = { __herdr_binding_device_id: DEV_B, herdr_binding: DEV_B };
  assert.equal(extractDeviceIdFromArgs(args3b), null);
  // ref still wins, binding ignored
  const args4 = { workspace: refA, binding_device_id: DEV_B };
  const ext4 = extractDeviceIdFromArgs(args4);
  assert.equal(ext4.deviceId, DEV_A);
  // type mismatch: workspace field with pane ref must fail closed
  const args5 = { workspace: paneA };
  const ext5 = extractDeviceIdFromArgs(args5);
  assert.equal(ext5.deviceId, "__type_mismatch__");
  const args6 = { pane_id: refA };
  const ext6 = extractDeviceIdFromArgs(args6);
  assert.equal(ext6.deviceId, "__type_mismatch__");
});

test("unwrapDeviceRefs strips opaque wrapper strictly by type and strips device/binding keys", async () => {
  const ref = encodeDeviceRef(DEV_A, "w1");
  const paneRef = encodeDeviceRef(DEV_B, undefined, "w2:p1");
  const args = { workspace: ref, pane_id: paneRef, device: DEV_A, binding_device_id: DEV_B, other: "keep" };
  const unwrapped = unwrapDeviceRefs(args);
  assert.equal(unwrapped.workspace, "w1");
  assert.equal(unwrapped.pane_id, "w2:p1");
  assert.equal(Object.hasOwn(unwrapped, "device"), false);
  assert.equal(Object.hasOwn(unwrapped, "binding_device_id"), false);
  assert.equal(unwrapped.other, "keep");
  // type mismatch: pane ref in workspace field must NOT be coerced
  const mismatch = unwrapDeviceRefs({ workspace: paneRef });
  assert.equal(mismatch.workspace, paneRef);
  const mismatch2 = unwrapDeviceRefs({ pane_id: ref });
  assert.equal(mismatch2.pane_id, ref);
});

test("wrapResultWithDevice retains device_id in opaque refs for device-routed calls (plain)", async () => {
  const result = {
    workspaces: [{ workspace_id: "w1", label: "repo" }],
    panes: [{ pane_id: "w1:p1", workspace_id: "w1" }],
    agents: [{ pane_id: "w1:p1", workspace_id: "w1" }],
  };
  const wrapped = wrapResultWithDevice(result, DEV_A, "macbook");
  assert.ok(isDeviceAwareRef(wrapped.workspaces[0].workspace_id));
  assert.equal(decodeDeviceRef(wrapped.workspaces[0].workspace_id_ref).d, DEV_A);
  assert.equal(wrapped.workspaces[0].workspace_id, wrapped.workspaces[0].workspace_id_ref);
  assert.equal(decodeDeviceRef(wrapped.panes[0].pane_id_ref).d, DEV_A);
  assert.equal(wrapped.panes[0].pane_id, wrapped.panes[0].pane_id_ref);
  assert.equal(decodeDeviceRef(wrapped.panes[0].workspace_id_ref).d, DEV_A);
  assert.equal(wrapped.device_id, DEV_A);
  assert.equal(wrapped.device_name, "macbook");
  // legacy null device leaves result unwrapped
  const unwrapped = wrapResultWithDevice(result, null);
  assert.deepEqual(unwrapped, result);
});

test("wrapResultWithDevice handles real CallToolResult shape and preserves image content", async () => {
  const callResult = {
    content: [
      { type: "text", text: JSON.stringify({ workspaces: [{ workspace_id: "w1" }] }) },
      { type: "image", data: "base64...", mimeType: "image/png" },
    ],
    structuredContent: {
      workspaces: [{ workspace_id: "w1", label: "repo" }],
      panes: [{ pane_id: "w1:p1", workspace_id: "w1" }],
    },
  };
  const wrapped = wrapResultWithDevice(callResult, DEV_A, "macbook");
  assert.ok(isDeviceAwareRef(wrapped.structuredContent.workspaces[0].workspace_id));
  assert.equal(decodeDeviceRef(wrapped.structuredContent.workspaces[0].workspace_id_ref).d, DEV_A);
  assert.equal(
    wrapped.structuredContent.workspaces[0].workspace_id,
    wrapped.structuredContent.workspaces[0].workspace_id_ref,
  );
  assert.equal(decodeDeviceRef(wrapped.structuredContent.panes[0].pane_id_ref).d, DEV_A);
  assert.equal(wrapped.structuredContent.device_id, DEV_A);
  assert.equal(wrapped.structuredContent.device_name, "macbook");
  const textPayload = JSON.parse(wrapped.content[0].text);
  assert.equal(textPayload.device_id, DEV_A);
  assert.equal(textPayload.device_name, "macbook");
  assert.equal(wrapped.content.length, 2);
  assert.equal(wrapped.content[1].type, "image");
  assert.equal(wrapped.content[1].data, "base64...");
});

test("device routing priority: explicit vs ref conflict fails closed; ref alone succeeds; binding ignored", async () => {
  const devices = [
    { device_id: DEV_A, workstation_id: "ws-a", name: "a", authorization: "active", scheduling: "enabled" },
    { device_id: DEV_B, workstation_id: "ws-b", name: "b", authorization: "active", scheduling: "enabled" },
  ];
  const refB = encodeDeviceRef(DEV_B, "w1");
  // explicit DEV_A plus ref to DEV_B must fail closed, not execute on B
  const conflict = await resolveDeviceRouteWithContext(registryWith(devices), { selector: DEV_A, args: { workspace: refB }, legacyWorkstationId: "legacy" });
  assert.equal(conflict.ok, false);
  assert.ok(conflict.code === "device_ref_conflict" || conflict.code === "device_ambiguous");
  // ref wins when no explicit
  assert.deepEqual(await resolveDeviceRouteWithContext(registryWith(devices), { args: { workspace: refB }, legacyWorkstationId: "legacy" }), {
    ok: true, device_id: DEV_B, device_name: "b", workstation_id: "ws-b", routing_reason: "device_ref",
  });
  // binding is NOT trusted – with only binding, should fall to ambiguous (2 routable)
  assert.deepEqual(await resolveDeviceRouteWithContext(registryWith(devices), { args: { binding_device_id: DEV_A }, legacyWorkstationId: "legacy" }), {
    ok: false,
    code: "device_ambiguous",
    candidate_devices: [
      { device_id: DEV_A, name: "a" },
      { device_id: DEV_B, name: "b" },
    ],
  });
  // intentional targeting of B via explicit + raw id should succeed
  assert.deepEqual(await resolveDeviceRouteWithContext(registryWith(devices), { selector: DEV_B, args: { workspace: "w1" }, legacyWorkstationId: "legacy" }), {
    ok: true, device_id: DEV_B, device_name: "b", workstation_id: "ws-b", routing_reason: "explicit_device",
  });
});

test("device routing via ref fails closed for unknown or paused device and ambiguous conflict and type mismatch", async () => {
  const devices = [
    { device_id: DEV_A, workstation_id: "ws-a", name: "a", authorization: "active", scheduling: "enabled" },
    { device_id: DEV_B, workstation_id: "ws-b", name: "b", authorization: "active", scheduling: "paused" },
  ];
  const UNKNOWN_DEV = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXY0";
  const unknownRef = encodeDeviceRef(UNKNOWN_DEV, "w1");
  assert.deepEqual(await resolveDeviceRouteWithContext(registryWith(devices), { args: { workspace: unknownRef }, legacyWorkstationId: "legacy" }), { ok: false, code: "device_not_found" });
  const pausedRef = encodeDeviceRef(DEV_B, "w1");
  assert.deepEqual(await resolveDeviceRouteWithContext(registryWith(devices), { args: { workspace: pausedRef }, legacyWorkstationId: "legacy" }), {
    ok: false,
    code: "device_paused",
    selected_device: { device_id: DEV_B, name: "b" },
  });
  // Conflicting refs in same call -> ambiguous/ref_conflict
  const refA = encodeDeviceRef(DEV_A, "w1");
  const refB = encodeDeviceRef(DEV_B, "w2");
  const conflict = await resolveDeviceRouteWithContext(registryWith(devices), { args: { workspace: refA, pane_id: refB }, legacyWorkstationId: "legacy" });
  assert.equal(conflict.ok, false);
  assert.ok(conflict.code === "device_ambiguous" || conflict.code === "device_ref_conflict");
  // type mismatch -> ref_conflict
  const paneRef = encodeDeviceRef(DEV_A, undefined, "w1:p1");
  const malformed = await resolveDeviceRouteWithContext(registryWith(devices), { args: { workspace: "herdr_ref_not-valid" }, legacyWorkstationId: "legacy" });
  assert.deepEqual(malformed, { ok: false, code: "device_ref_conflict" });
  const typeMismatch = await resolveDeviceRouteWithContext(registryWith(devices), { args: { workspace: paneRef }, legacyWorkstationId: "legacy" });
  assert.equal(typeMismatch.ok, false);
  assert.equal(typeMismatch.code, "device_ref_conflict");
  // Multiple routable + no selector/ref -> ambiguous
  const bothRoutable = [
    { device_id: DEV_A, workstation_id: "ws-a", name: "a", authorization: "active", scheduling: "enabled" },
    { device_id: DEV_B, workstation_id: "ws-b", name: "b", authorization: "active", scheduling: "enabled" },
  ];
  assert.deepEqual(await resolveDeviceRouteWithContext(registryWith(bothRoutable), { legacyWorkstationId: "legacy" }), {
    ok: false,
    code: "device_ambiguous",
    candidate_devices: [
      { device_id: DEV_A, name: "a" },
      { device_id: DEV_B, name: "b" },
    ],
  });
});

test("legacy single-device bindings/refs remain backward compatible", async () => {
  const single = [{ device_id: DEV_A, workstation_id: "ws-a", name: "a", authorization: "active", scheduling: "enabled" }];
  assert.deepEqual(await resolveDeviceRouteWithContext(registryWith(single), { args: { workspace: "w1" }, legacyWorkstationId: "legacy" }), {
    ok: true, device_id: DEV_A, device_name: "a", workstation_id: "ws-a", routing_reason: "single_available_device",
  });
  assert.deepEqual(await resolveDeviceRouteWithContext(registryWith([]), { args: { workspace: "w1" }, legacyWorkstationId: "legacy-ws" }), {
    ok: true, device_id: null, workstation_id: "legacy-ws", routing_reason: "legacy_default_device",
  });
});

test("follow-up tool calls with device-aware opaque ref route to same device before fallback (real CallToolResult)", async () => {
  const devices = [
    { device_id: DEV_A, workstation_id: "ws-a", name: "a", authorization: "active", scheduling: "enabled" },
    { device_id: DEV_B, workstation_id: "ws-b", name: "b", authorization: "active", scheduling: "enabled" },
  ];
  const deps = {
    limits: makeLimits(),
    logger: { warn() {} },
    getStub: (id) => ({ workstationId: id }),
    resolveDevice: async (sel, args) => resolveDeviceRouteWithContext(registryWith(devices), { selector: sel, args, legacyWorkstationId: "legacy" }),
    forward: async (_stub, body) => {
      const parsed = JSON.parse(body);
      assert.equal(Object.hasOwn(parsed.args, "device"), false);
      if (parsed.op === "herdr_exec") {
        assert.equal(parsed.args.workspace, "w1");
      }
      // Runtime returns full CallToolResult with structuredContent + image content
      const runtimeResult = {
        content: [
          { type: "text", text: JSON.stringify({ ok: true }) },
          { type: "image", data: "pngdata", mimeType: "image/png" },
        ],
        structuredContent: { workspaces: [{ workspace_id: "w1" }], ok: true },
      };
      return new Response(JSON.stringify({ status: "ok", completion: { status: "ok", result: runtimeResult } }), { headers: { "content-type": "application/json" } });
    },
    now: () => 1000,
  };
  const r1 = await handleMcp({ jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "herdr_inspect", arguments: { device: DEV_A } } }, "legacy", deps);
  // r1 must return wrapped ids inside structuredContent and preserve image content without leaking device_id into text
  const sc1 = r1.body.result.structuredContent;
  const wrappedId = sc1.workspaces[0].workspace_id_ref;
  assert.equal(sc1.workspaces[0].workspace_id, wrappedId);
  assert.ok(isDeviceAwareRef(wrappedId));
  assert.equal(decodeDeviceRef(wrappedId).d, DEV_A);
  assert.equal(sc1.device_id, DEV_A);
  assert.equal(sc1.device_name, "a");
  assert.equal(JSON.parse(r1.body.result.content[0].text).device_id, DEV_A);
  assert.equal(r1.body.result.content.length, 2);
  assert.equal(r1.body.result.content[1].type, "image");
  assert.equal(JSON.parse(r1.body.result.content[0].text).device_name, "a");
  // Follow-up uses the opaque ref without explicit device -> must route to same device
  const r2 = await handleMcp({ jsonrpc: "2.0", id: 2, method: "tools/call", params: { name: "herdr_exec", arguments: { workspace: wrappedId, command: "ls" } } }, "legacy", deps);
  assert.equal(r2.body.result.isError, undefined);
  // Ambiguous without selector/ref should fail
  const r3 = await handleMcp({ jsonrpc: "2.0", id: 3, method: "tools/call", params: { name: "herdr_exec", arguments: { workspace: "w1", command: "ls" } } }, "legacy", deps);
  assert.equal(r3.body.result.isError, true);
  assert.equal(r3.body.result.structuredContent.code, "device_ambiguous");
  assert.equal(r3.body.result.structuredContent.delivery_state, "not_delivered");
  assert.equal(r3.body.result.structuredContent.candidate_devices.length, 2);
  assert.match(r3.body.result.structuredContent.next_action, /explicit device/);
});

test("explicit device plus opaque ref to different device fails closed", async () => {
  const devices = [
    { device_id: DEV_A, workstation_id: "ws-a", name: "a", authorization: "active", scheduling: "enabled" },
    { device_id: DEV_B, workstation_id: "ws-b", name: "b", authorization: "active", scheduling: "enabled" },
  ];
  const deps = {
    limits: makeLimits(),
    logger: { warn() {} },
    getStub: (id) => ({ workstationId: id }),
    resolveDevice: async (sel, args) => resolveDeviceRouteWithContext(registryWith(devices), { selector: sel, args, legacyWorkstationId: "legacy" }),
    forward: async () => {
      assert.fail("should not forward on conflict");
    },
    now: () => 1000,
  };
  const refA = encodeDeviceRef(DEV_A, "w1");
  const r = await handleMcp({ jsonrpc: "2.0", id: 10, method: "tools/call", params: { name: "herdr_exec", arguments: { device: DEV_B, workspace: refA, command: "echo hi" } } }, "legacy", deps);
  assert.equal(r.body.result.isError, true);
  assert.ok(r.body.result.structuredContent.code === "device_ref_conflict" || r.body.result.structuredContent.code === "device_ambiguous");
});

test("mcp handler exposes device metadata in ChatGPT text", async () => {
  const deps = {
    limits: makeLimits(),
    logger: { warn() {} },
    getStub: (id) => ({ workstationId: id }),
    resolveDevice: async () => ({ ok: true, device_id: DEV_A, workstation_id: "ws-a", routing_reason: "explicit_device" }),
    forward: async (_stub, body) => {
      const parsed = JSON.parse(body);
      assert.equal(Object.hasOwn(parsed.args, "device"), false);
      // binding keys are stripped even though not trusted
      assert.equal(Object.hasOwn(parsed.args, "binding_device_id"), false);
      return new Response(JSON.stringify({
        status: "ok",
        completion: {
          status: "ok",
          result: {
            content: [{ type: "text", text: JSON.stringify({ ok: true, result: { workspaces: [{ id: "w1" }] } }) }],
            structuredContent: null,
          },
        },
      }), { headers: { "content-type": "application/json" } });
    },
    now: () => 1000,
  };
  const r = await handleMcp({ jsonrpc: "2.0", id: 20, method: "tools/call", params: { name: "herdr_fs_read", arguments: { device: DEV_A, path: "/repo/file.txt" } } }, "legacy", deps);
  const text = JSON.parse(r.body.result.content[0].text);
  assert.equal(text.device_id, DEV_A, "device_id must appear in ChatGPT-visible text");
  assert.equal(decodeDeviceRef(text.result.workspaces[0].workspace_id).d, DEV_A, "workspace ref must retain device affinity");
});
