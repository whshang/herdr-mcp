/**
 * Batch 5: pure lifecycle/policy parity for the herdr-link client.
 *
 * Validates the shared fixture `fixtures/link-lifecycle-policy-batch5.json`
 * against the Node implementation (src/link/client.ts). Pure exported helpers
 * (classifyFatalCode and WS close-code constants) are asserted
 * directly. Private lifecycle behavior (phase transitions, hello_ack refusal
 * handling, socket-close policy, reconnect cap, heartbeat/silence gates) is
 * exercised through a minimal fake-socket harness patterned only from the
 * existing link-client.test.mjs tests, keeping cases bounded.
 *
 * NOTE: this file is authored for parity with the Rust port. It imports from
 * `../dist/link/client.js` (the compiled output) exactly like the existing
 * link-client.test.mjs; when node_modules/dist are absent it is syntax-checked
 * only and not executed.
 */
import { readFileSync } from "node:fs";
import { test } from "node:test";
import assert from "node:assert/strict";

import {
  HerdrLink,
  classifyFatalCode,
  WS_CLOSE_SUPERSEDED,
  AUTH_REJECT_CLOSE_CODES,
} from "../dist/link/client.js";

const fixture = JSON.parse(
  readFileSync(new URL("./fixtures/link-lifecycle-policy-batch5.json", import.meta.url), "utf8"),
);

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
    queueMicrotask(() => this.#finishClose(code, reason));
  }

  terminate() {
    if (this.readyState === WS.CLOSED) return;
    this.terminatedWith = { code: 1006 };
    this.readyState = WS.CLOSED;
    this.#fire("close", { code: 1006, reason: "terminated" });
  }

  open() {
    assert.notEqual(this.readyState, WS.CLOSED, "fake socket already closed");
    this.readyState = WS.OPEN;
    this.#fire("open");
  }

  sendMessage(objOrText) {
    if (this.readyState !== WS.OPEN) throw new Error(`edge send on readyState ${this.readyState}`);
    const data = typeof objOrText === "string" ? objOrText : JSON.stringify(objOrText);
    this.#fire("message", { data });
  }

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

function fakeTransport() {
  const calls = { dispatch: [], cancel: [], runtimeInfo: 0, health: 0 };
  const transport = {
    name: "fake-runtime",
    async getRuntimeInfo() {
      calls.runtimeInfo += 1;
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
      return { healthy: true };
    },
    async dispatchRequest(req) {
      calls.dispatch.push(req);
      return { ok: true, result: { pong: req.operation } };
    },
    async cancelRequest(id, reason) {
      calls.cancel.push([id, reason]);
    },
  };
  return { transport, calls };
}

function harness(opts = {}) {
  const sockets = [];
  const { transport, calls } = fakeTransport();
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

async function ackHello(sock) {
  await until(() => sock.sent.some((f) => f.kind === "hello"));
  sock.sendMessage({
    protocol_version: 1,
    kind: "hello_ack",
    workstation_id: "w5C",
    ok: true,
  });
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. ConnectionPhase set / order / allowed transitions
// ─────────────────────────────────────────────────────────────────────────────

test("fixture connection_phases set and order match the ConnectionPhase union", () => {
  const set = fixture.connection_phases.set;
  const order = fixture.connection_phases.order;
  assert.deepEqual(set, order, "set and order must be identical");
  assert.deepEqual(set, ["idle", "connecting", "handshake", "online", "reconnecting", "closing", "closed"]);
  // Every transition endpoint must be a member of the set.
  for (const t of fixture.connection_phases.transitions) {
    assert.ok(set.includes(t.from), `transition from ${t.from} must be a valid phase`);
    assert.ok(set.includes(t.to), `transition to ${t.to} must be a valid phase`);
  }
});

test("phase advances idle -> connecting -> handshake -> online on a healthy connect", async () => {
  const h = harness();
  assert.equal(h.link.getStatus().phase, "idle");
  const connected = h.link.connect();
  const sock = await until(() => h.sockets[0]);
  sock.open();
  await until(() => h.link.getStatus().phase === "handshake");
  await ackHello(sock);
  await until(() => h.link.getStatus().phase === "online");
  assert.equal(h.link.getStatus().phase, "online");
  await h.link.close();
  await connected;
});

test("online drop -> reconnecting -> connecting on the next attempt", async () => {
  const h = harness({ client: { backoff: { baseMs: 40, maxMs: 100, rng: () => 0.5 } } });
  const { connected, sock } = await startOnline(h);
  sock.serverClose(1006, "gone");
  await until(() => h.link.getStatus().phase === "reconnecting");
  assert.equal(h.link.getStatus().reconnect_attempt, 1);
  const sock2 = await until(() => h.sockets[1]);
  sock2.open();
  await until(() => h.link.getStatus().phase === "handshake");
  await ackHello(sock2);
  await until(() => h.link.getStatus().phase === "online");
  await h.link.close();
  await connected;
});

test("close during reconnect -> closed/stopped without retry", async () => {
  const h = harness({ client: { backoff: { baseMs: 10_000, maxMs: 10_000, rng: () => 0.5 } } });
  const { connected, sock } = await startOnline(h);
  sock.serverClose(1006, "gone");
  await until(() => h.link.getStatus().phase === "reconnecting");
  const exitP = h.link.close();
  const exit = await exitP;
  assert.equal(exit.kind, "stopped");
  await connected;
  assert.equal(h.link.getStatus().phase, "closed");
  assert.equal(h.sockets.length, 1, "no retry after close during backoff");
});

test("fatal hello_ack refusal -> closed (no reconnect)", async () => {
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
  assert.equal(h.sockets.length, 1, "no reconnect after fatal refusal");
});

// ─────────────────────────────────────────────────────────────────────────────
// 2. hello_ack refusal classification
// ─────────────────────────────────────────────────────────────────────────────

test("hello_ack refusal classification matches the fixture oracle", () => {
  for (const entry of fixture.hello_ack_classification) {
    assert.equal(entry.oracle, "node_parity", `${entry.name}: oracle must be node_parity`);
    const classify = classifyFatalCode(entry.code);
    assert.equal(classify, entry.classify, `${entry.name}: classifyFatalCode`);
    // The client treats a refusal as fatal iff classifyFatalCode != "fatal_error".
    const fatal = classify !== "fatal_error";
    assert.equal(fatal, entry.fatal, `${entry.name}: fatal`);
    assert.equal(!fatal, entry.reconnect, `${entry.name}: reconnect`);
  }
});

test("auth/contract refusals exit fatal without reconnect; unknown/internal_error retry", async () => {
  const fatalCodes = ["auth_rejected", "auth_expired", "session_invalid", "contract_mismatch", "contract_rejected", "protocol_incompatible"];
  for (const code of fatalCodes) {
    const h = harness();
    const exitP = h.link.connect();
    const sock = await until(() => h.sockets[0]);
    sock.open();
    await until(() => sock.sent.some((f) => f.kind === "hello"));
    sock.sendMessage({ protocol_version: 1, kind: "hello_ack", workstation_id: "w5C", ok: false, code, message: "refused" });
    const exit = await exitP;
    const expectedKind = classifyFatalCode(code);
    assert.equal(exit.kind, expectedKind, `${code}: exit kind`);
    assert.equal(h.sockets.length, 1, `${code}: no reconnect`);
  }

  // internal_error / unknown classify fatal_error but are treated non-fatal → retry.
  for (const code of ["internal_error", "edge_hiccup"]) {
    const h = harness({ client: { backoff: { baseMs: 20, maxMs: 100, rng: () => 0.5 } } });
    const connectedP = h.link.connect();
    const sock1 = await until(() => h.sockets[0]);
    sock1.open();
    await until(() => sock1.sent.some((f) => f.kind === "hello"));
    sock1.sendMessage({ protocol_version: 1, kind: "hello_ack", workstation_id: "w5C", ok: false, code, message: "retry me" });
    // A second socket must be created (retryable drop).
    const sock2 = await until(() => h.sockets[1]);
    sock2.open();
    await ackHello(sock2);
    await until(() => h.link.getStatus().phase === "online");
    assert.equal(h.link.getStatus().phase, "online", `${code}: retried to online`);
    await h.link.close();
    await connectedP;
  }
});

test("handshake_timeout is retryable (reconnects)", async () => {
  const h = harness({ client: { handshakeTimeoutMs: 30, backoff: { baseMs: 30, maxMs: 100, rng: () => 0.5 } } });
  const connectedP = h.link.connect();
  const sock1 = await until(() => h.sockets[0]);
  sock1.open();
  const sock2 = await until(() => h.sockets[1]);
  sock2.open();
  await ackHello(sock2);
  await until(() => h.link.getStatus().phase === "online");
  assert.ok(sock1.sent.some((f) => f.kind === "hello"), "first hello sent");
  assert.ok(sock2.sent.some((f) => f.kind === "hello"), "second hello sent");
  await h.link.close();
  await connectedP;
});

// ─────────────────────────────────────────────────────────────────────────────
// 3. socket-close policy
// ─────────────────────────────────────────────────────────────────────────────

test("WS close-code constants match the fixture socket_close_policy", () => {
  assert.equal(WS_CLOSE_SUPERSEDED, 4409);
  assert.deepEqual([...AUTH_REJECT_CLOSE_CODES].sort((a, b) => a - b), [1008, 4401, 4403]);
  for (const entry of fixture.socket_close_policy) {
    assert.equal(entry.oracle, "node_parity", `${entry.name}: oracle must be node_parity`);
    const isSuperseded = entry.code === WS_CLOSE_SUPERSEDED;
    const isAuth = AUTH_REJECT_CLOSE_CODES.has(entry.code);
    const fatal = isSuperseded || isAuth;
    assert.equal(fatal, entry.fatal, `${entry.name}: fatal`);
    assert.equal(!fatal, entry.reconnect, `${entry.name}: reconnect`);
    if (fatal) {
      assert.equal(entry.exit_kind, isSuperseded ? "superseded" : "auth_rejected", `${entry.name}: exit_kind`);
    }
  }
});

test("superseded (4409) and auth (1008/4401/4403) closes exit fatal without reconnect", async () => {
  const fatalClose = [
    { code: 4409, kind: "superseded" },
    { code: 1008, kind: "auth_rejected" },
    { code: 4401, kind: "auth_rejected" },
    { code: 4403, kind: "auth_rejected" },
  ];
  for (const { code, kind } of fatalClose) {
    const h = harness();
    const { connected, sock } = await startOnline(h);
    sock.serverClose(code, "refused");
    const exit = await connected;
    assert.equal(exit.kind, kind, `${code}: exit kind`);
    assert.equal(h.sockets.length, 1, `${code}: no reconnect`);
  }
});

test("representative dropped closes (1000/1001/1006/4000) reconnect", async () => {
  for (const code of [1000, 1001, 1006, 4000]) {
    const h = harness({ client: { backoff: { baseMs: 20, maxMs: 100, rng: () => 0.5 } } });
    const { connected, sock } = await startOnline(h);
    sock.serverClose(code, "dropped");
    await until(() => h.link.getStatus().phase === "reconnecting");
    const sock2 = await until(() => h.sockets[1]);
    sock2.open();
    await ackHello(sock2);
    await until(() => h.link.getStatus().phase === "online");
    assert.equal(h.link.getStatus().phase, "online", `${code}: reconnected`);
    await h.link.close();
    await connected;
  }
});

// ─────────────────────────────────────────────────────────────────────────────
// 4. reconnect-cap vectors
// ─────────────────────────────────────────────────────────────────────────────

test("reconnect-cap vectors match the Node run loop", async () => {
  for (const entry of fixture.reconnect_cap) {
    assert.equal(entry.oracle, "node_parity", `${entry.name}: oracle must be node_parity`);
    let factoryCalls = 0;
    const h = harness({
      client: {
        socketFactory: () => {
          factoryCalls += 1;
          throw new Error("edge unreachable");
        },
        backoff: { baseMs: 1, maxMs: 1, rng: () => 0 },
        maxReconnectAttempts: entry.maxReconnectAttempts,
      },
    });
    const connectedP = h.link.connect();
    if (entry.expected_exit === null) {
      const observeAttempts = entry.observe_attempts;
      assert.ok(Number.isInteger(observeAttempts) && observeAttempts > 0, `${entry.name}: observe_attempts`);
      await until(() => factoryCalls >= observeAttempts + 1 && h.link.getStatus().phase === "reconnecting");
      assert.equal(h.link.getStatus().phase, "reconnecting", `${entry.name}: still reconnecting`);
      const exit = await h.link.close();
      assert.equal(exit.kind, "stopped", `${entry.name}: explicit stop`);
      await connectedP;
    } else {
      const exit = await connectedP;
      assert.equal(exit.kind, entry.expected_exit, `${entry.name}: exit kind`);
      assert.equal(factoryCalls, entry.factory_calls, `${entry.name}: factory calls`);
      assert.equal(h.link.getStatus().phase, "closed", `${entry.name}: closed`);
    }
  }
});

test("reconnect_attempt equals backoff.attempt after a drop", async () => {
  const h = harness({ client: { backoff: { baseMs: 40, maxMs: 100, rng: () => 0.5 } } });
  const { connected, sock } = await startOnline(h);
  sock.serverClose(1006, "gone");
  await until(() => h.link.getStatus().phase === "reconnecting");
  assert.equal(h.link.getStatus().reconnect_attempt, 1, "first reconnect attempt is 1");
  await h.link.close();
  await connected;
});

// ─────────────────────────────────────────────────────────────────────────────
// 5. heartbeat / silence pure vectors
// ─────────────────────────────────────────────────────────────────────────────

test("heartbeat gate: only online + not stopped + socket open", async () => {
  const gate = fixture.heartbeat_silence.heartbeat_gate;
  for (const c of gate.conditions) {
    // The gate is: phase==="online" && !stopped && socket open.
    const should = c.phase === "online" && !c.stopped && c.socket_open;
    assert.equal(should, c.should_heartbeat, `${c.phase}/${c.stopped}/${c.socket_open}`);
  }
});

test("silence interval = max(250, min(heartbeatMs, maxSilenceMs/3))", () => {
  for (const entry of fixture.heartbeat_silence.silence_interval) {
    const expected = Math.max(250, Math.min(entry.heartbeatMs, entry.maxSilenceMs / 3));
    assert.equal(expected, entry.expected, `${entry.name}: silence interval`);
  }
});

test("silence expires only when now - base > maxSilenceMs (base = lastEdgeSeen ?? connectedAt ?? 0)", () => {
  for (const entry of fixture.heartbeat_silence.silence_expiry) {
    const base = entry.lastEdgeSeenMs ?? entry.connectedAtMs ?? 0;
    const expired = entry.now - base > entry.maxSilenceMs;
    assert.equal(expired, entry.expired, `${entry.name}: expired`);
  }
});

test("a silent edge connection is recycled after maxSilenceMs", async () => {
  const h = harness({ client: { heartbeatMs: 60_000, maxSilenceMs: 60, backoff: { baseMs: 20, maxMs: 100, rng: () => 0.5 } } });
  const { connected, sock } = await startOnline(h);
  await until(() => sock.terminatedWith != null, { timeout: 3000 });
  const sock2 = await until(() => h.sockets[1]);
  assert.ok(sock2, "reconnect socket created after silence recycle");
  sock2.open();
  await ackHello(sock2);
  await until(() => h.link.getStatus().phase === "online");
  await h.link.close();
  await connected;
});
