/**
 * Tests for herdr-link client with canonical Relay Protocol v1 wire frames.
 *
 * All raw WebSocket JSON is canonical: protocol_version=1, kind
 * (hello/hello_ack/heartbeat/status/tool_request/tool_result/tool_error/
 * cancel/cancel_ack). Old wire fields (v, type, ping, pong, runtime_status,
 * drain, shutdown, error, request, response) are gone from the wire.
 * Internal response wrappers (final_status, runtime_ms) are also absent from
 * the wire — they live only in the event emission layer.
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  HerdrLink,
  LINK_CODE,
  LINK_VERSION,
  redactUrl,
  clampRange,
  clampRequestTimeout,
  classifyFatalCode,
  extractRequestId,
  decodeSocketData,
} from "../dist/link/client.js";
import { buildEdgeUrl, buildLinkAuthProtocol, buildLinkProtocols } from "../dist/link/socket.js";

const WS = { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 };

/** Deterministic fake WHATWG-style WebSocket driven by the test. */
class FakeSocket {
  constructor(url) {
    this.url = url;
    this.readyState = WS.CONNECTING;
    this.listeners = new Map();
    this.sentRaw = [];
    this.sent = [];
    this.closeCalledWith = null;
    this.terminatedWith = null;
  }

  addEventListener(type, fn) {
    if (!this.listeners.has(type)) this.listeners.set(type, []);
    this.listeners.get(type).push(fn);
  }

  removeEventListener(type, fn) {
    const list = this.listeners.get(type) ?? [];
    const i = list.indexOf(fn);
    if (i >= 0) list.splice(i, 1);
  }

  #fire(type, ev = {}) {
    for (const fn of this.listeners.get(type) ?? []) fn(ev);
  }

  send(data) {
    if (this.readyState === WS.CLOSED) throw new Error("send on closed socket");
    this.sentRaw.push(String(data));
    this.sent.push(JSON.parse(String(data)));
  }

  close(code = 1000, reason = "") {
    if (this.readyState === WS.CLOSED || this.closeCalledWith) return;
    this.closeCalledWith = { code, reason };
    this.readyState = WS.CLOSING;
    // Peer accepts the close handshake on a later tick.
    queueMicrotask(() => this.#finishClose(code, reason));
  }

  terminate() {
    if (this.readyState === WS.CLOSED) return;
    this.terminatedWith = { code: 1006 };
    this.readyState = WS.CLOSED;
    this.#fire("close", { code: 1006, reason: "terminated" });
  }

  /** Test-only: simulate the edge opening the socket. */
  open() {
    assert.notEqual(this.readyState, WS.CLOSED, "fake socket already closed");
    this.readyState = WS.OPEN;
    this.#fire("open");
  }

  /** Test-only: simulate the edge sending a canonical frame. */
  sendMessage(objOrText) {
    if (this.readyState !== WS.OPEN) throw new Error(`edge send on readyState ${this.readyState}`);
    const data = typeof objOrText === "string" ? objOrText : JSON.stringify(objOrText);
    this.#fire("message", { data });
  }

  /** Test-only: simulate the edge dropping the socket. */
  serverClose(code = 1006, reason = "") {
    if (this.readyState === WS.CLOSED || this.closeCalledWith) return;
    this.readyState = WS.CLOSED;
    this.#fire("close", { code, reason });
  }

  #finishClose(code, reason) {
    if (this.readyState === WS.CLOSED) return;
    this.readyState = WS.CLOSED;
    this.#fire("close", { code, reason });
  }
}

async function until(fn, { timeout = 2000, interval = 5 } = {}) {
  const start = Date.now();
  for (;;) {
    const value = fn();
    if (value) return value;
    if (Date.now() - start > timeout) throw new Error("until() timed out");
    await new Promise((resolve) => setTimeout(resolve, interval));
  }
}

function fakeTransport(over = {}) {
  const calls = { dispatch: [], cancel: [], runtimeInfo: 0, health: 0 };
  const runtimeInfoImpl = over.getRuntimeInfo;
  const healthImpl = over.getHealth;
  const dispatchImpl = over.dispatchRequest;
  const cancelImpl = over.cancelRequest;
  const transport = {
    name: "fake-runtime",
    async getRuntimeInfo() {
      calls.runtimeInfo += 1;
      if (runtimeInfoImpl) return runtimeInfoImpl();
      return {
        runtime_version: "0.3.26",
        runtime_commit: "abc123",
        runtime_generation: "gen-1",
        contract_epoch: 1,
        contract_hash: "hash1",
        herdr_version: "2.0.0",
        herdr_protocol: "hrpc1",
      };
    },
    async getHealth() {
      calls.health += 1;
      if (healthImpl) return healthImpl();
      return { healthy: true };
    },
    async dispatchRequest(req) {
      calls.dispatch.push(req);
      if (dispatchImpl) return dispatchImpl(req);
      return { ok: true, result: { pong: req.operation } };
    },
    async cancelRequest(id, reason) {
      calls.cancel.push([id, reason]);
      if (cancelImpl) await cancelImpl(id, reason);
    },
  };
  return { transport, calls };
}

function harness(opts = {}) {
  const sockets = [];
  const { transport, calls } = fakeTransport(opts.transport);
  const link = new HerdrLink({
    workstationId: "w5C",
    edgeUrl: "wss://edge.test/herdr",
    linkToken: "secret-token",
    transport,
    socketFactory: (url) => {
      const s = new FakeSocket(url);
      sockets.push(s);
      return s;
    },
    heartbeatMs: 60_000,
    maxSilenceMs: 3_600_000,
    handshakeTimeoutMs: 2000,
    requestTimeoutMs: 5000,
    maxPending: 8,
    drainMs: 50,
    backoff: { baseMs: 100, maxMs: 500, rng: () => 0.5 },
    ...opts.client,
  });
  return { link, sockets, transport, calls };
}

/** Connect, open socket #0, wait for hello, ack it, wait until online. */
async function startOnline(h, ack = true) {
  const connected = h.link.connect();
  const sock = await until(() => h.sockets[0]);
  sock.open();
  if (ack) await ackHello(sock);
  await until(() => h.link.getStatus().phase === "online");
  return { connected, sock };
}

/**
 * The edge can only ack after it has actually received our hello.
 * The hello_ack is a canonical frame: protocol_version, kind, workstation_id,
 * ok:true.
 */
async function ackHello(sock) {
  await until(() => sock.sent.some((f) => f.kind === "hello"));
  sock.sendMessage({
    protocol_version: 1,
    kind: "hello_ack",
    workstation_id: "w5C",
    ok: true,
  });
}

/**
 * Build a canonical tool_request frame for edge→workstation.
 * Default kind="tool_request", protocol_version=1.
 */
function requestFrame(rid, over = {}) {
  return {
    protocol_version: 1,
    kind: "tool_request",
    workstation_id: "w5C",
    request_id: rid,
    operation: over.operation ?? "herdr_inspect",
    arguments: over.arguments ?? {},
    timeout_ms: over.timeout_ms,
    contract_epoch: over.contract_epoch ?? 1,
    contract_hash: over.contract_hash ?? "hash1",
    ...over.extra,
  };
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

test("registers workstation identity via canonical hello and caches runtime snapshot", async () => {
  const h = harness();
  const { connected, sock } = await startOnline(h);
  const hello = sock.sent.find((f) => f.kind === "hello");
  assert.ok(hello, "canonical hello frame must be sent");
  assert.equal(hello.workstation_id, "w5C");
  assert.equal(hello.protocol_version, 1);
  assert.equal(hello.kind, "hello");
  assert.equal(hello.link_version, LINK_VERSION);
  assert.ok(hello.boot_id);
  assert.ok(hello.capabilities.includes("relay.request"));
  assert.equal(hello.runtime.runtime_version, "0.3.26");
  // Old wire fields must NOT be present
  assert.equal(hello.v, undefined, "old v field must not be on wire");
  assert.equal(hello.type, undefined, "old type field must not be on wire");
  assert.equal(hello.connection_id, undefined, "old connection_id must not be on wire");
  const s = h.link.getStatus();
  assert.equal(s.phase, "online");
  assert.ok(s.connected_at_ms != null);
  assert.equal(s.runtime.contract_hash, "hash1");
  assert.equal(s.frames_sent, 1); // hello only
  await h.link.close();
  await connected;
});

test("canonical tool_request is dispatched and result returned as tool_result", async () => {
  const h = harness();
  const { connected, sock } = await startOnline(h);
  sock.sendMessage(requestFrame("r1", { operation: "herdr_inspect", arguments: { scope: "w5C" } }));
  const resp = await until(() => sock.sent.find((f) => f.kind === "tool_result"));
  assert.equal(resp.kind, "tool_result");
  assert.equal(resp.request_id, "r1");
  assert.deepEqual(resp.result, { pong: "herdr_inspect" });
  assert.equal(resp.transport_name, "fake-runtime");
  assert.equal(resp.runtime_generation, "gen-1");
  // Old wire fields must NOT be present
  assert.equal(resp.type, undefined, "old response type must not be on wire");
  assert.equal(resp.final_status, undefined, "final_status must not be on wire");
  assert.equal(resp.runtime_ms, undefined, "runtime_ms must not be on wire");
  assert.equal(h.calls.dispatch.length, 1);
  assert.deepEqual(h.calls.dispatch[0].request_id, "r1");
  assert.equal(h.link.getStatus().active_requests, 0);
  await h.link.close();
  await connected;
});

test("transport failure becomes a canonical tool_error (retryable)", async () => {
  const h = harness({
    transport: {
      async dispatchRequest() {
        return { ok: false, code: "tool_failed", retryable: true, error: { message: "boom" } };
      },
    },
  });
  const { connected, sock } = await startOnline(h);
  sock.sendMessage(requestFrame("r1"));
  const resp = await until(() => sock.sent.find((f) => f.kind === "tool_error"));
  assert.equal(resp.kind, "tool_error");
  assert.equal(resp.request_id, "r1");
  assert.equal(resp.code, "tool_failed");
  assert.equal(resp.retryable, true);
  assert.equal(resp.message, "boom");
  assert.equal(resp.delivery_state, "delivered");
  assert.equal(resp.type, undefined, "old response type must not be on wire");
  await h.link.close();
  await connected;
});

test("thrown transport errors map to transport_error tool_error (retryable)", async () => {
  const h = harness({
    transport: {
      async dispatchRequest() {
        throw new Error("runtime down");
      },
    },
  });
  const { connected, sock } = await startOnline(h);
  sock.sendMessage(requestFrame("r1"));
  const resp = await until(() => sock.sent.find((f) => f.kind === "tool_error"));
  assert.equal(resp.code, "transport_error");
  assert.equal(resp.retryable, true);
  assert.equal(resp.kind, "tool_error");
  await h.link.close();
  await connected;
});

test("cancel is answered with cancel_ack, runtime notified, late result suppressed", async () => {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const h = harness({
    transport: {
      async dispatchRequest() {
        await gate;
        return { ok: true, result: "late" };
      },
    },
  });
  const { connected, sock } = await startOnline(h);
  sock.sendMessage(requestFrame("r1"));
  await until(() => h.link.getStatus().active_requests === 1);
  sock.sendMessage({
    protocol_version: 1,
    kind: "cancel",
    workstation_id: "w5C",
    request_id: "r1",
    reason: "user-abort",
  });
  const ack = await until(() => sock.sent.find((f) => f.kind === "cancel_ack"));
  assert.equal(ack.request_id, "r1");
  assert.equal(ack.accepted, true);
  assert.equal(ack.reason, "user-abort");
  assert.ok(ack.cancelled_at_ms > 0);
  // No tool_error should be sent — only cancel_ack.
  const toolErrors = sock.sent.filter((f) => f.kind === "tool_error");
  assert.equal(toolErrors.length, 0, "cancel must not also produce a tool_error");
  assert.deepEqual(h.calls.cancel, [["r1", "user-abort"]]);
  release();
  await new Promise((resolve) => setTimeout(resolve, 30));
  // Late result must not produce a second frame.
  const results = sock.sent.filter((f) => f.kind === "tool_result" || f.kind === "tool_error");
  assert.equal(results.length, 0, "late result must not produce a second frame");
  await h.link.close();
  await connected;
});

test("cancel_ack with accepted:false for unknown or already-settled requests", async () => {
  const h = harness();
  const { connected, sock } = await startOnline(h);
  // Cancel for unknown request_id
  sock.sendMessage({
    protocol_version: 1,
    kind: "cancel",
    workstation_id: "w5C",
    request_id: "nonexistent",
    reason: "whoops",
  });
  const ack = await until(() => sock.sent.find((f) => f.kind === "cancel_ack"));
  assert.equal(ack.request_id, "nonexistent");
  assert.equal(ack.accepted, false);
  await h.link.close();
  await connected;
});

test("bounded pending: overflow requests get tool_error with request_queue_full", async () => {
  const gate = new Promise(() => {});
  const h = harness({
    client: { maxPending: 1 },
    transport: { async dispatchRequest() { await gate; } },
  });
  const { connected, sock } = await startOnline(h);
  sock.sendMessage(requestFrame("r1"));
  await until(() => h.link.getStatus().active_requests === 1);
  sock.sendMessage(requestFrame("r2"));
  const resp = await until(() => sock.sent.find((f) => f.kind === "tool_error"));
  assert.equal(resp.request_id, "r2");
  assert.equal(resp.code, "request_queue_full");
  assert.equal(resp.retryable, true);
  assert.equal(h.link.getStatus().queue_overflow_responses, 1);
  assert.equal(h.calls.dispatch.length, 1, "overflow must not reach the runtime");
  await h.link.close();
  await connected;
});

test("duplicate request_id while in flight is not double-dispatched", async () => {
  const gate = new Promise(() => {});
  const h = harness({
    transport: { async dispatchRequest() { await gate; } },
  });
  const { connected, sock } = await startOnline(h);
  sock.sendMessage(requestFrame("r1"));
  await until(() => h.link.getStatus().active_requests === 1);
  sock.sendMessage(requestFrame("r1"));
  const resp = await until(() => sock.sent.find((f) => f.kind === "tool_error"));
  assert.equal(resp.code, "duplicate_request");
  assert.equal(resp.retryable, true);
  assert.equal(h.calls.dispatch.length, 1);
  await h.link.close();
  await connected;
});

test("inbound frames beyond maxFrameBytes are rejected with tool_error payload_too_large", async () => {
  const h = harness({ client: { maxFrameBytes: 560 } }); // hello fits (~486), request (~636) does not
  const { connected, sock } = await startOnline(h);
  sock.sendMessage(requestFrame("big", { arguments: { blob: "x".repeat(500) } }));
  const resp = await until(() => sock.sent.find((f) => f.kind === "tool_error"));
  assert.equal(resp.request_id, "big");
  assert.equal(resp.code, "payload_too_large");
  assert.equal(resp.retryable, false);
  assert.equal(h.calls.dispatch.length, 0, "oversized frame must not dispatch");
  assert.equal(h.link.getStatus().payload_too_large_rejected, 1);
  await h.link.close();
  await connected;
});

test("unexpected disconnect schedules backoff reconnect and re-registers with fresh hello", async () => {
  const h = harness({ client: { backoff: { baseMs: 40, maxMs: 100, rng: () => 0.5 } } });
  const { connected, sock } = await startOnline(h);
  const hello1 = sock.sent.find((f) => f.kind === "hello");
  sock.serverClose(1006, "gone");
  await until(() => h.link.getStatus().phase === "reconnecting");
  assert.equal(h.link.getStatus().reconnect_attempt, 1);
  assert.ok(h.link.getStatus().reconnect_at_ms != null);
  const sock2 = await until(() => h.sockets[1]);
  sock2.open();
  await ackHello(sock2);
  await until(() => h.link.getStatus().phase === "online");
  const hello2 = sock2.sent.find((f) => f.kind === "hello");
  assert.ok(hello2, "second hello must be sent");
  assert.equal(h.link.getStatus().reconnect_attempt, 0, "backoff resets after online");
  await h.link.close();
  await connected;
});

test("handshake timeout (no ack) reconnects via backoff and re-registers", async () => {
  const h = harness({ client: { handshakeTimeoutMs: 30, backoff: { baseMs: 30, maxMs: 100, rng: () => 0.5 } } });
  const connectedP = h.link.connect();
  const sock1 = await until(() => h.sockets[0]);
  sock1.open();
  const sock2 = await until(() => h.sockets[1]);
  sock2.open();
  await ackHello(sock2);
  await until(() => h.link.getStatus().phase === "online");
  const hello1 = sock1.sent.find((f) => f.kind === "hello");
  const hello2 = sock2.sent.find((f) => f.kind === "hello");
  assert.ok(hello1 && hello2);
  await h.link.close();
  await connectedP;
});

test("canonical heartbeat frames are sent on the heartbeat interval", async () => {
  const h = harness({ client: { heartbeatMs: 20 } });
  const { connected, sock } = await startOnline(h);
  await until(() => sock.sent.some((f) => f.kind === "heartbeat"));
  const hb = sock.sent.find((f) => f.kind === "heartbeat");
  assert.equal(hb.workstation_id, "w5C");
  assert.equal(hb.protocol_version, 1);
  assert.equal(hb.kind, "heartbeat");
  assert.equal(hb.runtime.runtime_version, "0.3.26");
  assert.equal(hb.active_requests, 0);
  assert.ok(h.link.getStatus().last_heartbeat_ms != null);
  await h.link.close();
  await connected;
});

test("a request over its local budget returns tool_error request_timeout; late result suppressed", async () => {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const h = harness({
    client: { requestTimeoutMs: 50 }, // wire-valid; local budget is small
    transport: {
      async dispatchRequest() {
        await gate;
        return { ok: true, result: 1 };
      },
    },
  });
  const { connected, sock } = await startOnline(h);
  // Note: canonical validation enforces timeout_ms >= 1000 on the wire, so
  // the short local budget comes from client requestTimeoutMs, not the frame.
  sock.sendMessage(requestFrame("t1"));
  const resp = await until(() => sock.sent.find((f) => f.kind === "tool_error"));
  assert.equal(resp.request_id, "t1");
  assert.equal(resp.code, "request_timeout");
  assert.equal(resp.retryable, true);
  assert.equal(resp.delivery_state, "delivery_unknown");
  assert.equal(h.link.getStatus().timeouts_sent, 1);
  release();
  await new Promise((resolve) => setTimeout(resolve, 30));
  const results = sock.sent.filter((f) => f.kind === "tool_result" || f.kind === "tool_error");
  assert.equal(results.length, 1, "late result must not produce a second frame");
  await h.link.close();
  await connected;
});

test("auth-rejected hello_ack stops the loop without reconnect spam", async () => {
  const h = harness();
  const exitP = h.link.connect();
  const sock = await until(() => h.sockets[0]);
  sock.open();
  await until(() => sock.sent.some((f) => f.kind === "hello"));
  sock.sendMessage({
    protocol_version: 1,
    kind: "hello_ack",
    workstation_id: "w5C",
    ok: false,
    code: "auth_rejected",
    message: "bad token",
  });
  const exit = await exitP;
  assert.equal(exit.kind, "auth_rejected");
  await until(() => h.link.getStatus().phase === "closed");
  assert.ok(h.link.getStatus().fatal_error);
  assert.equal(h.sockets.length, 1, "no reconnect after auth refusal");
});

test("protocol_incompatible hello_ack stops the loop as contract_rejected", async () => {
  const h = harness();
  const exitP = h.link.connect();
  const sock = await until(() => h.sockets[0]);
  sock.open();
  await until(() => sock.sent.some((f) => f.kind === "hello"));
  sock.sendMessage({
    protocol_version: 1,
    kind: "hello_ack",
    workstation_id: "w5C",
    ok: false,
    code: "protocol_incompatible",
    message: "epoch drift",
  });
  const exit = await exitP;
  assert.equal(exit.kind, "contract_rejected");
});

test("edge superseded close fences an old link without reconnecting", async () => {
  const h = harness();
  const { connected, sock } = await startOnline(h);
  sock.serverClose(4409, "superseded by newer workstation link");
  const exit = await connected;
  assert.equal(exit.kind, "superseded");
  assert.equal(h.sockets.length, 1, "fenced link must not reconnect and fight the replacement");
  assert.match(exit.message, /newer workstation connection/);
});

test("graceful shutdown closes the socket (no shutdown frame), drains pending, and exits stopped", async () => {
  const h = harness();
  const { connected, sock } = await startOnline(h);
  const exitP = h.link.close({ reason: "maintenance" });
  // No "shutdown" wire frame should be sent
  await new Promise((resolve) => setTimeout(resolve, 30));
  assert.equal(sock.sent.find((f) => f.kind === "shutdown"), undefined, "shutdown wire frame must not be sent");
  assert.equal(
    sock.sent.find((f) => f.kind === "tool_error" && f.code === "link_stopping"),
    undefined,
    "no pending requests to reject",
  );
  await until(() => sock.closeCalledWith != null);
  assert.equal(sock.closeCalledWith.code, 1000);
  const exit = await exitP;
  assert.equal(exit.kind, "stopped");
  assert.equal(h.link.getStatus().phase, "closed");
  await connected;
});

test("pending requests are rejected with link_stopping tool_error during shutdown", async () => {
  let release;
  const gate = new Promise((resolve) => {
    release = resolve;
  });
  const h = harness({
    transport: {
      async dispatchRequest() {
        await gate;
      },
    },
  });
  const { connected, sock } = await startOnline(h);
  sock.sendMessage(requestFrame("r1"));
  await until(() => h.link.getStatus().active_requests === 1);
  const exitP = h.link.close({ drainMs: 50, reason: "tests" });
  const resp = await until(() => sock.sent.find((f) => f.kind === "tool_error" && f.code === "link_stopping"));
  assert.equal(resp.request_id, "r1");
  assert.equal(resp.retryable, true);
  release();
  await exitP;
  await connected;
});

test("socket factory failures stop at maxReconnectAttempts with max_reconnect exit", async () => {
  let factoryCalls = 0;
  const h = harness({
    client: {
      socketFactory: () => {
        factoryCalls += 1;
        throw new Error("edge unreachable");
      },
      backoff: { baseMs: 1, maxMs: 1, rng: () => 0 },
      maxReconnectAttempts: 1,
    },
  });
  const exit = await h.link.connect();
  assert.equal(exit.kind, "max_reconnect");
  assert.equal(factoryCalls, 2); // first connect + one retry, then the cap stops the loop
  assert.equal(h.link.getStatus().phase, "closed");
  assert.equal(h.link.getStatus().stopped, false, "exit by cap leaves stopped=false (loop ended, link not closed)");
});

test("start() fire-and-forget connects, stays online, and shuts down cleanly", async () => {
  const h = harness();
  h.link.start();
  const sock = await until(() => h.sockets[0]);
  sock.open();
  await ackHello(sock);
  await until(() => h.link.getStatus().phase === "online");
  const exitP = h.link.close({ reason: "test-end" });
  await until(() => sock.closeCalledWith != null);
  const exit = await exitP;
  assert.equal(exit.kind, "stopped");
  // start() must never surface an unhandled rejection from connect().
  const second = await h.link.connect();
  assert.equal(second.kind, "stopped");
});

test("malformed, invalid version, or unknown kind frames are counted, not fatal", async () => {
  const h = harness();
  const { connected, sock } = await startOnline(h);
  // Not valid JSON
  sock.sendMessage("{\"not json\":");
  // Plain text
  sock.sendMessage("just-text");
  // Unsupported protocol_version
  sock.sendMessage({
    protocol_version: 99,
    kind: "tool_request",
    workstation_id: "w5C",
    request_id: "x",
    operation: "op",
  });
  // Unknown kind
  sock.sendMessage({
    protocol_version: 1,
    kind: "nonsense",
    workstation_id: "w5C",
  });
  // Valid but unexpected kind (hello is workstation→edge only)
  sock.sendMessage({
    protocol_version: 1,
    kind: "hello",
    workstation_id: "w5C",
    boot_id: "b1",
    link_version: "v1",
    capabilities: [],
  });
  await until(() => h.link.getStatus().malformed_frames >= 4);
  assert.equal(h.calls.dispatch.length, 0);
  assert.equal(h.link.getStatus().phase, "online");
  await h.link.close();
  await connected;
});

test("canonical status(query:true) query returns a canonical status report; replaces old runtime_status", async () => {
  const h = harness();
  const { connected, sock } = await startOnline(h);
  sock.sendMessage({
    protocol_version: 1,
    kind: "status",
    workstation_id: "w5C",
    query: true,
  });
  const rs = await until(() => sock.sent.filter((f) => f.kind === "status").pop());
  assert.equal(rs.query, false);
  assert.equal(rs.runtime.runtime_version, "0.3.26");
  assert.equal(rs.healthy, true);
  assert.equal(rs.active_requests, 0);
  assert.equal(rs.kind, "status");
  await h.link.close();
  await connected;
});

test("a silent edge connection is recycled after maxSilenceMs", async () => {
  const h = harness({ client: { heartbeatMs: 60_000, maxSilenceMs: 60, backoff: { baseMs: 20, maxMs: 100, rng: () => 0.5 } } });
  const { connected, sock } = await startOnline(h);
  await until(() => sock.terminatedWith != null, { timeout: 3000 });
  const sock2 = await until(() => h.sockets[1]);
  assert.ok(sock2);
  sock2.open();
  await ackHello(sock2);
  await until(() => h.link.getStatus().phase === "online");
  await h.link.close();
  await connected;
});

test("close() during reconnect wait cancels the retry immediately", async () => {
  const h = harness({ client: { backoff: { baseMs: 10_000, maxMs: 10_000, rng: () => 0.5 } } });
  const { connected, sock } = await startOnline(h);
  sock.serverClose(1006, "gone");
  await until(() => h.link.getStatus().phase === "reconnecting");
  const exitP = h.link.close();
  const exit = await exitP;
  assert.equal(exit.kind, "stopped");
  await connected;
  assert.equal(h.sockets.length, 1, "no retry after close during backoff");
});

test("oversized outbound results are replaced by a compact tool_error response_too_large", async () => {
  const h = harness({
    client: { maxFrameBytes: 600 },
    transport: {
      async dispatchRequest() {
        return { ok: true, result: { blob: "y".repeat(2000) } };
      },
    },
  });
  const { connected, sock } = await startOnline(h);
  sock.sendMessage(requestFrame("r1"));
  const resp = await until(() => sock.sent.find((f) => f.kind === "tool_error"));
  assert.equal(resp.request_id, "r1");
  assert.equal(resp.code, "response_too_large");
  assert.equal(resp.retryable, false);
  for (const raw of sock.sentRaw) {
    assert.ok(Buffer.byteLength(raw, "utf8") <= 600, "every sent frame stays within budget");
  }
  await h.link.close();
  await connected;
});

test("tool requests for another workstation are ignored", async () => {
  const h = harness();
  const { connected, sock } = await startOnline(h);
  sock.sendMessage({
    protocol_version: 1,
    kind: "tool_request",
    workstation_id: "someone-else",
    request_id: "r9",
    operation: "op",
  });
  await new Promise((resolve) => setTimeout(resolve, 40));
  assert.equal(h.calls.dispatch.length, 0);
  assert.equal(
    sock.sent.filter((f) => f.kind === "tool_result" || f.kind === "tool_error").length,
    0,
  );
  await h.link.close();
  await connected;
});

test("link handshake keeps credential out of URL and carries it as a WS subprotocol", () => {
  const url = buildEdgeUrl("wss://edge.test/ws", { workstationId: "w5C", linkToken: "tok-123" });
  const u = new URL(url);
  assert.equal(u.pathname, "/ws/w5C");
  assert.equal(u.searchParams.get("workstation_id"), null);
  assert.equal(u.searchParams.get("link_token"), null);
  assert.equal(url.includes("tok-123"), false);
  const authProtocol = buildLinkAuthProtocol("tok-123");
  assert.equal(authProtocol, "herdr-auth.746f6b2d313233");
  assert.deepEqual(buildLinkProtocols("herdr-link.v1", "tok-123"), ["herdr-link.v1", authProtocol]);
  assert.equal(buildLinkProtocols("herdr-link.v1", "").length, 1);
  assert.equal(redactUrl(url).includes("tok-123"), false);
});

test("constructor validation rejects missing/invalid inputs", () => {
  const { transport } = fakeTransport();
  assert.throws(() => new HerdrLink({ edgeUrl: "wss://x", linkToken: "t", transport }), /workstationId/);
  assert.throws(() => new HerdrLink({ workstationId: "w1", edgeUrl: "not a url", linkToken: "t", transport }), /edgeUrl/);
  assert.throws(() => new HerdrLink({ workstationId: "w1", edgeUrl: "http://edge.test/ws", linkToken: "t", transport }), /wss/);
  assert.throws(() => new HerdrLink({ workstationId: "w1", edgeUrl: "wss://x", linkToken: "t", transport: {} }), /transport/);
});

test("pure helpers behave deterministically", () => {
  assert.equal(clampRange(undefined, 7, 1, 9), 7);
  assert.equal(clampRange(50, 7, 1, 9), 9);
  assert.equal(clampRange(-1, 7, 1, 9), 1); // clamps to min, not the fallback
  assert.equal(clampRequestTimeout(5, 60_000), 5);
  assert.equal(clampRequestTimeout(1_000_000, 60_000), 60_000);
  assert.equal(clampRequestTimeout(undefined, 60_000), 60_000);
  assert.equal(classifyFatalCode("auth_expired"), "auth_rejected");
  assert.equal(classifyFatalCode("protocol_incompatible"), "contract_rejected");
  assert.equal(classifyFatalCode("contract_mismatch"), "contract_rejected");
  assert.equal(classifyFatalCode("whatever"), "fatal_error");
  assert.equal(extractRequestId('{"request_id":"abc"}'), "abc");
  assert.equal(extractRequestId("nope"), null);
  assert.equal(decodeSocketData("hello"), "hello");
  assert.equal(decodeSocketData(null), null);
  assert.equal(decodeSocketData(Buffer.from("buf")), "buf");
});