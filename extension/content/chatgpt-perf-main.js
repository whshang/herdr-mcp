// chatgpt-perf-main.js — MAIN-world, document-start ChatGPT code-viewer render optimization.
//
// Current ChatGPT (2026-09) renders code blocks as ordinary React DOM:
//   #code-block-viewer.cm-editor > .cm-scroller > pre.cm-content > code
// CodeMirror is used for parser/state/highlighting, not EditorView mounting.
//
// Do not interfere with React's commit path. Instead, mark the exact current
// code viewer before the next rendering opportunity and let Chromium skip
// offscreen layout/paint via content-visibility:auto. Derive the initial
// contain-intrinsic-size from the observed production geometry (12px vertical
// chrome + 20px per logical line), then let Chromium remember the real size.
(() => {
  "use strict";

  const VERSION = "3";
  const API_NAME = "__HERDR_CHATGPT_PERF__";
  const VIEWER_SELECTOR = "#code-block-viewer.cm-editor";
  const VIEWER_ATTR = "data-herdr-code-block-contained";
  const INTRINSIC_VAR = "--herdr-code-block-intrinsic-size";
  const LINE_HEIGHT_PX = 20;
  const VERTICAL_CHROME_PX = 12;
  const MIN_HEIGHT_PX = 32;
  const MAX_HEIGHT_PX = 200000;

  if (window[API_NAME]) return;

  const stats = {
    observer_batches: 0,
    viewers_prepared: 0,
    viewers_updated: 0,
    viewers_skipped: 0,
    last_batch_viewers: 0,
    last_intrinsic_height_px: 0,
    max_intrinsic_height_px: 0,
  };

  let enabled = true;
  let observer = null;
  let viewerStyle = null;
  const lastHeightByViewer = new WeakMap();

  function publish(value) {
    try {
      Object.defineProperty(window, API_NAME, {
        configurable: false,
        enumerable: false,
        writable: false,
        value,
      });
    } catch (_) {}
  }

  function clamp(value, min, max) {
    return Math.min(max, Math.max(min, value));
  }

  function isCurrentViewer(viewer) {
    return viewer instanceof Element
      && viewer.id === "code-block-viewer"
      && viewer.classList.contains("cm-editor");
  }

  function isWrappedViewer(viewer) {
    try {
      return Array.from(viewer.classList).some((name) => /(?:^|_)wrapLines$/.test(name));
    } catch (_) {
      return false;
    }
  }

  function codeElementFor(viewer) {
    if (!isCurrentViewer(viewer) || viewer.childElementCount !== 1) return null;
    const scroller = viewer.firstElementChild;
    if (!scroller?.classList.contains("cm-scroller") || scroller.childElementCount !== 1) return null;
    const pre = scroller.firstElementChild;
    if (pre?.tagName !== "PRE" || !pre.classList.contains("cm-content") || pre.childElementCount !== 1) return null;
    const code = pre.firstElementChild;
    return code?.tagName === "CODE" ? code : null;
  }

  function estimateHeight(code) {
    const text = code.textContent ?? "";
    const lines = text.length === 0 ? 1 : text.split("\n").length;
    return clamp(VERTICAL_CHROME_PX + (lines * LINE_HEIGHT_PX), MIN_HEIGHT_PX, MAX_HEIGHT_PX);
  }

  function prepareViewer(viewer) {
    if (!enabled || !isCurrentViewer(viewer)) return false;

    // Wrapped code has width-dependent visual line count. Do not guess an
    // intrinsic height that could destabilize scroll anchoring.
    if (isWrappedViewer(viewer)) {
      stats.viewers_skipped += 1;
      return false;
    }

    const code = codeElementFor(viewer);
    if (!code) {
      stats.viewers_skipped += 1;
      return false;
    }

    const height = estimateHeight(code);
    const previous = lastHeightByViewer.get(viewer);
    if (previous === height && viewer.getAttribute(VIEWER_ATTR) === "1") return false;

    try {
      viewer.setAttribute(VIEWER_ATTR, "1");
      viewer.style.setProperty(INTRINSIC_VAR, `${height}px`);
    } catch (_) {
      stats.viewers_skipped += 1;
      return false;
    }

    if (previous == null) stats.viewers_prepared += 1;
    else stats.viewers_updated += 1;
    lastHeightByViewer.set(viewer, height);
    stats.last_intrinsic_height_px = height;
    stats.max_intrinsic_height_px = Math.max(stats.max_intrinsic_height_px, height);
    return true;
  }

  function installStyle() {
    if (viewerStyle?.isConnected) return true;
    if (typeof document.createElement !== "function") return false;
    const parent = document.head || document.documentElement;
    if (!parent) return false;

    const style = document.createElement("style");
    style.dataset.herdrChatgptPerf = VERSION;
    style.textContent = `
      @supports (content-visibility: auto) {
        ${VIEWER_SELECTOR}[${VIEWER_ATTR}="1"] {
          content-visibility: auto;
          contain-intrinsic-size: auto var(${INTRINSIC_VAR}, 52px);
        }
      }
    `;
    parent.appendChild(style);
    viewerStyle = style;
    return true;
  }

  function collectViewers(records) {
    const viewers = new Set();
    for (const record of records) {
      const target = record.target;
      if (target instanceof Element) {
        if (isCurrentViewer(target)) viewers.add(target);
        const owner = target.closest?.(VIEWER_SELECTOR);
        if (owner) viewers.add(owner);
      }

      for (const node of record.addedNodes || []) {
        if (!(node instanceof Element)) continue;
        if (isCurrentViewer(node)) viewers.add(node);
        for (const viewer of node.querySelectorAll?.(VIEWER_SELECTOR) || []) viewers.add(viewer);
      }
    }
    return viewers;
  }

  function handleMutations(records) {
    if (!enabled) return;
    installStyle();
    const viewers = collectViewers(records);
    let prepared = 0;
    for (const viewer of viewers) prepared += prepareViewer(viewer) ? 1 : 0;
    stats.observer_batches += 1;
    stats.last_batch_viewers = viewers.size;
    return prepared;
  }

  function scan() {
    if (!enabled) return 0;
    installStyle();
    let prepared = 0;
    for (const viewer of document.querySelectorAll?.(VIEWER_SELECTOR) || []) {
      prepared += prepareViewer(viewer) ? 1 : 0;
    }
    return prepared;
  }

  function startObserver() {
    if (!enabled || observer || typeof MutationObserver !== "function") return false;
    observer = new MutationObserver(handleMutations);
    observer.observe(document, { childList: true, subtree: true, characterData: true });
    return true;
  }

  function cleanCurrentViewers() {
    for (const viewer of document.querySelectorAll?.(`${VIEWER_SELECTOR}[${VIEWER_ATTR}="1"]`) || []) {
      try {
        viewer.removeAttribute(VIEWER_ATTR);
        viewer.style.removeProperty(INTRINSIC_VAR);
      } catch (_) {}
    }
  }

  installStyle();
  startObserver();
  queueMicrotask?.(scan);

  const api = {
    version: VERSION,
    stats,
    scan,
    disable() {
      enabled = false;
      observer?.disconnect();
      observer = null;
      viewerStyle?.remove?.();
      viewerStyle = null;
      cleanCurrentViewers();
      return { enabled };
    },
    enable() {
      enabled = true;
      installStyle();
      startObserver();
      scan();
      return { enabled };
    },
    get enabled() { return enabled; },
  };

  publish(Object.freeze(api));
})();
