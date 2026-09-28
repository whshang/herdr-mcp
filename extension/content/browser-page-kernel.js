// browser-page-kernel.js — shared generic browser-page inspection and interaction.
// Opaque generation-bound element refs fail closed on stale refs, sensitive fields,
// hidden/disabled elements, and cross-origin iframes.

(function (global) {
  "use strict";

  if (global.H2W_BROWSER_PAGE_KERNEL) return;

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

  function fastPathActionClass(el, tag) {
    if (tag === "a") {
      if (el.hasAttribute?.("download")) return undefined;
      const target = String(el.getAttribute("target") || "").trim().toLowerCase();
      if (target && target !== "_self") return undefined;
      if (el.getAttribute("onclick") !== null) return undefined;
      try {
        const url = new URL(el.getAttribute("href") || "", global.location?.href);
        const unsafePath = /(^|\/)(logout|signout|delete|remove|unsubscribe|purchase|checkout|pay|submit|confirm|approve|reject|authorize|grant|install|download|upload)(\/|$)/i;
        const unsafeQuery = /(?:^|[?&])(?:action|do|op|operation)=(?:logout|signout|delete|remove|unsubscribe|purchase|checkout|pay|submit|confirm|approve|reject|authorize|grant|install|download|upload)(?:&|$)/i;
        if (
          (url.protocol === "http:" || url.protocol === "https:")
          && url.origin === global.location?.origin
          && !unsafePath.test(url.pathname)
          && !unsafeQuery.test(url.search)
        ) {
          return "same_origin_navigation";
        }
      } catch (_) {}
      return undefined;
    }
    if (
      el.getAttribute("aria-haspopup") !== null
      || el.getAttribute("aria-expanded") !== null
      || el.getAttribute("aria-controls") !== null
    ) {
      if (
        tag === "button"
        && el.form
        && ["submit", "reset"].includes(String(el.type || "submit").toLowerCase())
      ) {
        return undefined;
      }
      return "reveal";
    }
    return undefined;
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
        type: (tag === "input" || tag === "button")
          ? String(el.type || (tag === "input" ? "text" : "")).slice(0, 64)
          : undefined,
        text: label,
        fast_path: fastPathActionClass(el, tag),
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

  function pageText() {
    const doc = global.document;
    const rawBody = doc?.body ? (doc.body.innerText || doc.body.textContent || "") : "";
    return cleanText(rawBody, MAX_CHARS_CEILING);
  }

  function expectCondition(params = {}) {
    if (hasForbiddenKeys(params)) return { ok: false, error: "disallowed_parameter" };
    if (isCrossOriginFrame()) return { ok: false, error: "cross_origin_iframe_blocked" };
    if (params.expectedOrigin && global.location) {
      if (String(params.expectedOrigin).toLowerCase() !== global.location.origin.toLowerCase()) {
        return { ok: false, error: "origin_mismatch" };
      }
    }

    const condition = String(params.condition || "").toLowerCase();
    const value = typeof params.value === "string" ? params.value : "";
    if (!["document_ready", "url_equals", "text_present", "text_absent"].includes(condition)) {
      return { ok: false, error: "unsupported_expect_condition" };
    }
    if (condition === "document_ready") {
      return { ok: global.document?.readyState === "complete", condition };
    }
    if (!value || value.length > 4096) {
      return { ok: false, error: "expect_value_invalid", condition };
    }
    if (condition === "url_equals") {
      return {
        ok: String(global.location?.href || "") === value,
        condition,
        value,
      };
    }
    const text = pageText();
    const present = text.includes(value);
    return {
      ok: condition === "text_present" ? present : !present,
      condition,
      value,
    };
  }

  async function executeExpect(params = {}) {
    const timeoutMs = Math.max(0, Math.min(Number(params.timeoutMs) || 0, 5000));
    const startedAt = Date.now();
    while (true) {
      const observed = expectCondition(params);
      if (observed.error) return observed;
      if (observed.ok) {
        return {
          ...observed,
          elapsed_ms: Math.max(0, Date.now() - startedAt),
        };
      }
      if (Date.now() - startedAt >= timeoutMs) {
        return {
          ok: false,
          error: "expect_timeout",
          condition: observed.condition,
          elapsed_ms: Math.max(0, Date.now() - startedAt),
        };
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }

  function handleAction(msg) {
    const action = String(msg?.action || "").toLowerCase();
    if (action === "inspect" || action === "observe") return scanDocument(msg);
    if (action === "click") return executeClick(msg);
    if (action === "fill") return executeFill(msg);
    if (action === "expect") return executeExpect(msg);
    return { ok: false, error: "unsupported_action" };
  }

  global.H2W_BROWSER_PAGE_KERNEL = Object.freeze({
    handleAction,
    inspect: scanDocument,
    click: executeClick,
    fill: executeFill,
    invalidateGeneration,
  });

})(typeof globalThis !== "undefined" ? globalThis : window);
