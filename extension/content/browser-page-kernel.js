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
  const MAX_ELEMENT_OFFSET = 4096;
  const MAX_HOVER_TARGETS = 32;
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

  // Fixed single-key vocabulary shared with the Runtime validation. No text,
  // no modifier combos, no caller-controlled raw key codes.
  const PRESS_KEY_EVENTS = {
    Enter: { key: "Enter", code: "Enter", keyCode: 13 },
    Escape: { key: "Escape", code: "Escape", keyCode: 27 },
    Tab: { key: "Tab", code: "Tab", keyCode: 9 },
    Backspace: { key: "Backspace", code: "Backspace", keyCode: 8 },
    Delete: { key: "Delete", code: "Delete", keyCode: 46 },
    ArrowUp: { key: "ArrowUp", code: "ArrowUp", keyCode: 38 },
    ArrowDown: { key: "ArrowDown", code: "ArrowDown", keyCode: 40 },
    ArrowLeft: { key: "ArrowLeft", code: "ArrowLeft", keyCode: 37 },
    ArrowRight: { key: "ArrowRight", code: "ArrowRight", keyCode: 39 },
    Home: { key: "Home", code: "Home", keyCode: 36 },
    End: { key: "End", code: "End", keyCode: 35 },
    PageUp: { key: "PageUp", code: "PageUp", keyCode: 33 },
    PageDown: { key: "PageDown", code: "PageDown", keyCode: 34 },
    Space: { key: " ", code: "Space", keyCode: 32 },
  };

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

  // Visible-text projection for extract: never falls back to raw textContent,
  // so hidden, aria-hidden, sensitive or form-control descendants cannot leak.
  function collectVisibleText(root, limit) {
    if (!root || !root.childNodes) return String(root?.innerText || "");
    const parts = [];
    let size = 0;
    const walk = (node) => {
      if (size >= limit || !node) return;
      if (node.nodeType === 3) {
        const t = String(node.nodeValue || "");
        if (t) { parts.push(t); size += t.length; }
        return;
      }
      if (node.nodeType !== 1) return;
      if (node !== root) {
        const tag = (node.tagName || "").toLowerCase();
        if (tag === "script" || tag === "style" || tag === "template" || tag === "noscript") return;
        if (tag === "input" || tag === "textarea" || tag === "select" || tag === "option") return;
        if (node.hidden === true || node.getAttribute?.("aria-hidden") === "true") return;
        if (isSensitiveField(node) || isElementHidden(node)) return;
        if (tag === "br" || tag === "p" || tag === "div" || tag === "li" || tag === "tr") { parts.push(" "); }
      }
      for (const child of Array.from(node.childNodes || [])) walk(child);
    };
    walk(root);
    return parts.join("").replace(/\s+/g, " ");
  }

  const documentNonce = (() => {
    if (global.crypto?.getRandomValues) {
      const bytes = new Uint8Array(8);
      global.crypto.getRandomValues(bytes);
      return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
    }
    return Math.random().toString(36).slice(2) + Math.random().toString(36).slice(2);
  })();

  const OBSERVABLE_CONTROL_SELECTOR =
    'button, a[href], input, textarea, select, option, table, [role="button"], [role="link"], [role="textbox"], [role="checkbox"], [role="radio"], [role="option"], [role="group"], [role="listitem"], [tabindex], label, span, div[class], article, section[class], li[class], [contenteditable="true"]';

  let generationSeq = 0;
  let currentGenerationToken = "";
  const elementMap = new Map();
  const hoverOnlyRefs = new Set();
  const selectOptionMap = new Map();
  // Refs whose observed option list was a truncated prefix of the live list.
  const selectOptionTruncatedRefs = new Set();

  function invalidateGeneration() {
    currentGenerationToken = "";
    elementMap.clear();
    hoverOnlyRefs.clear();
    selectOptionMap.clear();
    selectOptionTruncatedRefs.clear();
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

  function structuralToken(value, maxLength = 80) {
    const token = String(value || "").trim();
    return token.length <= maxLength && /^[A-Za-z0-9_-]+$/.test(token)
      ? token
      : undefined;
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

  function isObservableControlCandidate(el) {
    const tag = (el.tagName || "").toLowerCase();
    const role = el.getAttribute("role");
    const native = ["button", "a", "input", "textarea", "select", "option", "table"].includes(tag);
    const semantic = ["button", "link", "textbox", "checkbox", "radio", "option"].includes(role);
    const focusable = el.hasAttribute("tabindex") && Number(el.getAttribute("tabindex")) >= 0
      && String(el.innerText || "").length <= 256 && (el.children?.length || 0) <= 4;
    const labelControl = tag === "label" && el.control && !isSensitiveField(el.control);
    if (native || semantic || focusable || labelControl || el.isContentEditable) return true;
    if (!["span", "div"].includes(tag)) return false;
    // Framework choice tags often delegate clicks to a form/dialog ancestor.
    const choice = /(?:^|[\s_-])(?:chip|choice|tag|option)(?:$|[\s_-])/i.test(el.getAttribute("class") || "")
      && !!el.closest?.('form, dialog, [role="dialog"]');
    if (!choice && global.getComputedStyle?.(el)?.cursor !== "pointer") return false;
    const text = String(el.innerText || "").trim();
    if (!text.length || text.length > 256 || (el.children?.length || 0) > 4) return false;
    const rect = el.getBoundingClientRect();
    return rect.width > 0 && rect.width <= 600 && rect.height > 0 && rect.height <= 120
      && !el.querySelector?.("input, textarea, select, button, a[href], [contenteditable], [role]");
  }

  function isHoverOnlyCandidate(el) {
    const tag = (el.tagName || "").toLowerCase();
    if (!["div", "article", "section", "li"].includes(tag)) return false;
    const role = String(el.getAttribute("role") || "");
    const classes = String(el.getAttribute("class") || "");
    if (!/(?:card|tile|row|item)/i.test(classes) && !["group", "listitem"].includes(role)) return false;
    const rect = el.getBoundingClientRect?.();
    if (!rect || rect.width < 16 || rect.height < 16 || rect.width > 1600 || rect.height > 800) return false;
    return cleanText(collectVisibleText(el, 121), 120).length > 0;
  }

  // Page cursor over eligible, visible controls only. The 128-element response
  // cap is retained; the offset lets a caller reach controls past it instead of
  // seeing the same document-order prefix on every observation.
  function parseObserveOffset(options = {}) {
    const offset = options.elementOffset === undefined ? 0 : options.elementOffset;
    if (!Number.isSafeInteger(offset) || offset < 0 || offset > MAX_ELEMENT_OFFSET) return null;
    return offset;
  }

  function scanDocument(options = {}) {
    if (isCrossOriginFrame()) {
      return { ok: false, error: "cross_origin_iframe_blocked" };
    }
    const elementOffset = parseObserveOffset(options);
    if (elementOffset === null) return { ok: false, error: "element_offset_invalid" };

    generationSeq += 1;
    currentGenerationToken = `pa_gen_${documentNonce}_${generationSeq}_${Date.now().toString(36)}`;
    elementMap.clear();
    hoverOnlyRefs.clear();
    selectOptionMap.clear();
    selectOptionTruncatedRefs.clear();

    const doc = global.document;
    if (!doc) return { ok: false, error: "document_unavailable" };

    const candidates = doc.querySelectorAll(OBSERVABLE_CONTROL_SELECTOR);

    const elements = [];
    let elementIndex = 0;
    let eligibleIndex = 0;
    let hoverTargets = 0;
    let hasMoreElements = false;

    for (const el of candidates) {
      const tag = (el.tagName || "").toLowerCase();

      const clickable = isObservableControlCandidate(el);
      const hoverOnly = !clickable && hoverTargets < MAX_HOVER_TARGETS && isHoverOnlyCandidate(el);
      if (!clickable && !hoverOnly) continue;

      // Exclude prohibited inputs and sensitive autocomplete fields
      if (isSensitiveField(el) || el.closest?.('[hidden], [aria-hidden="true"], [inert]')) continue;

      // Exclude hidden or disabled elements
      if (isElementHidden(el)) continue;
      if (isElementDisabled(el)) continue;

      // Only eligible, visible controls advance the cursor, so a skipped
      // password/hidden/disabled control never consumes a page slot.
      if (hoverOnly) hoverTargets += 1;
      if (eligibleIndex++ < elementOffset) continue;
      if (elements.length >= MAX_ELEMENTS) {
        hasMoreElements = true;
        break;
      }

      const ref = `ref_${currentGenerationToken}_${elementIndex++}`;
      elementMap.set(ref, el);
      if (hoverOnly) hoverOnlyRefs.add(ref);

      const roleAttr = el.getAttribute("role");
      const observedRole = roleAttr || (tag === "a" ? "link" : tag);
      const label = hoverOnly ? cleanText(collectVisibleText(el, 121), 121) : (
        el.getAttribute("aria-label") ||
        el.getAttribute("placeholder") ||
        el.innerText ||
        el.textContent ||
        el.value ||
        ""
      ).trim();
      const classes = String(el.getAttribute("class") || "")
        .split(/\s+/)
        .map((value) => structuralToken(value, 48))
        .filter(Boolean)
        .slice(0, 8);
      // Expose bounded visible labels only; never an option's raw value.
      const selectOptions = tag === "select"
        ? [...(el.options || [])].filter((option) => (
          !option.hidden && !option.disabled && !option.parentElement?.disabled
        ))
        : null;
      if (selectOptions) {
        selectOptionMap.set(ref, selectOptions.slice(0, 24).map((option) => ({
          node: option,
          value: String(option.value),
          label: String(option.label || option.textContent || "").trim(),
        })));
        // Record a truncated observation window: absence from it is then not
        // evidence that a requested option does not exist.
        if (selectOptions.length > 24) selectOptionTruncatedRefs.add(ref);
      }

      elements.push({
        ref: ref.slice(0, 256),
        tag,
        role: String(observedRole || "").slice(0, 64),
        hover_only: hoverOnly || undefined,
        id: structuralToken(el.getAttribute("id")),
        data_testid: structuralToken(el.getAttribute("data-testid")),
        classes: classes.length ? classes : undefined,
        contenteditable: el.getAttribute("contenteditable") === "true" || undefined,
        type: (tag === "input" || tag === "button")
          ? String(el.type || (tag === "input" ? "text" : "")).slice(0, 64)
          : undefined,
        checked: tag === "input" && ["checkbox", "radio"].includes(String(el.type || "").toLowerCase())
          ? el.checked === true
          : undefined,
        text: label.slice(0, 120),
        text_truncated: label.length > 120 || undefined,
        options: selectOptions ? selectOptions.slice(0, 24).map((option) => (
          cleanText(option.label || option.textContent, 80)
        )) : undefined,
        options_truncated: selectOptions && selectOptions.length > 24 ? true : undefined,
        selected_option: tag === "select" && !el.multiple
          ? cleanText((selectOptions || []).find((option) => option.value === el.value)?.label
            || (selectOptions || []).find((option) => option.value === el.value)?.textContent, 80) || undefined
          : undefined,
        fast_path: hoverOnly ? undefined : fastPathActionClass(el, tag),
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
      ...(hasMoreElements || options.elementOffset !== undefined ? {
        element_offset: elementOffset,
        next_element_offset: hasMoreElements ? elementOffset + elements.length : null,
      } : {}),
      scroll: {
        y: Math.max(0, Math.round(Number(global.scrollY) || 0)),
        max_y: Math.max(0, Math.round((Number(doc.documentElement?.scrollHeight) || 0) - (Number(global.innerHeight) || 0))),
      },
      text,
      elements,
    };
  }

  function executeExtract(params = {}) {
    if (hasForbiddenKeys(params)) return { ok: false, error: "disallowed_parameter" };
    if (isCrossOriginFrame()) return { ok: false, error: "cross_origin_iframe_blocked" };
    if (params.expectedOrigin && global.location
        && String(params.expectedOrigin).toLowerCase() !== global.location.origin.toLowerCase()) {
      return { ok: false, error: "origin_mismatch" };
    }
    const generation = String(params.generation || "").trim();
    if (!generation || generation !== currentGenerationToken) return { ok: false, error: "stale_generation" };
    const ref = String(params.ref || "");
    if (!ref.startsWith(`ref_${generation}_`)) return { ok: false, error: "invalid_ref" };
    const maxChars = params.maxChars === undefined ? 4000 : params.maxChars;
    if (!Number.isInteger(maxChars) || maxChars < 1 || maxChars > 8192) {
      return { ok: false, error: "extract_params_invalid" };
    }
    const el = elementMap.get(ref);
    if (!el || !el.isConnected) return { ok: false, error: "element_detached" };
    if (isSensitiveField(el) || isElementHidden(el) || isElementDisabled(el)) {
      return { ok: false, error: "unsupported_element" };
    }
    const raw = collectVisibleText(el, maxChars + 1).trim();
    return {
      ok: true, ref, generation,
      text: cleanText(raw, maxChars),
      truncated: raw.length > maxChars,
    };
  }

  const MAX_TABLE_CELL_CHARS = 200;
  const MAX_TABLE_COLUMNS = 32;

  function executeExtractTable(params = {}) {
    if (hasForbiddenKeys(params)) return { ok: false, error: "disallowed_parameter" };
    if (isCrossOriginFrame()) return { ok: false, error: "cross_origin_iframe_blocked" };
    if (params.expectedOrigin && global.location
        && String(params.expectedOrigin).toLowerCase() !== global.location.origin.toLowerCase()) {
      return { ok: false, error: "origin_mismatch" };
    }
    const generation = String(params.generation || "").trim();
    if (!generation || generation !== currentGenerationToken) return { ok: false, error: "stale_generation" };
    const ref = String(params.ref || "");
    if (!ref.startsWith(`ref_${generation}_`)) return { ok: false, error: "invalid_ref" };
    const maxRows = params.maxRows === undefined ? 25 : params.maxRows;
    if (!Number.isInteger(maxRows) || maxRows < 1 || maxRows > 100) {
      return { ok: false, error: "extract_table_params_invalid" };
    }
    const el = elementMap.get(ref);
    if (!el || !el.isConnected) return { ok: false, error: "element_detached" };
    if ((el.tagName || "").toLowerCase() !== "table") return { ok: false, error: "unsupported_element" };
    if (isSensitiveField(el) || isElementHidden(el) || isElementDisabled(el)) {
      return { ok: false, error: "unsupported_element" };
    }

    // Visible cell text only, through the same projection as extract: no raw
    // textContent fallback, so an unrendered cell cannot leak its markup text
    // and hidden/aria-hidden/sensitive/form-control descendants are skipped.
    // An excluded cell yields an empty string so column positions stay aligned.
    const cellText = (cell) => {
      if (isSensitiveField(cell) || isElementHidden(cell)) return "";
      return cleanText(collectVisibleText(cell, MAX_TABLE_CELL_CHARS + 1), MAX_TABLE_CELL_CHARS);
    };
    const rowCells = (row) => [...(row.children || [])]
      .filter((cell) => ["td", "th"].includes(String(cell.tagName || "").toLowerCase()))
      .slice(0, MAX_TABLE_COLUMNS)
      .map(cellText);
    // Hidden rows (display:none placeholders, aria-hidden templates) are not
    // rendered content, so they are neither counted against max_rows nor returned.
    const trList = [...(el.querySelectorAll?.("tr") || [])]
      .filter((row) => !isSensitiveField(row) && !isElementHidden(row));
    const allRows = trList.map(rowCells).filter((cells) => cells.length > 0);
    let headers = [];
    let dataRows = allRows;
    const firstRowCells = trList.length ? [...(trList[0].children || [])] : [];
    const firstRowHeaders = firstRowCells
      .filter((cell) => String(cell.tagName || "").toLowerCase() === "th")
      .slice(0, MAX_TABLE_COLUMNS)
      .map(cellText);
    const firstRowDataCells = firstRowCells
      .filter((cell) => ["td", "th"].includes(String(cell.tagName || "").toLowerCase()));
    if (firstRowHeaders.length > 0 && firstRowHeaders.length === firstRowDataCells.length) {
      headers = firstRowHeaders;
      dataRows = allRows.slice(1);
    }
    const rows_truncated = dataRows.length > maxRows;
    return {
      ok: true,
      ref,
      generation,
      headers,
      rows: dataRows.slice(0, maxRows),
      rows_truncated,
      requires_verification: true,
    };
  }

  const SCROLL_ANCESTOR_DEPTH = 10;
  const SCROLL_OBSERVED_LIMIT = 32;
  const SCROLL_CONTAINER_LIMIT = 3;

  function computedStyleOf(el) {
    try {
      return global.getComputedStyle ? global.getComputedStyle(el) : null;
    } catch (_) {
      return null;
    }
  }

  function isScrollableOverflow(el) {
    const overflowY = String(computedStyleOf(el)?.overflowY || "").toLowerCase();
    return overflowY === "auto" || overflowY === "scroll" || overflowY === "overlay";
  }

  // Movement is decided on the raw finite position. Reported values are rounded
  // separately, so a subpixel move is never mistaken for a boundary.
  function viewportScrollPosition() {
    const value = Number(global.scrollY);
    return Number.isFinite(value) ? value : 0;
  }

  // A container only qualifies when it currently has scrollable overflow; the
  // caller still measures the resulting position, so a wrong guess can never be
  // reported as movement.
  function scrollContainerState(el) {
    if (!el) return null;
    const style = computedStyleOf(el);
    if (style && style.display === "none") return null;
    const top = Number(el.scrollTop);
    const scrollHeight = Number(el.scrollHeight);
    const clientHeight = Number(el.clientHeight);
    if (![top, scrollHeight, clientHeight].every(Number.isFinite)) return null;
    if (scrollHeight <= clientHeight) return null;
    return { top: Math.max(0, top) };
  }

  function nearestObservedScrollContainer(el) {
    let node = el?.parentElement || null;
    for (let depth = 0; node && depth < SCROLL_ANCESTOR_DEPTH; depth += 1) {
      if (isScrollableOverflow(node) && scrollContainerState(node)) return node;
      node = node.parentElement || null;
    }
    return null;
  }

  // Production scroll carries no element ref, so the inner container is derived
  // from the current generation's observed elements: the scrollable ancestor
  // shared by the most of them wins, then the scrolling element, then the body.
  function observedScrollCandidates() {
    const counts = new Map();
    let inspected = 0;
    for (const el of elementMap.values()) {
      if (inspected++ >= SCROLL_OBSERVED_LIMIT) break;
      if (!el || el.isConnected === false) continue;
      const node = nearestObservedScrollContainer(el);
      if (node) counts.set(node, (counts.get(node) || 0) + 1);
    }
    const candidates = [...counts.entries()]
      .sort((a, b) => b[1] - a[1])
      .slice(0, SCROLL_CONTAINER_LIMIT)
      .map(([node]) => node);
    for (const node of [global.document?.scrollingElement, global.document?.body]) {
      if (node && !candidates.includes(node)) candidates.push(node);
    }
    return candidates;
  }

  function attemptContainerScroll(container, delta) {
    const before = scrollContainerState(container);
    if (!before) return null;
    if (typeof container.scrollBy === "function") {
      container.scrollBy({ top: delta, behavior: "instant" });
    } else {
      container.scrollTop = before.top + delta;
    }
    const after = scrollContainerState(container);
    if (!after || after.top === before.top) return null;
    return { before: before.top, after: after.top };
  }

  function executeScroll(params = {}) {
    if (hasForbiddenKeys(params)) return { ok: false, error: "disallowed_parameter" };
    if (isCrossOriginFrame()) return { ok: false, error: "cross_origin_iframe_blocked" };
    if (params.expectedOrigin && global.location
        && String(params.expectedOrigin).toLowerCase() !== global.location.origin.toLowerCase()) {
      return { ok: false, error: "origin_mismatch" };
    }
    const generation = String(params.generation || "").trim();
    if (!generation || generation !== currentGenerationToken) return { ok: false, error: "stale_generation" };
    const direction = params.direction;
    const amount = params.amount === undefined ? 500 : params.amount;
    if ((direction !== "up" && direction !== "down")
        || !Number.isInteger(amount) || amount < 1 || amount > 1500) {
      return { ok: false, error: "scroll_params_invalid" };
    }
    if (typeof global.scrollBy !== "function") return { ok: false, error: "scroll_unavailable" };
    const beforeRaw = viewportScrollPosition();
    const delta = direction === "down" ? amount : -amount;
    try {
      global.scrollBy({ top: delta, behavior: "instant" });
    } catch (_) {
      return { ok: false, error: "scroll_delivery_unknown" };
    }
    const afterRaw = viewportScrollPosition();
    const beforeY = Math.max(0, Math.round(beforeRaw));
    const afterY = Math.max(0, Math.round(afterRaw));

    // The document viewport keeps its exact original semantics when it moves.
    let movedContainer = afterRaw !== beforeRaw
      ? { name: "viewport", before: Math.max(0, beforeRaw), after: Math.max(0, afterRaw) }
      : null;

    if (!movedContainer) {
      // Applications that disable document scrolling move an inner container
      // instead. Only containers that hold observed content are considered and
      // every attempt is measured; nothing is reported without real movement.
      let candidates = [];
      try {
        candidates = observedScrollCandidates();
      } catch (_) {
        candidates = [];
      }
      for (const candidate of candidates) {
        let attempt = null;
        try {
          attempt = attemptContainerScroll(candidate, delta);
        } catch (_) {
          return { ok: false, error: "scroll_delivery_unknown" };
        }
        if (attempt) {
          const isViewportOwner = candidate === global.document?.scrollingElement
            || candidate === global.document?.body;
          movedContainer = {
            name: isViewportOwner ? "document_scrolling_element" : "observed_ancestor",
            tag: String(candidate.tagName || "").toLowerCase().slice(0, 32),
            before: attempt.before,
            after: attempt.after,
          };
          break;
        }
      }
    }

    invalidateGeneration();
    return {
      ok: true, direction, amount,
      before_y: beforeY, after_y: afterY,
      moved: movedContainer !== null,
      at_boundary: movedContainer === null,
      ...(movedContainer ? {
        scroll_container: movedContainer.name,
        ...(movedContainer.tag ? { scroll_container_tag: movedContainer.tag } : {}),
        scroll_container_before: movedContainer.before,
        scroll_container_after: movedContainer.after,
      } : {}),
      requires_verification: true,
    };
  }

  function executePress(params = {}) {
    if (hasForbiddenKeys(params)) return { ok: false, error: "disallowed_parameter" };
    if (isCrossOriginFrame()) return { ok: false, error: "cross_origin_iframe_blocked" };
    if (params.expectedOrigin && global.location
        && String(params.expectedOrigin).toLowerCase() !== global.location.origin.toLowerCase()) {
      return { ok: false, error: "origin_mismatch" };
    }
    const generation = String(params.generation || "").trim();
    if (!generation || generation !== currentGenerationToken) return { ok: false, error: "stale_generation" };
    if (params.value !== undefined) return { ok: false, error: "press_params_invalid" };
    const keyName = String(params.key || "");
    const spec = PRESS_KEY_EVENTS[keyName];
    if (!spec) return { ok: false, error: "press_params_invalid" };

    let target = null;
    let ref = "";
    if (params.ref !== undefined && params.ref !== null) {
      ref = String(params.ref);
      if (!ref.startsWith(`ref_${generation}_`)) return { ok: false, error: "invalid_ref" };
      const el = elementMap.get(ref);
      if (!el || !el.isConnected) return { ok: false, error: "element_detached" };
      if (hoverOnlyRefs.has(ref)) return { ok: false, error: "hover_only_target" };
      if (isSensitiveField(el) || isElementHidden(el) || isElementDisabled(el)) {
        return { ok: false, error: "unsupported_element" };
      }
      target = el;
    }
    if (!target) {
      const doc = global.document;
      const active = doc?.activeElement;
      target = active && active.isConnected ? active : doc?.body;
    }
    if (!target || typeof target.dispatchEvent !== "function") {
      return { ok: false, error: "press_unavailable" };
    }

    try {
      target.focus?.();
      // Synthetic events reach page key handlers (SPA composers, feeds) but
      // never browser default actions; text input belongs to fill.
      for (const type of ["keydown", "keypress", "keyup"]) {
        target.dispatchEvent(new KeyboardEvent(type, {
          key: spec.key, code: spec.code, keyCode: spec.keyCode, which: spec.keyCode,
          bubbles: true, cancelable: true,
        }));
      }
      invalidateGeneration();
      return {
        ok: true,
        key: keyName,
        ...(ref ? { ref } : {}),
        generation,
        requires_verification: true,
      };
    } catch (_) {
      return { ok: false, error: "press_delivery_unknown" };
    }
  }

  function executeHover(params = {}) {
    if (hasForbiddenKeys(params)) return { ok: false, error: "disallowed_parameter" };
    if (isCrossOriginFrame()) return { ok: false, error: "cross_origin_iframe_blocked" };
    if (params.expectedOrigin && global.location
        && String(params.expectedOrigin).toLowerCase() !== global.location.origin.toLowerCase()) {
      return { ok: false, error: "origin_mismatch" };
    }
    const gen = String(params.generation || "").trim();
    if (!gen || gen !== currentGenerationToken) return { ok: false, error: "stale_generation" };
    const ref = String(params.ref || "").trim();
    if (!ref.startsWith(`ref_${gen}_`)) return { ok: false, error: "invalid_ref" };
    const el = elementMap.get(ref);
    if (!el || !el.isConnected) return { ok: false, error: "element_detached" };
    if (isSensitiveField(el) || el.closest?.('[hidden], [aria-hidden="true"], [inert]')) {
      return { ok: false, error: "unsupported_element" };
    }
    if (isElementHidden(el) || isElementDisabled(el)) return { ok: false, error: "element_hidden" };
    try { el.scrollIntoView?.({ block: "center", inline: "center", behavior: "instant" }); } catch (_) {}
    const rect = el.getBoundingClientRect?.();
    const width = Number(global.innerWidth || global.document?.documentElement?.clientWidth);
    const height = Number(global.innerHeight || global.document?.documentElement?.clientHeight);
    if (!rect || !Number.isFinite(width) || !Number.isFinite(height) || width < 1 || height < 1) {
      return { ok: false, error: "hover_target_offscreen" };
    }
    const left = Math.max(0, rect.left), top = Math.max(0, rect.top);
    const right = Math.min(width, rect.right), bottom = Math.min(height, rect.bottom);
    if (!(right > left + 1 && bottom > top + 1)) {
      return { ok: false, error: "hover_target_offscreen" };
    }
    const x = Math.round((left + right) / 2);
    const y = Math.round((top + bottom) / 2);
    const hit = global.document?.elementFromPoint?.(x, y);
    if (hit && hit !== el && !el.contains?.(hit)) {
      return { ok: false, error: "hover_target_obscured" };
    }
    invalidateGeneration();
    return { ok: true, ref, generation: gen, hover_point: { x, y } };
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
    if (hoverOnlyRefs.has(ref)) return { ok: false, error: "hover_only_target" };

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
      let navigationUrl;
      const tag = (el.tagName || "").toLowerCase();
      if (fastPathActionClass(el, tag) === "same_origin_navigation") {
        try {
          navigationUrl = new URL(el.getAttribute("href") || "", global.location?.href).href.slice(0, 4096);
        } catch (_) {}
      }
      el.focus?.();
      el.click?.();
      invalidateGeneration();
      return {
        ok: true,
        ref,
        generation: gen,
        ...(navigationUrl ? { navigation_url: navigationUrl } : {}),
      };
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
    if (tag !== "input" && tag !== "textarea" && tag !== "select" && !isContentEditable) {
      return { ok: false, error: "element_not_fillable" };
    }

    if (typeof params.value !== "string") {
      return { ok: false, error: "value_required" };
    }

    const fillValue = params.value.slice(0, MAX_FILL_CHARS);
    const controlType = tag === "input" ? String(el.type || "").toLowerCase() : "";
    const isCheckable = controlType === "checkbox" || controlType === "radio";
    let targetChecked = null;
    if (isCheckable) {
      if (fillValue !== "true" && fillValue !== "false") {
        return { ok: false, error: "checkable_state_required" };
      }
      targetChecked = fillValue === "true";
      if (controlType === "radio" && !targetChecked) {
        return { ok: false, error: "radio_uncheck_unsupported" };
      }
      if ((el.checked === true) === targetChecked) {
        return { ok: false, error: "control_already_in_state" };
      }
    }

    let selectedOption = null;
    if (tag === "select") {
      if (el.multiple === true) return { ok: false, error: "multiple_select_unsupported" };
      // Match only options offered in the latest observation. A page must not
      // introduce a new option after inspect and have it selected by old refs.
      const observedOptions = selectOptionMap.get(ref) || [];
      const matches = observedOptions.filter((option) => (
        option.value === fillValue || option.label === fillValue
      ));
      if (matches.length !== 1) {
        if (matches.length === 0 && selectOptionTruncatedRefs.has(ref)) {
          // The observation held only a truncated prefix of the options, so the
          // request cannot be proven absent. Never select an unobserved option;
          // the caller must re-observe to widen the window.
          return {
            ok: false,
            error: "select_option_not_observed",
            options_observed: observedOptions.length,
          };
        }
        return { ok: false, error: matches.length ? "select_option_ambiguous" : "select_option_unavailable" };
      }
      const snapshot = matches[0];
      selectedOption = snapshot.node;
      const liveMatches = [...(el.options || [])].filter((option) => (
        !option.hidden && !option.disabled && !option.parentElement?.disabled
        && (String(option.value) === fillValue
          || String(option.label || option.textContent || "").trim() === fillValue)
      ));
      if (liveMatches.length > 1) return { ok: false, error: "select_option_ambiguous" };
      if (
        selectedOption.isConnected === false
        || ![...(el.options || [])].includes(selectedOption)
        || liveMatches.length !== 1 || liveMatches[0] !== selectedOption
        || selectedOption.hidden || selectedOption.disabled || selectedOption.parentElement?.disabled
        || String(selectedOption.value) !== snapshot.value
        || String(selectedOption.label || selectedOption.textContent || "").trim() !== snapshot.label
      ) {
        return { ok: false, error: "select_option_stale" };
      }
    }

    try {
      el.focus?.();
      if (isCheckable) {
        const descriptor = global.HTMLInputElement?.prototype
          ? Object.getOwnPropertyDescriptor(global.HTMLInputElement.prototype, "checked")
          : null;
        // Prefer the native activation path so framework click/change handlers
        // (React, Vue) observe a real toggle; fall back to the native setter plus
        // events only when the click did not land, then require the final state.
        if ((el.checked === true) !== targetChecked && typeof el.click === "function") {
          el.click();
        }
        if ((el.checked === true) !== targetChecked) {
          if (descriptor?.set) descriptor.set.call(el, targetChecked);
          else el.checked = targetChecked;
          el.dispatchEvent(new Event("input", { bubbles: true }));
          el.dispatchEvent(new Event("change", { bubbles: true }));
        }
        if ((el.checked === true) !== targetChecked) {
          return { ok: false, error: "control_state_not_applied", checked: el.checked === true };
        }
      } else if (tag === "select") {
        const descriptor = global.HTMLSelectElement?.prototype
          ? Object.getOwnPropertyDescriptor(global.HTMLSelectElement.prototype, "value")
          : null;
        if (descriptor?.set) descriptor.set.call(el, selectedOption.value);
        else el.value = selectedOption.value;
        if (el.value !== selectedOption.value) return { ok: false, error: "select_not_applied" };
        el.dispatchEvent(new Event("input", { bubbles: true }));
        el.dispatchEvent(new Event("change", { bubbles: true }));
      } else if (tag === "input" || tag === "textarea") {
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
        // Rich editors (ProseMirror/Lexical: Claude, ChatGPT, Grok) ignore a
        // raw innerText write and re-render their own model, so a blind write
        // reads as success while nothing lands. Use native editing
        // (select-all + insertText, as h2w_insert_main does) and verify.
        if (params.mainWorldFill === true) {
          // The extension commits rich editors from the MAIN world (same path
          // as WebChat composer insertion) and verifies the readback there.
          const token = `f${Date.now().toString(36)}${Math.random().toString(36).slice(2, 10)}`;
          el.setAttribute("data-h2w-fill-token", token);
          return { ok: true, ref, generation: gen, main_world_fill_token: token, value: fillValue };
        }
        const norm = (value) => String(value || "").replace(/[\u00a0\ufeff]/g, " ").replace(/\s+/g, " ").trim();
        let inserted = false;
        try {
          el.focus?.();
          const doc = el.ownerDocument || global.document;
          const sel = doc?.getSelection?.();
          if (sel && doc?.createRange) {
            const range = doc.createRange();
            range.selectNodeContents(el);
            sel.removeAllRanges();
            sel.addRange(range);
          }
          inserted = doc?.execCommand?.("insertText", false, fillValue) === true;
        } catch (_) {
          inserted = false;
        }
        if (!inserted) {
          el.innerText = fillValue;
          el.dispatchEvent(new Event("input", { bubbles: true }));
        }
        if (norm(el.innerText ?? el.textContent) !== norm(fillValue)) {
          return { ok: false, error: "fill_not_applied", applied: false, method: inserted ? "insert_text" : "inner_text" };
        }
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
    const scoped = ["input_value_equals", "input_value_contains", "element_checked", "element_selected", "element_class_contains", "element_visible", "element_enabled"];
    if (scoped.includes(condition)) {
      if (!params.generation || params.generation !== currentGenerationToken) return { ok: false, error: "stale_generation" };
      const el = elementMap.get(params.ref);
      if (!el) return { ok: false, error: "invalid_ref" };
      if (!el.isConnected) return { ok: false, error: "element_detached" };
      if (isSensitiveField(el)) return { ok: false, error: "sensitive_field_blocked" };
      if (condition === "element_visible") return { ok: !isElementHidden(el), condition };
      if (isElementHidden(el)) return { ok: false, error: "element_hidden" };
      if (condition === "element_enabled") return { ok: !isElementDisabled(el), condition };
      if (condition.startsWith("input_value_")) {
        if (!["input", "textarea"].includes((el.tagName || "").toLowerCase())) return { ok: false, error: "unsupported_element" };
        if (typeof params.value !== "string" || params.value.length > 4096) return { ok: false, error: "expect_value_invalid" };
        return { ok: condition === "input_value_equals" ? el.value === params.value : String(el.value).includes(params.value), condition };
      }
      if (condition === "element_class_contains") {
        if (typeof params.value !== "string" || !/^[A-Za-z0-9_-]{1,48}$/.test(params.value)) {
          return { ok: false, error: "expect_value_invalid" };
        }
        return { ok: String(el.getAttribute("class") || "").split(/\s+/).includes(params.value), condition };
      }
      const attribute = condition === "element_checked" ? "aria-checked" : "aria-selected";
      const property = condition === "element_checked" ? "checked" : "selected";
      if (typeof el[property] !== "boolean" && el.getAttribute(attribute) === null) return { ok: false, error: "unsupported_element" };
      return { ok: el[property] === true || el.getAttribute(attribute) === "true", condition };
    }
    if (!["document_ready", "url_equals", "text_present", "text_absent", "element_present", "element_absent"].includes(condition)) {
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
    if (condition === "element_present" || condition === "element_absent") {
      // No caller-controlled selector languages or scripts. Compare an exact
      // label on the same fixed, non-sensitive, visible control vocabulary.
      if (value.length > 256 || value !== value.trim()) {
        return { ok: false, error: "expect_value_invalid", condition };
      }
      const controls = global.document?.querySelectorAll?.(OBSERVABLE_CONTROL_SELECTOR) || [];
      let truncated = false;
      let found = false;
      let inspected = 0;
      for (const el of controls) {
        if (!el.isConnected || !isObservableControlCandidate(el)
          || isSensitiveField(el) || el.closest?.('[hidden], [aria-hidden="true"], [inert]')
          || isElementHidden(el) || isElementDisabled(el)) continue;
        if (++inspected > MAX_ELEMENTS) {
          truncated = true;
          break;
        }
        const exactLabels = [
          el.getAttribute?.("aria-label"),
          el.getAttribute?.("placeholder"),
          el.innerText,
          el.textContent,
        ];
        if (exactLabels.some((label) => String(label || "").trim() === value)) {
          found = true;
          break;
        }
      }
      if (!found && truncated) {
        return { ok: false, error: "element_scan_truncated", condition };
      }
      return { ok: condition === "element_present" ? found : !found, condition };
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
    if (action === "hover") return executeHover(msg);
    if (action === "fill") return executeFill(msg);
    if (action === "extract") return executeExtract(msg);
    if (action === "extract_table") return executeExtractTable(msg);
    if (action === "scroll") return executeScroll(msg);
    if (action === "press") return executePress(msg);
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
