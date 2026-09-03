import test from "node:test";
import assert from "node:assert/strict";
import { WorkstationDO } from "../dist/workstation-do.js";

class FakeStorage {
  constructor(entries = []) {
    this.map = new Map(entries);
    this.alarm = null;
    this.mutations = [];
  }
  async get(key) { return this.map.get(key); }
  async put(key, value) { this.mutations.push(["put", String(key)]); this.map.set(key, value); }
  async delete(key) { this.mutations.push(["delete", String(key)]); this.map.delete(key); }
  async list({ prefix } = {}) {
    return new Map([...this.map].filter(([key]) => !prefix || String(key).startsWith(prefix)));
  }
  async getAlarm() { return this.alarm; }
  async setAlarm(value) { this.mutations.push(["setAlarm", Number(value)]); this.alarm = Number(value); }
  async deleteAlarm() { this.mutations.push(["deleteAlarm"]); this.alarm = null; }
}

function pending(id, deadlineMs, state = "sent", opClass = "mutating") {
  return {
    requestId: id,
    workstationId: "prod-real-runtime",
    op: "herdr_call",
    opClass,
    argsSummary: { argKeys: ["method"] },
    state,
    createdAtMs: deadlineMs - 1000,
    ...(state === "sent" ? { sentAtMs: deadlineMs - 500 } : {}),
    deadlineMs,
    idempotencyKey: `idem-${id}`,
    contractEpoch: 1,
  };
}

function fakeState(storage, sockets = []) {
  return {
    id: { name: "prod-real-runtime" },
    storage,
    blockConcurrencyWhile: async (fn) => fn(),
    getWebSockets: () => sockets,
    acceptWebSocket: () => {},
  };
}

async function waitForToolRequests(sent, count) {
  for (let attempt = 0; attempt < 20; attempt += 1) {
    if (sent.filter((frame) => frame.kind === "tool_request").length >= count) return;
    await new Promise((resolve) => setImmediate(resolve));
  }
}

test("cold start is read-only and never replays stale durable mutations", async () => {
  const now = Date.now();
  const rows = Array.from({ length: 16 }, (_, i) => {
    const p = pending(`stale-${i}`, now - 1000 - i);
    return [`pending:${p.requestId}`, p];
  });
  const storage = new FakeStorage(rows);
  const sent = [];
  const activeSocket = {
    deserializeAttachment: () => ({ active: true, registered: true }),
    send: (frame) => sent.push(frame),
  };
  const subject = new WorkstationDO(fakeState(storage, [activeSocket]), {});

  const response = await subject.fetch(new Request("https://do/internal/status"));
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.equal(body.activeRequests, 16);
  assert.equal([...storage.map.keys()].filter((key) => key.startsWith("pending:")).length, 16);
  assert.equal([...storage.map.keys()].filter((key) => key.startsWith("completed:")).length, 0);
  assert.equal(sent.length, 0, "rehydration must never replay a stored tool_request");
  assert.deepEqual(storage.mutations, [], "initialization must not spend Durable Storage write quota");
});

test("stale durable mutation backlog cannot starve new ephemeral reads", async () => {
  const now = Date.now();
  const rows = Array.from({ length: 16 }, (_, i) => {
    const p = pending(`stale-${i}`, now - 1000 - i);
    return [`pending:${p.requestId}`, p];
  });
  const storage = new FakeStorage(rows);
  const sent = [];
  const activeSocket = {
    deserializeAttachment: () => ({ active: true, registered: true }),
    send: (frame) => sent.push(JSON.parse(frame)),
  };
  const subject = new WorkstationDO(fakeState(storage, [activeSocket]), {});
  await subject.fetch(new Request("https://do/internal/status"));
  storage.mutations.length = 0;

  const requestId = "read-despite-stale-mutations";
  const responsePromise = subject.forwardInternal({
    kind: "request",
    requestId,
    op: "herdr_inspect",
    deadlineMs: Date.now() + 30_000,
  });
  assert.equal(sent.at(-1)?.kind, "tool_request");
  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: "prod-real-runtime",
    request_id: requestId,
    result: { ok: true },
    served_at_ms: Date.now(),
  });
  const response = await responsePromise;
  assert.equal(response.status, 200);
  assert.deepEqual(storage.mutations, [], "read must remain quota-free despite stale durable backlog");
});

test("cold start keeps future durable mutation without spending an alarm write", async () => {
  const deadline = Date.now() + 60_000;
  const p = pending("future", deadline, "sent", "mutating");
  const storage = new FakeStorage([[`pending:${p.requestId}`, p]]);
  const subject = new WorkstationDO(fakeState(storage), {});

  const response = await subject.fetch(new Request("https://do/internal/status"));
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.equal(body.activeRequests, 1);
  assert.ok(storage.map.has("pending:future"));
  assert.equal(storage.alarm, null);
  assert.deepEqual(storage.mutations, []);
});

test("cold start ignores legacy persisted reads without cleaning them up", async () => {
  const deadline = Date.now() + 60_000;
  const p = { ...pending("legacy-read", deadline, "sent", "read"), op: "herdr_inspect" };
  const storage = new FakeStorage([[`pending:${p.requestId}`, p]]);
  const subject = new WorkstationDO(fakeState(storage), {});

  const response = await subject.fetch(new Request("https://do/internal/status"));
  const body = await response.json();
  assert.equal(body.activeRequests, 0);
  assert.ok(storage.map.has("pending:legacy-read"), "do not burn quota deleting legacy rows on init");
  assert.deepEqual(storage.mutations, []);
});

test("deadline settlement cancels an already-sent Link request exactly once", async () => {
  const deadline = Date.now() + 60_000;
  const p = pending("sent-timeout", deadline, "sent", "read");
  const storage = new FakeStorage([[`pending:${p.requestId}`, p]]);
  const sent = [];
  const activeSocket = {
    deserializeAttachment: () => ({ active: true, registered: true }),
    send: (frame) => sent.push(JSON.parse(frame)),
  };
  const subject = new WorkstationDO(fakeState(storage, [activeSocket]), {});

  await subject.fetch(new Request("https://do/internal/status"));
  await subject.settleAsTimeout(p.requestId);

  assert.equal(storage.map.has(`pending:${p.requestId}`), false);
  assert.equal(storage.map.has(`completed:${p.requestId}`), true);
  assert.equal(sent.length, 1);
  assert.equal(sent[0].kind, "cancel");
  assert.equal(sent[0].request_id, p.requestId);
  assert.equal(sent[0].reason, "deadline exceeded");

  await subject.settleAsTimeout(p.requestId);
  assert.equal(sent.length, 1, "settled timeout must not emit duplicate cancel");
});

test("generation supersede proven not delivered releases mutation idempotency for a real retry", async () => {
  const storage = new FakeStorage();
  const sent = [];
  const activeSocket = {
    deserializeAttachment: () => ({ active: true, registered: true }),
    send: (frame) => sent.push(JSON.parse(frame)),
  };
  const subject = new WorkstationDO(fakeState(storage, [activeSocket]), {});
  const idempotencyKey = "generation-reresolve";
  await subject.fetch(new Request("https://do/internal/status"));

  const first = subject.forwardInternal({
    kind: "request",
    requestId: "generation-old",
    op: "herdr_prompt",
    args: { target: "w1:p1", text: "continue" },
    deadlineMs: Date.now() + 1_000,
    idempotencyKey,
  });
  await waitForToolRequests(sent, 1);
  await subject.handleToolError({
    protocol_version: 1,
    kind: "tool_error",
    workstation_id: "prod-real-runtime",
    request_id: "generation-old",
    code: "runtime_generation_superseded_before_dispatch",
    retryable: true,
    delivery_state: "not_delivered",
    served_at_ms: Date.now(),
  });
  await first;
  assert.equal(storage.map.has(`idem:${idempotencyKey}`), false);

  const second = subject.forwardInternal({
    kind: "request",
    requestId: "generation-new",
    op: "herdr_prompt",
    args: { target: "w1:p1", text: "continue" },
    deadlineMs: Date.now() + 1_000,
    idempotencyKey,
  });
  await waitForToolRequests(sent, 2);
  assert.equal(sent.filter((frame) => frame.kind === "tool_request").length, 2);
  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: "prod-real-runtime",
    request_id: "generation-new",
    result: { ok: true },
    served_at_ms: Date.now(),
  });
  await second;
  assert.equal(storage.map.get(`idem:${idempotencyKey}`).requestId, "generation-new");
});
