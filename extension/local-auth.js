// local-auth.js — tokenless Native Messaging transport for local Herdr access.
// Current extensions never receive HERDR_MCP_TOKEN or an expiring bearer.
// Chrome authenticates the native host origin; the host talks to herdr-mcp over
// its mode-0600 Unix-domain socket. Older extension builds remain compatible
// with the server's historical /extension/session endpoint.

import { NATIVE_HOST_NOT_INSTALLED, NATIVE_ORIGIN_NOT_ACTIVE } from "./native-host-diagnostics.js";

export const HERDR_NATIVE_HOST = "dev.herdr.mcp";

function isNativeAdmissionDenied(message) {
  const text = String(message || "").toLowerCase();
  return text.includes("forbidden")
    || text.includes("not allowed")
    || text.includes("access denied")
    || text.includes("permission denied");
}

// Chromium reports a missing Native Messaging manifest as "Specified native
// messaging host not found." That is a different product state from an
// installed host whose registered origin is not this extension.
function isNativeHostMissing(message) {
  const text = String(message || "").toLowerCase();
  return text.includes("native messaging host") && text.includes("not found");
}

function normalizedNativeFailure(message) {
  const err = String(message || "native-host-error");
  if (isNativeHostMissing(err)) return { ok: false, error: NATIVE_HOST_NOT_INSTALLED };
  if (isNativeAdmissionDenied(err)) {
    return { ok: true, active: false, reason: NATIVE_ORIGIN_NOT_ACTIVE };
  }
  return { ok: false, error: err };
}

const NATIVE_CONTROL_IDLE_MS = 30_000;
let nativeControlPort = null;
let nativeControlIdleTimer = null;
let nativeControlSequence = 0;
let nativeControlLegacyResponseSafe = true;
const nativeControlPending = new Map();

function clearNativeControlIdleTimer() {
  if (nativeControlIdleTimer) clearTimeout(nativeControlIdleTimer);
  nativeControlIdleTimer = null;
}

function scheduleNativeControlIdleClose(port) {
  clearNativeControlIdleTimer();
  if (nativeControlPort !== port || nativeControlPending.size > 0) return;
  nativeControlIdleTimer = setTimeout(() => {
    nativeControlIdleTimer = null;
    if (nativeControlPort !== port || nativeControlPending.size > 0) return;
    nativeControlPort = null;
    try { port.disconnect(); } catch (_) {}
  }, NATIVE_CONTROL_IDLE_MS);
}

function settleNativeControlPending(value) {
  const pending = [...nativeControlPending.values()];
  nativeControlPending.clear();
  for (const entry of pending) {
    clearTimeout(entry.timer);
    entry.resolve(value);
  }
}

function nativeControlDisconnected(port, message = "native-host-empty-response") {
  if (nativeControlPort !== port) return;
  nativeControlPort = null;
  clearNativeControlIdleTimer();
  settleNativeControlPending(normalizedNativeFailure(message));
}

function nativeControlPortForRequests() {
  if (!globalThis.chrome?.runtime?.connectNative) return null;
  if (nativeControlPort) return nativeControlPort;
  const port = chrome.runtime.connectNative(HERDR_NATIVE_HOST);
  nativeControlPort = port;
  nativeControlLegacyResponseSafe = true;
  port.onMessage.addListener((response) => {
    if (nativeControlPort !== port) return;
    if (!response || typeof response !== "object") {
      nativeControlDisconnected(port);
      try { port.disconnect(); } catch (_) {}
      return;
    }
    const requestId = typeof response.request_id === "string" ? response.request_id : null;
    if (!requestId && (!nativeControlLegacyResponseSafe || nativeControlPending.size !== 1)) {
      nativeControlDisconnected(port, "native-host-uncorrelated-response");
      try { port.disconnect(); } catch (_) {}
      return;
    }
    const entry = requestId ? nativeControlPending.get(requestId) : nativeControlPending.values().next().value;
    if (!entry) return;
    nativeControlPending.delete(entry.requestId);
    clearTimeout(entry.timer);
    const { request_id: _requestId, ...businessResponse } = response;
    entry.resolve(businessResponse);
    scheduleNativeControlIdleClose(port);
  });
  port.onDisconnect.addListener(() => {
    const error = chrome.runtime.lastError?.message || "native-host-empty-response";
    nativeControlDisconnected(port, error);
  });
  return port;
}

function nativePortMessage(message, timeoutMs = 30_000) {
  if (!globalThis.chrome?.runtime?.connectNative) return Promise.resolve(null);
  let port;
  try {
    port = nativeControlPortForRequests();
  } catch (error) {
    return Promise.resolve(normalizedNativeFailure(error?.message || error || "native-host-error"));
  }
  if (!port) return Promise.resolve(null);
  const requestId = `native-${++nativeControlSequence}`;
  const boundedTimeout = Math.max(1_000, Math.min(Number(timeoutMs) || 30_000, 60_000));
  return new Promise((resolve) => {
    clearNativeControlIdleTimer();
    const timer = setTimeout(() => {
      if (!nativeControlPending.delete(requestId)) return;
      resolve({ ok: false, error: "native-host-timeout" });
      nativeControlLegacyResponseSafe = false;
      if (nativeControlPending.size === 0 && nativeControlPort === port) {
        nativeControlDisconnected(port, "native-host-timeout");
        try { port.disconnect(); } catch (_) {}
      }
    }, boundedTimeout);
    if (nativeControlPending.size > 0) nativeControlLegacyResponseSafe = false;
    nativeControlPending.set(requestId, { requestId, resolve, timer });
    try {
      port.postMessage({ ...message, request_id: requestId });
    } catch (error) {
      nativeControlPending.delete(requestId);
      clearTimeout(timer);
      nativeControlDisconnected(port, error?.message || error || "native-host-error");
      try { port.disconnect(); } catch (_) {}
      resolve(normalizedNativeFailure(error?.message || error || "native-host-error"));
    }
  });
}

function legacyNativeMessage(message) {
  return new Promise((resolve) => {
    if (!globalThis.chrome?.runtime?.sendNativeMessage) {
      resolve({ ok: false, error: "native-messaging-unavailable" });
      return;
    }
    try {
      chrome.runtime.sendNativeMessage(HERDR_NATIVE_HOST, message, (response) => {
        const err = chrome.runtime.lastError?.message;
        if (err) {
          resolve(normalizedNativeFailure(err));
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

async function nativeMessage(message, timeoutMs = 30_000) {
  const response = await nativePortMessage(message, timeoutMs);
  return response ?? legacyNativeMessage(message);
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

function nativeRequestPayload(input, init = {}) {
  const { baseUrl, path } = requestParts(input);
  return {
    base_url: baseUrl,
    path,
    method: String(init.method || "GET").toUpperCase(),
    headers: normalizedHeaders(init.headers),
    body: init.body == null ? "" : String(init.body),
    timeout_ms: Number(init.nativeTimeoutMs || 10_000),
  };
}

export async function getNativeExtensionOwnerStatus() {
  return nativeMessage({ type: "identity" }, 5_000);
}

// Generic image capture over the existing strict Native Messaging artifact boundary.
// source_id and artifact_key stay local identifiers; raw image bytes never enter MCP
// results. The native host recomputes digest/magic and stores the bounded artifact.
export async function captureImageArtifactNative(artifact) {
  if (!artifact || typeof artifact !== "object" || Array.isArray(artifact)) {
    return { ok: false, error: "artifact-capture-invalid" };
  }
  const allowed = ["source_id", "artifact_key", "mime", "bytes_b64", "sha256"];
  const keys = Object.keys(artifact);
  if (keys.length > allowed.length || keys.some((key) => !allowed.includes(key))) {
    return { ok: false, error: "artifact-capture-invalid" };
  }
  for (const key of ["source_id", "artifact_key", "mime", "bytes_b64"]) {
    if (typeof artifact[key] !== "string" || !artifact[key]) {
      return { ok: false, error: "artifact-capture-invalid" };
    }
  }
  if (artifact.sha256 != null
      && (typeof artifact.sha256 !== "string" || !/^[0-9a-f]{64}$/i.test(artifact.sha256))) {
    return { ok: false, error: "artifact-capture-invalid" };
  }
  const strict = {
    conversation_id: artifact.source_id,
    file_id: artifact.artifact_key,
    mime: artifact.mime,
    bytes_b64: artifact.bytes_b64,
  };
  if (artifact.sha256 != null) strict.sha256 = artifact.sha256;
  return nativeMessage({ type: "artifact_capture", ...strict }, 60_000);
}

// ChatGPT generated-image capture keeps its existing public helper and wire shape.
export async function captureWebArtifactNative(artifact) {
  if (!artifact || typeof artifact !== "object" || Array.isArray(artifact)) {
    return { ok: false, error: "artifact-capture-invalid" };
  }
  const allowed = ["conversation_id", "file_id", "mime", "bytes_b64", "sha256"];
  const keys = Object.keys(artifact);
  if (keys.length > allowed.length || keys.some((key) => !allowed.includes(key))) {
    return { ok: false, error: "artifact-capture-invalid" };
  }
  const generic = {
    source_id: artifact.conversation_id,
    artifact_key: artifact.file_id,
    mime: artifact.mime,
    bytes_b64: artifact.bytes_b64,
  };
  if (artifact.sha256 != null) generic.sha256 = artifact.sha256;
  return captureImageArtifactNative(generic);
}


export async function localHerdrFetch(input, init = {}) {
  const nativeTimeoutMs = Number(init.nativeTimeoutMs || 10_000);
  const response = await nativeMessage({
    type: "request",
    ...nativeRequestPayload(input, init),
  }, nativeTimeoutMs + 1_500);
  // Standby is a successful transport answer with no active owner origin. It
  // must surface as a diagnosable failure instead of an empty 500 response.
  if (response?.active === false && response?.reason === NATIVE_ORIGIN_NOT_ACTIVE) {
    throw new Error(NATIVE_ORIGIN_NOT_ACTIVE);
  }
  if (response?.ok !== true) {
    throw new Error(String(response?.error || "native-host-request-failed"));
  }
  return new Response(String(response.body || ""), {
    status: Number(response.status || 500),
    headers: response.headers && typeof response.headers === "object" ? response.headers : {},
  });
}

export async function localHerdrBatchFetch(requests = []) {
  if (!Array.isArray(requests) || requests.length === 0) {
    return { ok: false, error: "native-request-batch-empty" };
  }
  if (requests.length > 24) {
    return { ok: false, error: "native-request-batch-too-large" };
  }
  let encoded;
  try {
    encoded = requests.map((request) => nativeRequestPayload(request?.input, request?.init || {}));
  } catch (error) {
    return { ok: false, error: "native-request-batch-invalid", detail: String(error?.message || error || "") };
  }
  const nativeTimeoutMs = Math.max(10_000, ...encoded.map((request) => Number(request.timeout_ms || 10_000)));
  const response = await nativeMessage(
    { type: "request_batch", requests: encoded },
    nativeTimeoutMs + 1_500,
  );
  if (response?.ok !== true) {
    return { ok: false, error: String(response?.error || "native-host-request-batch-failed") };
  }
  if (!Array.isArray(response.responses) || response.responses.length !== encoded.length) {
    return { ok: false, error: "native-host-request-batch-malformed" };
  }
  return {
    ok: true,
    responses: response.responses.map((item) => {
      if (item?.ok !== true) {
        return { ok: false, error: String(item?.error || "native-host-request-failed") };
      }
      return {
        ok: true,
        response: new Response(String(item.body || ""), {
          status: Number(item.status || 500),
          headers: item.headers && typeof item.headers === "object" ? item.headers : {},
        }),
      };
    }),
  };
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
  // Native IPC can fail both phases before the caller advances from awaiting
  // `opened` to awaiting `done`. Mark both promises observed immediately so
  // MV3 does not emit Uncaught (in promise); later awaits still see the same
  // rejection on the original promise.
  void opened.catch(() => {});
  void done.catch(() => {});

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
  // Rebuilding the event stream must not cancel control requests that may
  // already have reached the runtime. Close only an idle control port.
  if (nativeControlPending.size > 0) return;
  clearNativeControlIdleTimer();
  const port = nativeControlPort;
  nativeControlPort = null;
  settleNativeControlPending({ ok: false, error: "native-host-reset" });
  try { port?.disconnect(); } catch (_) {}
}
