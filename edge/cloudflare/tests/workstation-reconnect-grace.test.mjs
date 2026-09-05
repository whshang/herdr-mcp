import test from "node:test";
import assert from "node:assert/strict";
import { WorkstationDO } from "../dist/workstation-do.js";
import { EPOCH2_CONTRACT } from "../dist/contracts/epoch2.js";

class FakeStorage {
  constructor(events) {
    this.events = events;
    this.map = new Map();
  }
  async get(key) { return this.map.get(key); }
  async put(key, value) { this.events.push(["put", String(key)]); this.map.set(key, value); }
  async delete(key) { this.events.push(["delete", String(key)]); this.map.delete(key); }
  async list({ prefix } = {}) {
    return new Map([...this.map].filter(([key]) => !prefix || String(key).startsWith(prefix)));
  }
  async getAlarm() { return null; }
  async setAlarm(value) { this.events.push(["setAlarm", Number(value)]); }
  async deleteAlarm() { this.events.push(["deleteAlarm"]); }
}

function activeSocket() {
  return {
    deserializeAttachment: () => ({ active: true, registered: true }),
    serializeAttachment: () => {},
    send: () => {},
    close: () => {},
  };
}

function makeSubject() {
  const events = [];
  const storage = new FakeStorage(events);
  const sockets = [];
  const state = {
    id: { name: "prod-real-runtime" },
    storage,
    blockConcurrencyWhile: async (fn) => fn(),
    getWebSockets: () => sockets,
    acceptWebSocket: () => {},
  };
  return { subject: new WorkstationDO(state, { LINK_RECONNECT_GRACE_MS: "100" }), sockets, events };
}

async function init(subject) {
  const response = await subject.fetch(new Request("https://do/internal/status"));
  assert.equal(response.status, 200);
}

test("reconnect grace fails fast for a workstation that has never connected", async () => {
  const { subject, events } = makeSubject();
  await init(subject);
  const started = Date.now();
  assert.equal(await subject.waitForActiveLink(Date.now() + 1_000), false);
  assert.ok(Date.now() - started < 80, "never-connected workstation must not spend reconnect grace");
  assert.deepEqual(events, [], "grace decision must not mutate Durable Storage or alarms");
});

test("offline forward returns a self-describing bounded recovery policy", async () => {
  const { subject, events } = makeSubject();
  await init(subject);
  const response = await subject.forwardInternal({
    kind: "request",
    requestId: "offline-policy",
    op: "herdr_inspect",
    deadlineMs: Date.now() + 1_000,
  });
  assert.equal(response.status, 503);
  const body = await response.json();
  assert.equal(body.status, "error");
  assert.equal(body.error.code, "workstation_offline");
  assert.equal(body.error.retryable, true);
  assert.equal(body.error.delivery_state, "not_delivered");
  assert.equal(body.error.retry_after_ms, 5_000);
  assert.deepEqual(body.error.recovery, {
    action: "retry_read_only_probe",
    probe_tool: "herdr_inspect",
    max_attempts: 3,
    backoff_ms: [5_000, 10_000, 20_000],
    mutation_replay: "only_after_not_delivered_or_verified_not_applied",
  });
  assert.deepEqual(events, [], "offline recovery metadata must not consume Durable Storage writes or alarms");
});

test("recently disconnected workstation waits and wakes immediately when a validated link returns", async () => {
  const { subject, sockets, events } = makeSubject();
  await init(subject);
  const now = Date.now();
  subject.session = {
    schemaVersion: 1,
    workstationId: "prod-real-runtime",
    status: "offline",
    connectedAtMs: now - 1_000,
    disconnectedAtMs: now,
  };
  const started = Date.now();
  const waiting = subject.waitForActiveLink(Date.now() + 1_000);
  setTimeout(() => {
    sockets.push(activeSocket());
    subject.notifyLinkAvailable();
  }, 15);
  assert.equal(await waiting, true);
  const elapsed = Date.now() - started;
  assert.ok(elapsed >= 10 && elapsed < 80, `validated link should wake grace promptly, elapsed=${elapsed}`);
  assert.deepEqual(events, [], "reconnect grace must remain process-local with zero DO writes/alarms");
});

test("validated hello records disconnect recovery duration and count in durable status", async () => {
  const { subject } = makeSubject();
  await init(subject);
  const disconnectedAtMs = Date.now() - 42;
  subject.session = {
    schemaVersion: 1,
    workstationId: "prod-real-runtime",
    status: "offline",
    connectedAtMs: disconnectedAtMs - 1_000,
    disconnectedAtMs,
    reconnectCount: 3,
  };
  const sent = [];
  let attachment = null;
  const ws = {
    serializeAttachment: (value) => { attachment = value; },
    deserializeAttachment: () => attachment ?? {},
    send: (frame) => sent.push(JSON.parse(frame)),
    close: () => {},
  };
  await subject.handleHello({
    protocol_version: 1,
    kind: "hello",
    workstation_id: "prod-real-runtime",
    link_version: "0.4.6",
    boot_id: "boot-recovered",
    connected_at_ms: Date.now(),
    runtime: {
      runtime_version: "0.4.6",
      runtime_generation: "rust-recovered",
      contract_epoch: EPOCH2_CONTRACT.contract_epoch,
      contract_hash: EPOCH2_CONTRACT.contract_hash,
    },
    capabilities: [],
  }, ws);

  const response = await subject.fetch(new Request("https://do/internal/status"));
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.equal(body.status, "online");
  assert.equal(body.reconnectCount, 4);
  assert.ok(body.lastReconnectDurationMs >= 0);
  assert.ok(body.lastReconnectDurationMs < 5_000);
  assert.equal(body.lastReconnectCrossedRecycleThreshold, false);
  assert.equal(typeof body.lastRecoveredAtMs, "number");
  assert.equal(sent.at(-1).kind, "hello_ack");
  assert.equal(sent.at(-1).ok, true);
});
