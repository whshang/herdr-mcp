import test from "node:test";
import assert from "node:assert/strict";
import { WorkstationDO } from "../dist/workstation-do.js";

class FakeStorage {
  constructor(events) {
    this.events = events;
    this.map = new Map();
    this.alarm = null;
    this.failWrites = false;
  }
  async get(key) { return this.map.get(key); }
  async put(key, value) {
    this.events.push(["put", String(key)]);
    if (this.failWrites) throw new Error("rows_written quota exceeded");
    this.map.set(key, value);
  }
  async delete(key) {
    this.events.push(["delete", String(key)]);
    if (this.failWrites) throw new Error("rows_written quota exceeded");
    this.map.delete(key);
  }
  async list({ prefix } = {}) {
    return new Map([...this.map].filter(([key]) => !prefix || String(key).startsWith(prefix)));
  }
  async getAlarm() { return this.alarm; }
  async setAlarm(value) {
    this.events.push(["setAlarm", Number(value)]);
    if (this.failWrites) throw new Error("rows_written quota exceeded");
    this.alarm = Number(value);
  }
  async deleteAlarm() {
    this.events.push(["deleteAlarm"]);
    if (this.failWrites) throw new Error("rows_written quota exceeded");
    this.alarm = null;
  }
}

function makeSubject() {
  const events = [];
  const storage = new FakeStorage(events);
  const socket = {
    deserializeAttachment: () => ({ active: true, registered: true }),
    send: (frame) => events.push(["send", JSON.parse(frame)]),
    serializeAttachment: () => {},
    close: () => {},
  };
  const state = {
    id: { name: "prod-real-runtime" },
    storage,
    blockConcurrencyWhile: async (fn) => fn(),
    getWebSockets: () => [socket],
    acceptWebSocket: () => {},
  };
  return { subject: new WorkstationDO(state, {}), storage, events };
}

async function init(subject, events) {
  const response = await subject.fetch(new Request("https://do/internal/status"));
  assert.equal(response.status, 200);
  assert.deepEqual(events, [], "cold init must be read-only");
}

const READ_OPS = [
  "herdr_inspect",
  "herdr_since",
  "herdr_methods",
  "herdr_skill",
  "herdr_fs_image",
  "herdr_fs_list",
  "herdr_fs_grep",
  "herdr_fs_read",
  "herdr_git",
  "herdr_exec_read",
];

test("known reads settle successfully with zero Durable Storage mutations", async () => {
  for (const [index, op] of READ_OPS.entries()) {
    const { subject, events } = makeSubject();
    await init(subject, events);
    const requestId = `read-${index}`;
    const pending = subject.forwardInternal({
      kind: "request",
      requestId,
      op,
      opClass: "mutating",
      deadlineMs: Date.now() + 30_000,
    });
    const sent = events.find((event) => event[0] === "send" && event[1].kind === "tool_request");
    assert.ok(sent, `${op} should reach the Link`);
    await subject.handleToolResult({
      protocol_version: 1,
      kind: "tool_result",
      workstation_id: "prod-real-runtime",
      request_id: requestId,
      result: { ok: true },
      served_at_ms: Date.now(),
    });
    const response = await pending;
    assert.equal(response.status, 200);
    const storageMutations = events.filter((event) => event[0] !== "send");
    assert.deepEqual(storageMutations, [], `${op} must not spend DO write quota`);
  }
});

test("read timeout emits cancel but performs zero Durable Storage mutations", async () => {
  const { subject, events } = makeSubject();
  await init(subject, events);
  const requestId = "read-timeout";
  const pending = subject.forwardInternal({
    kind: "request",
    requestId,
    op: "herdr_inspect",
    deadlineMs: Date.now() + 30_000,
  });
  await subject.settleAsTimeout(requestId);
  const response = await pending;
  assert.equal(response.status, 200);
  assert.equal(events.filter((event) => event[0] === "send" && event[1].kind === "cancel").length, 1);
  assert.deepEqual(events.filter((event) => event[0] !== "send"), []);
});

test("read tool_error settles with zero Durable Storage mutations", async () => {
  const { subject, events } = makeSubject();
  await init(subject, events);
  const requestId = "read-error";
  const pending = subject.forwardInternal({
    kind: "request",
    requestId,
    op: "herdr_fs_read",
    idempotencyKey: "read-idem-must-stay-ephemeral",
    deadlineMs: Date.now() + 30_000,
  });
  await subject.handleToolError({
    protocol_version: 1,
    kind: "tool_error",
    workstation_id: "prod-real-runtime",
    request_id: requestId,
    code: "local_mcp_bad_request",
    retryable: false,
    message: "fixture",
    served_at_ms: Date.now(),
  });
  const response = await pending;
  assert.equal(response.status, 200);
  assert.deepEqual(events.filter((event) => event[0] !== "send"), [], "read errors/idempotency must not persist");
});

test("read link loss is retryable and performs zero request-ledger/alarm mutations", async () => {
  const { subject, events } = makeSubject();
  await init(subject, events);
  const requestId = "read-link-drop";
  const pending = subject.forwardInternal({
    kind: "request",
    requestId,
    op: "herdr_since",
    deadlineMs: Date.now() + 30_000,
  });
  await subject.handleLinkGone("test.link_drop", {});
  const response = await pending;
  const body = await response.json();
  assert.equal(body.completion?.status, "error");
  assert.equal(body.completion?.error?.retryable, true);
  assert.deepEqual(events.filter((event) => event[0] !== "send"), []);
});

test("DO recomputes classification so spoofed mutation remains durable", async () => {
  const { subject, events } = makeSubject();
  await init(subject, events);
  const requestId = "spoofed-prompt";
  const pending = subject.forwardInternal({
    kind: "request",
    requestId,
    op: "herdr_prompt",
    opClass: "read",
    deadlineMs: Date.now() + 100,
  });
  await new Promise((resolve) => setImmediate(resolve));
  const firstPut = events.findIndex((event) => event[0] === "put" && event[1] === `pending:${requestId}`);
  const alarm = events.findIndex((event) => event[0] === "setAlarm");
  const send = events.findIndex((event) => event[0] === "send" && event[1].kind === "tool_request");
  assert.ok(firstPut >= 0, "mutation must create durable pending state");
  assert.ok(alarm > firstPut, "mutation deadline alarm must be durable after pending state");
  assert.ok(send > alarm, "all mutation durability setup must finish before Link send");
  assert.equal(
    events.filter((event) => event[0] === "put" && event[1] === `pending:${requestId}`).length,
    1,
    "mutation should need only one pending-row write before delivery",
  );
  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: "prod-real-runtime",
    request_id: requestId,
    result: { ok: true },
    served_at_ms: Date.now(),
  });
  await pending;
});

test("mutation storage failure is fail-closed before Link delivery", async () => {
  const { subject, storage, events } = makeSubject();
  await init(subject, events);
  storage.failWrites = true;
  await assert.rejects(
    subject.forwardInternal({
      kind: "request",
      requestId: "quota-blocked-mutation",
      op: "herdr_prompt",
      opClass: "read",
      deadlineMs: Date.now() + 30_000,
    }),
    /rows_written quota exceeded/,
  );
  assert.equal(events.filter((event) => event[0] === "send" && event[1].kind === "tool_request").length, 0);
});

test("ephemeral reads still succeed when Durable Storage writes are exhausted", async () => {
  const { subject, storage, events } = makeSubject();
  await init(subject, events);
  storage.failWrites = true;
  const requestId = "read-under-quota";
  const pending = subject.forwardInternal({
    kind: "request",
    requestId,
    op: "herdr_fs_read",
    deadlineMs: Date.now() + 30_000,
  });
  assert.equal(events.filter((event) => event[0] === "send" && event[1].kind === "tool_request").length, 1);
  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: "prod-real-runtime",
    request_id: requestId,
    result: { ok: true, path: "README.md" },
    served_at_ms: Date.now(),
  });
  const response = await pending;
  assert.equal(response.status, 200);
  assert.deepEqual(events.filter((event) => event[0] !== "send"), []);
});

test("online link drop settles ephemeral reads even when session persist hits write quota", async () => {
  const { serializeSession } = await import("../dist/state.js");
  const events = [];
  const storage = new FakeStorage(events);
  storage.map.set(
    "session",
    serializeSession({
      schemaVersion: 1,
      workstationId: "prod-real-runtime",
      status: "online",
      connectedAtMs: Date.now() - 1_000,
      lastSeenAtMs: Date.now() - 1_000,
    }),
  );
  const socket = {
    deserializeAttachment: () => ({ active: true, registered: true }),
    send: (frame) => events.push(["send", JSON.parse(frame)]),
    serializeAttachment: () => {},
    close: () => {},
  };
  const state = {
    id: { name: "prod-real-runtime" },
    storage,
    blockConcurrencyWhile: async (fn) => fn(),
    getWebSockets: () => [socket],
    acceptWebSocket: () => {},
  };
  const subject = new WorkstationDO(state, {});
  await init(subject, events);
  storage.failWrites = true;

  const requestId = "read-link-drop-under-quota";
  const pending = subject.forwardInternal({
    kind: "request",
    requestId,
    op: "herdr_inspect",
    deadlineMs: Date.now() + 30_000,
  });
  await subject.handleLinkGone("test.link_drop_quota", {});
  const response = await pending;
  const body = await response.json();
  assert.equal(body.completion?.status, "error");
  assert.equal(body.completion?.error?.retryable, true);
  assert.deepEqual(
    events.filter((event) => event[0] !== "send"),
    [["put", "session"]],
    "only the offline session checkpoint may attempt a write; reads must still settle",
  );
  assert.equal(storage.map.get("session") !== undefined, true, "failed put must not clear the prior online session row");
});
