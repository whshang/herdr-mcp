import { test } from "node:test";
import assert from "node:assert/strict";

import { makeLimits } from "../dist/limits.js";
import { handleMcp } from "../dist/mcp-handler.js";
import { WorkstationDO } from "../dist/workstation-do.js";

// ---------------------------------------------------------------------------
// Edge relay idempotency replay scope.
//
// The WorkstationDO replay ledger must be scoped to the authenticated
// principal / connector grant generation, the routed device and a canonical
// request hash; a page-scoped completion may only be replayed while the caller
// still holds the endpoint grant. These tests drive the real WorkstationDO with
// a serialized fake Durable Storage and a fake Link socket, and the real Edge
// handler for the published-schema surface.
// ---------------------------------------------------------------------------

const WORKSTATION_ID = "prod-real-runtime";
const ROUTE_DEVICE = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYZ";
const CONNECTOR_A = "principal:connectorA";
const CONNECTOR_B = "principal:connectorB";
const SHARED_KEY = "shared-idempotency-key";
const ENDPOINT_E = "be_eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const ENDPOINT_F = "be_ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
const CACHED_PAGE_CONTENT = { ok: true, page_text: "cached page content for connector A" };
const PAGE_REQUEST_HASH = "sha256:page-assist-inspect-request";
const OTHER_REQUEST_HASH = "sha256:page-assist-open-request";

const PAGE_ARGS = {
  method: "herdr_mcp.page_assist",
  params: JSON.stringify({
    endpoint_ref: ENDPOINT_E,
    action: "inspect",
    target_origin: "https://example.com",
  }),
};

class FakeStorage {
  constructor() {
    this.map = new Map();
    this.alarm = null;
  }
  async get(key) { return this.map.get(key); }
  async put(key, value) { this.map.set(key, value); }
  async delete(key) { this.map.delete(key); }
  async list({ prefix } = {}) {
    return new Map([...this.map].filter(([key]) => !prefix || String(key).startsWith(prefix)));
  }
  async getAlarm() { return this.alarm; }
  async setAlarm(value) { this.alarm = Number(value); }
  async deleteAlarm() { this.alarm = null; }
}

function makeSubject() {
  const events = [];
  const storage = new FakeStorage();
  const socket = {
    deserializeAttachment: () => ({ active: true, registered: true }),
    send: (frame) => events.push(["send", JSON.parse(frame)]),
    serializeAttachment: () => {},
    close: () => {},
  };
  const state = {
    id: { name: WORKSTATION_ID },
    storage,
    blockConcurrencyWhile: async (fn) => fn(),
    getWebSockets: () => [socket],
    acceptWebSocket: () => {},
  };
  return { subject: new WorkstationDO(state, {}), events, storage };
}

function routeFence(revision = 10) {
  return {
    device_id: ROUTE_DEVICE,
    workstation_id: WORKSTATION_ID,
    authorization: "active",
    scheduling: "enabled",
    revision,
  };
}

function toolRequests(events) {
  return events.filter((event) => event[0] === "send" && event[1].kind === "tool_request");
}

function pageAssistCall(overrides = {}) {
  return {
    kind: "request",
    requestId: overrides.requestId ?? "replay-request",
    resourcePrincipalRef: overrides.principalRef ?? CONNECTOR_A,
    op: "herdr_call",
    args: PAGE_ARGS,
    idempotencyKey: overrides.idempotencyKey ?? SHARED_KEY,
    replayBinding: {
      requestHash: overrides.requestHash ?? PAGE_REQUEST_HASH,
      pageScopedEndpointRef: overrides.pageScopedEndpointRef ?? ENDPOINT_E,
      ...(overrides.grantGeneration === undefined ? { grantGeneration: 7 } : { grantGeneration: overrides.grantGeneration }),
    },
    routeDeviceId: ROUTE_DEVICE,
    executionFence: routeFence(overrides.fenceRevision ?? 10),
    ...(overrides.authorizedEndpointRefs === undefined
      ? { trace: { page_assist_grants: [{ endpoint_ref: ENDPOINT_E }] } }
      : { trace: { page_assist_grants: overrides.authorizedEndpointRefs.map((endpoint_ref) => ({ endpoint_ref })) } }),
    deadlineMs: Date.now() + 2_000,
  };
}

async function settleOk(subject, events, requestId, result) {
  const pending = subject.forwardInternal(pageAssistCall({ requestId }));
  await new Promise((resolve) => setImmediate(resolve));
  const sent = toolRequests(events).find((event) => event[1].request_id === requestId);
  assert.ok(sent, `${requestId} must reach the Link`);
  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: WORKSTATION_ID,
    request_id: requestId,
    result,
    served_at_ms: Date.now(),
  });
  return await pending;
}

test("user replays a page-scoped action safely | Given a settled page-scoped call for connector A | When other callers and other requests reuse the key | Then only the exact authorized retry returns the cached completion", async () => {
  const { subject, events } = makeSubject();
  await subject.fetch(new Request("https://do.internal/internal/status"));

  // Connector A settles a page-scoped mutation under a top-level relay key.
  const settled = await settleOk(subject, events, "replay-a-settled", CACHED_PAGE_CONTENT);
  const settledBody = await settled.json();
  assert.equal(settledBody.completion.status, "ok");
  assert.deepEqual(settledBody.completion.result, CACHED_PAGE_CONTENT);
  assert.equal(toolRequests(events).length, 1, "the original call is delivered exactly once");

  // (1) Same principal, same device, same grant generation, same request and a
  // still-granted endpoint: the original completion is replayed with no resend.
  const retry = await subject.forwardInternal(pageAssistCall({ requestId: "replay-a-retry" }));
  const retryBody = await retry.json();
  assert.equal(retryBody.status, "ok");
  assert.deepEqual(retryBody.completion.result, CACHED_PAGE_CONTENT, "an identical authorized retry returns the original completion");
  assert.equal(toolRequests(events).length, 1, "an idempotent replay must never resend to the device");

  // (2) Same principal after the endpoint grant is revoked: no cached content
  // and nothing is sent to the device.
  const revoked = await subject.forwardInternal(pageAssistCall({
    requestId: "replay-a-revoked",
    authorizedEndpointRefs: [],
  }));
  const revokedBody = await revoked.json();
  assert.equal(revokedBody.status, "error");
  assert.equal(revokedBody.error.code, "idempotency_replay_unauthorized");
  assert.equal(revokedBody.error.delivery_state, "not_delivered");
  assert.equal(revokedBody.error.details.endpoint_ref, ENDPOINT_E);
  assert.equal(JSON.stringify(revokedBody).includes("cached page content for connector A"), false,
    "a revoked caller must never receive the cached page content");
  assert.equal(toolRequests(events).length, 1, "a refused replay must not reach the device (no actuation)");

  // (3) Same principal, same key, different endpoint / inner method: rejected
  // without cached content and without actuation.
  const mismatch = await subject.forwardInternal(pageAssistCall({
    requestId: "replay-a-mismatch",
    requestHash: OTHER_REQUEST_HASH,
    pageScopedEndpointRef: ENDPOINT_F,
  }));
  const mismatchBody = await mismatch.json();
  assert.equal(mismatchBody.status, "error");
  assert.equal(mismatchBody.error.code, "idempotency_key_payload_mismatch");
  assert.equal(JSON.stringify(mismatchBody).includes("cached page content for connector A"), false);
  assert.equal(toolRequests(events).length, 1, "a payload mismatch must not reach the device (no actuation)");

  // (4) A different principal reusing the same key never reads connector A's
  // completion: the request is forwarded and the runtime decides authorization.
  const otherPrincipal = subject.forwardInternal(pageAssistCall({
    requestId: "replay-b",
    principalRef: CONNECTOR_B,
    authorizedEndpointRefs: [],
  }));
  await new Promise((resolve) => setImmediate(resolve));
  const forwardedForB = toolRequests(events).find((event) => event[1].request_id === "replay-b");
  assert.ok(forwardedForB, "connector B's request reaches the runtime grant check instead of a cached replay");
  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: WORKSTATION_ID,
    request_id: "replay-b",
    result: { ok: false, code: "caller_grant_missing", delivery_state: "not_delivered" },
    served_at_ms: Date.now(),
  });
  const otherBody = await (await otherPrincipal).json();
  assert.equal(otherBody.completion.result.code, "caller_grant_missing");
  assert.equal(JSON.stringify(otherBody).includes("cached page content for connector A"), false,
    "connector B must never receive connector A's cached result");

  // Connector A's own authorized retry still works after B's independent call.
  const retryAfterB = await subject.forwardInternal(pageAssistCall({ requestId: "replay-a-after-b" }));
  const retryAfterBBody = await retryAfterB.json();
  assert.deepEqual(retryAfterBBody.completion.result, CACHED_PAGE_CONTENT);
});

test("user keeps a page completion bound to the connector generation | Given a settled page-scoped call at generation 7 | When the caller presents generation 8 | Then the cached completion is not replayed", async () => {
  const { subject, events } = makeSubject();
  await subject.fetch(new Request("https://do.internal/internal/status"));
  await settleOk(subject, events, "generation-a-settled", CACHED_PAGE_CONTENT);

  const newer = subject.forwardInternal(pageAssistCall({
    requestId: "generation-a-newer",
    grantGeneration: 8,
  }));
  await new Promise((resolve) => setImmediate(resolve));
  assert.ok(toolRequests(events).some((event) => event[1].request_id === "generation-a-newer"),
    "a bumped connector grant generation is an independent request, not a replay");
  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: WORKSTATION_ID,
    request_id: "generation-a-newer",
    result: CACHED_PAGE_CONTENT,
    served_at_ms: Date.now(),
  });
  const body = await (await newer).json();
  assert.equal(body.completion.status, "ok");
  assert.equal(toolRequests(events).length, 2);
});

test("user cannot replay another device's completion | Given a settled page-scoped call routed to device A | When the same key is routed to device B | Then no cached completion is returned", async () => {
  const { subject, events } = makeSubject();
  await subject.fetch(new Request("https://do.internal/internal/status"));
  await settleOk(subject, events, "device-a-settled", CACHED_PAGE_CONTENT);

  const otherDevice = subject.forwardInternal({
    ...pageAssistCall({ requestId: "device-b-request" }),
    routeDeviceId: undefined,
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.ok(toolRequests(events).some((event) => event[1].request_id === "device-b-request"),
    "an unrouted request is a fresh request, not a replay of the device-scoped completion");
  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: WORKSTATION_ID,
    request_id: "device-b-request",
    result: { ok: false, code: "caller_grant_missing" },
    served_at_ms: Date.now(),
  });
  const body = await (await otherDevice).json();
  assert.equal(JSON.stringify(body).includes("cached page content for connector A"), false);
});

// ---------------------------------------------------------------------------
// Edge surface: published herdr_call schema + relay idempotency binding.
// ---------------------------------------------------------------------------

function edgeDeps(over = {}) {
  const calls = [];
  return {
    calls,
    value: {
      limits: makeLimits(),
      logger: { warn() {} },
      getStub: (workstationId) => ({ workstationId }),
      client: over.client,
      resolveDevice: over.resolveDevice,
      forward: async (_stub, body) => {
        calls.push(JSON.parse(body));
        return new Response(JSON.stringify({
          status: "ok",
          completion: { status: "ok", result: { served: true } },
        }), { headers: { "content-type": "application/json" } });
      },
      now: () => 1_000,
      ...(over.extra ?? {}),
    },
  };
}

const edgeReq = (id, args) => ({
  jsonrpc: "2.0",
  id,
  method: "tools/call",
  params: { name: "herdr_call", arguments: args },
});

const PAGE_CONNECTOR_CLIENT = {
  connectorId: "conn_pageconnector123",
  grantGeneration: 7,
  pageAssistGrants: [
    { device_id: ROUTE_DEVICE, endpoint_ref: ENDPOINT_E },
    { device_id: "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYA", endpoint_ref: ENDPOINT_F },
  ],
};

test("user is protected from unknown herdr_call fields | Given the published herdr_call schema | When an unpublished top-level argument is sent | Then it is rejected before any workstation routing", async () => {
  const d = edgeDeps({ client: PAGE_CONNECTOR_CLIENT });
  const res = await handleMcp(edgeReq(1, {
    method: "herdr_mcp.page_assist",
    params: JSON.stringify({ endpoint_ref: ENDPOINT_E, action: "inspect" }),
    route_device_id: ROUTE_DEVICE,
  }), "w1", d.value);
  assert.equal(res.body.result.isError, true);
  assert.equal(res.body.result.structuredContent.code, "invalid_params");
  assert.match(res.body.result.structuredContent.message, /unknown top-level argument 'route_device_id'/);
  assert.equal(d.calls.length, 0, "an unpublished top-level field must never reach a workstation");

  const nested = await handleMcp(edgeReq(2, {
    method: "herdr_mcp.page_assist",
    params: JSON.stringify({ endpoint_ref: ENDPOINT_E, action: "inspect" }),
    principal_ref: CONNECTOR_A,
  }), "w1", d.value);
  assert.equal(nested.body.result.structuredContent.code, "invalid_params");
  assert.equal(d.calls.length, 0);
});

test("user keeps relay idempotency scoped | Given a page-scoped herdr_call with a relay idempotency key | When the Edge forwards it | Then the binding carries the canonical request hash, endpoint and connector generation", async () => {
  const d = edgeDeps({ client: PAGE_CONNECTOR_CLIENT });
  const res = await handleMcp(edgeReq(3, {
    method: "herdr_mcp.page_assist",
    device: ROUTE_DEVICE,
    idempotency_key: "edge-replay-scope-key",
    params: JSON.stringify({
      endpoint_ref: ENDPOINT_E,
      action: "inspect",
      target_origin: "https://example.com",
    }),
  }), "w1", d.value);
  assert.equal(res.body.result.isError, undefined, "the deliberate relay idempotency key is accepted");
  assert.equal(d.calls.length, 1);
  const forwarded = d.calls[0];
  assert.equal(forwarded.idempotencyKey, "edge-replay-scope-key");
  assert.match(forwarded.replayBinding.requestHash, /^[0-9a-f]{64}$/);
  assert.equal(forwarded.replayBinding.pageScopedEndpointRef, ENDPOINT_E);
  assert.equal(forwarded.replayBinding.grantGeneration, 7);
  assert.deepEqual(forwarded.trace.page_assist_grants, [{ endpoint_ref: ENDPOINT_E }]);
  assert.equal(JSON.stringify(forwarded).includes("conn_pageconnector123"), false,
    "the forwarded binding never carries the raw connector id");

  // A semantically identical request re-serialized with different key order and
  // whitespace produces the same request hash, so a legitimate retry replays.
  const reserialized = await handleMcp(edgeReq(4, {
    method: "herdr_mcp.page_assist",
    device: ROUTE_DEVICE,
    idempotency_key: "edge-replay-scope-key",
    params: "{\n  \"action\": \"inspect\",\n  \"target_origin\": \"https://example.com\",\n  \"endpoint_ref\": \"" + ENDPOINT_E + "\"\n}",
  }), "w1", d.value);
  assert.equal(reserialized.body.result.isError, undefined);
  assert.equal(d.calls[1].replayBinding.requestHash, forwarded.replayBinding.requestHash);

  // A different inner method or endpoint must produce a different binding hash.
  const otherEndpoint = await handleMcp(edgeReq(5, {
    method: "herdr_mcp.page_assist",
    device: ROUTE_DEVICE,
    idempotency_key: "edge-replay-scope-key",
    params: JSON.stringify({ endpoint_ref: ENDPOINT_F, action: "inspect", target_origin: "https://example.com" }),
  }), "w1", d.value);
  assert.equal(otherEndpoint.body.result.isError, undefined);
  assert.notEqual(d.calls[2].replayBinding.requestHash, forwarded.replayBinding.requestHash);

  const malformedKey = await handleMcp(edgeReq(6, {
    method: "herdr_mcp.page_assist",
    device: ROUTE_DEVICE,
    idempotency_key: 42,
    params: JSON.stringify({ endpoint_ref: ENDPOINT_E, action: "inspect" }),
  }), "w1", d.value);
  assert.equal(malformedKey.body.result.structuredContent.code, "invalid_params");
  assert.equal(d.calls.length, 3, "a malformed relay key is rejected before routing");
});
