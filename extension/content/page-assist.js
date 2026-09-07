// page-assist.js — generic page inspection, bounded visible text, and
// opaque generation-bound element click/fill. Fail-closed on stale generation/ref,
// hidden/disabled elements, file/password fields, and cross-origin iframes.

(function (global) {
  "use strict";

  const DEFAULT_MAX_CHARS = 16384;
  const MAX_CHARS_CEILING = 100000;
  const MAX_FILL_CHARS = 10000;
  const MAX_ELEMENTS = 128;
  const DISALLOWED_INPUT_TYPES = new Set(["password", "file", "hidden"]);
  const FORBIDDEN_PARAM_KEYS = new Set([
    "selector", "css", "xpath", "js", "script", "eval", "code", "cookie", "storage", "cdp"
  ]);

  const SENSITIVE_AUTOCOMPLETE_TOKENS = new Set([
    "current-password",
    "new-password",
    "one-time-code",
    "cc-number",
    "cc-csc",
  ]);

  function isSensitiveField(el) {
    if (!el) return false;
    const tag = (el.tagName || "").toLowerCase();
    if (tag === "input") {
      const inputType = String(el.type || "").toLowerCase();
      if (DISALLOWED_INPUT_TYPES.has(inputType)) return true;
    }
    const autocomplete = String(el.getAttribute("autocomplete") || "").trim().toLowerCase();
    if (autocomplete) {
      const tokens = autocomplete.split(/\s+/);
      for (const t of tokens) {
        if (SENSITIVE_AUTOCOMPLETE_TOKENS.has(t)) return true;
      }
    }
    return false;
  }

  let generationSeq = 0;
  let currentGenerationToken = "";
  const elementMap = new Map();

  function invalidateGeneration() {
    currentGenerationToken = "";
    elementMap.clear();
  }

  function isCrossOriginFrame() {
    try {
      if (global.top && global.self !== global.top) {
        return global.location.origin !== global.top.location.origin;
      }
      return false;
    } catch (_) {
      return true;
    }
  }

  function isElementHidden(el) {
    if (!el || !el.ownerDocument) return true;
    if (el.getAttribute("aria-hidden") === "true") return true;
    const style = global.getComputedStyle ? global.getComputedStyle(el) : null;
    if (style) {
      if (style.display === "none" || style.visibility === "hidden" || style.opacity === "0") {
        return true;
      }
      if (el.offsetParent === null && style.position !== "fixed") {
        return true;
      }
    }
    if (typeof el.getBoundingClientRect === "function") {
      const rect = el.getBoundingClientRect();
      if (rect.width === 0 && rect.height === 0) {
        return true;
      }
    }
    return false;
  }

  function isElementDisabled(el) {
    if (!el) return true;
    if (el.disabled === true) return true;
    if (el.getAttribute("aria-disabled") === "true") return true;
    return false;
  }

  function cleanText(raw, maxChars = DEFAULT_MAX_CHARS) {
    const text = String(raw || "").replace(/\r\n?/g, "\n").trim();
    const limit = Math.max(1, Math.min(Number(maxChars) || DEFAULT_MAX_CHARS, MAX_CHARS_CEILING));
    return text.slice(0, limit);
  }

  function hasForbiddenKeys(obj) {
    if (!obj || typeof obj !== "object") return false;
    for (const key of Object.keys(obj)) {
      const lower = key.toLowerCase();
      for (const forbidden of FORBIDDEN_PARAM_KEYS) {
        if (lower === forbidden || lower.includes(forbidden)) return true;
      }
      if (typeof obj[key] === "object" && hasForbiddenKeys(obj[key])) return true;
    }
    return false;
  }

  function scanDocument(options = {}) {
    if (isCrossOriginFrame()) {
      return { ok: false, error: "cross_origin_iframe_blocked" };
    }

    generationSeq += 1;
    currentGenerationToken = `pa_gen_${generationSeq}_${Date.now().toString(36)}`;
    elementMap.clear();

    const doc = global.document;
    if (!doc) return { ok: false, error: "document_unavailable" };

    const candidates = doc.querySelectorAll(
      'button, a[href], input, textarea, select, [role="button"], [role="link"], [role="textbox"], [role="checkbox"], [contenteditable="true"]'
    );

    const elements = [];
    let elementIndex = 0;

    for (const el of candidates) {
      if (elements.length >= MAX_ELEMENTS) break;
      const tag = (el.tagName || "").toLowerCase();

      // Exclude prohibited inputs and sensitive autocomplete fields
      if (isSensitiveField(el)) continue;

      // Exclude hidden or disabled elements
      if (isElementHidden(el)) continue;
      if (isElementDisabled(el)) continue;

      const ref = `ref_${currentGenerationToken}_${elementIndex++}`;
      elementMap.set(ref, el);

      const roleAttr = el.getAttribute("role");
      const role = roleAttr || (tag === "a" ? "link" : tag);
      const label = (
        el.getAttribute("aria-label") ||
        el.getAttribute("placeholder") ||
        el.innerText ||
        el.textContent ||
        el.value ||
        ""
      ).trim().slice(0, 120);

      elements.push({
        ref: ref.slice(0, 256),
        role: String(role || "").slice(0, 64),
        type: tag === "input" ? String(el.type || "text").slice(0, 64) : undefined,
        text: label,
      });
    }

    const rawBody = doc.body ? (doc.body.innerText || doc.body.textContent || "") : "";
    const text = cleanText(rawBody, options.maxChars);

    return {
      ok: true,
      url: String(global.location ? global.location.href : "").slice(0, 4096),
      origin: String(global.location ? global.location.origin : "").slice(0, 512),
      title: String(doc.title || "").slice(0, 512),
      generation: currentGenerationToken,
      text,
      elements,
    };
  }

  function executeClick(params = {}) {
    if (hasForbiddenKeys(params)) return { ok: false, error: "disallowed_parameter" };
    if (isCrossOriginFrame()) return { ok: false, error: "cross_origin_iframe_blocked" };

    if (params.expectedOrigin && global.location) {
      if (String(params.expectedOrigin).toLowerCase() !== global.location.origin.toLowerCase()) {
        return { ok: false, error: "origin_mismatch" };
      }
    }

    const gen = String(params.generation || "").trim();
    if (!gen || gen !== currentGenerationToken) {
      return { ok: false, error: "stale_generation" };
    }

    const ref = String(params.ref || "").trim();
    if (!ref || !elementMap.has(ref)) {
      return { ok: false, error: "invalid_ref" };
    }

    const el = elementMap.get(ref);
    if (!el || !el.isConnected) {
      return { ok: false, error: "element_detached" };
    }

    if (isElementDisabled(el)) {
      return { ok: false, error: "element_disabled" };
    }

    if (isElementHidden(el)) {
      return { ok: false, error: "element_hidden" };
    }

    if (isSensitiveField(el)) {
      return { ok: false, error: "unsupported_element" };
    }

    try {
      el.scrollIntoView?.({ block: "center", inline: "center", behavior: "instant" });
    } catch (_) {}

    try {
      el.focus?.();
      el.click?.();
      invalidateGeneration();
      return { ok: true, ref, generation: gen };
    } catch (err) {
      return { ok: false, error: String(err?.message || err || "click_failed") };
    }
  }

  function executeFill(params = {}) {
    if (hasForbiddenKeys(params)) return { ok: false, error: "disallowed_parameter" };
    if (isCrossOriginFrame()) return { ok: false, error: "cross_origin_iframe_blocked" };

    if (params.expectedOrigin && global.location) {
      if (String(params.expectedOrigin).toLowerCase() !== global.location.origin.toLowerCase()) {
        return { ok: false, error: "origin_mismatch" };
      }
    }

    const gen = String(params.generation || "").trim();
    if (!gen || gen !== currentGenerationToken) {
      return { ok: false, error: "stale_generation" };
    }

    const ref = String(params.ref || "").trim();
    if (!ref || !elementMap.has(ref)) {
      return { ok: false, error: "invalid_ref" };
    }

    const el = elementMap.get(ref);
    if (!el || !el.isConnected) {
      return { ok: false, error: "element_detached" };
    }

    if (isSensitiveField(el)) {
      return { ok: false, error: "sensitive_field_prohibited" };
    }

    if (isElementDisabled(el) || el.readOnly) {
      return { ok: false, error: "element_disabled" };
    }

    if (isElementHidden(el)) {
      return { ok: false, error: "element_hidden" };
    }

    const tag = (el.tagName || "").toLowerCase();
    const isContentEditable = el.isContentEditable === true || el.getAttribute("contenteditable") === "true";
    if (tag !== "input" && tag !== "textarea" && !isContentEditable) {
      return { ok: false, error: "element_not_fillable" };
    }

    if (typeof params.value !== "string") {
      return { ok: false, error: "value_required" };
    }

    const fillValue = params.value.slice(0, MAX_FILL_CHARS);

    try {
      el.focus?.();
      if (tag === "input" || tag === "textarea") {
        const proto = tag === "input" ? global.HTMLInputElement?.prototype : global.HTMLTextAreaElement?.prototype;
        const descriptor = proto ? Object.getOwnPropertyDescriptor(proto, "value") : null;
        if (descriptor?.set) {
          descriptor.set.call(el, fillValue);
        } else {
          el.value = fillValue;
        }
        el.dispatchEvent(new Event("input", { bubbles: true }));
        el.dispatchEvent(new Event("change", { bubbles: true }));
      } else if (isContentEditable) {
        el.innerText = fillValue;
        el.dispatchEvent(new Event("input", { bubbles: true }));
      }
      invalidateGeneration();
      return { ok: true, ref, generation: gen };
    } catch (err) {
      return { ok: false, error: String(err?.message || err || "fill_failed") };
    }
  }

  function handleAction(msg) {
    const action = String(msg?.action || "").toLowerCase();
    if (action === "inspect") return scanDocument(msg);
    if (action === "click") return executeClick(msg);
    if (action === "fill") return executeFill(msg);
    return { ok: false, error: "unsupported_action" };
  }

  if (typeof chrome !== "undefined" && chrome.runtime?.onMessage) {
    chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
      if (msg?.type !== "h2w_page_assist") return false;
      const result = handleAction(msg);
      sendResponse(result);
      return false;
    });
  }

})(typeof globalThis !== "undefined" ? globalThis : window);
