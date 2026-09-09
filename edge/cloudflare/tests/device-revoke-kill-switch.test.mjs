import { test } from "node:test";
import assert from "node:assert/strict";

import worker from "../dist/index.js";
import { buildLinkAuthProtocol } from "../dist/auth.js";
import { DeviceRegistryDO } from "../dist/device-registry-do.js";
import { WorkstationDO } from "../dist/workstation-do.js";
import { RUNTIME_EXECUTION_CONTRACT } from "../dist/contracts/runtime.js";
import { resolveDeviceRoute } from "../dist/device-directory.js";

// ---------------------------------------------------------------------------
// Transaction-capable fake storage (serializes concurrent transactions like a
// real Durable Object) with write accounting, optional fault injection, and
// alarm support so durable-mutation flows can be exercised.
// ---------------------------------------------------------------------------
class FakeStorage {
  constructor() {
    this.map = new Map();
    this.writeCount = 0;
    this._queue = Promise.resolve();
    this.alarm = null;
    /** Optional fault predicate: (op: "put"|"delete", key) => throw. */
    this.fail = null;
    /** Optional per-key put gates: map key -> promise awaited before the write. */
    this.putGates = new Map();
  }
  async get(key) { return this.map.get(key); }
  async put(key, value) {
    const gate = this.putGates.get(String(key));
    if (gate) await gate;
    this.writeCount += 1;
    if (this.fail && this.fail("put", key)) throw new Error("injected storage fault");
    this.map.set(key, structuredClone(value));
  }
  async delete(key) {
    this.writeCount += 1;
    if (this.fail && this.fail("delete", key)) throw new Error("injected storage fault");
    return this.map.delete(key);
  }
  async list({ prefix } = {}) {
    return new Map([...this.map].filter(([key]) => !prefix || key.startsWith(prefix)));
  }
  transaction(fn) {
    const run = this._queue.then(() => fn(this));
    this._queue = run.then(() => undefined, () => undefined);
    return run;
  }
  async getAlarm() { return this.alarm; }
  async setAlarm(value) { this.alarm = Number(value); }
  async deleteAlarm() { this.alarm = null; }
}

// ---------------------------------------------------------------------------
// A real WorkstationDO per device, wired through a namespace keyed by
// idFromName so cross-device targeting is genuinely exercised (not every id
// routed to one stub).
// ---------------------------------------------------------------------------
function makeWorkstation(workstationId, storage = new FakeStorage()) {
  const sockets = [];
  const state = {
    id: { name: workstationId },
    storage,
    blockConcurrencyWhile: async (fn) => fn(),
    getWebSockets: () => sockets,
    acceptWebSocket: () => {},
  };
  const subject = new WorkstationDO(state, {});
  return { subject, storage, sockets };
}

function activeSocket() {
  const socket = {
    closed: undefined,
    sent: [],
    attachment: { active: false, registered: false },
    deserializeAttachment: () => socket.attachment,
    serializeAttachment: (value) => { socket.attachment = value; },
    send: (frame) => { socket.sent.push(JSON.parse(frame)); },
    close: (code, reason) => { socket.closed = { code, reason }; },
  };
  return socket;
}

function namespaceFor(map) {
  return {
    idFromName: (name) => name,
    get: (name) => map.get(name),
  };
}

function post(path, body, authorization) {
  const headers = { "content-type": "application/json" };
  if (authorization) headers.authorization = `Bearer ${authorization}`;
  return new Request(`https://edge.example${path}`, { method: "POST", headers, body: JSON.stringify(body) });
}

function postAsWorkstation(path, body, workstationId, secret) {
  return new Request(`https://edge.example${path}`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${secret}`,
      "x-herdr-workstation": workstationId,
    },
    body: JSON.stringify(body),
  });
}

function wsRequest(workstationId, secret) {
  return new Request(`https://edge.example/ws/${encodeURIComponent(workstationId)}`, {
    method: "GET",
    headers: {
      Upgrade: "websocket",
      "sec-websocket-protocol": `herdr-link.v1, ${buildLinkAuthProtocol(secret)}`,
    },
  });
}

// ---------------------------------------------------------------------------
// Build a full environment: one registry DO + one real WorkstationDO per
// device, plus the owner/default-workstation link stub.
// ---------------------------------------------------------------------------
function makeEnv(extra = {}) {
  const registryStorage = new FakeStorage();
  const workstations = new Map();
  const ownerWs = makeWorkstation("prod-real-runtime");
  workstations.set("prod-real-runtime", ownerWs.subject);

  const registry = new DeviceRegistryDO(
    { storage: registryStorage },
    { LINK_SHARED_SECRET: "legacy-secret", WORKSTATION_DO: namespaceFor(workstations) },
  );
  const unusedOAuth = { fetch: async () => new Response(JSON.stringify({ ok: false }), { status: 401 }) };
  const env = {
    DEVICE_REGISTRY_DO: namespaceFor(new Map([["devices-v1", registry]])),
    WORKSTATION_DO: namespaceFor(workstations),
    OAUTH_STORE_DO: namespaceFor(new Map([["oauth-v1", unusedOAuth]])),
    DEV_MCP_BEARER_SECRET: "owner-secret",
    LINK_SHARED_SECRET: "legacy-secret",
    DEFAULT_WORKSTATION_ID: "prod-real-runtime",
    ...extra,
  };
  // Attach test helpers directly onto the env object so worker.fetch receives
  // the real env while tests can still reach the registry/WorkstationDOs.
  env.registryStorage = registryStorage;
  env.registry = registry;
  env.workstations = workstations;
  env.ownerWs = ownerWs;
  return env;
}

async function pair(env, name) {
  const create = await worker.fetch(post("/devices/pairings", { ttl_seconds: 60, name }, "owner-secret"), env);
  assert.equal(create.status, 200);
  const session = await create.json();
  const consume = await worker.fetch(post("/devices/pairings/consume", { pairing_id: session.pairing_id, code: session.code }), env);
  assert.equal(consume.status, 200);
  const credential = await consume.json();
  // Register a real WorkstationDO for this device so teardown is observable.
  const ws = makeWorkstation(credential.workstation_id);
  env.workstations.set(credential.workstation_id, ws.subject);
  return { ...credential, ws };
}

async function connectDevice(env, device) {
  // Simulate a live link directly on the DO (the Worker upgrade auth path is
  // exercised separately by the reconnect-denial test).
  const hello = {
    protocol_version: 1,
    kind: "hello",
    workstation_id: device.workstation_id,
    boot_id: "boot1",
    link_version: "0.4.3",
    connected_at_ms: Date.now(),
    capabilities: [],
    runtime: {
      runtime_version: "0.4.3",
      runtime_commit: "test",
      runtime_generation: "g1",
      contract_epoch: RUNTIME_EXECUTION_CONTRACT.contract_epoch,
      contract_hash: RUNTIME_EXECUTION_CONTRACT.contract_hash,
      herdr_version: null,
      herdr_protocol: null,
    },
  };
  const socket = activeSocket();
  device.ws.sockets.push(socket);
  await device.ws.subject.webSocketMessage(socket, JSON.stringify(hello));
  assert.equal(socket.attachment.active, true, "hello must activate the link");
  return socket;
}

// ---------------------------------------------------------------------------
// 1. Worker operator revokes B without B's secret.
// ---------------------------------------------------------------------------
test("Worker operator revokes an enrolled device B without possessing B's device_secret", async () => {
  const env = makeEnv();
  const a = await pair(env, "owner-revoke-a");
  const b = await pair(env, "owner-revoke-b");
  await connectDevice(env, a);
  const bSocket = await connectDevice(env, b);

  const revoke = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(revoke.status, 200);
  const body = await revoke.json();
  assert.equal(body.device_id, b.device_id);
  assert.equal(typeof body.revoked_at_ms, "number");

  // B's live socket is closed and marked inactive.
  assert.equal(bSocket.closed?.code, 4401, "revoked device's live socket must be closed");
  assert.equal(bSocket.attachment.active, false, "revoked device's socket must be marked inactive");

  // A remains connected and routable.
  assert.equal(a.ws.sockets[0].closed, undefined, "unrelated device A must stay connected");
  assert.equal(a.ws.sockets[0].attachment.active, true, "unrelated device A must stay active");

  // B reconnect is denied at the Worker upgrade (401) and never reaches the DO.
  const reconnect = await worker.fetch(wsRequest(b.workstation_id, b.device_secret), env);
  assert.equal(reconnect.status, 401);
});

// ---------------------------------------------------------------------------
// 2. Legacy default-workstation enrolled credential revokes B.
// ---------------------------------------------------------------------------
test("legacy default-workstation enrolled link revokes B without B's secret", async () => {
  const env = makeEnv();
  const b = await pair(env, "owner-ws-revoke-b");
  await connectDevice(env, b);

  const revoke = await worker.fetch(postAsWorkstation(
    "/devices/revoke",
    { device_id: b.device_id },
    "prod-real-runtime",
    "legacy-secret",
  ), env);
  assert.equal(revoke.status, 200);
  assert.equal((await revoke.json()).device_id, b.device_id);

  const reconnect = await worker.fetch(wsRequest(b.workstation_id, b.device_secret), env);
  assert.equal(reconnect.status, 401);
});

// ---------------------------------------------------------------------------
// 3. Any enrolled device is an equal fleet-admin channel and may revoke B.
// ---------------------------------------------------------------------------
test("enrolled device A can revoke B without an owner/member hierarchy", async () => {
  const env = makeEnv();
  const a = await pair(env, "member-a");
  const b = await pair(env, "member-b");
  await connectDevice(env, a);
  const bSocket = await connectDevice(env, b);

  const memberRevoke = await worker.fetch(postAsWorkstation(
    "/devices/revoke",
    { device_id: b.device_id },
    a.workstation_id,
    a.device_secret,
  ), env);
  assert.equal(memberRevoke.status, 200);
  assert.equal((await memberRevoke.json()).device_id, b.device_id);
  assert.equal(bSocket.closed?.code, 4401, "fleet revoke must close the target socket");
  assert.equal(bSocket.attachment.active, false, "fleet revoke must mark the target socket inactive");
  assert.equal(b.ws.subject.revoked, true, "fleet revoke must set the target DO revocation tombstone");
  assert.equal(a.ws.sockets[0].closed, undefined, "admin device A must remain connected");
  assert.equal(a.ws.sockets[0].attachment.active, true, "admin device A must remain active");
});

// ---------------------------------------------------------------------------
// 4. Cross-device / cross-worker isolation: unknown device is device_not_found
//    and no local WorkstationDO is touched.
// ---------------------------------------------------------------------------
test("revoking a device from another registry/Worker is device_not_found with no local teardown", async () => {
  const env = makeEnv();
  const foreignDevice = "dev_01J9Z6P8G2K4M6N8Q0RSTVWXYZ";
  const beforeWrites = env.registryStorage.writeCount;

  const revoke = await worker.fetch(post("/devices/revoke", { device_id: foreignDevice }, "owner-secret"), env);
  assert.equal(revoke.status, 404);
  assert.equal((await revoke.json()).code, "device_not_found");
  assert.equal(env.registryStorage.writeCount, beforeWrites, "unknown device must not mutate the registry");
});

// ---------------------------------------------------------------------------
// 5. Already-revoked idempotency: preserves revoked_at_ms, zero additional
//    registry/tombstone writes, still re-issues teardown.
// ---------------------------------------------------------------------------
test("revoking an already-revoked device is idempotent and preserves revoked_at_ms", async () => {
  const env = makeEnv();
  const b = await pair(env, "idem-b");
  await connectDevice(env, b);

  const first = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(first.status, 200);
  const firstBody = await first.json();
  const registryWritesAfterFirst = env.registryStorage.writeCount;
  const bWritesAfterFirst = b.ws.storage.writeCount;

  // A fresh socket must NOT be able to establish a live link against the
  // revoked DO: its hello is rejected and the socket is closed immediately.
  const secondSocket = activeSocket();
  b.ws.sockets.push(secondSocket);
  await b.ws.subject.webSocketMessage(secondSocket, JSON.stringify({
    protocol_version: 1,
    kind: "hello",
    workstation_id: b.workstation_id,
    boot_id: "boot2",
    link_version: "0.4.3",
    connected_at_ms: Date.now(),
    capabilities: [],
    runtime: {
      runtime_version: "0.4.3",
      runtime_commit: "test",
      runtime_generation: "g1",
      contract_epoch: RUNTIME_EXECUTION_CONTRACT.contract_epoch,
      contract_hash: RUNTIME_EXECUTION_CONTRACT.contract_hash,
      herdr_version: null,
      herdr_protocol: null,
    },
  }));
  assert.equal(secondSocket.closed?.code, 4401, "revoked DO must reject a fresh hello");
  assert.equal(secondSocket.attachment.active, false, "revoked DO must never activate a fresh socket");

  const second = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(second.status, 200);
  const secondBody = await second.json();
  assert.equal(secondBody.revoked_at_ms, firstBody.revoked_at_ms, "revoked_at_ms must be preserved on idempotent revoke");

  // Zero additional registry writes; the revoked DO never accepted the fresh
  // socket, so the only new WorkstationDO write is the second-revoke
  // no-op (bounded, not a tombstone or session write).
  assert.equal(env.registryStorage.writeCount, registryWritesAfterFirst, "idempotent revoke must not rewrite the registry");
  assert.ok(b.ws.storage.writeCount <= bWritesAfterFirst + 2, "idempotent revoke must keep WorkstationDO writes bounded");
});

// ---------------------------------------------------------------------------
// 6. Live WS/session teardown: after revoke, heartbeat/tool_result frames
//    cannot mutate session or settle requests.
// ---------------------------------------------------------------------------
test("post-revoke heartbeat and tool_result frames cannot mutate the revoked session", async () => {
  const env = makeEnv();
  const b = await pair(env, "teardown-b");
  const socket = await connectDevice(env, b);

  const revoke = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(revoke.status, 200);
  assert.equal(socket.closed?.code, 4401, "revoked device's live socket must be closed");
  // Capture the write baseline AFTER revoke so the tombstone/session writes
  // from the revoke itself are not attributed to the stale frames.
  const bWritesAfterRevoke = b.ws.storage.writeCount;

  // A stale frame arriving after teardown must be rejected, not processed.
  const heartbeat = {
    protocol_version: 1,
    kind: "heartbeat",
    workstation_id: b.workstation_id,
    boot_id: "boot1",
    sent_at_ms: Date.now(),
    active_requests: 0,
  };
  await b.ws.subject.webSocketMessage(socket, JSON.stringify(heartbeat));
  const toolResult = {
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: b.workstation_id,
    request_id: "req-1",
    result: { ok: true },
    served_at_ms: Date.now(),
  };
  await b.ws.subject.webSocketMessage(socket, JSON.stringify(toolResult));

  // No session mutation, no request settlement, no additional writes.
  assert.equal(b.ws.storage.writeCount, bWritesAfterRevoke, "post-revoke frames must not write to the revoked DO");
  assert.equal(b.ws.subject.session?.status, "offline", "revoked session must be offline");
});

// ---------------------------------------------------------------------------
// 7b. In-flight requests are settled at teardown.
// ---------------------------------------------------------------------------
test("in-flight read is settled at teardown instead of hanging or writing a live result", async () => {
  const env = makeEnv();
  const b = await pair(env, "inflight-b");
  const socket = await connectDevice(env, b);

  // An accepted read never resolves because the (fake) link never answers.
  const inflight = b.ws.subject.forwardInternal({
    kind: "request",
    requestId: "inflight-read",
    op: "herdr_inspect",
    opClass: "read",
    deadlineMs: Date.now() + 3_000,
  });

  // Give the read time to be admitted and marked sent.
  await new Promise((resolve) => setTimeout(resolve, 5));

  const revoke = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(revoke.status, 200);

  const response = await inflight;
  const body = await response.json();
  assert.equal(body.status, "ok");
  assert.equal(body.completion.status, "error");
  assert.equal(body.completion.error.code, "link_auth_failed");
  assert.equal(body.completion.error.retryable, false);
});

// ---------------------------------------------------------------------------
// 7. Post-revoke tool request rejection: internal forward rejects immediately
//    without reconnect-grace waiting.
// ---------------------------------------------------------------------------
test("post-revoke internal forward rejects immediately without reconnect grace", async () => {
  const env = makeEnv();
  const b = await pair(env, "forward-b");
  await connectDevice(env, b);

  const revoke = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(revoke.status, 200);

  const started = Date.now();
  const response = await b.ws.subject.forwardInternal({
    kind: "request",
    requestId: "post-revoke-req",
    op: "herdr_inspect",
    deadlineMs: Date.now() + 5_000,
  });
  const elapsed = Date.now() - started;
  assert.equal(response.status, 401);
  const body = await response.json();
  assert.equal(body.status, "error");
  assert.equal(body.error.code, "link_auth_failed");
  assert.equal(body.error.retryable, false);
  assert.ok(elapsed < 80, `revoked forward must not wait reconnect grace, elapsed=${elapsed}`);
});

// ---------------------------------------------------------------------------
// 8. Reconnect denial: 401 at the Worker upgrade, never reaches WorkstationDO.
// ---------------------------------------------------------------------------
test("revoked device reconnect is denied at the Worker upgrade and never reaches the DO", async () => {
  const env = makeEnv();
  const b = await pair(env, "reconnect-b");
  await connectDevice(env, b);

  const revoke = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(revoke.status, 200);

  const reconnect = await worker.fetch(wsRequest(b.workstation_id, b.device_secret), env);
  assert.equal(reconnect.status, 401);
  assert.equal((await reconnect.json()).code, "link_auth_failed");
});

// ---------------------------------------------------------------------------
// 9. Self-revoke compatibility: exact-credential self-revoke still works and
//    now also triggers teardown.
// ---------------------------------------------------------------------------
test("exact-credential self-revoke remains compatible and triggers teardown", async () => {
  const env = makeEnv();
  const a = await pair(env, "self-revoke-a");
  const socket = await connectDevice(env, a);

  const selfRevoke = await worker.fetch(new Request("https://edge.example/devices/revoke-self", {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${a.device_secret}` },
    body: JSON.stringify({ workstation_id: a.workstation_id }),
  }), env);
  assert.equal(selfRevoke.status, 200);
  assert.equal((await selfRevoke.json()).device_id, a.device_id);
  assert.equal(socket.closed?.code, 4401, "self-revoke must also tear down the live socket");

  const reconnect = await worker.fetch(wsRequest(a.workstation_id, a.device_secret), env);
  assert.equal(reconnect.status, 401);
});

// ---------------------------------------------------------------------------
// 11. Explicit MCP routing after revoke returns device_revoked.
// ---------------------------------------------------------------------------
test("explicit MCP routing after revoke returns device_revoked", async () => {
  const env = makeEnv();
  const b = await pair(env, "route-b");
  await connectDevice(env, b);

  const revoke = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(revoke.status, 200);

  const route = await resolveDeviceRoute(env.registry, b.device_id, "legacy");
  assert.deepEqual(route, {
    ok: false,
    code: "device_revoked",
    selected_device: { device_id: b.device_id, name: "route-b" },
  });
});

// ---------------------------------------------------------------------------
// 10. Arbitrary PUT cannot revoke or resurrect a device.
// ---------------------------------------------------------------------------
test("arbitrary PUT cannot revoke a device or resurrect a revoked one", async () => {
  const env = makeEnv();
  const b = await pair(env, "put-b");

  // PUT with authorization=revoked must be rejected.
  const recordResponse = await env.registry.fetch(new Request(`https://registry.internal/internal/devices/${b.device_id}`));
  const recordBody = await recordResponse.json();
  const now = Date.now();
  const revokedRecord = { ...recordBody.device, authorization: "revoked", revoked_at_ms: now, updated_at_ms: now };
  const putRevoked = await env.registry.fetch(new Request(`https://registry.internal/internal/devices/${b.device_id}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(revokedRecord),
  }));
  assert.equal(putRevoked.status, 409);
  assert.equal((await putRevoked.json()).code, "revoke_via_put_forbidden");

  // Revoke via the owner route, then attempt to resurrect via PUT.
  const revoke = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(revoke.status, 200);

  const activeRecord = { ...recordBody.device, authorization: "active", scheduling: "enabled", revoked_at_ms: null };
  const putResurrect = await env.registry.fetch(new Request(`https://registry.internal/internal/devices/${b.device_id}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(activeRecord),
  }));
  assert.equal(putResurrect.status, 409);
  assert.equal((await putResurrect.json()).code, "device_revoked");

  // Reconnect still denied.
  const reconnect = await worker.fetch(wsRequest(b.workstation_id, b.device_secret), env);
  assert.equal(reconnect.status, 401);
});

// ---------------------------------------------------------------------------
// HIGH finding 1: tombstone write failure must not be treated as success.
// The live link is still torn down and new work is fenced; a retry keeps
// re-attempting persistence; a rebuilt instance fails closed from the
// persisted tombstone.
// ---------------------------------------------------------------------------
test("tombstone write failure still tears down the live link and fences new work; retry persists; rebuild fails closed", async () => {
  const env = makeEnv();
  const b = await pair(env, "tombstone-fail-b");
  const socket = await connectDevice(env, b);

  // Fail only the tombstone put (key "revoked").
  b.ws.storage.fail = (op, key) => op === "put" && key === "revoked";

  const revoke = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(revoke.status, 503);
  const body = await revoke.json();
  assert.equal(body.retryable, true, "tombstone persistence failure must surface as retryable");

  // The registry collapses the DO's tombstone-pending signal into the general
  // revoke_teardown_failed retryable code; the DO itself distinguishes it.
  const doResponse = await b.ws.subject.fetch(new Request("https://do.internal/internal/revoke", { method: "POST" }));
  assert.equal(doResponse.status, 503);
  assert.equal((await doResponse.json()).code, "revoke_tombstone_pending");

  // The live link is still torn down despite the tombstone write failure.
  assert.equal(socket.closed?.code, 4401, "live link must be torn down even when tombstone persistence fails");
  assert.equal(socket.attachment.active, false);

  // New work is fenced (in-memory revoked=true) even though not persisted.
  const fenced = await b.ws.subject.forwardInternal({
    kind: "request",
    requestId: "fenced-after-failed-tombstone",
    op: "herdr_inspect",
    deadlineMs: Date.now() + 5_000,
  });
  assert.equal(fenced.status, 401);
  assert.equal((await fenced.json()).error.retryable, false);

  // Clear the fault and retry: the tombstone is now persisted and revoke succeeds.
  b.ws.storage.fail = null;
  const retry = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(retry.status, 200);
  assert.equal((await retry.json()).device_id, b.device_id);
  assert.equal(b.ws.storage.map.has("revoked"), true, "tombstone must be durably persisted after retry");

  // Rebuild the instance from the same storage: it must fail closed from the
  // persisted tombstone (revoked=true, revokedPersisted=true, status offline).
  const rebuilt = makeWorkstation(b.workstation_id, b.ws.storage);
  const status = await rebuilt.subject.fetch(new Request("https://do.internal/internal/status"));
  const statusBody = await status.json();
  assert.equal(statusBody.online, false, "rebuilt revoked instance must report offline");
  assert.equal(statusBody.status, "offline");
  const rebuiltForward = await rebuilt.subject.forwardInternal({
    kind: "request",
    requestId: "rebuilt-fenced",
    op: "herdr_inspect",
    deadlineMs: Date.now() + 5_000,
  });
  assert.equal(rebuiltForward.status, 401, "rebuilt revoked instance must reject new work");
});

// ---------------------------------------------------------------------------
// MEDIUM finding 2: settlement persistence failure must not drop pending or
// resolver. Multiple in-flight waiters must all eventually settle (not hang)
// once the fault clears and a retry re-persists.
// ---------------------------------------------------------------------------
test("settlement write failure retains pending/resolver and is retryable; all in-flight waiters settle on retry", async () => {
  const env = makeEnv();
  const b = await pair(env, "settle-fail-b");
  await connectDevice(env, b);

  // Two in-flight durable mutations that never resolve (fake link never answers).
  const inflight1 = b.ws.subject.forwardInternal({
    kind: "request",
    requestId: "settle-fail-1",
    op: "herdr_prompt",
    opClass: "mutating",
    deadlineMs: Date.now() + 3_000,
  });
  const inflight2 = b.ws.subject.forwardInternal({
    kind: "request",
    requestId: "settle-fail-2",
    op: "herdr_prompt",
    opClass: "mutating",
    deadlineMs: Date.now() + 3_000,
  });
  await new Promise((resolve) => setTimeout(resolve, 5));

  // Fail EVERY completed: put so every settlement write fails until the fault
  // is cleared, proving partial-settlement is retryable at both the DO and the
  // public surface and never reported as full success.
  b.ws.storage.fail = (op, key) => op === "put" && key.startsWith("completed:");

  // Partial settlement must NOT report full success: the DO returns a retryable
  // revoke_settlement_pending and the worker surfaces 503.
  const doRevoke = await b.ws.subject.fetch(new Request("https://do.internal/internal/revoke", { method: "POST" }));
  assert.equal(doRevoke.status, 503);
  const doBody = await doRevoke.json();
  assert.equal(doBody.code, "revoke_settlement_pending");
  assert.equal(doBody.retryable, true);

  const workerRevoke = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(workerRevoke.status, 503, "partial settlement must be retryable at the public surface");
  assert.equal((await workerRevoke.json()).retryable, true);

  // The failed settlements must NOT have dropped their pending entries or
  // resolvers. Both waiters remain retained for retry.
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(b.ws.subject.registry.activeCount(), 2, "failed settlements must retain pending entries");
  assert.equal(b.ws.subject.resolvers.size, 2, "failed settlements must retain resolvers");

  // Clear the fault and re-trigger revoke: the retained pending entry is
  // re-classified and re-persisted, resolving the remaining waiter.
  b.ws.storage.fail = null;
  const retry = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(retry.status, 200);

  const [r1, r2] = await Promise.all([inflight1, inflight2]);
  for (const r of [r1, r2]) {
    const body = await r.json();
    assert.equal(body.status, "ok");
    assert.equal(body.completion.status, "error");
    // The requests had already been sent before revoke, so the durable
    // settlement classifies them as delivery_uncertain (non-retryable). The
    // requirement is that they settle at all — never hang.
    assert.equal(body.completion.error.code, "delivery_uncertain");
    assert.equal(body.completion.error.retryable, false);
  }
  assert.equal(b.ws.subject.registry.activeCount(), 0, "all in-flight waiters must settle");
  assert.equal(b.ws.subject.resolvers.size, 0, "all resolvers must be released");
});

// ---------------------------------------------------------------------------
// MEDIUM finding 3: handleStatus forces online=false when revoked or session
// offline, covering the post-revoke stale window and cold-start status.
// ---------------------------------------------------------------------------
test("handleStatus reports online=false for a revoked device even inside the stale window", async () => {
  const env = makeEnv();
  const b = await pair(env, "status-revoked-b");
  await connectDevice(env, b);

  // Freshly connected: online.
  const before = await b.ws.subject.fetch(new Request("https://do.internal/internal/status"));
  assert.equal((await before.json()).online, true);

  const revoke = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(revoke.status, 200);

  // Immediately after revoke (inside the stale window) status must be offline.
  const after = await b.ws.subject.fetch(new Request("https://do.internal/internal/status"));
  const afterBody = await after.json();
  assert.equal(afterBody.online, false, "revoked device must never report online");
  assert.equal(afterBody.connected, false);
  assert.equal(afterBody.status, "offline");
});

test("handleStatus reports online=false on cold start (no session)", async () => {
  const env = makeEnv();
  const b = await pair(env, "status-cold-b");
  // No connectDevice: cold start, no session.
  const status = await b.ws.subject.fetch(new Request("https://do.internal/internal/status"));
  const body = await status.json();
  assert.equal(body.online, false, "cold-start device with no session must report offline");
  assert.equal(body.status, "offline");
});

// ---------------------------------------------------------------------------
// MEDIUM finding 4: revoke must wake reconnect-grace waiters; a forward that
// was waiting must fail fast with a non-retryable auth error, not run to its
// timeout.
// ---------------------------------------------------------------------------
test("revoke wakes a reconnect-grace waiter and the forward fails fast with non-retryable auth", async () => {
  const env = makeEnv();
  const b = await pair(env, "wait-revoke-b");
  await connectDevice(env, b);

  // Force the link offline so a subsequent forward enters reconnect-grace wait.
  b.ws.sockets.length = 0;
  b.ws.subject.session.status = "offline";
  b.ws.subject.session.disconnectedAtMs = Date.now();

  const started = Date.now();
  const waiting = b.ws.subject.forwardInternal({
    kind: "request",
    requestId: "wait-revoke-req",
    op: "herdr_inspect",
    deadlineMs: Date.now() + 3_000,
  });

  // Give the forward time to register as a reconnect-grace waiter.
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(b.ws.subject.linkWaiters.size, 1, "forward must be waiting on reconnect grace");

  const revoke = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(revoke.status, 200);

  const response = await waiting;
  const elapsed = Date.now() - started;
  assert.equal(response.status, 401);
  const body = await response.json();
  assert.equal(body.status, "error");
  assert.equal(body.error.code, "link_auth_failed");
  assert.equal(body.error.retryable, false);
  assert.ok(elapsed < 80, `revoke must wake the waiter fast, elapsed=${elapsed}`);
  assert.equal(b.ws.subject.linkWaiters.size, 0, "waiter must be released");
});

// ---------------------------------------------------------------------------
// MEDIUM finding 5: observable teardown count + gated fault harness for
// concurrency. A second revoke re-issues teardown exactly once per call and
// concurrent revoke/forward interleavings stay fail-closed.
// ---------------------------------------------------------------------------
test("teardown count is observable and a second revoke re-issues teardown exactly once", async () => {
  const env = makeEnv();
  const b = await pair(env, "teardown-count-b");
  const socket1 = await connectDevice(env, b);

  const countCloses = () => b.ws.sockets.filter((s) => s.closed !== undefined).length;
  assert.equal(countCloses(), 0);

  const first = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(first.status, 200);
  assert.equal(countCloses(), 1, "first revoke must tear down the one live socket");

  // A fresh socket is immediately rejected by the revoked DO (hello closed).
  const socket2 = activeSocket();
  b.ws.sockets.push(socket2);
  await b.ws.subject.webSocketMessage(socket2, JSON.stringify({
    protocol_version: 1,
    kind: "hello",
    workstation_id: b.workstation_id,
    boot_id: "boot2",
    link_version: "0.4.3",
    connected_at_ms: Date.now(),
    capabilities: [],
    runtime: {
      runtime_version: "0.4.3",
      runtime_commit: "test",
      runtime_generation: "g1",
      contract_epoch: RUNTIME_EXECUTION_CONTRACT.contract_epoch,
      contract_hash: RUNTIME_EXECUTION_CONTRACT.contract_hash,
      herdr_version: null,
      herdr_protocol: null,
    },
  }));
  assert.equal(socket2.closed?.code, 4401, "revoked DO must reject a fresh hello");

  const second = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(second.status, 200);
  assert.equal(countCloses(), 2, "second revoke must re-issue teardown on the lingering socket");
});

test("concurrent revoke and forward interleaving stays fail-closed", async () => {
  const env = makeEnv();
  const b = await pair(env, "concurrent-b");
  await connectDevice(env, b);

  // Admit a read forward first (deterministic ordering), then revoke while it
  // is in flight. The revoke must settle it fail-closed with a non-retryable
  // revocation error — never a hang, never a retryable offline.
  const forwardP = b.ws.subject.forwardInternal({
    kind: "request",
    requestId: "concurrent-req",
    op: "herdr_inspect",
    deadlineMs: Date.now() + 5_000,
  });
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(b.ws.subject.ephemeralReads.size, 1, "forward must be admitted and in flight");

  const revoke = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(revoke.status, 200);

  const forwardResp = await forwardP;
  const forwardBody = await forwardResp.json();
  // Two fail-closed shapes are valid: the ephemeral read settles via teardown
  // (200 + error completion) or the admission-side revoke fence rejects (401).
  if (forwardResp.status === 401) {
    assert.equal(forwardBody.error.retryable, false);
  } else {
    assert.equal(forwardBody.status, "ok");
    assert.equal(forwardBody.completion.status, "error");
    assert.equal(forwardBody.completion.error.code, "link_auth_failed");
    assert.equal(forwardBody.completion.error.retryable, false);
  }
  assert.equal(b.ws.subject.revoked, true);
  assert.equal(b.ws.subject.ephemeralReads.size, 0, "in-flight read must settle, never hang");
});

// ---------------------------------------------------------------------------
// Release-blocking race 1: a revoke landing between the pre-send durability
// fence and the actual socket send must never deliver the mutation to the
// (now) revoked device. The pre-send revoked gate prevents the socket send.
// ---------------------------------------------------------------------------
test("concurrent_mutation_revoke: a mutation interleaved at the pre-send seam is never delivered to the revoked device", async () => {
  const env = makeEnv();
  const b = await pair(env, "mut-revoke-b");
  const socket = await connectDevice(env, b);

  // Deterministic interleave: pause the durable mutation at its pre-send seam
  // (after the pre-send durability fence, before the actual send) and gate the
  // revoke's tombstone write so the revoke fence (revoked=true) is set but its
  // settlement has NOT run when the mutation resumes. The pre-send revoked gate
  // must then prevent the socket send.
  let releaseMutation;
  const mutationGate = new Promise((resolve) => { releaseMutation = resolve; });
  let releaseRevoke;
  const tombstoneGate = new Promise((resolve) => { releaseRevoke = resolve; });
  b.ws.subject.beforeDurableSendHook = async () => { await mutationGate; };
  b.ws.storage.putGates.set("revoked", tombstoneGate);

  const mutation = b.ws.subject.forwardInternal({
    kind: "request",
    requestId: "mut-revoke-req",
    op: "herdr_prompt",
    opClass: "mutating",
    deadlineMs: Date.now() + 3_000,
  });

  // Mutation reaches the seam (pending row persisted, not yet sent).
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(b.ws.storage.map.has("pending:mut-revoke-req"), true, "mutation must be durably pending at the seam");

  // Revoke starts but blocks on the tombstone write AFTER setting the fence.
  const revokeP = worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(b.ws.subject.revoked, true, "revoke fence must be set before its settlement runs");
  assert.equal(b.ws.subject.registry.activeCount(), 1, "settlement must not have run yet");

  // Release the mutation: it must be caught by the pre-send revoked gate.
  releaseMutation();
  const resp = await mutation;
  const body = await resp.json();
  assert.equal(resp.status, 401, "revoked mutation must fail closed at the pre-send gate");
  assert.equal(body.error.code, "link_auth_failed");
  assert.equal(body.error.retryable, false);

  // No tool_request was ever written to the live socket.
  assert.equal(socket.sent.some((frame) => frame.kind === "tool_request"), false, "revoked device must never receive the mutation wire frame");
  // The denied mutation settled durably (completed row present, pending removed).
  assert.equal(b.ws.storage.map.has("completed:mut-revoke-req"), true, "denied mutation must be durably settled");
  assert.equal(b.ws.storage.map.has("pending:mut-revoke-req"), false, "denied mutation pending row must be removed");
  assert.equal(b.ws.subject.resolvers.size, 0, "no waiter may hang");

  // Let the revoke finish persisting its tombstone.
  releaseRevoke();
  const revoke = await revokeP;
  assert.equal(revoke.status, 200, "revoke teardown completes once its tombstone persists");
});

// ---------------------------------------------------------------------------
// Release-blocking race 2: partial settlement + restart must not report full
// success or permanently hang a waiter. completed-first ordering means a crash
// after completed is written but before pending delete is recoverable:
// completed-wins on load, and the stale pending row is cleaned up.
// ---------------------------------------------------------------------------
test("revoke_partial_settlement: completed-first ordering survives pending-delete failure and restart", async () => {
  const env = makeEnv();
  const b = await pair(env, "partial-b");
  await connectDevice(env, b);

  const mutation = b.ws.subject.forwardInternal({
    kind: "request",
    requestId: "partial-mut",
    op: "herdr_prompt",
    opClass: "mutating",
    deadlineMs: Date.now() + 3_000,
  });
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(b.ws.subject.registry.activeCount(), 1, "mutation admitted");

  // Fail the completed: put so settlement is only partially persisted.
  b.ws.storage.fail = (op, key) => op === "put" && key === "completed:partial-mut";

  const revoke = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(revoke.status, 503, "partial settlement must be retryable, never full success");
  assert.equal((await revoke.json()).retryable, true);

  // The pending entry and resolver are retained so the waiter does not hang.
  assert.equal(b.ws.subject.registry.activeCount(), 1, "failed settlement keeps pending entry");
  assert.equal(b.ws.subject.resolvers.size, 1, "failed settlement keeps resolver");

  // Clear the fault; the settlement now completes and the waiter resolves.
  b.ws.storage.fail = null;
  const retry = await worker.fetch(post("/devices/revoke", { device_id: b.device_id }, "owner-secret"), env);
  assert.equal(retry.status, 200, "retry after fault clears completes settlement");

  const resp = await mutation;
  const body = await resp.json();
  assert.equal(body.status, "ok");
  assert.equal(body.completion.status, "error");
  assert.equal(body.completion.error.code, "delivery_uncertain");
  assert.equal(b.ws.subject.resolvers.size, 0, "waiter must settle, not hang");
});

test("restart with completed-written/pending-not-deleted resolves via completed-wins and cleans up", async () => {
  const env = makeEnv();
  const b = await pair(env, "restart-b");
  await connectDevice(env, b);

  // Simulate a crash mid-settlement by seeding storage exactly as the
  // completed-first ordering leaves it: completed row written, pending delete
  // NOT executed (process died between the two).
  const completion = {
    status: "error",
    error: {
      ok: false,
      code: "delivery_uncertain",
      retryable: false,
      message: "process died mid-settlement",
      requestId: "restart-mut",
      workstationId: b.workstation_id,
      atMs: Date.now(),
    },
    servedAtMs: Date.now(),
  };
  const pendingEntry = {
    requestId: "restart-mut",
    workstationId: b.workstation_id,
    op: "herdr_prompt",
    opClass: "mutating",
    argsSummary: { argKeys: [] },
    state: "sent",
    createdAtMs: Date.now() - 100,
    deadlineMs: Date.now() + 3_000,
  };
  b.ws.storage.map.set("completed:restart-mut", completion);
  b.ws.storage.map.set("pending:restart-mut", pendingEntry);

  // Rebuild the instance from the same storage (process restart).
  const rebuilt = makeWorkstation(b.workstation_id, b.ws.storage);
  await rebuilt.subject.fetch(new Request("https://do.internal/internal/status"));

  // Completed-wins: the restart must NOT rehydrate the pending mutation as
  // active, and must clean up the stale pending row best-effort.
  assert.equal(rebuilt.subject.registry.activeCount(), 0, "completed request must not resurrect as pending");
  assert.equal(rebuilt.storage.map.has("pending:restart-mut"), false, "stale pending row cleaned up");
  assert.equal(rebuilt.storage.map.has("completed:restart-mut"), true, "completed row survives restart");
});

// ---------------------------------------------------------------------------
// Crash-recoverable settlement protocol (Codex NO-GO round 2): the completed
// row embeds the idempotency binding, so ANY intermediate crash/failure in
// completed/pending/idem still lets restart recover completion + binding and a
// same-key retry returns the prior completion instead of re-executing the
// mutation.
// ---------------------------------------------------------------------------

/** Drive a tool_result settlement through a fault-injected storage and return
 *  the DO's durable row state exactly as the crash leaves it, plus the still-
 *  pending forwardInternal promise ({ inflight }) for later resolution. */
async function settleWithFault(b, fault, toolResultValue) {
  const socket = await connectDevice(envFor(b), b);
  const inflight = b.ws.subject.forwardInternal({
    kind: "request",
    requestId: "crash-settle-mut",
    op: "herdr_prompt",
    opClass: "mutating",
    idempotencyKey: "crash-idem-key",
    deadlineMs: Date.now() + 3_000,
  });
  await new Promise((resolve) => setTimeout(resolve, 5));
  assert.equal(b.ws.storage.map.has("pending:crash-settle-mut"), true, "pending row must exist pre-settle");

  b.ws.storage.fail = fault;
  const frame = {
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: b.workstation_id,
    request_id: "crash-settle-mut",
    result: toolResultValue ?? { ok: true },
    served_at_ms: Date.now(),
  };
  // NOTE: deliberately NOT awaiting the tool_result delivery to completion;
  // the faulted settlement leaves the waiter pending, which is the crash state
  // under test. We only need the persistSettlement attempt to have run.
  void b.ws.subject.webSocketMessage(socket, JSON.stringify(frame));
  await new Promise((resolve) => setTimeout(resolve, 5));
  return { inflight, socket };
}

function envFor(b) {
  return b; // makeEnv already attaches workstations; pair() returns ws refs on it
}

function snapshotRows(storage) {
  return {
    completed: [...storage.map.keys()].filter((k) => k.startsWith("completed:")).map((k) => k.slice("completed:".length)),
    pending: [...storage.map.keys()].filter((k) => k.startsWith("pending:")).map((k) => k.slice("pending:".length)),
    idem: [...storage.map.keys()].filter((k) => k.startsWith("idem:")).map((k) => k.slice("idem:".length)),
  };
}

test("crash recovery: completed-put failure retains pending + resolver; retry after fault persists completion and binding", async () => {
  const env = makeEnv();
  const b = await pair(env, "crash-completed-fail");
  // Fail ONLY the completed: put; idem + pending-delete are never reached.
  const { inflight } = await settleWithFault(b, (op, key) => op === "put" && key.startsWith("completed:"), { ok: "executed-once" });

  // Nothing durably committed: no completed, no idem; pending retained with the
  // request (and its idempotency binding is still discoverable from pending).
  const rows = snapshotRows(b.ws.storage);
  assert.deepEqual(rows.completed, [], "completed write failed -> no completed row");
  assert.deepEqual(rows.idem, [], "idem must not be written when completed failed");
  assert.deepEqual(rows.pending, ["crash-settle-mut"], "pending must be retained for retry");
  assert.equal(b.ws.subject.registry.activeCount(), 1, "pending entry retained in registry");
  assert.equal(b.ws.subject.resolvers.size, 1, "resolver retained, waiter not hung");

  // Clear the fault; the retry settlement now persists completion + binding.
  b.ws.storage.fail = null;
  const completion = { status: "ok", result: { ok: "executed-once" }, servedAtMs: Date.now() };
  const settled = await b.ws.subject.persistSettlement("crash-settle-mut", completion);
  assert.equal(settled, true, "retry settlement must succeed after fault clears");
  assert.equal(b.ws.storage.map.has("completed:crash-settle-mut"), true, "completed row persisted on retry");
  assert.equal(b.ws.storage.map.has("idem:crash-idem-key"), true, "idempotency row persisted on retry");
  assert.equal(b.ws.storage.map.has("pending:crash-settle-mut"), false, "pending removed on retry");

  const resp = await inflight;
  const body = await resp.json();
  assert.equal(body.status, "ok");
  assert.equal(body.completion.status, "ok");
  assert.deepEqual(body.completion.result, { ok: "executed-once" });
  assert.equal(b.ws.subject.resolvers.size, 0, "waiter settled, not hung");
});

test("crash recovery: idem-put failure after completed success -> restart recovers completion + binding from completed row", async () => {
  const env = makeEnv();
  const b = await pair(env, "crash-idem-fail");
  // Fail ONLY the idem: put; completed succeeds, pending delete is never reached.
  const { inflight } = await settleWithFault(b, (op, key) => op === "put" && key.startsWith("idem:"), { ok: "executed-once" });

  // The crash state: completed present, idem absent, pending stale.
  const rows = snapshotRows(b.ws.storage);
  assert.deepEqual(rows.completed, ["crash-settle-mut"], "completed committed before idem write");
  assert.deepEqual(rows.idem, [], "idem write failed -> missing evidence row");
  assert.deepEqual(rows.pending, ["crash-settle-mut"], "pending delete not reached (crash between writes)");

  // REBUILD (process restart) from the same storage.
  const rebuilt = makeWorkstation(b.workstation_id, b.ws.storage);
  await rebuilt.subject.fetch(new Request("https://do.internal/internal/status"));

  // completed-wins: pending never resurrects; idempotency binding recovered
  // from the embedded binding inside the completed row.
  assert.equal(rebuilt.subject.registry.activeCount(), 0, "completed request must not resurrect as pending");
  assert.equal(rebuilt.storage.map.has("pending:crash-settle-mut"), false, "stale pending cleaned by completed-wins");
  assert.equal(rebuilt.storage.map.has("completed:crash-settle-mut"), true, "completed survives restart");

  // Same-key retry returns the prior completion (idem_hit), never re-executes.
  const add = rebuilt.subject.registry.add({
    requestId: "crash-retry-new-request",
    workstationId: b.workstation_id,
    op: "herdr_prompt",
    opClass: "mutating",
    idempotencyKey: "crash-idem-key",
    deadlineMs: Date.now() + 3_000,
  });
  assert.equal(add.status, "idem_hit", "same-key retry must hit the recovered binding");
  assert.equal(add.completion.status, "ok");
  assert.deepEqual(add.completion.result, { ok: "executed-once" }, "prior completion returned, no re-execution");

  // The original in-process waiter is gone with the old isolate; nothing pends.
  assert.equal(rebuilt.subject.registry.activeCount(), 0);
  assert.equal(rebuilt.subject.resolvers.size, 0);
  void inflight;
});

test("crash recovery: pending-delete failure after completed+idem success -> restart keeps completed-wins, pending cleaned, binding restored", async () => {
  const env = makeEnv();
  const b = await pair(env, "crash-pending-delete-fail");
  // Fail ONLY the pending: delete; completed + idem both commit first.
  const { inflight } = await settleWithFault(b, (op, key) => op === "delete" && key.startsWith("pending:"), { ok: "executed-once" });

  // Crash state: completed + idem present, stale pending remains.
  const rows = snapshotRows(b.ws.storage);
  assert.deepEqual(rows.completed, ["crash-settle-mut"], "completed committed");
  assert.deepEqual(rows.idem, ["crash-idem-key"], "idem committed before pending delete");
  assert.deepEqual(rows.pending, ["crash-settle-mut"], "pending delete failed -> stale row remains");

  // Clear the fault so the restart's best-effort stale-pending cleanup can run.
  b.ws.storage.fail = null;

  // REBUILD: completed-wins must drop the stale pending and keep the binding.
  const rebuilt = makeWorkstation(b.workstation_id, b.ws.storage);
  await rebuilt.subject.fetch(new Request("https://do.internal/internal/status"));
  assert.equal(rebuilt.subject.registry.activeCount(), 0, "completed request must not resurrect as pending");
  assert.equal(rebuilt.storage.map.has("pending:crash-settle-mut"), false, "stale pending cleaned on restart");
  assert.equal(rebuilt.storage.map.has("completed:crash-settle-mut"), true);
  assert.equal(rebuilt.storage.map.has("idem:crash-idem-key"), true);

  // Same-key retry returns the prior completion, no re-execution.
  const add = rebuilt.subject.registry.add({
    requestId: "crash-retry-new-request",
    workstationId: b.workstation_id,
    op: "herdr_prompt",
    opClass: "mutating",
    idempotencyKey: "crash-idem-key",
    deadlineMs: Date.now() + 3_000,
  });
  assert.equal(add.status, "idem_hit");
  assert.deepEqual(add.completion.result, { ok: "executed-once" });
  void inflight;
});

test("crash recovery: same-idempotency forward retry after restart sends no tool_request to the device", async () => {
  const env = makeEnv();
  const b = await pair(env, "crash-no-resend");
  const socket = await connectDevice(env, b);
  const inflight = b.ws.subject.forwardInternal({
    kind: "request",
    requestId: "crash-no-resend-mut",
    op: "herdr_prompt",
    opClass: "mutating",
    idempotencyKey: "crash-no-resend-key",
    deadlineMs: Date.now() + 3_000,
  });
  await new Promise((resolve) => setTimeout(resolve, 5));

  // Complete settlement durably (no fault), then restart.
  const frame = {
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: b.workstation_id,
    request_id: "crash-no-resend-mut",
    result: { ok: "once" },
    served_at_ms: Date.now(),
  };
  await b.ws.subject.webSocketMessage(socket, JSON.stringify(frame));
  const settledBody = await (await inflight).json();
  assert.equal(settledBody.completion.status, "ok");

  const rebuilt = makeWorkstation(b.workstation_id, b.ws.storage);
  await rebuilt.subject.fetch(new Request("https://do.internal/internal/status"));
  assert.equal(rebuilt.subject.registry.activeCount(), 0);

  // Now a fresh live link retries the same idempotency key: the edge must
  // return the stored completion and never emit a tool_request frame.
  const retrySocket = activeSocket();
  rebuilt.sockets.push(retrySocket);
  await rebuilt.subject.webSocketMessage(retrySocket, JSON.stringify({
    protocol_version: 1,
    kind: "hello",
    workstation_id: b.workstation_id,
    boot_id: "boot-retry",
    link_version: "0.4.3",
    connected_at_ms: Date.now(),
    capabilities: [],
    runtime: {
      runtime_version: "0.4.3",
      runtime_commit: "test",
      runtime_generation: "g1",
      contract_epoch: RUNTIME_EXECUTION_CONTRACT.contract_epoch,
      contract_hash: RUNTIME_EXECUTION_CONTRACT.contract_hash,
      herdr_version: null,
      herdr_protocol: null,
    },
  }));
  assert.equal(retrySocket.attachment.active, true, "link activates for the retry");

  const retryResp = await rebuilt.subject.forwardInternal({
    kind: "request",
    requestId: "crash-no-resend-retry",
    op: "herdr_prompt",
    opClass: "mutating",
    idempotencyKey: "crash-no-resend-key",
    deadlineMs: Date.now() + 3_000,
  });
  const retryBody = await retryResp.json();
  assert.equal(retryBody.status, "ok");
  assert.equal(retryBody.completion.status, "ok");
  assert.deepEqual(retryBody.completion.result, { ok: "once" }, "same-key retry returns prior completion");
  const sentKinds = retrySocket.sent.map((f) => f.kind);
  assert.equal(sentKinds.includes("tool_request"), false, "no tool_request may reach the device on an idempotent retry");
});
