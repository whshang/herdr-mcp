// Normalized Native Messaging failure tokens shared by the local transport and
// every surface that renders its failures.
//
// local-auth.js rejects with these exact tokens, so Control Center (main
// runtime snapshot + Worker fleet panel) and the Options connection test must
// classify them instead of echoing the raw token. A host that is not installed
// and an installed host whose registered origin is a different extension are
// distinct product states with different remediations.
//
// This module is pure and deliberately independent from chrome.* APIs.

export const NATIVE_HOST_NOT_INSTALLED = "native-host-not-installed";
export const NATIVE_ORIGIN_NOT_ACTIVE = "native-origin-not-active";

// Raw Chromium wording for a missing Native Messaging host manifest. Kept in
// sync with local-auth.js so consumers that still receive the un-normalized
// browser error (older builds, direct chrome.runtime callers) classify it too.
const NATIVE_HOST_MESSAGE = /native messaging host|native host|specified native messaging host|forbidden/i;

/**
 * Classify a Native Messaging failure string.
 *
 * @param {unknown} error Raw error text or normalized token.
 * @returns {"owner-inactive"|"host-missing"|null} Product state, or null when
 *   the failure is unrelated and must keep its own rendering.
 */
export function nativeHostFailure(error) {
  const raw = String(error || "").trim();
  if (!raw) return null;
  if (raw === NATIVE_ORIGIN_NOT_ACTIVE) return "owner-inactive";
  if (raw === NATIVE_HOST_NOT_INSTALLED) return "host-missing";
  if (NATIVE_HOST_MESSAGE.test(raw)) return "host-missing";
  return null;
}
