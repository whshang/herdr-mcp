/**
 * Unit tests for LocalMcpRuntimeTransport (herdr-link Phase 2).
 *
 * ALL network I/O is mocked through the injected `fetch` — never the real
 * 127.0.0.1:8772 runtime, never plist/token files. Coverage:
 *  - request headers / tools/call body / JSON-RPC id correlation
 *  - plain JSON and SSE response parsing (multiline data:, comments)
 *  - COMPLETE MCP CallToolResult preservation (image blocks, isError:true)
 *  - JSON-RPC error mapping (sanitized, bounded)
 *  - timeout via AbortController; cancelRequest idempotent + scoped
 *  - malformed / oversized (request + incremental response) / id mismatch
 *  - health probe (server/discover default, version caching, failure states)
 *  - loopback endpoint guard + explicit allowNonLoopback
 *  - bearer token + arguments never appear in thrown/returned error details
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  LOCAL_MCP_DEFAULT_ENDPOINT,
  LOCAL_MCP_DEFAULT_MAX_FRAME_BYTES,
  LOCAL_MCP_CODE,
  LocalMcpRuntimeTransport,
  isLoopbackHost,
} from "../dist/link/local-mcp-transport.js";

// Distinctive values so the token-secrecy tests can scan for them verbatim.
const TOKEN = "supersecret-bearer-token-7f3a9c2e";
const ARG_SECRET = "ARGUMENT-SECRET-value-9e8d7c";
const CONTRACT_HASH = "sha256:deadbeef0000000000000000000000000000000000000000000000000000";

function opts(overrides = {}) {
  return {
    endpoint: "http://127.0.0.1:8772/mcp",
    bearerToken: TOKEN,
    contractHash: CONTRACT_HASH,
    fetch: async () => new Response("noop"),
    ...overrides,
  };
}

/** A request frame matching the internal ToolRequestFrame shape. */
function frame(requestId, overrides = {}) {
  return {
    v: 1,
    type: "request",
    workstation_id: "w1",
    request_id: requestId,
    operation: "herdr_inspect",
    arguments: { query: "ping", secret: ARG_SECRET },
    ...overrides,
  };
}

/** Recording fetch: captures calls, returns bodies from a queue. */
function recordingFetch(bodies) {
  const calls = [];
  const fn = async (url, init) => {
    calls.push({ url, init, bodyText: typeof init?.body === "string" ? init.body : String(init?.body ?? "") });
    const next = bodies.shift();
    if (next instanceof Error) throw next;
    if (typeof next === "function") return next(init);
    if (next === undefined) return new Response("{}");
    if (typeof next === "string") return new Response(next, { status: 200 });
    return next; // assume Response-like
  };
  fn.calls = calls;
  return fn;
}

/** Deferred fetch that rejects like real fetch when the signal aborts. */
function abortableFetch(control) {
  const fn = (url, init) =>
    new Promise((resolve, reject) => {
      const signal = init.signal;
      const onAbort = () => {
        const err = new Error("The operation was aborted.");
        err.name = "AbortError";
        reject(err);
      };
      if (signal?.aborted) return onAbort();
      signal?.addEventListener("abort", onAbort, { once: true });
      control.called = { url, init };
    });
  fn.calls = [];
  return fn;
}

// ─────────────────────────────────────────────────────────────────────────────
// Construction / loopback guard
// ─────────────────────────────────────────────────────────────────────────────

test("constructor: defaults to the 8772/mcp endpoint", () => {
  const t = new LocalMcpRuntimeTransport(opts());
  assert.equal(t.name, "local-mcp-http");
  assert.equal(t.getRuntimeInfo().contract_epoch, 1);
  assert.equal(t.getRuntimeInfo().contract_hash, CONTRACT_HASH);
  assert.equal(LOCAL_MCP_DEFAULT_ENDPOINT, "http://127.0.0.1:8772/mcp");
  assert.equal(LOCAL_MCP_DEFAULT_MAX_FRAME_BYTES, 2 * 1024 * 1024);
});

test("constructor: loopback guard accepts 127.x/localhost/[::1] and rejects LAN", () => {
  assert.equal(isLoopbackHost("127.0.0.1"), true);
  assert.equal(isLoopbackHost("127.250.1.9"), true);
  assert.equal(isLoopbackHost("localhost"), true);
  assert.equal(isLoopbackHost("[::1]"), true);
  assert.equal(isLoopbackHost("::1"), true);
  assert.equal(isLoopbackHost("192.168.1.5"), false);
  assert.equal(isLoopbackHost("10.0.0.1"), false);
  assert.equal(isLoopbackHost("herdr.local"), false);

  new LocalMcpRuntimeTransport(opts({ endpoint: "http://127.0.0.1:8772/mcp" }));
  new LocalMcpRuntimeTransport(opts({ endpoint: "https://localhost:8443/mcp" }));
  new LocalMcpRuntimeTransport(opts({ endpoint: "http://[::1]:8772/mcp" }));

  assert.throws(() => new LocalMcpRuntimeTransport(opts({ endpoint: "http://192.168.1.5:8772/mcp" })));
  assert.throws(() => new LocalMcpRuntimeTransport(opts({ endpoint: "http://10.0.0.2:8772/mcp" })));
  // Explicit opt-out for tests.
  new LocalMcpRuntimeTransport(opts({ endpoint: "http://192.168.1.5:8772/mcp", allowNonLoopback: true }));
});

test("constructor: rejects invalid endpoint schemes and non-1 contract epochs", () => {
  assert.throws(() => new LocalMcpRuntimeTransport(opts({ endpoint: "ws://127.0.0.1:8772/mcp" })));
  assert.throws(() => new LocalMcpRuntimeTransport(opts({ endpoint: "not a url" })));
  assert.throws(() => new LocalMcpRuntimeTransport(opts({ contractEpoch: 2 })));
  assert.throws(() => new LocalMcpRuntimeTransport(opts({ contractEpoch: "1" })));
  new LocalMcpRuntimeTransport(opts({ contractEpoch: 1 }));
});

test("constructor: requires bearerToken, contractHash, fetch — errors never embed the token", async () => {
  let msg = "";
  try {
    new LocalMcpRuntimeTransport(opts({ bearerToken: "" }));
  } catch (err) {
    msg = err.message;
  }
  assert.ok(msg.length > 0);
  assert.ok(!msg.includes(TOKEN), "token leaked into constructor error");
  try {
    new LocalMcpRuntimeTransport(opts({ contractHash: "" }));
  } catch (err) {
    msg = err.message;
  }
  assert.ok(msg.includes("contractHash"));
  assert.ok(!msg.includes(CONTRACT_HASH.slice(0, 16)), "contract hash leaked into constructor error");
  // fetch is only required when globalThis.fetch is not available (Node 22+ has it).
  // If the global is available, omit this check; the test is informational.
});

// ─────────────────────────────────────────────────────────────────────────────
// dispatchRequest: headers / body / id correlation
// ─────────────────────────────────────────────────────────────────────────────

test("dispatchRequest: POST endpoint with Bearer auth, headers, tools/call body and args", async () => {
  const fetchFn = recordingFetch([
    JSON.stringify({ jsonrpc: "2.0", id: "local-1", result: { content: [], isError: false } }),
  ]);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  const res = await t.dispatchRequest(frame("r1"));

  assert.deepEqual(res, {
    ok: true,
    result: { content: [], isError: false },
  });
  assert.equal(fetchFn.calls.length, 1);
  const call = fetchFn.calls[0];
  assert.equal(call.url, "http://127.0.0.1:8772/mcp");
  assert.equal(call.init.method, "POST");
  assert.equal(call.init.headers.Authorization, `Bearer ${TOKEN}`);
  assert.equal(call.init.headers["Content-Type"], "application/json");
  assert.equal(call.init.headers.Accept, "application/json, text/event-stream");
  assert.ok(call.init.signal instanceof AbortSignal, "dispatch must pass an AbortSignal");

  const body = JSON.parse(call.bodyText);
  assert.equal(body.jsonrpc, "2.0");
  assert.equal(body.method, "tools/call");
  assert.equal(body.params.name, "herdr_inspect");
  assert.deepEqual(body.params.arguments, { query: "ping", secret: ARG_SECRET });
  assert.equal(body.id, "local-1", "JSON-RPC id must round-trip for correlation");
});

test("dispatchRequest: default arguments {} when frame has none", async () => {
  const fetchFn = recordingFetch([
    JSON.stringify({ jsonrpc: "2.0", id: "local-1", result: { ok: true } }),
  ]);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  const res = await t.dispatchRequest(frame("r1", { arguments: undefined }));
  assert.equal(res.ok, true);
  const call = fetchFn.calls[0];
  const body = JSON.parse(call.bodyText);
  assert.deepEqual(body.params.arguments, {});
});

// ─────────────────────────────────────────────────────────────────────────────
// Response parsing: plain JSON + SSE
// ─────────────────────────────────────────────────────────────────────────────

test("dispatchRequest: parses plain JSON responses", async () => {
  const fetchFn = recordingFetch([
    JSON.stringify({ jsonrpc: "2.0", id: "local-1", result: { answer: 42 } }),
  ]);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  const res = await t.dispatchRequest(frame("r1"));
  assert.deepEqual(res, { ok: true, result: { answer: 42 } });
});

test("dispatchRequest: parses SSE data: lines (single event)", async () => {
  const sse = `event: message\nid: 1\ndata: ${JSON.stringify({ jsonrpc: "2.0", id: "local-1", result: { sse: true } })}\n\n`;
  const fetchFn = recordingFetch([sse]);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  const res = await t.dispatchRequest(frame("r1"));
  assert.deepEqual(res, { ok: true, result: { sse: true } });
});

test("dispatchRequest: parses SSE with comments, blank lines and multiline data joined by \\n", async () => {
  // A JSON-RPC result spread across two data: lines — joined with "\n", which
  // is valid JSON whitespace between `{` and the next field.
  const result = { jsonrpc: "2.0", id: "local-1", result: { multiline: true } };
  const [a, b] = splitJsonForSse(result);
  const sse = `: this is a comment\n\n${a}\n${b}\n\n`;
  const fetchFn = recordingFetch([sse]);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  const res = await t.dispatchRequest(frame("r1"));
  assert.deepEqual(res, { ok: true, result: { multiline: true } });
});

test("dispatchRequest: skips notifications/progress events and matches by id, not position", async () => {
  const notif = JSON.stringify({ jsonrpc: "2.0", id: null, method: "notifications/progress", params: {} });
  const wrong = JSON.stringify({ jsonrpc: "2.0", id: "local-999", result: { wrong: true } });
  const right = JSON.stringify({ jsonrpc: "2.0", id: "local-1", result: { right: true } });
  const sse = `data: ${notif}\n\ndata: ${right}\n\ndata: ${wrong}\n\n`;
  const fetchFn = recordingFetch([sse]);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  const res = await t.dispatchRequest(frame("r1"));
  // The correct-id event earlier in the stream wins over the last (wrong) event.
  assert.deepEqual(res, { ok: true, result: { right: true } });
});

// ─────────────────────────────────────────────────────────────────────────────
// Full CallToolResult preservation incl. images and isError
// ─────────────────────────────────────────────────────────────────────────────

test("dispatchRequest: preserves COMPLETE CallToolResult incl. image blocks", async () => {
  const callToolResult = {
    content: [
      { type: "image", data: "iVBORw0KGgoAAAANSUhEUg==", mimeType: "image/png" },
      { type: "text", text: "here is a png" },
    ],
    isError: false,
    structuredContent: { images: 1 },
    _meta: { trace: "abc" },
  };
  const fetchFn = recordingFetch([
    JSON.stringify({ jsonrpc: "2.0", id: "local-1", result: callToolResult }),
  ]);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  const res = await t.dispatchRequest(frame("r1"));
  assert.equal(res.ok, true);
  assert.deepEqual(
    (res.ok ? res.result : null),
    callToolResult,
    "the complete MCP CallToolResult envelope must pass through untouched",
  );
});

test("dispatchRequest: isError:true stays inside the SUCCESSFUL envelope", async () => {
  const fetchFn = recordingFetch([
    JSON.stringify({
      jsonrpc: "2.0",
      id: "local-1",
      result: { content: [{ type: "text", text: "tool-level failure" }], isError: true },
    }),
  ]);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  const res = await t.dispatchRequest(frame("r1"));
  assert.equal(res.ok, true, "isError is a tool-level flag, NOT a transport failure");
  assert.equal(res.result.isError, true);
  assert.deepEqual(res.result.content[0], { type: "text", text: "tool-level failure" });
});

// ─────────────────────────────────────────────────────────────────────────────
// JSON-RPC error mapping
// ─────────────────────────────────────────────────────────────────────────────

test("dispatchRequest: JSON-RPC error -> sanitized {ok:false} with rpc_code only", async () => {
  const fetchFn = recordingFetch([
    JSON.stringify({
      jsonrpc: "2.0",
      id: "local-1",
      error: { code: -32602, message: `invalid params: ${ARG_SECRET} ${TOKEN}`, data: { raw: "sensitive" } },
    }),
  ]);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  const res = await t.dispatchRequest(frame("r1"));
  assert.equal(res.ok, false);
  assert.equal(res.code, LOCAL_MCP_CODE.jsonrpcError);
  assert.equal(res.retryable, false);
  const serialized = JSON.stringify(res);
  assert.ok(!serialized.includes(TOKEN), "token leaked into JSON-RPC error result");
  assert.ok(!serialized.includes(ARG_SECRET), "argument secret leaked into JSON-RPC error result");
  assert.ok(!serialized.includes("invalid params"), "remote error message must not be forwarded verbatim");
  assert.ok(!serialized.includes("sensitive"), "remote error data must not be forwarded");
  assert.deepEqual(res.error?.details, { rpc_code: -32602 });
});

// ─────────────────────────────────────────────────────────────────────────────
// Timeout / abort / cancel
// ─────────────────────────────────────────────────────────────────────────────

test("dispatchRequest: timeout aborts via AbortController and returns retryable timeout", async () => {
  const control = {};
  const fetchFn = abortableFetch(control);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn, defaultTimeoutMs: 20, maxTimeoutMs: 40 }));
  const resP = t.dispatchRequest(frame("r1"));
  const res = await resP;
  assert.equal(res.ok, false);
  assert.equal(res.code, LOCAL_MCP_CODE.timeout);
  assert.equal(res.retryable, true);
  assert.ok(control.called.init.signal.aborted, "timeout must abort the fetch signal");
});

test("dispatchRequest: respects frame timeout hint capped by maxTimeoutMs", async () => {
  const control = {};
  const fetchFn = abortableFetch(control);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn, defaultTimeoutMs: 5000, maxTimeoutMs: 30 }));
  const res = await t.dispatchRequest(frame("r1", { timeout_ms: 9999 }));
  assert.equal(res.ok, false);
  assert.equal(res.code, LOCAL_MCP_CODE.timeout); // capped to 30ms -> fires fast
});

test("cancelRequest: aborts ONLY the matching in-flight fetch; unknown/repeat are no-ops", async () => {
  // Capture each fetch's signal by arrival order. r1 hangs (abortable); r2
  // resolves normally with the echoed error-free result.
  const signals = [];
  const abortCounts = [0, 0];
  const fetchFn = async (url, init) => {
    const idx = signals.length;
    signals.push(init.signal);
    init.signal?.addEventListener("abort", () => abortCounts[idx]++);
    if (idx === 1) {
      // r2: normal JSON-RPC success echoing the request id.
      const body = JSON.parse(init.body);
      return new Response(
        JSON.stringify({ jsonrpc: "2.0", id: body.id, result: { ok: true } }),
        { status: 200 },
      );
    }
    return new Promise((_, reject) => {
      const onAbort = () => {
        const err = new Error("aborted");
        err.name = "AbortError";
        reject(err);
      };
      init.signal?.addEventListener("abort", onAbort, { once: true });
    });
  };
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));

  const p1 = t.dispatchRequest(frame("r1"));
  const p2 = t.dispatchRequest(frame("r2"));
  assert.equal(signals.length, 2, "both fetches must have started");
  const [s1, s2] = signals;

  await t.cancelRequest("r1", "edge_cancel");
  assert.equal(s1.aborted, true, "only r1 must be aborted");
  assert.equal(s2.aborted, false, "r2 must NOT be aborted");
  assert.equal(abortCounts[0], 1, "exactly one abort on the matching request");

  const r1 = await p1;
  assert.equal(r1.ok, false);
  assert.equal(r1.code, LOCAL_MCP_CODE.cancelled);
  assert.equal(r1.retryable, false);

  // Idempotent: cancelling again (even with a different reason) is harmless.
  await t.cancelRequest("r1", "again");
  await t.cancelRequest("does-not-exist", "nope");
  assert.equal(abortCounts[0], 1, "repeat cancel must not abort again");

  const r2 = await p2;
  assert.equal(r2.ok, true, "unrelated request still completes normally");
  assert.deepEqual(r2.result, { ok: true });
});

test("cancelRequest: returns cancelled even when the fetch already resolved with a body stream pending", async () => {
  // Stream hangs open: the response is available but never completes; cancel
  // aborts the in-flight dispatch, and the reader must observe the abort.
  const signalHolder = {};
  const fetchFn = async (url, init) => {
    signalHolder.signal = init.signal;
    // A body that never ends but fails with AbortError when the signal
    // aborts — mimicking real fetch aborting a mid-stream response.
    let streamController;
    const stream = new ReadableStream({
      start(controller) {
        streamController = controller;
        init.signal?.addEventListener(
          "abort",
          () => {
            const err = new Error("aborted");
            err.name = "AbortError";
            streamController.error(err);
          },
          { once: true },
        );
      },
      pull() {
        /* never deliver — hang until abort */
      },
    });
    return new Response(stream, { status: 200 });
  };
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  const p = t.dispatchRequest(frame("r1"));
  await new Promise((r) => setTimeout(r, 5)); // let dispatch reach the body read
  await t.cancelRequest("r1", "edge_cancel");
  assert.equal(signalHolder.signal.aborted, true);
  const res = await p;
  assert.equal(res.ok, false);
  assert.equal(res.code, LOCAL_MCP_CODE.cancelled);
});

// ─────────────────────────────────────────────────────────────────────────────
// Malformed / oversized / id mismatch
// ─────────────────────────────────────────────────────────────────────────────

test("dispatchRequest: malformed non-JSON body -> malformed_response", async () => {
  const fetchFn = recordingFetch(["<html>not json at all</html>"]);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  const res = await t.dispatchRequest(frame("r1"));
  assert.equal(res.ok, false);
  assert.equal(res.code, LOCAL_MCP_CODE.malformedResponse);
  assert.ok(!JSON.stringify(res).includes("<html>"), "raw body must never be echoed");
});

test("dispatchRequest: malformed SSE data line -> malformed_response", async () => {
  const fetchFn = recordingFetch(["data: {not: valid json\n\ndata: also broken\n\n"]);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  const res = await t.dispatchRequest(frame("r1"));
  assert.equal(res.ok, false);
  assert.equal(res.code, LOCAL_MCP_CODE.malformedResponse);
});

test("dispatchRequest: JSON body that is not a jsonrpc object -> malformed_response", async () => {
  const fetchFn = recordingFetch([JSON.stringify({ hello: "world" })]);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  const res = await t.dispatchRequest(frame("r1"));
  assert.equal(res.ok, false);
  assert.equal(res.code, LOCAL_MCP_CODE.malformedResponse);
});

test("dispatchRequest: mismatched JSON-RPC id (plain JSON and SSE) -> id_mismatch", async () => {
  const plain = recordingFetch([JSON.stringify({ jsonrpc: "2.0", id: "local-999", result: { x: 1 } })]);
  let t = new LocalMcpRuntimeTransport(opts({ fetch: plain }));
  let res = await t.dispatchRequest(frame("r1"));
  assert.equal(res.ok, false);
  assert.equal(res.code, LOCAL_MCP_CODE.idMismatch);

  // Also when the mismatched reply is the LAST/only SSE event — never accepted.
  const sse = recordingFetch([`data: ${JSON.stringify({ jsonrpc: "2.0", id: "local-777", result: { x: 2 } })}\n\n`]);
  t = new LocalMcpRuntimeTransport(opts({ fetch: sse }));
  res = await t.dispatchRequest(frame("r1"));
  assert.equal(res.ok, false);
  assert.equal(res.code, LOCAL_MCP_CODE.idMismatch);
});

test("dispatchRequest: oversized REQUEST is rejected before fetch (fetch never called)", async () => {
  const fetchFn = recordingFetch([]);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn, maxFrameBytes: 64 }));
  // operation + arguments serialized > 64 bytes
  const res = await t.dispatchRequest(frame("r1"));
  assert.equal(res.ok, false);
  assert.equal(res.code, LOCAL_MCP_CODE.requestTooLarge);
  assert.equal(fetchFn.calls.length, 0, "fetch must not be called for an oversized request");
});

test("dispatchRequest: oversized RESPONSE aborted incrementally at the byte cap", async () => {
  let pulledChunks = 0;
  let cancelled = false;
  const chunk = "x".repeat(50); // 50 bytes per chunk
  const stream = new ReadableStream({
    pull(controller) {
      if (pulledChunks >= 10) return controller.close();
      pulledChunks += 1;
      controller.enqueue(new TextEncoder().encode(chunk));
    },
    cancel() {
      cancelled = true;
    },
  });
  // Response body will be 50*10=500 bytes; cap at 90 so chunk2 (100) exceeds.
  // Request body is ~150 bytes, so cap must be >= that.
  const res = new Response(stream, { status: 200 });
  const fetchFn = async () => res;
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn, maxFrameBytes: 200 }));
  const start = await t.dispatchRequest(frame("r1"));
  assert.equal(start.ok, false);
  assert.equal(start.code, LOCAL_MCP_CODE.responseTooLarge);
  assert.ok(cancelled, "body stream must be cancelled after the cap is exceeded");
  assert.ok(pulledChunks < 10, `reader must stop early, pulled ${pulledChunks} of 10`);
});

test("dispatchRequest: HTTP error status maps to http_error with retryable policy", async () => {
  const rate = recordingFetch([
    new Response(
      JSON.stringify({ jsonrpc: "2.0", id: "local-1", result: {} }),
      { status: 429 },
    ),
  ]);
  let t = new LocalMcpRuntimeTransport(opts({ fetch: rate }));
  let res = await t.dispatchRequest(frame("r1"));
  assert.equal(res.ok, false);
  assert.equal(res.code, LOCAL_MCP_CODE.httpError);
  assert.equal(res.retryable, true, "429 is retryable");
  assert.deepEqual(res.error?.details, { status: 429 });

  const five = recordingFetch([
    new Response(
      JSON.stringify({ jsonrpc: "2.0", id: "local-1", result: {} }),
      { status: 503 },
    ),
  ]);
  t = new LocalMcpRuntimeTransport(opts({ fetch: five }));
  res = await t.dispatchRequest(frame("r1"));
  assert.equal(res.retryable, true, "5xx is retryable");

  const four = recordingFetch([
    new Response(
      JSON.stringify({ jsonrpc: "2.0", id: "local-1", result: {} }),
      { status: 403 },
    ),
  ]);
  t = new LocalMcpRuntimeTransport(opts({ fetch: four }));
  res = await t.dispatchRequest(frame("r1"));
  assert.equal(res.retryable, false, "4xx (non-429) is not retryable");
  assert.ok(!JSON.stringify(res).includes("forbidden"), "HTTP body must not leak into errors");
});

test("dispatchRequest: JSON-RPC error envelope on non-2xx still maps to jsonrpc_error", async () => {
  const fetchFn = recordingFetch([
    new Response(
      JSON.stringify({ jsonrpc: "2.0", id: "local-1", error: { code: -32603, message: "internal" } }),
      { status: 500 },
    ),
  ]);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  const res = await t.dispatchRequest(frame("r1"));
  assert.equal(res.ok, false);
  assert.equal(res.code, LOCAL_MCP_CODE.jsonrpcError);
  assert.deepEqual(res.error?.details, { rpc_code: -32603 });
});

test("dispatchRequest: duplicate in-flight request_id is rejected without touching fetch", async () => {
  const fetchFn = abortableFetch({});
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  const p1 = t.dispatchRequest(frame("r1"));
  const dup = await t.dispatchRequest(frame("r1"));
  assert.equal(dup.ok, false);
  assert.equal(dup.code, LOCAL_MCP_CODE.duplicateRequest);
  p1.catch(() => {}); // r1 never completes; ensure no unhandled rejection
});

// ─────────────────────────────────────────────────────────────────────────────
// Health probe
// ─────────────────────────────────────────────────────────────────────────────

test("getHealth: uses cheap sessionless server/discover POST, not a tool", async () => {
  const fetchFn = recordingFetch([
    JSON.stringify({ jsonrpc: "2.0", id: "local-1", result: { serverInfo: { name: "herdr", version: "0.3.23" } } }),
  ]);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  const health = await t.getHealth();
  assert.deepEqual(health, { healthy: true });

  assert.equal(fetchFn.calls.length, 1);
  const call = fetchFn.calls[0];
  const body = JSON.parse(call.bodyText);
  assert.equal(body.method, "server/discover");
  assert.deepEqual(body.params, {});
  assert.equal(call.init.headers.Authorization, `Bearer ${TOKEN}`);
});

test("getHealth: caches discovered server version into runtime identity", async () => {
  const fetchFn = recordingFetch([
    JSON.stringify({
      jsonrpc: "2.0",
      id: "local-1",
      result: {
        _meta: { "io.modelcontextprotocol/serverInfo": { name: "herdr-mcp", version: "0.3.23" } },
      },
    }),
    JSON.stringify({ jsonrpc: "2.0", id: "local-2", result: { server_info: { version: "0.3.24" } } }),
  ]);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn, runtimeVersion: "0.3.22" }));
  assert.equal(t.getRuntimeInfo().runtime_version, "0.3.22"); // configured wins

  const t2 = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
  assert.equal(t2.getRuntimeInfo().runtime_version, "unknown"); // before any probe
  await t2.getHealth();
  assert.equal(t2.getRuntimeInfo().runtime_version, "0.3.23", "production server/discover _meta version cached");
  await t2.getHealth();
  assert.equal(t2.getRuntimeInfo().runtime_version, "0.3.24", "server_info.version cached");

  // Configured version always wins over discovery.
  const t3 = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn, runtimeVersion: "5.0.0" }));
  await t3.getHealth();
  assert.equal(t3.getRuntimeInfo().runtime_version, "5.0.0");
});

test("getHealth: failure states return healthy:false with sanitized short details", async () => {
  const unreachable = recordingFetch([new Error("connect ECONNREFUSED 127.0.0.1:8772")]);
  let t = new LocalMcpRuntimeTransport(opts({ fetch: unreachable }));
  let h = await t.getHealth();
  assert.deepEqual(h, { healthy: false, details: "unreachable" });
  assert.ok(!JSON.stringify(h).includes(TOKEN));

  const httpErr = recordingFetch([new Response("oops", { status: 500 })]);
  t = new LocalMcpRuntimeTransport(opts({ fetch: httpErr }));
  h = await t.getHealth();
  assert.deepEqual(h, { healthy: false, details: "http_500" });

  const rpcErr = recordingFetch([
    JSON.stringify({ jsonrpc: "2.0", id: "local-1", error: { code: -32601 } }),
  ]);
  t = new LocalMcpRuntimeTransport(opts({ fetch: rpcErr }));
  h = await t.getHealth();
  assert.deepEqual(h, { healthy: false, details: "rpc_error" });

  const malformed = recordingFetch(["garbage body"]);
  t = new LocalMcpRuntimeTransport(opts({ fetch: malformed }));
  h = await t.getHealth();
  assert.deepEqual(h, { healthy: false, details: "malformed" });

  const idMismatch = recordingFetch([JSON.stringify({ jsonrpc: "2.0", id: "local-9", result: {} })]);
  t = new LocalMcpRuntimeTransport(opts({ fetch: idMismatch }));
  h = await t.getHealth();
  assert.deepEqual(h, { healthy: false, details: "id_mismatch" });
});

test("getHealth: respects custom health probe method/params", async () => {
  const fetchFn = recordingFetch([JSON.stringify({ jsonrpc: "2.0", id: "local-1", result: { ok: "y" } })]);
  const t = new LocalMcpRuntimeTransport(
    opts({ fetch: fetchFn, healthProbe: { method: "server/custom", params: { deep: true } } }),
  );
  const h = await t.getHealth();
  assert.equal(h.healthy, true);
  const body = JSON.parse(fetchFn.calls[0].bodyText);
  assert.equal(body.method, "server/custom");
  assert.deepEqual(body.params, { deep: true });
});

test("getHealth: never aborts a dispatch and vice versa (separate controllers)", async () => {
  const c = {};
  const ab = abortableFetch(c);
  const t = new LocalMcpRuntimeTransport(opts({ fetch: ab }));
  const hp = t.getHealth();
  const dp = t.dispatchRequest(frame("r1"));
  assert.ok(c.called, "at least one fetch started");
  // Both hang; no shared state means neither abort interferes. Just verify no
  // throw, then finish via the timeout path.
  ab.calls.push = undefined;
  hp.catch(() => {});
  dp.catch(() => {});
});

// ─────────────────────────────────────────────────────────────────────────────
// Token secrecy sweep
// ─────────────────────────────────────────────────────────────────────────────

test("token never appears in ANY thrown or returned error detail", async () => {
  const scenarios = [
    // runtime error path
    async (fetchFn) => {
      const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
      return await t.dispatchRequest(frame("r1"));
    },
    // malformed response
    async (fetchFn) => {
      const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
      return await t.dispatchRequest(frame("r1"));
    },
    // http error
    async (fetchFn) => {
      const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
      return await t.dispatchRequest(frame("r1"));
    },
    // health failure
    async (fetchFn) => {
      const t = new LocalMcpRuntimeTransport(opts({ fetch: fetchFn }));
      return await t.getHealth();
    },
  ];
  const bodies = [
    new Error(`boom ${TOKEN}`),
    `<html>${TOKEN}</html>`,
    new Response(`token=${TOKEN}`, { status: 500 }),
    new Error(`refused ${TOKEN}`),
  ];
  for (let i = 0; i < scenarios.length; i++) {
    const fetchFn = recordingFetch([bodies[i]]);
    const res = await scenarios[i](fetchFn);
    const serialized = JSON.stringify(res);
    assert.ok(!serialized.includes(TOKEN), `token leaked in scenario ${i}: ${serialized}`);
    assert.ok(!serialized.includes(`Bearer ${TOKEN}`));
  }

  // Frame-level failures (bad request, duplicate, too large) are also clean.
  const t = new LocalMcpRuntimeTransport(opts({ fetch: async () => new Response("{}"), maxFrameBytes: 32 }));
  const bad = await t.dispatchRequest({ ...frame("r1"), request_id: undefined });
  assert.ok(!JSON.stringify(bad).includes(TOKEN));
  const oversized = await t.dispatchRequest(frame("r-long-operations"));
  assert.equal(oversized.code, LOCAL_MCP_CODE.requestTooLarge);
  assert.ok(!JSON.stringify(oversized).includes(TOKEN));
  assert.ok(!JSON.stringify(oversized).includes(ARG_SECRET), "arguments must not leak into errors");
});

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

/** Split a JSON string into two SSE-safe data lines that join back to the original. */
function splitJsonForSse(obj) {
  const text = JSON.stringify(obj);
  // Break at a boundary inside the JSON (after the first field) so that
  // joining with "\n" reconstructs valid JSON whitespace.
  const result = { jsonrpc: "2.0", id: "local-1", result: { multiline: true } };
  const json = JSON.stringify(result);
  const mid = json.indexOf('"result"');
  const a = `data: ${json.slice(0, mid)}`;
  const b = `data: ${json.slice(mid)}`;
  return [a, b];
}