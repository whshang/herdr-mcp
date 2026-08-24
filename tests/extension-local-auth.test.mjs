import test from "node:test";
import assert from "node:assert/strict";
import { getLocalAuth, resetLocalAuth, HERDR_NATIVE_HOST } from "../extension/local-auth.js";

test("extension prefers cached short-lived native credentials and supports legacy fallback", async () => {
  const oldChrome = globalThis.chrome;
  let calls = 0;
  globalThis.chrome = {
    runtime: {
      lastError: null,
      sendNativeMessage(host, message, callback) {
        calls++;
        assert.equal(host, HERDR_NATIVE_HOST);
        assert.equal(message.type, "session");
        callback({
          ok: true,
          token: `native-${calls}`,
          expires_at: new Date(Date.now() + 5 * 60_000).toISOString(),
          auth_mode: "native_session",
        });
      },
    },
  };

  try {
    resetLocalAuth();
    const first = await getLocalAuth({ baseUrl: "http://127.0.0.1:8772", legacyToken: "legacy" });
    assert.equal(first.source, "native");
    assert.equal(first.token, "native-1");
    const cached = await getLocalAuth({ baseUrl: "http://127.0.0.1:8772", legacyToken: "legacy" });
    assert.equal(cached.token, "native-1");
    assert.equal(calls, 1);
    const refreshed = await getLocalAuth({ baseUrl: "http://127.0.0.1:8772", legacyToken: "legacy", force: true });
    assert.equal(refreshed.token, "native-2");
    assert.equal(calls, 2);

    resetLocalAuth();
    globalThis.chrome.runtime.sendNativeMessage = (_host, _message, callback) => {
      globalThis.chrome.runtime.lastError = { message: "native host missing" };
      callback(undefined);
      globalThis.chrome.runtime.lastError = null;
    };
    const fallback = await getLocalAuth({ baseUrl: "http://127.0.0.1:8772", legacyToken: "legacy" });
    assert.equal(fallback.source, "legacy");
    assert.equal(fallback.token, "legacy");
  } finally {
    resetLocalAuth();
    globalThis.chrome = oldChrome;
  }
});
