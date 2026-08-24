// local-auth.js — tokenless Native Messaging transport for local Herdr access.
// Current extensions never receive HERDR_MCP_TOKEN or an expiring bearer.
// Chrome authenticates the native host origin; the host talks to herdr-mcp over
// its mode-0600 Unix-domain socket. Older extension builds remain compatible
// with the server's historical /extension/session endpoint.

export const HERDR_NATIVE_HOST = "dev.herdr.mcp";

function nativeMessage(message) {
  return new Promise((resolve) => {
    if (!globalThis.chrome?.runtime?.sendNativeMessage) {
      resolve({ ok: false, error: "native-messaging-unavailable" });
      return;
    }
    try {
      chrome.runtime.sendNativeMessage(HERDR_NATIVE_HOST, message, (response) => {
        const err = chrome.runtime.lastError?.message;
        if (err) {
          resolve({ ok: false, error: err });
          return;
        }
        resolve(response && typeof response === "object"
          ? response
          : { ok: false, error: "native-host-empty-response" });
      });
    } catch (error) {
      resolve({ ok: false, error: String(error?.message || error || "native-host-error") });
    }
  });
}

function normalizedHeaders(headers) {
  const out = {};
  try {
    const source = new Headers(headers || {});
    source.forEach((value, name) => {
      // The browser must never supply a Herdr Authorization header. The native
      // host owns the trusted local transport and any legacy compatibility auth.
      if (name.toLowerCase() === "authorization") return;
      out[name] = value;
    });
  } catch (_) {}
  return out;
}

function requestParts(input) {
  const url = new URL(String(input));
  return {
    baseUrl: url.origin,
    path: `${url.pathname}${url.search}`,
  };
}

export async function localHerdrFetch(input, init = {}) {
  const { baseUrl, path } = requestParts(input);
  const body = init.body == null ? "" : String(init.body);
  const timeoutMs = Number(init.nativeTimeoutMs || 10_000);
  const response = await nativeMessage({
    type: "request",
    base_url: baseUrl,
    path,
    method: String(init.method || "GET").toUpperCase(),
    headers: normalizedHeaders(init.headers),
    body,
    timeout_ms: timeoutMs,
  });
  if (response?.ok !== true) {
    throw new Error(String(response?.error || "native-host-request-failed"));
  }
  return new Response(String(response.body || ""), {
    status: Number(response.status || 500),
    headers: response.headers && typeof response.headers === "object" ? response.headers : {},
  });
}

function decodeBase64(value) {
  const text = atob(String(value || ""));
  const bytes = new Uint8Array(text.length);
  for (let i = 0; i < text.length; i += 1) bytes[i] = text.charCodeAt(i);
  return bytes;
}

export function openLocalHerdrStream({ baseUrl, path = "/push/events", timeoutMs = 10_000, onChunk } = {}) {
  let port = null;
  let openedSettled = false;
  let finished = false;
  let resolveOpened;
  let rejectOpened;
  let resolveDone;
  let rejectDone;
  const opened = new Promise((resolve, reject) => { resolveOpened = resolve; rejectOpened = reject; });
  const done = new Promise((resolve, reject) => { resolveDone = resolve; rejectDone = reject; });

  const fail = (error) => {
    const err = error instanceof Error ? error : new Error(String(error || "native-stream-failed"));
    if (!openedSettled) {
      openedSettled = true;
      rejectOpened(err);
    }
    if (!finished) {
      finished = true;
      rejectDone(err);
    }
  };

  if (!globalThis.chrome?.runtime?.connectNative) {
    fail(new Error("native-messaging-unavailable"));
    return { opened, done, close() {} };
  }

  try {
    port = chrome.runtime.connectNative(HERDR_NATIVE_HOST);
    port.onMessage.addListener((message) => {
      if (!message || typeof message !== "object") return;
      if (message.type === "stream_open") {
        if (!openedSettled) {
          openedSettled = true;
          resolveOpened({ status: Number(message.status || 0), transport: String(message.transport || "native") });
        }
        return;
      }
      if (message.type === "stream_chunk") {
        try { onChunk?.(decodeBase64(message.chunk_b64)); } catch (error) { fail(error); }
        return;
      }
      if (message.type === "stream_end") {
        if (!openedSettled) {
          openedSettled = true;
          resolveOpened({ status: 200, transport: "native" });
        }
        if (!finished) {
          finished = true;
          resolveDone();
        }
        return;
      }
      if (message.ok === false || message.type === "stream_error") {
        fail(new Error(String(message.error || `native-stream-http-${message.status || "error"}`)));
      }
    });
    port.onDisconnect.addListener(() => {
      const detail = chrome.runtime.lastError?.message || "native-stream-disconnected";
      if (!finished) fail(new Error(detail));
    });
    port.postMessage({
      type: "stream",
      base_url: String(baseUrl || "").replace(/\/+$/, ""),
      path,
      timeout_ms: Number(timeoutMs || 10_000),
    });
  } catch (error) {
    fail(error);
  }

  return {
    opened,
    done,
    close() {
      if (finished) return;
      finished = true;
      try { port?.disconnect(); } catch (_) {}
      if (!openedSettled) {
        openedSettled = true;
        rejectOpened(new Error("native-stream-closed"));
      }
      resolveDone();
    },
  };
}

export function resetLocalAuth() {
  // Compatibility export for older background/tests. Tokenless transport has
  // no credential cache to expire or refresh.
}
