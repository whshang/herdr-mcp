import test from "node:test";
import assert from "node:assert/strict";
import { localHerdrFetch, openLocalHerdrStream, HERDR_NATIVE_HOST } from "../extension/local-auth.js";

test("extension proxies localhost requests through Native Messaging without forwarding bearer auth", async () => {
  const oldChrome = globalThis.chrome;
  let seen = null;
  globalThis.chrome = {
    runtime: {
      lastError: null,
      sendNativeMessage(host, message, callback) {
        assert.equal(host, HERDR_NATIVE_HOST);
        seen = message;
        callback({
          ok: true,
          transport: "ipc",
          status: 200,
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ ok: true, source: "ipc" }),
        });
      },
    },
  };

  try {
    const response = await localHerdrFetch("http://127.0.0.1:8772/push/state", {
      headers: { Authorization: "Bearer must-not-cross-native-boundary", "X-Herdr-Client": "test" },
      nativeTimeoutMs: 4321,
    });
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), { ok: true, source: "ipc" });
    assert.equal(seen.type, "request");
    assert.equal(seen.base_url, "http://127.0.0.1:8772");
    assert.equal(seen.path, "/push/state");
    assert.equal(seen.timeout_ms, 4321);
    assert.equal(seen.headers.authorization, undefined);
    assert.equal(seen.headers["x-herdr-client"], "test");
  } finally {
    globalThis.chrome = oldChrome;
  }
});

test("extension receives push SSE bytes over one persistent Native Messaging stream", async () => {
  const oldChrome = globalThis.chrome;
  const messageListeners = [];
  const disconnectListeners = [];
  let posted = null;
  let disconnected = false;
  globalThis.chrome = {
    runtime: {
      lastError: null,
      connectNative(host) {
        assert.equal(host, HERDR_NATIVE_HOST);
        return {
          onMessage: { addListener(fn) { messageListeners.push(fn); } },
          onDisconnect: { addListener(fn) { disconnectListeners.push(fn); } },
          postMessage(message) {
            posted = message;
            queueMicrotask(() => {
              for (const fn of messageListeners) fn({ type: "stream_open", status: 200, transport: "ipc" });
              const bytes = Buffer.from("event: hello\ndata: {\"ok\":true}\n\n", "utf8");
              for (const fn of messageListeners) fn({ type: "stream_chunk", chunk_b64: bytes.toString("base64") });
              for (const fn of messageListeners) fn({ type: "stream_end" });
            });
          },
          disconnect() {
            disconnected = true;
            for (const fn of disconnectListeners) fn();
          },
        };
      },
    },
  };

  try {
    const chunks = [];
    const stream = openLocalHerdrStream({
      baseUrl: "http://127.0.0.1:8772",
      path: "/push/events",
      onChunk: (bytes) => chunks.push(Buffer.from(bytes)),
    });
    assert.deepEqual(await stream.opened, { status: 200, transport: "ipc" });
    await stream.done;
    assert.equal(posted.type, "stream");
    assert.equal(posted.path, "/push/events");
    assert.equal(Buffer.concat(chunks).toString("utf8"), "event: hello\ndata: {\"ok\":true}\n\n");
    stream.close();
    assert.equal(disconnected, false, "already-ended native stream needs no forced disconnect");
  } finally {
    globalThis.chrome = oldChrome;
  }
});
