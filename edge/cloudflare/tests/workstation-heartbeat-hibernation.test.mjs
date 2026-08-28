import test from "node:test";
import assert from "node:assert/strict";
import { WorkstationDO } from "../dist/workstation-do.js";
import {
  HEARTBEAT_PERSIST_THROTTLE_MS,
  EDGE_STATUS_REPLY_INTERVAL_MS,
} from "../dist/limits.js";
import { sessionFromClaims, serializeSession } from "../dist/state.js";

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

function fakeState(storage, sockets = []) {
  return {
    id: { name: "ws-hibernate" },
    storage,
    blockConcurrencyWhile: async (fn) => fn(),
    getWebSockets: () => sockets,
    acceptWebSocket: () => {},
  };
}

function steadyHeartbeat(runtime) {
  return {
    protocol_version: 1,
    kind: "heartbeat",
    workstation_id: "ws-hibernate",
    boot_id: "boot1",
    sent_at_ms: Date.now(),
    active_requests: 0,
    runtime,
  };
}

const BASE_RUNTIME = {
  runtime_version: "0.4.0-alpha.18",
  runtime_generation: "g1",
  herdr_protocol: "20",
};

test("steady-state heartbeats avoid DO storage writes and status replies", async () => {
  const session = sessionFromClaims({
    workstationId: "ws-hibernate",
    linkVersion: "0.4.0",
    bootId: "boot1",
    protocolVersion: "1",
    connectedAtMs: 1_000,
    runtimeVersion: BASE_RUNTIME.runtime_version,
    runtimeGeneration: BASE_RUNTIME.runtime_generation,
    herdProtocolVersion: BASE_RUNTIME.herdr_protocol,
  });
  const storage = new FakeStorage([["session", serializeSession(session)]]);
  const sent = [];
  const ws = {
    deserializeAttachment: () => ({ active: true, registered: true }),
    send: (frame) => sent.push(JSON.parse(frame)),
  };
  const subject = new WorkstationDO(fakeState(storage, [ws]), {});
  await subject.fetch(new Request("https://do/internal/status"));
  // Warm runtimeStatus/lastSeen checkpoints so subsequent identical beats are steady-state.
  await subject.handleHeartbeat(steadyHeartbeat(BASE_RUNTIME), ws);
  storage.mutations.length = 0;
  sent.length = 0;

  for (let i = 0; i < 5; i += 1) {
    await subject.handleHeartbeat(
      steadyHeartbeat(BASE_RUNTIME),
      ws,
    );
  }

  assert.deepEqual(storage.mutations, [], "steady-state beats must not persist session");
  assert.equal(sent.length, 0, "steady-state beats must not send status within reply interval");
});

test("runtime change on heartbeat persists immediately and replies status", async () => {
  const session = sessionFromClaims({
    workstationId: "ws-hibernate",
    linkVersion: "0.4.0",
    bootId: "boot1",
    protocolVersion: "1",
    connectedAtMs: 1_000,
    runtimeVersion: BASE_RUNTIME.runtime_version,
    runtimeGeneration: BASE_RUNTIME.runtime_generation,
    herdProtocolVersion: BASE_RUNTIME.herdr_protocol,
  });
  const storage = new FakeStorage([["session", serializeSession(session)]]);
  const sent = [];
  const ws = {
    deserializeAttachment: () => ({ active: true, registered: true }),
    send: (frame) => sent.push(JSON.parse(frame)),
  };
  const subject = new WorkstationDO(fakeState(storage, [ws]), {});
  await subject.fetch(new Request("https://do/internal/status"));
  storage.mutations.length = 0;

  await subject.handleHeartbeat(
    steadyHeartbeat({
      ...BASE_RUNTIME,
      runtime_generation: "g2",
    }),
    ws,
  );

  assert.equal(storage.mutations.length, 1);
  assert.equal(storage.mutations[0][0], "put");
  assert.equal(sent.length, 1);
  assert.equal(sent[0]?.kind, "status");
});

test("heartbeat persistence throttle still applies without runtime change", async () => {
  const session = sessionFromClaims({
    workstationId: "ws-hibernate",
    linkVersion: "0.4.0",
    bootId: "boot1",
    protocolVersion: "1",
    connectedAtMs: 1_000,
    runtimeVersion: BASE_RUNTIME.runtime_version,
    runtimeGeneration: BASE_RUNTIME.runtime_generation,
    herdProtocolVersion: BASE_RUNTIME.herdr_protocol,
  });
  const storage = new FakeStorage([["session", serializeSession(session)]]);
  const ws = {
    deserializeAttachment: () => ({ active: true, registered: true }),
    send: () => {},
  };
  const subject = new WorkstationDO(fakeState(storage, [ws]), {});
  await subject.fetch(new Request("https://do/internal/status"));
  subject.lastSeenPersistedAtMs = Date.now() - HEARTBEAT_PERSIST_THROTTLE_MS - 1;
  subject.lastEdgeStatusReplyAtMs = Date.now() - EDGE_STATUS_REPLY_INTERVAL_MS - 1;
  storage.mutations.length = 0;

  await subject.handleHeartbeat(steadyHeartbeat(BASE_RUNTIME), ws);
  assert.equal(storage.mutations.length, 1, "throttled checkpoint must still persist");
});
