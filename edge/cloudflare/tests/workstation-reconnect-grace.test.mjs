import test from "node:test";
import assert from "node:assert/strict";
import { WorkstationDO } from "../dist/workstation-do.js";

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
