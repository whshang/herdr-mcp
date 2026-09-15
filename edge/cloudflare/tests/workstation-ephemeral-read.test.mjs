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

test("private exec.wait herdr_call is recomputed read-only and spends zero Durable Storage mutations", async () => {
  const { subject, events } = makeSubject();
  await init(subject, events);
  const requestId = "exec-wait-read";
  const pending = subject.forwardInternal({
    kind: "request",
    requestId,
    op: "herdr_call",
    args: { method: "herdr_mcp.exec.wait", params: JSON.stringify({ session_id: "es-1", offset: 10 }) },
    opClass: "mutating",
    deadlineMs: Date.now() + 30_000,
  });
  const sent = events.find((event) => event[0] === "send" && event[1].kind === "tool_request");
  assert.ok(sent, "exec.wait should reach the Link");
  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: "prod-real-runtime",
    request_id: requestId,
    result: { ok: true, running: true, next_offset: 10, wait_timed_out: true },
    served_at_ms: Date.now(),
  });
  const response = await pending;
  assert.equal(response.status, 200);
  assert.deepEqual(events.filter((event) => event[0] !== "send"), []);
});

test("Edge settlement grace does not widen the legacy Link wire timeout", async () => {
  const { subject, events } = makeSubject();
  await init(subject, events);
  const requestId = "settlement-grace-wire-cap";
  const pending = subject.forwardInternal({
    kind: "request",
    requestId,
    op: "herdr_exec",
    opClass: "unknown",
    deadlineMs: Date.now() + 65_000,
  });
  await new Promise((resolve) => setImmediate(resolve));
  const sent = events.find((event) => event[0] === "send" && event[1].kind === "tool_request");
  assert.ok(sent, "request should reach the Link");
  assert.equal(sent[1].timeout_ms, 60_000, "legacy/current Link wire timeout must remain <=60s");
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


test("identical read dedupe keys coalesce in-flight and recent host duplicates", async () => {
  const { subject, events } = makeSubject();
  await init(subject, events);
  const deadlineMs = Date.now() + 30_000;
  const first = subject.forwardInternal({
    kind: "request",
    requestId: "dedupe-read-1",
    op: "herdr_git",
    args: { root: "/repo", action: "status" },
    readDedupeKey: "read_same_fixture",
    deadlineMs,
  });
  await new Promise((resolve) => setImmediate(resolve));
  const second = subject.forwardInternal({
    kind: "request",
    requestId: "dedupe-read-2",
    op: "herdr_git",
    args: { root: "/repo", action: "status" },
    readDedupeKey: "read_same_fixture",
    deadlineMs,
  });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(
    events.filter((event) => event[0] === "send" && event[1].kind === "tool_request").length,
    1,
    "an in-flight duplicate read must reuse the original Link request",
  );
  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: "prod-real-runtime",
    request_id: "dedupe-read-1",
    result: { ok: true, output: "clean" },
    served_at_ms: Date.now(),
  });
  const [firstResponse, secondResponse] = await Promise.all([first, second]);
  assert.equal(firstResponse.status, 200);
  assert.equal(secondResponse.status, 200);

  const thirdResponse = await subject.forwardInternal({
    kind: "request",
    requestId: "dedupe-read-3",
    op: "herdr_git",
    args: { root: "/repo", action: "status" },
    readDedupeKey: "read_same_fixture",
    deadlineMs: Date.now() + 30_000,
  });
  assert.equal(thirdResponse.status, 200);
  assert.equal(
    events.filter((event) => event[0] === "send" && event[1].kind === "tool_request").length,
    1,
    "an immediate settled duplicate read must reuse the recent completion",
  );
  assert.deepEqual(events.filter((event) => event[0] !== "send"), [], "read dedupe must stay storage-free");
});

test("a mutation invalidates recent read dedupe state", async () => {
  const { subject, events } = makeSubject();
  await init(subject, events);
  const readKey = "read_invalidated_by_mutation";
  const first = subject.forwardInternal({
    kind: "request",
    requestId: "invalidate-read-1",
    op: "herdr_git",
    args: { root: "/repo", action: "status" },
    readDedupeKey: readKey,
    deadlineMs: Date.now() + 30_000,
  });
  await new Promise((resolve) => setImmediate(resolve));
  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: "prod-real-runtime",
    request_id: "invalidate-read-1",
    result: { ok: true, output: "before" },
    served_at_ms: Date.now(),
  });
  await first;

  const mutation = subject.forwardInternal({
    kind: "request",
    requestId: "invalidate-mutation",
    op: "herdr_prompt",
    args: { target: "worker", text: "fixture" },
    idempotencyKey: "invalidate-mutation-idem",
    deadlineMs: Date.now() + 100,
  });
  await new Promise((resolve) => setImmediate(resolve));
  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: "prod-real-runtime",
    request_id: "invalidate-mutation",
    result: { ok: true },
    served_at_ms: Date.now(),
  });
  await mutation;

  const afterMutation = subject.forwardInternal({
    kind: "request",
    requestId: "invalidate-read-2",
    op: "herdr_git",
    args: { root: "/repo", action: "status" },
    readDedupeKey: readKey,
    deadlineMs: Date.now() + 30_000,
  });
  await new Promise((resolve) => setImmediate(resolve));
  const sent = events.filter((event) => event[0] === "send" && event[1].kind === "tool_request");
  assert.deepEqual(
    sent.map((event) => event[1].request_id),
    ["invalidate-read-1", "invalidate-mutation", "invalidate-read-2"],
    "the same read must be delivered again after a mutation boundary",
  );
  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: "prod-real-runtime",
    request_id: "invalidate-read-2",
    result: { ok: true, output: "after" },
    served_at_ms: Date.now(),
  });
  assert.equal((await afterMutation).status, 200);
});

test("a read that settles after a mutation cannot repopulate stale dedupe state", async () => {
  const { subject, events } = makeSubject();
  await init(subject, events);
  const readKey = "read_crossing_mutation";
  const staleRead = subject.forwardInternal({
    kind: "request",
    requestId: "cross-read-1",
    op: "herdr_git",
    args: { root: "/repo", action: "status" },
    readDedupeKey: readKey,
    deadlineMs: Date.now() + 30_000,
  });
  await new Promise((resolve) => setImmediate(resolve));

  const mutation = subject.forwardInternal({
    kind: "request",
    requestId: "cross-mutation",
    op: "herdr_prompt",
    args: { target: "worker", text: "fixture" },
    idempotencyKey: "cross-mutation-idem",
    deadlineMs: Date.now() + 100,
  });
  await new Promise((resolve) => setImmediate(resolve));
  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: "prod-real-runtime",
    request_id: "cross-mutation",
    result: { ok: true },
    served_at_ms: Date.now(),
  });
  await mutation;

  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: "prod-real-runtime",
    request_id: "cross-read-1",
    result: { ok: true, output: "stale" },
    served_at_ms: Date.now(),
  });
  await staleRead;

  const freshRead = subject.forwardInternal({
    kind: "request",
    requestId: "cross-read-2",
    op: "herdr_git",
    args: { root: "/repo", action: "status" },
    readDedupeKey: readKey,
    deadlineMs: Date.now() + 30_000,
  });
  await new Promise((resolve) => setImmediate(resolve));
  const sent = events.filter((event) => event[0] === "send" && event[1].kind === "tool_request");
  assert.deepEqual(
    sent.map((event) => event[1].request_id),
    ["cross-read-1", "cross-mutation", "cross-read-2"],
    "a pre-mutation read settling late must not satisfy a post-mutation duplicate",
  );
  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: "prod-real-runtime",
    request_id: "cross-read-2",
    result: { ok: true, output: "fresh" },
    served_at_ms: Date.now(),
  });
  assert.equal((await freshRead).status, 200);
});

test("one noisy principal cannot consume all downstream request capacity", async () => {
  const { subject, events } = makeSubject();
  await init(subject, events);

  const noisy = "principal:noisy";
  const quiet = "principal:quiet";
  const noisyPending = [];
  for (let i = 0; i < 8; i += 1) {
    noisyPending.push(subject.forwardInternal({
      kind: "request",
      requestId: `noisy-${i}`,
      resourcePrincipalRef: noisy,
      op: "herdr_inspect",
      deadlineMs: Date.now() + 30_000,
    }));
  }
  assert.equal(events.filter((event) => event[0] === "send").length, 8);

  const rejected = await subject.forwardInternal({
    kind: "request",
    requestId: "noisy-over-limit",
    resourcePrincipalRef: noisy,
    op: "herdr_inspect",
    deadlineMs: Date.now() + 30_000,
  });
  assert.equal(rejected.status, 429);
  const rejection = await rejected.json();
  assert.equal(rejection.error.code, "edge_capacity_exceeded");
  assert.equal(rejection.error.retryable, true);
  assert.equal(rejection.error.delivery_state, "not_delivered");
  assert.equal(rejection.error.retry_after_ms, 1_000);
  assert.deepEqual(rejection.error.details, {
    quota_scope: "principal",
    resource_key: noisy,
    limit: 8,
    active: 8,
  });

  const quietPending = subject.forwardInternal({
    kind: "request",
    requestId: "quiet-0",
    resourcePrincipalRef: quiet,
    op: "herdr_inspect",
    deadlineMs: Date.now() + 30_000,
  });
  assert.equal(events.filter((event) => event[0] === "send").length, 9,
    "an unrelated principal must retain downstream progress capacity");
  await subject.handleToolResult({
    protocol_version: 1,
    kind: "tool_result",
    workstation_id: "prod-real-runtime",
    request_id: "quiet-0",
    result: { ok: true },
    served_at_ms: Date.now(),
  });
  assert.equal((await quietPending).status, 200);

  for (let i = 0; i < 8; i += 1) {
    await subject.handleToolResult({
      protocol_version: 1,
      kind: "tool_result",
      workstation_id: "prod-real-runtime",
      request_id: `noisy-${i}`,
      result: { ok: true },
      served_at_ms: Date.now(),
    });
  }
  const settled = await Promise.all(noisyPending);
  assert.ok(settled.every((response) => response.status === 200));
});

test("principal admission counts current Link occupancy past its deadline and idempotency replay bypasses saturation", async () => {
  const { subject, events } = makeSubject();
  await init(subject, events);
  const principal = "principal:saturated";

  subject.registry.add({
    requestId: "replay-source",
    workstationId: "prod-real-runtime",
    resourcePrincipalRef: principal,
    op: "herdr_prompt",
    opClass: "mutating",
    argsSummary: { argKeys: [] },
    deadlineMs: Date.now() + 30_000,
    idempotencyKey: "replay-key",
  });
  subject.registry.markSent("replay-source", Date.now());
  subject.registry.settle("replay-source", {
    status: "ok",
    result: { replayed: true },
    servedAtMs: Date.now(),
  });

  const active = [];
  for (let i = 0; i < 8; i += 1) {
    const requestId = `occupied-${i}`;
    active.push(subject.forwardInternal({
      kind: "request",
      requestId,
      resourcePrincipalRef: principal,
      op: "herdr_prompt",
      args: { target: "worker", text: `occupy-${i}` },
      idempotencyKey: `occupied-idem-${i}`,
      deadlineMs: Date.now() + 30_000,
    }));
  }
  for (let i = 0; i < 8; i += 1) {
    const entry = subject.registry.get(`occupied-${i}`);
    assert.ok(entry);
    entry.deadlineMs = Date.now() - 1;
  }
  await new Promise((resolve) => setImmediate(resolve));

  const blockedRead = await subject.forwardInternal({
    kind: "request",
    requestId: "expired-principal-read",
    resourcePrincipalRef: principal,
    op: "herdr_inspect",
    deadlineMs: Date.now() + 30_000,
  });
  assert.equal(blockedRead.status, 429,
    "current Link work must keep consuming principal capacity even if its stored deadline has passed");

  const sendsBeforeReplay = events.filter((event) => event[0] === "send").length;
  const replay = await subject.forwardInternal({
    kind: "request",
    requestId: "replay-at-limit",
    resourcePrincipalRef: principal,
    op: "herdr_prompt",
    args: { target: "worker", text: "must not resend" },
    idempotencyKey: "replay-key",
    deadlineMs: Date.now() + 30_000,
  });
  assert.equal(replay.status, 200);
  const replayBody = await replay.json();
  assert.equal(replayBody.completion.result.replayed, true);
  assert.equal(events.filter((event) => event[0] === "send").length, sendsBeforeReplay,
    "an idempotent replay must return the stored completion without consuming capacity or resending");

  for (let i = 0; i < 8; i += 1) {
    await subject.handleToolResult({
      protocol_version: 1,
      kind: "tool_result",
      workstation_id: "prod-real-runtime",
      request_id: `occupied-${i}`,
      result: { ok: true },
      served_at_ms: Date.now(),
    });
  }
  assert.ok((await Promise.all(active)).every((response) => response.status === 200));
});

test("ordinary mutation settlement bounds durable completed and idempotency rows", async () => {
  const { subject, storage, events } = makeSubject();
  await init(subject, events);
  subject.limits.maxCompletedRecords = 2;

  for (let i = 0; i < 4; i += 1) {
    const requestId = `retained-${i}`;
    const pending = subject.forwardInternal({
      kind: "request",
      requestId,
      resourcePrincipalRef: "principal:retention",
      op: "herdr_prompt",
      args: { target: "worker", text: `turn-${i}` },
      idempotencyKey: `retained-idem-${i}`,
      deadlineMs: Date.now() + 30_000,
    });
    await new Promise((resolve) => setImmediate(resolve));
    await subject.handleToolResult({
      protocol_version: 1,
      kind: "tool_result",
      workstation_id: "prod-real-runtime",
      request_id: requestId,
      result: { ok: true, i },
      served_at_ms: Date.now() + i,
    });
    assert.equal((await pending).status, 200);
  }

  const completedKeys = [...storage.map.keys()].filter((key) => String(key).startsWith("completed:"));
  const idemKeys = [...storage.map.keys()].filter((key) => String(key).startsWith("idem:"));
  assert.equal(completedKeys.length, 2);
  assert.equal(idemKeys.length, 2);
  assert.deepEqual(completedKeys.sort(), ["completed:retained-2", "completed:retained-3"]);
  assert.deepEqual(idemKeys.sort(), ["idem:retained-idem-2", "idem:retained-idem-3"]);
});
