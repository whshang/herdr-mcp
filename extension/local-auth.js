// local-auth.js — tokenless Native Messaging transport for local Herdr access.
// Current extensions never receive HERDR_MCP_TOKEN or an expiring bearer.
// Chrome authenticates the native host origin; the host talks to herdr-mcp over
// its mode-0600 Unix-domain socket. Older extension builds remain compatible
// with the server's historical /extension/session endpoint.

export const HERDR_NATIVE_HOST = "dev.herdr.mcp";
export const HERDR_STORE_EXTENSION_ID = "kpcengcaammanfnbclapecdgahdmhanp";
export const HERDR_STANDALONE_EXTENSION_ID = "jbcjhnnmhaekdnbgfllpfipennppfedh";

export function extensionChannelForId(extensionId) {
  const id = String(extensionId || "").trim();
  if (!id) return "unknown";
  if (id === HERDR_STORE_EXTENSION_ID) return "store";
  if (id === HERDR_STANDALONE_EXTENSION_ID) return "standalone";
  return "dev";
}

export function getCurrentExtensionIdentity() {
  const extensionId = String(globalThis.chrome?.runtime?.id || "").trim();
  if (!extensionId) return {};
  return {
    current_extension_id: extensionId,
    current_extension_origin: `chrome-extension://${extensionId}/`,
    current_channel: extensionChannelForId(extensionId),
  };
}

function isNativeAdmissionDenied(message) {
  const text = String(message || "").toLowerCase();
  return text.includes("forbidden")
    || text.includes("not allowed")
    || text.includes("access denied")
    || text.includes("permission denied");
}

function isNativeHostMissing(message) {
  const text = String(message || "").toLowerCase();
  return text.includes("native messaging host") && text.includes("not found");
}

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
          if (isNativeHostMissing(err)) {
            resolve({ ok: false, error: "native-host-not-installed" });
            return;
          }
          if (isNativeAdmissionDenied(err)) {
            resolve({
              ok: true,
              active: false,
              reason: "native-origin-not-active",
              ...getCurrentExtensionIdentity(),
            });
            return;
          }
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
  const current = getCurrentExtensionIdentity();
  const status = await nativeMessage({ type: "identity" });
  if (status?.ok !== true) return status;
  if (status.active === true) {
    return {
      ...status,
      ...current,
      active_extension_id: current.current_extension_id || null,
      active_extension_origin: status.extension_origin || current.current_extension_origin || null,
      active_channel: current.current_channel || null,
    };
  }
  return { ...status, ...current };
}

// Dedicated ChatGPT Web generated-image capture over Native Messaging.
// Accepts ONLY the strict native-boundary artifact shape
// { conversation_id, file_id, mime, bytes_b64, sha256? }. Any extra field
// (token, cookie, authorization, download URL, ...) is rejected before the
// message is sent, so secrets can never cross to the native host.
export async function captureWebArtifactNative(artifact) {
  if (!artifact || typeof artifact !== "object" || Array.isArray(artifact)) {
    return { ok: false, error: "artifact-capture-invalid" };
  }
  const allowed = ["conversation_id", "file_id", "mime", "bytes_b64", "sha256"];
  const keys = Object.keys(artifact);
  if (keys.length > allowed.length) return { ok: false, error: "artifact-capture-invalid" };
  for (const key of keys) {
    if (!allowed.includes(key)) return { ok: false, error: "artifact-capture-invalid" };
  }
  for (const key of ["conversation_id", "file_id", "mime", "bytes_b64"]) {
    if (typeof artifact[key] !== "string" || !artifact[key]) return { ok: false, error: "artifact-capture-invalid" };
  }
  if (artifact.sha256 != null && (typeof artifact.sha256 !== "string" || !/^[0-9a-f]{64}$/i.test(artifact.sha256))) {
    return { ok: false, error: "artifact-capture-invalid" };
  }
  const strict = {
    conversation_id: artifact.conversation_id,
    file_id: artifact.file_id,
    mime: artifact.mime,
    bytes_b64: artifact.bytes_b64,
  };
  if (artifact.sha256 != null) strict.sha256 = artifact.sha256;
  return nativeMessage({ type: "artifact_capture", ...strict });
}


export async function localHerdrFetch(input, init = {}) {
  const response = await nativeMessage({
    type: "request",
    ...nativeRequestPayload(input, init),
  });
  if (response?.active === false && response?.reason === "native-origin-not-active") {
    throw new Error("native-origin-not-active");
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
  const response = await nativeMessage({ type: "request_batch", requests: encoded });
  if (response?.active === false && response?.reason === "native-origin-not-active") {
    return { ok: false, error: "native-origin-not-active" };
  }
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
  // Compatibility export for older background/tests. Tokenless transport has
  // no credential cache to expire or refresh.
}
