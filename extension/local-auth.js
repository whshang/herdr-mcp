// local-auth.js — short-lived loopback credentials for the extension worker.
// Preferred path: Chrome Native Messaging. Legacy static Token remains a
// compatibility fallback for older runtimes / unregistered native hosts.

export const HERDR_NATIVE_HOST = "dev.herdr.mcp";

let cached = null;
let inFlight = null;

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
        resolve(response && typeof response === "object" ? response : { ok: false, error: "native-host-empty-response" });
      });
    } catch (error) {
      resolve({ ok: false, error: String(error?.message || error || "native-host-error") });
    }
  });
}

function usable(entry, baseUrl) {
  if (!entry || entry.baseUrl !== baseUrl) return false;
  if (entry.authMode === "open") return true;
  return Boolean(entry.token) && Number(entry.expiresAt || 0) > Date.now() + 30_000;
}

export function resetLocalAuth() {
  cached = null;
  inFlight = null;
}

export async function getLocalAuth({ baseUrl, legacyToken = "", force = false } = {}) {
  const normalizedBase = String(baseUrl || "").trim().replace(/\/+$/, "");
  if (!force && usable(cached, normalizedBase)) return cached;
  if (!force && inFlight) return inFlight;

  inFlight = (async () => {
    const native = await nativeMessage({ type: "session", base_url: normalizedBase });
    if (native?.ok === true) {
      const expiresAt = native.expires_at ? Date.parse(native.expires_at) : 0;
      cached = {
        baseUrl: normalizedBase,
        token: String(native.token || ""),
        expiresAt: Number.isFinite(expiresAt) ? expiresAt : 0,
        authMode: String(native.auth_mode || "native_session"),
        source: "native",
      };
      return cached;
    }

    const legacy = String(legacyToken || "").trim();
    cached = {
      baseUrl: normalizedBase,
      token: legacy,
      expiresAt: Number.MAX_SAFE_INTEGER,
      authMode: legacy ? "legacy_static" : "none",
      source: legacy ? "legacy" : "none",
      error: String(native?.error || "native-auth-unavailable"),
    };
    return cached;
  })().finally(() => { inFlight = null; });
  return inFlight;
}

export async function localAuthHeaders(options = {}) {
  const auth = await getLocalAuth(options);
  return auth.token ? { Authorization: `Bearer ${auth.token}` } : {};
}
