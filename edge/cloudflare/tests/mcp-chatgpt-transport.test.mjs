import { test } from "node:test";
import assert from "node:assert/strict";

import {
  isOpenAiMcpUserAgent,
  isChatgptOAuthClientId,
  createSessionlessMcpProbeResponse,
  serializeMcpResponse,
  sseFrameEvent,
  OPENAI_MCP_UA_MARKER,
  DEFAULT_PROBE_HEARTBEAT_MS,
} from "../dist/mcp-chatgpt-transport.js";

// ---------------------------------------------------------------------------
// 1. Detection
// ---------------------------------------------------------------------------

test("isOpenAiMcpUserAgent: detects openai-mcp UA case-insensitively", () => {
  assert.equal(OPENAI_MCP_UA_MARKER, "openai-mcp");
  assert.equal(isOpenAiMcpUserAgent("openai-mcp/1.0.0"), true);
  assert.equal(isOpenAiMcpUserAgent("OpenAI-MCP/1.0"), true);
  assert.equal(isOpenAiMcpUserAgent("OPENAI-MCP"), true);
  assert.equal(isOpenAiMcpUserAgent("something openai-mcp too"), true);
});

test("isOpenAiMcpUserAgent: rejects null, undefined, and non-matching UAs", () => {
  assert.equal(isOpenAiMcpUserAgent(null), false);
  assert.equal(isOpenAiMcpUserAgent(undefined), false);
  assert.equal(isOpenAiMcpUserAgent(""), false);
  assert.equal(isOpenAiMcpUserAgent("curl/7.88"), false);
  assert.equal(isOpenAiMcpUserAgent("claude-connector/1.0"), false);
  assert.equal(isOpenAiMcpUserAgent("Mozilla/5.0"), false);
});

test("isChatgptOAuthClientId: recognizes ChatGPT CIMD client ids only", () => {
  assert.equal(isChatgptOAuthClientId("https://chatgpt.com/client"), true);
  assert.equal(isChatgptOAuthClientId("https://www.chatgpt.com/oauth/client"), true);
  assert.equal(isChatgptOAuthClientId("https://example.com/chatgpt"), false);
  assert.equal(isChatgptOAuthClientId("not-a-url"), false);
  assert.equal(isChatgptOAuthClientId(undefined), false);
});

// ---------------------------------------------------------------------------
// 2. GET probe: first chunk, heartbeat, no session header, no credential leak
// ---------------------------------------------------------------------------

test("GET probe: default heartbeat constant mirrors production 15s", () => {
  assert.equal(DEFAULT_PROBE_HEARTBEAT_MS, 15_000);
  // Changing the default from production's 15s would violate proven behavior.
});

test("GET probe: first chunk is ': connected\\n\\n', no Mcp-Session-Id, no Authorization", async () => {
  const res = createSessionlessMcpProbeResponse();
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("content-type"), "text/event-stream");
  assert.equal(res.headers.get("cache-control"), "no-cache, no-transform");
  assert.equal(res.headers.get("mcp-session-id"), null, "must NOT issue a session id");
  assert.equal(res.headers.get("authorization"), null, "must NOT leak credentials");
  // Read the first chunk
  const reader = res.body.getReader();
  const { value } = await reader.read();
  const first = new TextDecoder().decode(value);
  assert.equal(first, ": connected\n\n");
  reader.releaseLock();
  await res.body.cancel();
});

test("GET probe: heartbeat frame is written after heartbeatMs elapses", async () => {
  // Use a short injectable interval so the test runs fast.
  const res = createSessionlessMcpProbeResponse({ heartbeatMs: 10 });
  assert.equal(res.status, 200);
  const reader = res.body.getReader();
  // Read the first 'connected' frame
  await reader.read();
  // Wait for one heartbeat
  await new Promise((r) => setTimeout(r, 30));
  const { value: hbVal } = await reader.read();
  const hb = new TextDecoder().decode(hbVal);
  assert.equal(hb, ": keepalive\n\n");
  reader.releaseLock();
  await res.body.cancel();
});

test("GET probe: cleanup timer fires on cancel, onCleanup invoked once", async () => {
  let cleanupCount = 0;
  const res = createSessionlessMcpProbeResponse({
    heartbeatMs: 10,
    onCleanup: () => { cleanupCount++; },
  });
  await res.body.cancel();
  // Wait a bit to prove no second fire
  await new Promise((r) => setTimeout(r, 30));
  assert.equal(cleanupCount, 1, "onCleanup must be called exactly once");
});

test("GET probe: cleanup also fires on signal abort", async () => {
  const ac = new AbortController();
  let cleanupCount = 0;
  const res = createSessionlessMcpProbeResponse({
    heartbeatMs: 10,
    signal: ac.signal,
    onCleanup: () => { cleanupCount++; },
  });
  ac.abort();
  // Give the queued abort task a moment to run.
  await new Promise((r) => setTimeout(r, 10));
  assert.equal(cleanupCount, 1, "onCleanup must be called on abort");
  await res.body.cancel();
});

test("GET probe: pre-aborted signal triggers immediate cleanup", async () => {
  const ac = new AbortController();
  ac.abort();
  let cleanupCount = 0;
  const res = createSessionlessMcpProbeResponse({
    heartbeatMs: 10,
    signal: ac.signal,
    onCleanup: () => { cleanupCount++; },
  });
  assert.equal(cleanupCount, 1, "onCleanup must fire when signal is already aborted");
  await res.body.cancel();
});

// ---------------------------------------------------------------------------
// 3. POST SSE framing: initialize and tools/list
// ---------------------------------------------------------------------------

test("SSE frame: event: message + data: json", () => {
  const payload = { jsonrpc: "2.0", id: 1, result: { protocolVersion: "2025-11-25", capabilities: {}, serverInfo: { name: "test", version: "1" } } };
  const frame = sseFrameEvent(payload);
  assert.match(frame, /^event: message\n/);
  assert.match(frame, /\ndata: /);
  const parsed = JSON.parse(frame.split("\n").find((l) => l.startsWith("data: ")).slice(6));
  assert.equal(parsed.id, 1);
  assert.equal(parsed.result.protocolVersion, "2025-11-25");
  assert.equal(frame.endsWith("\n\n"), true, "frame must end with double newline");
});

test("serializeMcpResponse: ChatGPT initialize -> SSE framing", () => {
  const result = { status: 200, body: { jsonrpc: "2.0", id: 1, result: { serverInfo: { name: "test", version: "1" } } } };
  const res = serializeMcpResponse(result, { userAgent: "openai-mcp/1.0.0", method: "initialize" });
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("content-type"), "text/event-stream");
  assert.equal(res.headers.get("cache-control"), "no-cache, no-transform");
  assert.equal(res.headers.get("mcp-session-id"), null, "must NOT issue session id");
  // Verify the body is the SSE event frame
  return res.text().then((body) => {
    assert.match(body, /^event: message\n/);
    assert.match(body, /serverInfo/);
  });
});

test("serializeMcpResponse: ChatGPT tools/list -> SSE framing", () => {
  const result = { status: 200, body: { jsonrpc: "2.0", id: 2, result: { tools: [] } } };
  const res = serializeMcpResponse(result, { userAgent: "openai-mcp/1.0.0", method: "tools/list" });
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("content-type"), "text/event-stream");
  return res.text().then((body) => {
    assert.match(body, /^event: message\n/);
  });
});

test("serializeMcpResponse: validated ChatGPT OAuth client_id triggers SSE without OpenAI UA", () => {
  const result = { status: 200, body: { jsonrpc: "2.0", id: 22, result: { tools: [] } } };
  const res = serializeMcpResponse(result, {
    userAgent: "generic-mcp-client/1.0",
    oauthClientId: "https://chatgpt.com/client",
    method: "tools/list",
  });
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("content-type"), "text/event-stream");
});

// ---------------------------------------------------------------------------
// 4. POST JSON framing: tools/call, server/discover, errors, non-ChatGPT
// ---------------------------------------------------------------------------

test("serializeMcpResponse: ChatGPT tools/call -> JSON", () => {
  const result = { status: 200, body: { jsonrpc: "2.0", id: 3, result: { content: [{ type: "text", text: "ok" }] } } };
  const res = serializeMcpResponse(result, { userAgent: "openai-mcp/1.0.0", method: "tools/call" });
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("content-type"), "application/json");
  assert.equal(res.headers.get("cache-control"), "no-store");
  return res.json().then((body) => {
    assert.equal(body.id, 3);
  });
});

test("serializeMcpResponse: ChatGPT server/discover -> JSON (follows production)", () => {
  const result = { status: 200, body: { jsonrpc: "2.0", id: "d", result: { supportedVersions: ["2025-11-25"], capabilities: {}, instructions: "", ttlMs: 3_600_000, cacheScope: "private" } } };
  const res = serializeMcpResponse(result, { userAgent: "openai-mcp/1.0.0", method: "server/discover" });
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("content-type"), "application/json");
  return res.json().then((body) => {
    assert.equal(body.result.supportedVersions[0], "2025-11-25");
  });
});

test("serializeMcpResponse: non-ChatGPT -> JSON for all methods", () => {
  const result = { status: 200, body: { jsonrpc: "2.0", id: 1, result: { ok: true } } };
  const ua = "curl/7.88";
  for (const method of ["initialize", "tools/list", "tools/call", "server/discover"]) {
    const res = serializeMcpResponse(result, { userAgent: ua, method });
    assert.equal(res.status, 200);
    assert.equal(res.headers.get("content-type"), "application/json", `method=${method}`);
    assert.equal(res.headers.get("cache-control"), "no-store", `method=${method}`);
  }
});

// ---------------------------------------------------------------------------
// 5. 204 notification (body === null)
// ---------------------------------------------------------------------------

test("serializeMcpResponse: notification 204 with null body, no content-type", () => {
  const res = serializeMcpResponse({ status: 204, body: null }, { userAgent: "openai-mcp/1.0.0", method: "notifications/initialized" });
  assert.equal(res.status, 204);
  assert.equal(res.headers.get("content-type"), null, "204 must not carry content-type");
  // Must not be a JSON "null" body
  return res.text().then((body) => {
    assert.equal(body, "", "204 body must be empty");
  });
});

// ---------------------------------------------------------------------------
// 6. Stale Mcp-Session-Id ignored (by construction, no session channel)
// ---------------------------------------------------------------------------

test("GET probe never echoes Mcp-Session-Id (no session channel)", async () => {
  const res = createSessionlessMcpProbeResponse();
  assert.equal(res.headers.get("mcp-session-id"), null);
  const reader = res.body.getReader();
  const first = await reader.read();
  assert.match(new TextDecoder().decode(first.value), /^: connected/);
  reader.releaseLock();
  await res.body.cancel();
});

// Realistic stale-header simulation: a router receives a request carrying both
// the openai-mcp User-Agent AND a stale Mcp-Session-Id. Only the UA-derived
// value flows into these helpers (which have no session input), so the stale
// header cannot change framing, status, or headers — and neither response
// ever emits Mcp-Session-Id.
test("stale Mcp-Session-Id header has no effect on framing or headers", async () => {
  const incoming = new Headers({
    "user-agent": "openai-mcp/1.0.0",
    "mcp-session-id": "stale-session-id",
    authorization: "Bearer should-not-leak",
  });

  // GET probe: router forwards only the UA.
  const probe = createSessionlessMcpProbeResponse();
  assert.equal(probe.headers.get("mcp-session-id"), null, "probe must not echo stale session");
  assert.equal(probe.headers.get("authorization"), null, "probe must not leak credentials");
  const reader = probe.body.getReader();
  const first = await reader.read();
  assert.match(new TextDecoder().decode(first.value), /^: connected/);
  reader.releaseLock();
  await probe.body.cancel();

  // POST: router forwards only UA + method.
  const list = serializeMcpResponse(
    { status: 200, body: { jsonrpc: "2.0", id: 2, result: { tools: [] } } },
    { userAgent: incoming.get("user-agent"), method: "tools/list" },
  );
  assert.match(list.headers.get("content-type") ?? "", /text\/event-stream/i, "SSE framing unchanged despite stale sid");
  assert.equal(list.headers.get("mcp-session-id"), null);

  const call = serializeMcpResponse(
    { status: 200, body: { jsonrpc: "2.0", id: 3, result: { ok: true } } },
    { userAgent: incoming.get("user-agent"), method: "tools/call" },
  );
  assert.match(call.headers.get("content-type") ?? "", /application\/json/i, "JSON framing unchanged despite stale sid");
  assert.equal(call.headers.get("mcp-session-id"), null);
});

test("serializeMcpResponse never echoes Mcp-Session-Id (no input to channel)", () => {
  const result = { status: 200, body: { jsonrpc: "2.0", id: 1, result: { ok: true } } };
  // No session parameter exists on the helpers — stale session is invisible by construction.
  const res = serializeMcpResponse(result, { userAgent: "openai-mcp/1.0.0", method: "initialize" });
  assert.equal(res.headers.get("mcp-session-id"), null);

  const jsonRes = serializeMcpResponse(result, { userAgent: "claude/1.0", method: "initialize" });
  assert.equal(jsonRes.headers.get("mcp-session-id"), null);
});

// ---------------------------------------------------------------------------
// 7. No credential leakage
// ---------------------------------------------------------------------------

test("all responses omit Authorization header from response", async () => {
  const probe = createSessionlessMcpProbeResponse();
  assert.equal(probe.headers.get("authorization"), null);
  await probe.body.cancel(); // stop the default 15s heartbeat interval

  const sse = serializeMcpResponse({ status: 200, body: { jsonrpc: "2.0", id: 1, result: {} } }, { userAgent: "openai-mcp/1.0.0", method: "initialize" });
  assert.equal(sse.headers.get("authorization"), null);

  const json = serializeMcpResponse({ status: 200, body: { jsonrpc: "2.0", id: 1, result: {} } }, { userAgent: "curl/1.0", method: "tools/call" });
  assert.equal(json.headers.get("authorization"), null);
});

// ---------------------------------------------------------------------------
// 8. Edge: heartbeat disabled with non-finite / <1
// ---------------------------------------------------------------------------

test("GET probe: heartbeat disabled when heartbeatMs is 0", async () => {
  const res = createSessionlessMcpProbeResponse({ heartbeatMs: 0, onCleanup: () => {} });
  const reader = res.body.getReader();
  const { value } = await reader.read();
  const first = new TextDecoder().decode(value);
  assert.equal(first, ": connected\n\n");
  reader.releaseLock();
  await res.body.cancel();
});

test("GET probe: heartbeat disabled when heartbeatMs is NaN", async () => {
  const res = createSessionlessMcpProbeResponse({ heartbeatMs: NaN });
  assert.equal(res.status, 200);
  await res.body.cancel();
});

// ---------------------------------------------------------------------------
// 9. Default probe headers (production-proven set)
// ---------------------------------------------------------------------------

test("GET probe: production header set", async () => {
  const res = createSessionlessMcpProbeResponse();
  assert.equal(res.headers.get("content-type"), "text/event-stream");
  assert.equal(res.headers.get("cache-control"), "no-cache, no-transform");
  assert.equal(res.headers.get("connection"), "keep-alive");
  assert.equal(res.headers.get("x-accel-buffering"), "no");
  await res.body.cancel();
});