// page-assist-core.js — pure validation, normalization, and ephemeral ref logic
// for generic page inspection and interaction. Direct module export for testing.

export const PAGE_ASSIST_ALLOWED_ACTIONS = Object.freeze(["inspect", "click", "fill"]);

export const SENSITIVE_AUTOCOMPLETE_TOKENS = Object.freeze([
  "current-password",
  "new-password",
  "one-time-code",
  "cc-number",
  "cc-csc",
]);

export function isSensitiveAutocomplete(autocompleteAttr) {
  if (!autocompleteAttr || typeof autocompleteAttr !== "string") return false;
  const tokens = autocompleteAttr.trim().toLowerCase().split(/\s+/);
  return tokens.some((token) => SENSITIVE_AUTOCOMPLETE_TOKENS.includes(token));
}

export const PAGE_ASSIST_DISALLOWED_INPUT_TYPES = Object.freeze([
  "password",
  "file",
  "hidden",
]);

export const PAGE_ASSIST_FORBIDDEN_PARAM_KEYS = Object.freeze([
  "selector",
  "css",
  "xpath",
  "js",
  "script",
  "eval",
  "code",
  "cookie",
  "storage",
  "cdp",
]);

export const DEFAULT_PAGE_ASSIST_MAX_CHARS = 16384;
export const MAX_PAGE_ASSIST_CHARS = 100000;
export const MAX_PAGE_ASSIST_FILL_CHARS = 10000;

/**
 * Normalizes a raw URL or origin to a standard HTTP/HTTPS origin.
 * Returns null if invalid or protocol is not http/https.
 */
export function normalizeOrigin(raw) {
  if (!raw || typeof raw !== "string") return null;
  const trimmed = raw.trim();
  if (!trimmed) return null;
  try {
    const parsed = new URL(trimmed);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return null;
    return parsed.origin.toLowerCase();
  } catch (_) {
    return null;
  }
}

/**
 * Converts a URL or origin into an extension host permission pattern, e.g. "https://example.com/*".
 */
export function originToMatchPattern(raw) {
  const origin = normalizeOrigin(raw);
  if (!origin) return null;
  return `${origin}/*`;
}

/**
 * Parses multiline string or array of origins into a unique normalized list.
 */
export function parseAllowedOrigins(input) {
  if (!input) return [];
  const list = Array.isArray(input)
    ? input
    : String(input).split(/[\r\n,]+/);
  const out = [];
  for (const item of list) {
    const origin = normalizeOrigin(item);
    if (origin && !out.includes(origin)) {
      out.push(origin);
    }
  }
  return out;
}

/**
 * Checks if a target origin is included in the allowed list.
 */
export function isOriginAllowed(targetOrigin, allowedOrigins) {
  const normalizedTarget = normalizeOrigin(targetOrigin);
  if (!normalizedTarget) return false;
  const allowed = parseAllowedOrigins(allowedOrigins);
  return allowed.includes(normalizedTarget);
}

/**
 * Recursively checks if an object contains any forbidden parameter names.
 */
export function hasDisallowedParameters(obj) {
  if (!obj || typeof obj !== "object") return false;
  for (const key of Object.keys(obj)) {
    const lowerKey = key.toLowerCase();
    for (const forbidden of PAGE_ASSIST_FORBIDDEN_PARAM_KEYS) {
      if (lowerKey === forbidden || lowerKey.includes(forbidden)) {
        return true;
      }
    }
    const val = obj[key];
    if (val && typeof val === "object" && hasDisallowedParameters(val)) {
      return true;
    }
  }
  return false;
}

/**
 * Creates an opaque ephemeral generation token.
 */
export function createGenerationToken(seq = 1) {
  const timestamp = Date.now().toString(36);
  const safeSeq = Math.max(1, Math.floor(Number(seq) || 1));
  return `pa_gen_${safeSeq}_${timestamp}`;
}

/**
 * Creates an opaque ephemeral element reference tied to a generation.
 */
export function createElementRef(generationToken, index) {
  const safeIndex = Math.max(0, Math.floor(Number(index) || 0));
  return `ref_${generationToken}_${safeIndex}`;
}

/**
 * Validates whether an element ref belongs to the specified generation.
 */
export function isRefValidForGeneration(ref, generationToken) {
  if (!ref || typeof ref !== "string" || !generationToken || typeof generationToken !== "string") {
    return false;
  }
  const prefix = `ref_${generationToken}_`;
  return ref.startsWith(prefix) && ref.length > prefix.length;
}

/**
 * Checks if an input element type is disallowed (file/password/hidden).
 */
export function isDisallowedInputType(type) {
  if (!type || typeof type !== "string") return false;
  return PAGE_ASSIST_DISALLOWED_INPUT_TYPES.includes(type.trim().toLowerCase());
}

/**
 * Clamps visible text to configured maximum character bounds.
 */
export function clampVisibleText(rawText, maxChars = DEFAULT_PAGE_ASSIST_MAX_CHARS) {
  const text = String(rawText || "").trim();
  const limit = Math.max(1, Math.min(Number(maxChars) || DEFAULT_PAGE_ASSIST_MAX_CHARS, MAX_PAGE_ASSIST_CHARS));
  return text.slice(0, limit);
}

/**
 * Validates a page assist request fail-closed against allowed operations,
 * parameters, and origin permissions.
 */
export function validatePageAssistRequest(request, allowedOrigins) {
  if (!request || typeof request !== "object") {
    return { ok: false, error: "invalid_request" };
  }

  // Fail-closed on forbidden parameters (selectors, XPath, JS, cookies, storage, CDP)
  if (hasDisallowedParameters(request)) {
    return { ok: false, error: "disallowed_parameter" };
  }

  const action = String(request.action || "").trim().toLowerCase();
  if (!PAGE_ASSIST_ALLOWED_ACTIONS.includes(action)) {
    return { ok: false, error: "invalid_action" };
  }

  const targetOrigin = normalizeOrigin(request.targetOrigin || request.url);
  if (!targetOrigin) {
    return { ok: false, error: "target_origin_required" };
  }

  if (!isOriginAllowed(targetOrigin, allowedOrigins)) {
    return { ok: false, error: "origin_not_permitted" };
  }


  if (action === "inspect") {
    const maxChars = Math.max(
      1,
      Math.min(Number(request.maxChars) || DEFAULT_PAGE_ASSIST_MAX_CHARS, MAX_PAGE_ASSIST_CHARS)
    );
    return { ok: true, action, targetOrigin, maxChars };
  }

  if (action === "click") {
    const ref = String(request.ref || "").trim();
    if (!ref) return { ok: false, error: "ref_required" };
    const generation = String(request.generation || "").trim();
    if (!generation) return { ok: false, error: "generation_required" };
    if (!isRefValidForGeneration(ref, generation)) {
      return { ok: false, error: "stale_ref" };
    }
    return { ok: true, action, targetOrigin, ref, generation };
  }

  if (action === "fill") {
    const ref = String(request.ref || "").trim();
    if (!ref) return { ok: false, error: "ref_required" };
    const generation = String(request.generation || "").trim();
    if (!generation) return { ok: false, error: "generation_required" };
    if (!isRefValidForGeneration(ref, generation)) {
      return { ok: false, error: "stale_ref" };
    }
    if (typeof request.value !== "string") {
      return { ok: false, error: "value_required" };
    }
    const value = request.value.slice(0, MAX_PAGE_ASSIST_FILL_CHARS);
    return { ok: true, action, targetOrigin, ref, generation, value };
  }

  return { ok: false, error: "unsupported_action" };
}
