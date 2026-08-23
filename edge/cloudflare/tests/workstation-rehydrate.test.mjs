import test from "node:test";
import assert from "node:assert/strict";
import { WorkstationDO } from "../dist/workstation-do.js";

class FakeStorage {
  constructor(entries = []) {
    this.map = new Map(entries);
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

test("cold start settles a full stale pending registry without replaying operations", async () => {
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
  assert.equal(body.active_requests, 0);
  assert.equal([...storage.map.keys()].filter((key) => key.startsWith("pending:")).length, 0);
  assert.equal([...storage.map.keys()].filter((key) => key.startsWith("completed:")).length, 16);
  assert.equal(sent.length, 0, "rehydration must never replay a stored tool_request");
});

test("cold start keeps future pending work and re-arms its deadline", async () => {
  const deadline = Date.now() + 60_000;
  const p = pending("future", deadline, "sent", "read");
  const storage = new FakeStorage([[`pending:${p.requestId}`, p]]);
  const subject = new WorkstationDO(fakeState(storage), {});

  const response = await subject.fetch(new Request("https://do/internal/status"));
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.equal(body.active_requests, 1);
  assert.ok(storage.map.has("pending:future"));
  assert.equal(storage.alarm, deadline);
});
