// chatgpt-perf-main.js — MAIN-world, document-start ChatGPT render containment.
//
// Current ChatGPT (2026-09) already virtualizes conversation history. The
// pathological case is a small number of very large mounted assistant trees.
// Never add synchronous layout work to React's mutation hot path: doing so can
// turn a long mount into layout thrash. The hot path here only records dirtiness
// and registers exact nodes; geometry comes from browser-scheduled observers.
//
// Optimization layers:
//   1. exact current code viewers use the proven line-count intrinsic estimate;
//   2. large top-level blocks inside an unfocused ProseMirror writing block are
//      contained only when they are far from the viewport. IntersectionObserver
//      supplies their committed height asynchronously, so no forced layout is
//      introduced by MutationObserver.
(() => {
  "use strict";

  const VERSION = "6";
  const API_NAME = "__HERDR_CHATGPT_PERF__";

  const VIEWER_SELECTOR = "#code-block-viewer.cm-editor";
  const VIEWER_ATTR = "data-herdr-code-block-contained";
  const VIEWER_INTRINSIC_VAR = "--herdr-code-block-intrinsic-size";
  const LINE_HEIGHT_PX = 20;
  const VERTICAL_CHROME_PX = 12;
  const MIN_VIEWER_HEIGHT_PX = 32;
  const MAX_VIEWER_HEIGHT_PX = 200000;

  const MESSAGE_SELECTOR = "[data-message-author-role]";
  const EDITABLE_ROOT_SELECTOR = ".ProseMirror[contenteditable=\"true\"]";
  const EDITABLE_BLOCK_ATTR = "data-herdr-editable-block-contained";
  const EDITABLE_BLOCK_INTRINSIC_VAR = "--herdr-editable-block-intrinsic-size";
  const EDITABLE_BLOCK_MIN_HEIGHT_PX = 80;
  const MAX_EDITABLE_BLOCK_HEIGHT_PX = 200000;
  const BLOCK_ROOT_MARGIN = "1400px 0px";

  const QUIET_MS = 300;
  const STOP_SELECTORS = [
    'button[data-testid="stop-button"]',
    '[role="button"][data-testid="stop-button"]',
    'button[aria-label="Stop generating" i]',
    'button[aria-label="Stop streaming" i]',
    'button[aria-label="停止生成"]',
    'button[aria-label="停止流式"]',
  ];
  const STREAMING_THROTTLE_ATTR = "data-herdr-streaming-throttle";
  const DYNAMIC_MEDIA_SELECTOR = "img,video,iframe,canvas";

  if (window[API_NAME]) return;

  const stats = {
    observer_batches: 0,
    mutation_records: 0,
    idle_scans: 0,
    viewers_prepared: 0,
    viewers_updated: 0,
    viewers_cleared: 0,
    viewers_skipped: 0,
    editable_roots_observed: 0,
    editable_blocks_observed: 0,
    editable_blocks_contained: 0,
    editable_blocks_updated: 0,
    editable_blocks_revealed: 0,
    editable_blocks_skipped: 0,
    streaming_throttle_activations: 0,
    streaming_throttle_deactivations: 0,
    last_batch_records: 0,
    last_scan_ms: 0,
    last_intrinsic_height_px: 0,
    max_intrinsic_height_px: 0,
    last_editable_block_height_px: 0,
    max_editable_block_height_px: 0,
  };

  let enabled = true;
  let observer = null;
  let blockObserver = null;
  let styleElement = null;
  let listenersInstalled = false;
  let quietTimer = null;
  let idleHandle = null;
  let lastMutationAt = 0;
  let discoveryPending = false;

  const viewerHeights = new WeakMap();
  const pendingViewers = new Set();
  const observedRoots = new Set();
  const observedBlocks = new Set();
  const blockHeights = new WeakMap();

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

  function cssPixels(value, max) {
    const bounded = clamp(Number(value) || 0, 1, max);
    return `${Math.round(bounded * 1000) / 1000}px`;
  }

  function elementForNode(node) {
    if (node instanceof Element) return node;
    return node?.parentElement instanceof Element ? node.parentElement : null;
  }

  function isCurrentViewer(viewer) {
    return viewer instanceof Element
      && viewer.id === "code-block-viewer"
      && viewer.classList.contains("cm-editor");
  }

  function viewerFor(node) {
    const element = elementForNode(node);
    const viewer = element?.closest?.(VIEWER_SELECTOR);
    return isCurrentViewer(viewer) ? viewer : null;
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

  function estimateViewerHeight(code) {
    const text = code.textContent ?? "";
    let lines = 1;
    for (let i = 0; i < text.length; i += 1) {
      if (text.charCodeAt(i) === 10) lines += 1;
    }
    return clamp(
      VERTICAL_CHROME_PX + (lines * LINE_HEIGHT_PX),
      MIN_VIEWER_HEIGHT_PX,
      MAX_VIEWER_HEIGHT_PX,
    );
  }

  function clearViewer(viewer) {
    if (!(viewer instanceof Element) || viewer.getAttribute(VIEWER_ATTR) !== "1") return false;
    try {
      viewer.removeAttribute(VIEWER_ATTR);
      viewer.style.removeProperty(VIEWER_INTRINSIC_VAR);
      stats.viewers_cleared += 1;
      return true;
    } catch (_) {
      return false;
    }
  }

  function prepareViewer(viewer) {
    if (!enabled || !isCurrentViewer(viewer)) return false;
    if (isWrappedViewer(viewer)) {
      clearViewer(viewer);
      stats.viewers_skipped += 1;
      return false;
    }
    const code = codeElementFor(viewer);
    if (!code) return false;

    const height = estimateViewerHeight(code);
    const previous = viewerHeights.get(viewer);
    if (previous === height && viewer.getAttribute(VIEWER_ATTR) === "1") return false;

    try {
      viewer.setAttribute(VIEWER_ATTR, "1");
      viewer.style.setProperty(VIEWER_INTRINSIC_VAR, `${height}px`);
    } catch (_) {
      stats.viewers_skipped += 1;
      return false;
    }

    if (previous == null) stats.viewers_prepared += 1;
    else stats.viewers_updated += 1;
    viewerHeights.set(viewer, height);
    stats.last_intrinsic_height_px = height;
    stats.max_intrinsic_height_px = Math.max(stats.max_intrinsic_height_px, height);
    return true;
  }

  function isMessage(message) {
    if (!(message instanceof Element)) return false;
    const role = message.getAttribute("data-message-author-role");
    return role === "assistant" || role === "user";
  }

  function messageFor(node) {
    const element = elementForNode(node);
    const message = element?.closest?.(MESSAGE_SELECTOR);
    return isMessage(message) ? message : null;
  }

  function isEditableRoot(root) {
    return root instanceof Element
      && root.classList.contains("ProseMirror")
      && root.getAttribute("contenteditable") === "true"
      && messageFor(root)?.getAttribute("data-message-author-role") === "assistant";
  }

  function editableRootFor(node) {
    const element = elementForNode(node);
    const root = element?.closest?.(EDITABLE_ROOT_SELECTOR);
    return isEditableRoot(root) ? root : null;
  }

  function editableBlockFor(node) {
    const element = elementForNode(node);
    const root = editableRootFor(element);
    if (!root || !element) return null;
    let block = element;
    while (block && block.parentElement !== root) block = block.parentElement;
    return block instanceof Element && block.parentElement === root ? block : null;
  }

  function clearEditableBlock(block) {
    if (!(block instanceof Element) || block.getAttribute(EDITABLE_BLOCK_ATTR) !== "1") return false;
    try {
      block.removeAttribute(EDITABLE_BLOCK_ATTR);
      block.style.removeProperty(EDITABLE_BLOCK_INTRINSIC_VAR);
      stats.editable_blocks_revealed += 1;
      return true;
    } catch (_) {
      return false;
    }
  }

  function clearEditableRoot(root) {
    if (!isEditableRoot(root)) return 0;
    let cleared = 0;
    for (const block of root.children || []) cleared += clearEditableBlock(block) ? 1 : 0;
    return cleared;
  }

  function composerGenerating() {
    for (const selector of STOP_SELECTORS) {
      try {
        if (document.querySelector?.(selector)) return true;
      } catch (_) {}
    }
    return false;
  }

  function setStreamingThrottle(active) {
    const root = document.documentElement;
    if (!root) return false;
    const current = root.getAttribute(STREAMING_THROTTLE_ATTR) === "1";
    const next = Boolean(enabled && active);
    if (current === next) return false;
    if (next) {
      root.setAttribute(STREAMING_THROTTLE_ATTR, "1");
      stats.streaming_throttle_activations += 1;
    } else {
      root.removeAttribute(STREAMING_THROTTLE_ATTR);
      stats.streaming_throttle_deactivations += 1;
    }
    return true;
  }

  function nodeHasStopControl(node) {
    const element = elementForNode(node);
    if (!element) return false;
    for (const selector of STOP_SELECTORS) {
      try {
        if (element.matches?.(selector) || element.querySelector?.(selector)) return true;
      } catch (_) {}
    }
    return false;
  }

  function updateStreamingThrottleFromMutations(records) {
    let added = false;
    let removed = false;
    for (const record of records) {
      if (record.type && record.type !== "childList") continue;
      for (const node of record.addedNodes || []) added ||= nodeHasStopControl(node);
      for (const node of record.removedNodes || []) removed ||= nodeHasStopControl(node);
    }
    if (added && removed) return setStreamingThrottle(composerGenerating());
    if (added) return setStreamingThrottle(true);
    if (removed) return setStreamingThrottle(composerGenerating());
    return false;
  }

  function safetySnapshot() {
    const generating = composerGenerating();
    let streamingMessage = null;
    if (generating) {
      const assistants = document.querySelectorAll?.('[data-message-author-role="assistant"]') || [];
      streamingMessage = assistants.length ? assistants[assistants.length - 1] : null;
    }

    const selected = new Set();
    const selection = window.getSelection?.();
    if (selection && selection.rangeCount > 0 && !selection.isCollapsed) {
      for (const node of [selection.anchorNode, selection.focusNode]) {
        const element = elementForNode(node);
        if (element) selected.add(element);
      }
    }

    return {
      active: document.activeElement,
      selected,
      streamingMessage,
    };
  }

  function rootUnsafe(root, safety) {
    if (!isEditableRoot(root)) return true;
    if (safety.active && safety.active !== document.body && root.contains?.(safety.active)) return true;
    for (const element of safety.selected) {
      if (root.contains?.(element)) return true;
    }
    return Boolean(safety.streamingMessage && messageFor(root) === safety.streamingMessage);
  }

  function hasDynamicMedia(block) {
    try {
      return Boolean(block?.querySelector?.(DYNAMIC_MEDIA_SELECTOR));
    } catch (_) {
      return true;
    }
  }

  function applyEditableContainment(block, height) {
    const cssHeight = cssPixels(height, MAX_EDITABLE_BLOCK_HEIGHT_PX);
    const previous = blockHeights.get(block);
    if (previous === cssHeight && block.getAttribute(EDITABLE_BLOCK_ATTR) === "1") return false;
    try {
      block.setAttribute(EDITABLE_BLOCK_ATTR, "1");
      block.style.setProperty(EDITABLE_BLOCK_INTRINSIC_VAR, cssHeight);
    } catch (_) {
      stats.editable_blocks_skipped += 1;
      return false;
    }

    if (previous == null) stats.editable_blocks_contained += 1;
    else stats.editable_blocks_updated += 1;
    blockHeights.set(block, cssHeight);
    stats.last_editable_block_height_px = height;
    stats.max_editable_block_height_px = Math.max(stats.max_editable_block_height_px, height);
    return true;
  }

  function handleBlockEntries(entries) {
    if (!enabled) return;
    const safety = safetySnapshot();
    const unsafeByRoot = new Map();

    for (const entry of entries) {
      const block = entry?.target;
      const root = block?.parentElement;
      if (!(block instanceof Element) || !isEditableRoot(root) || !block.isConnected) {
        try { blockObserver?.unobserve?.(block); } catch (_) {}
        observedBlocks.delete(block);
        continue;
      }

      if (!unsafeByRoot.has(root)) unsafeByRoot.set(root, rootUnsafe(root, safety));
      if (entry.isIntersecting || unsafeByRoot.get(root) || hasDynamicMedia(block)) {
        clearEditableBlock(block);
        continue;
      }

      const height = Number(entry.boundingClientRect?.height || 0);
      if (!Number.isFinite(height) || height < EDITABLE_BLOCK_MIN_HEIGHT_PX) {
        clearEditableBlock(block);
        continue;
      }
      applyEditableContainment(block, height);
    }
  }

  function installBlockObserver() {
    if (blockObserver || typeof IntersectionObserver !== "function") return Boolean(blockObserver);
    blockObserver = new IntersectionObserver(handleBlockEntries, {
      root: null,
      rootMargin: BLOCK_ROOT_MARGIN,
      threshold: 0,
    });
    return true;
  }

  function observeBlock(block) {
    if (!enabled || !(block instanceof Element) || !isEditableRoot(block.parentElement)) return false;
    if (observedBlocks.has(block)) return false;
    if (!installBlockObserver()) return false;
    observedBlocks.add(block);
    blockObserver.observe(block);
    stats.editable_blocks_observed += 1;
    return true;
  }

  function observeRoot(root, rearm = false) {
    if (!enabled || !isEditableRoot(root)) return false;
    if (!observedRoots.has(root)) {
      observedRoots.add(root);
      stats.editable_roots_observed += 1;
    }
    for (const block of root.children || []) {
      if (!observedBlocks.has(block)) observeBlock(block);
      else if (rearm && blockObserver) {
        blockObserver.unobserve(block);
        blockObserver.observe(block);
      }
    }
    return true;
  }

  function installStyle() {
    if (styleElement?.isConnected) return true;
    if (typeof document.createElement !== "function") return false;
    const parent = document.head || document.documentElement;
    if (!parent) return false;

    const style = document.createElement("style");
    style.dataset.herdrChatgptPerf = VERSION;
    style.textContent = `
      @supports (content-visibility: auto) {
        ${VIEWER_SELECTOR}[${VIEWER_ATTR}="1"] {
          content-visibility: auto;
          contain-intrinsic-size: auto var(${VIEWER_INTRINSIC_VAR}, 52px);
        }
        [data-message-author-role="assistant"] ${EDITABLE_ROOT_SELECTOR} > [${EDITABLE_BLOCK_ATTR}="1"] {
          content-visibility: auto;
          contain-intrinsic-size: auto var(${EDITABLE_BLOCK_INTRINSIC_VAR}, 80px);
        }
      }
      html[${STREAMING_THROTTLE_ATTR}="1"] [data-message-author-role="assistant"]
        :not(button, button *, [role="button"], [role="button"] *,
          [class~="group/tool-message"], [class~="group/tool-message"] *,
          [aria-busy="true"], [aria-busy="true"] *,
          [class*="animate-spin"], [class*="animate-pulse"],
          video, audio, iframe, canvas, svg) {
        animation-duration: 0.001ms !important;
        animation-delay: 0ms !important;
        animation-iteration-count: 1 !important;
        transition-duration: 0.001ms !important;
        transition-delay: 0ms !important;
      }
    `;
    parent.appendChild(style);
    styleElement = style;
    return true;
  }

  function markViewerDirty(viewer) {
    if (!isCurrentViewer(viewer)) return false;
    clearViewer(viewer);
    pendingViewers.add(viewer);
    return true;
  }

  function processMutationNode(node) {
    const element = elementForNode(node);
    if (!element) return;

    const viewer = viewerFor(element);
    if (viewer) markViewerDirty(viewer);
    else if (isCurrentViewer(element)) markViewerDirty(element);

    const root = isEditableRoot(element) ? element : editableRootFor(element);
    if (root) {
      const block = editableBlockFor(element);
      if (block) {
        clearEditableBlock(block);
        observeBlock(block);
      }
      if (element === root) observeRoot(root);
    } else if (isEditableRoot(element.parentElement)) {
      clearEditableBlock(element);
      observeBlock(element);
      observeRoot(element.parentElement);
    }
  }

  function handleMutations(records) {
    if (!enabled) return;
    installStyle();
    stats.observer_batches += 1;
    stats.mutation_records += records.length;
    stats.last_batch_records = records.length;

    for (const record of records) {
      processMutationNode(record.target);
      for (const node of record.addedNodes || []) processMutationNode(node);
      for (const node of record.removedNodes || []) {
        const element = elementForNode(node);
        if (element && observedBlocks.has(element)) {
          try { blockObserver?.unobserve?.(element); } catch (_) {}
          observedBlocks.delete(element);
        }
      }
    }

    updateStreamingThrottleFromMutations(records);
    scheduleSettledScan();
  }

  function scan() {
    if (!enabled) return 0;
    installStyle();
    installBlockObserver();
    setStreamingThrottle(composerGenerating());
    const started = performance.now?.() || 0;
    let discovered = 0;

    for (const viewer of document.querySelectorAll?.(VIEWER_SELECTOR) || []) {
      pendingViewers.add(viewer);
      discovered += 1;
    }

    for (const root of document.querySelectorAll?.(EDITABLE_ROOT_SELECTOR) || []) {
      if (!isEditableRoot(root)) continue;
      observeRoot(root, true);
      discovered += 1;
    }

    for (const block of Array.from(observedBlocks)) {
      if (!block?.isConnected || !isEditableRoot(block.parentElement)) {
        try { blockObserver?.unobserve?.(block); } catch (_) {}
        observedBlocks.delete(block);
      }
    }
    for (const root of Array.from(observedRoots)) {
      if (!root?.isConnected || !isEditableRoot(root)) observedRoots.delete(root);
    }

    for (const viewer of Array.from(pendingViewers)) {
      pendingViewers.delete(viewer);
      if (viewer?.isConnected) prepareViewer(viewer);
    }

    stats.idle_scans += 1;
    const ended = performance.now?.() || started;
    stats.last_scan_ms = Math.max(0, ended - started);
    return discovered;
  }

  function runSettledScan() {
    quietTimer = null;
    if (!enabled || !discoveryPending) return;
    const now = performance.now?.() || Date.now();
    const remaining = QUIET_MS - (now - lastMutationAt);
    if (remaining > 0) {
      quietTimer = setTimeout(runSettledScan, Math.ceil(remaining));
      return;
    }
    discoveryPending = false;

    const run = () => {
      idleHandle = null;
      if (enabled) scan();
    };
    if (typeof requestIdleCallback === "function") {
      idleHandle = requestIdleCallback(run);
    } else {
      idleHandle = setTimeout(run, 0);
    }
  }

  function scheduleSettledScan() {
    discoveryPending = true;
    lastMutationAt = performance.now?.() || Date.now();
    if (quietTimer != null) return;
    quietTimer = setTimeout(runSettledScan, QUIET_MS);
  }

  function startObserver() {
    if (!enabled || observer || typeof MutationObserver !== "function") return false;
    observer = new MutationObserver(handleMutations);
    observer.observe(document, { childList: true, subtree: true, characterData: true });
    return true;
  }

  function handleFocusIn(event) {
    const root = editableRootFor(event?.target);
    if (root) clearEditableRoot(root);
  }

  function handleFocusOut(event) {
    const root = editableRootFor(event?.target);
    if (root) observeRoot(root, true);
    scheduleSettledScan();
  }

  function handleSelectionChange() {
    if (!enabled) return;
    const selection = window.getSelection?.();
    if (!selection || selection.rangeCount === 0 || selection.isCollapsed) {
      scheduleSettledScan();
      return;
    }
    const roots = new Set();
    for (const node of [selection.anchorNode, selection.focusNode]) {
      const root = editableRootFor(node);
      if (root) roots.add(root);
    }
    for (const root of roots) clearEditableRoot(root);
  }

  function installListeners() {
    if (listenersInstalled || typeof document.addEventListener !== "function") return;
    document.addEventListener("focusin", handleFocusIn, true);
    document.addEventListener("focusout", handleFocusOut, true);
    document.addEventListener("selectionchange", handleSelectionChange, true);
    listenersInstalled = true;
  }

  function removeListeners() {
    if (!listenersInstalled || typeof document.removeEventListener !== "function") return;
    document.removeEventListener("focusin", handleFocusIn, true);
    document.removeEventListener("focusout", handleFocusOut, true);
    document.removeEventListener("selectionchange", handleSelectionChange, true);
    listenersInstalled = false;
  }

  function cleanupContainment() {
    for (const viewer of document.querySelectorAll?.(`${VIEWER_SELECTOR}[${VIEWER_ATTR}="1"]`) || []) {
      clearViewer(viewer);
    }
    for (const block of document.querySelectorAll?.(`[${EDITABLE_BLOCK_ATTR}="1"]`) || []) {
      clearEditableBlock(block);
    }
    try { document.documentElement?.removeAttribute(STREAMING_THROTTLE_ATTR); } catch (_) {}
  }

  function cancelScheduledWork() {
    if (quietTimer != null) {
      clearTimeout(quietTimer);
      quietTimer = null;
    }
    if (idleHandle != null) {
      if (typeof cancelIdleCallback === "function") cancelIdleCallback(idleHandle);
      else clearTimeout(idleHandle);
      idleHandle = null;
    }
    discoveryPending = false;
  }

  installStyle();
  installBlockObserver();
  startObserver();
  installListeners();
  scheduleSettledScan();

  const api = {
    version: VERSION,
    stats,
    scan,
    disable() {
      enabled = false;
      observer?.disconnect?.();
      observer = null;
      blockObserver?.disconnect?.();
      blockObserver = null;
      cancelScheduledWork();
      removeListeners();
      cleanupContainment();
      pendingViewers.clear();
      observedRoots.clear();
      observedBlocks.clear();
      styleElement?.remove?.();
      styleElement = null;
      return { enabled };
    },
    enable() {
      if (enabled) return { enabled };
      enabled = true;
      installStyle();
      installBlockObserver();
      startObserver();
      installListeners();
      setStreamingThrottle(composerGenerating());
      scheduleSettledScan();
      return { enabled };
    },
    get enabled() { return enabled; },
  };

  publish(Object.freeze(api));
})();
