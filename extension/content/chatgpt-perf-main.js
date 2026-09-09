// chatgpt-perf-main.js — MAIN-world, document-start ChatGPT render containment.
//
// Current ChatGPT (2026-09) already virtualizes conversation history, so the
// remaining hot path is usually a small number of very large mounted messages.
// Rendered code uses ordinary React DOM rather than CodeMirror EditorView.
//
// Do not interfere with React's commit path or delete React-owned DOM. Let
// Chromium skip offscreen work with content-visibility:auto at three safe
// boundaries:
//   1. exact current code viewers;
//   2. large mounted message roots;
//   3. large top-level blocks in an unfocused ProseMirror writing block.
//
// Message/block intrinsic sizes are measured from the real committed layout
// before containment is enabled. This preserves scroll geometry while letting
// Chromium decide when offscreen contents become relevant again.
(() => {
  "use strict";

  const VERSION = "4";
  const API_NAME = "__HERDR_CHATGPT_PERF__";

  const VIEWER_SELECTOR = "#code-block-viewer.cm-editor";
  const VIEWER_ATTR = "data-herdr-code-block-contained";
  const VIEWER_INTRINSIC_VAR = "--herdr-code-block-intrinsic-size";
  const LINE_HEIGHT_PX = 20;
  const VERTICAL_CHROME_PX = 12;
  const MIN_VIEWER_HEIGHT_PX = 32;
  const MAX_VIEWER_HEIGHT_PX = 200000;

  const MESSAGE_SELECTOR = "[data-message-author-role]";
  const MESSAGE_ATTR = "data-herdr-message-contained";
  const MESSAGE_INTRINSIC_VAR = "--herdr-message-intrinsic-size";
  const MESSAGE_MIN_HEIGHT_PX = 600;
  const MAX_MESSAGE_HEIGHT_PX = 1000000;

  const EDITABLE_ROOT_SELECTOR = ".ProseMirror[contenteditable=\"true\"]";
  const EDITABLE_BLOCK_ATTR = "data-herdr-editable-block-contained";
  const EDITABLE_BLOCK_INTRINSIC_VAR = "--herdr-editable-block-intrinsic-size";
  const EDITABLE_BLOCK_MIN_HEIGHT_PX = 64;
  const EDITABLE_BLOCK_MIN_DESCENDANTS = 8;
  const MAX_EDITABLE_BLOCK_HEIGHT_PX = 200000;

  const STOP_SELECTORS = [
    'button[data-testid="stop-button"]',
    '[role="button"][data-testid="stop-button"]',
    'button[aria-label="Stop generating" i]',
    'button[aria-label="Stop streaming" i]',
    'button[aria-label="停止生成"]',
    'button[aria-label="停止流式"]',
  ];
  const DYNAMIC_MEDIA_TAGS = ["IMG", "VIDEO", "IFRAME", "CANVAS"];

  if (window[API_NAME]) return;

  const stats = {
    observer_batches: 0,
    viewers_prepared: 0,
    viewers_updated: 0,
    viewers_skipped: 0,
    messages_prepared: 0,
    messages_updated: 0,
    messages_skipped: 0,
    messages_cleared: 0,
    editable_blocks_prepared: 0,
    editable_blocks_updated: 0,
    editable_blocks_skipped: 0,
    editable_blocks_cleared: 0,
    last_batch_viewers: 0,
    last_batch_messages: 0,
    last_batch_editable_blocks: 0,
    last_intrinsic_height_px: 0,
    max_intrinsic_height_px: 0,
    last_message_height_px: 0,
    max_message_height_px: 0,
    last_editable_block_height_px: 0,
    max_editable_block_height_px: 0,
  };

  let enabled = true;
  let observer = null;
  let styleElement = null;
  let listenersInstalled = false;
  let lastGenerating = false;
  let selectionRefreshScheduled = false;

  const lastHeightByViewer = new WeakMap();
  const lastHeightByMessage = new WeakMap();
  const lastHeightByEditableBlock = new WeakMap();

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
    const lines = text.length === 0 ? 1 : text.split("\n").length;
    return clamp(
      VERTICAL_CHROME_PX + (lines * LINE_HEIGHT_PX),
      MIN_VIEWER_HEIGHT_PX,
      MAX_VIEWER_HEIGHT_PX,
    );
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

    const height = estimateViewerHeight(code);
    const previous = lastHeightByViewer.get(viewer);
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
    lastHeightByViewer.set(viewer, height);
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

  function hasDynamicMedia(root) {
    if (!(root instanceof Element)) return false;
    for (const tag of DYNAMIC_MEDIA_TAGS) {
      if (root.querySelector?.(tag.toLowerCase())) return true;
    }
    return false;
  }

  function composerGenerating() {
    for (const selector of STOP_SELECTORS) {
      try {
        if (document.querySelector?.(selector)) return true;
      } catch (_) {}
    }
    return false;
  }

  function lastAssistantMessage() {
    const assistants = document.querySelectorAll?.('[data-message-author-role="assistant"]') || [];
    return assistants.length ? assistants[assistants.length - 1] : null;
  }

  function isStreamingMessage(message, generating = composerGenerating()) {
    return generating
      && message?.getAttribute?.("data-message-author-role") === "assistant"
      && message === lastAssistantMessage();
  }

  function containsActiveElement(root) {
    const active = document.activeElement;
    return Boolean(active && active !== document.body && root?.contains?.(active));
  }

  function selectionElements() {
    const selection = window.getSelection?.();
    if (!selection || selection.rangeCount === 0 || selection.isCollapsed) return [];
    const elements = new Set();
    for (const node of [selection.anchorNode, selection.focusNode]) {
      const element = elementForNode(node);
      if (element) elements.add(element);
    }
    return Array.from(elements);
  }

  function selectionTouches(root) {
    if (!(root instanceof Element)) return false;
    return selectionElements().some((element) => root.contains?.(element));
  }

  function clearMessage(message) {
    if (!(message instanceof Element) || message.getAttribute(MESSAGE_ATTR) !== "1") return false;
    try {
      message.removeAttribute(MESSAGE_ATTR);
      message.style.removeProperty(MESSAGE_INTRINSIC_VAR);
      stats.messages_cleared += 1;
      return true;
    } catch (_) {
      return false;
    }
  }

  function prepareMessage(message, generating = composerGenerating()) {
    if (!enabled || !isMessage(message)) return false;

    if (isStreamingMessage(message, generating)
      || containsActiveElement(message)
      || selectionTouches(message)
      || hasDynamicMedia(message)) {
      clearMessage(message);
      stats.messages_skipped += 1;
      return false;
    }

    const rect = message.getBoundingClientRect?.();
    const height = Number(rect?.height || 0);
    if (!Number.isFinite(height) || height < MESSAGE_MIN_HEIGHT_PX) {
      clearMessage(message);
      return false;
    }

    const previous = lastHeightByMessage.get(message);
    const cssHeight = cssPixels(height, MAX_MESSAGE_HEIGHT_PX);
    if (previous === cssHeight && message.getAttribute(MESSAGE_ATTR) === "1") return false;

    try {
      message.setAttribute(MESSAGE_ATTR, "1");
      message.style.setProperty(MESSAGE_INTRINSIC_VAR, cssHeight);
    } catch (_) {
      stats.messages_skipped += 1;
      return false;
    }

    if (previous == null) stats.messages_prepared += 1;
    else stats.messages_updated += 1;
    lastHeightByMessage.set(message, cssHeight);
    stats.last_message_height_px = height;
    stats.max_message_height_px = Math.max(stats.max_message_height_px, height);
    return true;
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
      stats.editable_blocks_cleared += 1;
      return true;
    } catch (_) {
      return false;
    }
  }

  function clearEditableRoot(root) {
    if (!isEditableRoot(root)) return 0;
    let cleared = 0;
    for (const block of root.querySelectorAll?.(`[${EDITABLE_BLOCK_ATTR}="1"]`) || []) {
      cleared += clearEditableBlock(block) ? 1 : 0;
    }
    return cleared;
  }

  function isHeavyEditableBlock(block) {
    if (!(block instanceof Element)) return false;
    const root = block.parentElement;
    if (!isEditableRoot(root)) return false;
    if (block.getAttribute("contenteditable") === "false" || hasDynamicMedia(block)) return false;
    const rect = block.getBoundingClientRect?.();
    const height = Number(rect?.height || 0);
    const descendants = Number(block.querySelectorAll?.("*")?.length || 0);
    return Number.isFinite(height)
      && height > 0
      && (height >= EDITABLE_BLOCK_MIN_HEIGHT_PX || descendants >= EDITABLE_BLOCK_MIN_DESCENDANTS);
  }

  function editableRootUnsafe(root, generating = composerGenerating()) {
    if (!isEditableRoot(root)) return true;
    return containsActiveElement(root)
      || selectionTouches(root)
      || isStreamingMessage(messageFor(root), generating);
  }

  function prepareEditableBlock(block, generating = composerGenerating(), rootUnsafe = null) {
    if (!enabled || !(block instanceof Element)) return false;
    const root = block.parentElement;
    if (!isEditableRoot(root)) return false;

    const unsafe = rootUnsafe == null ? editableRootUnsafe(root, generating) : rootUnsafe;
    if (unsafe || !isHeavyEditableBlock(block)) {
      clearEditableBlock(block);
      return false;
    }

    const height = Number(block.getBoundingClientRect?.().height || 0);
    const cssHeight = cssPixels(height, MAX_EDITABLE_BLOCK_HEIGHT_PX);
    const previous = lastHeightByEditableBlock.get(block);
    if (previous === cssHeight && block.getAttribute(EDITABLE_BLOCK_ATTR) === "1") return false;

    try {
      block.setAttribute(EDITABLE_BLOCK_ATTR, "1");
      block.style.setProperty(EDITABLE_BLOCK_INTRINSIC_VAR, cssHeight);
    } catch (_) {
      stats.editable_blocks_skipped += 1;
      return false;
    }

    if (previous == null) stats.editable_blocks_prepared += 1;
    else stats.editable_blocks_updated += 1;
    lastHeightByEditableBlock.set(block, cssHeight);
    stats.last_editable_block_height_px = height;
    stats.max_editable_block_height_px = Math.max(stats.max_editable_block_height_px, height);
    return true;
  }

  function scanEditableRoot(root, generating = composerGenerating()) {
    if (!enabled || !isEditableRoot(root)) return 0;
    const unsafe = editableRootUnsafe(root, generating);
    if (unsafe) {
      clearEditableRoot(root);
      return 0;
    }
    let prepared = 0;
    for (const block of root.children || []) {
      prepared += prepareEditableBlock(block, generating, false) ? 1 : 0;
    }
    return prepared;
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
        ${MESSAGE_SELECTOR}[${MESSAGE_ATTR}="1"] {
          content-visibility: auto;
          contain-intrinsic-size: auto var(${MESSAGE_INTRINSIC_VAR}, 600px);
        }
        [data-message-author-role="assistant"] ${EDITABLE_ROOT_SELECTOR} > [${EDITABLE_BLOCK_ATTR}="1"] {
          content-visibility: auto;
          contain-intrinsic-size: auto var(${EDITABLE_BLOCK_INTRINSIC_VAR}, 64px);
        }
      }
    `;
    parent.appendChild(style);
    styleElement = style;
    return true;
  }

  function collectViewers(records) {
    const viewers = new Set();
    for (const record of records) {
      const target = elementForNode(record.target);
      if (target) {
        if (isCurrentViewer(target)) viewers.add(target);
        const owner = target.closest?.(VIEWER_SELECTOR);
        if (owner) viewers.add(owner);
      }

      for (const node of record.addedNodes || []) {
        const element = elementForNode(node);
        if (!element) continue;
        if (isCurrentViewer(element)) viewers.add(element);
        for (const viewer of element.querySelectorAll?.(VIEWER_SELECTOR) || []) viewers.add(viewer);
      }
    }
    return viewers;
  }

  function collectMessages(records) {
    const messages = new Set();
    for (const record of records) {
      const owner = messageFor(record.target);
      if (owner) messages.add(owner);

      for (const node of record.addedNodes || []) {
        const element = elementForNode(node);
        if (!element) continue;
        if (isMessage(element)) messages.add(element);
        const nestedOwner = messageFor(element);
        if (nestedOwner) messages.add(nestedOwner);
        for (const message of element.querySelectorAll?.(MESSAGE_SELECTOR) || []) {
          if (isMessage(message)) messages.add(message);
        }
      }
    }
    return messages;
  }

  function collectEditableBlocks(records) {
    const blocks = new Set();
    for (const record of records) {
      const owner = editableBlockFor(record.target);
      if (owner) blocks.add(owner);

      for (const node of record.addedNodes || []) {
        const element = elementForNode(node);
        if (!element) continue;
        const block = editableBlockFor(element);
        if (block) blocks.add(block);
        if (isEditableRoot(element)) {
          for (const child of element.children || []) blocks.add(child);
        }
        for (const root of element.querySelectorAll?.(EDITABLE_ROOT_SELECTOR) || []) {
          if (!isEditableRoot(root)) continue;
          for (const child of root.children || []) blocks.add(child);
        }
      }
    }
    return blocks;
  }

  function handleMutations(records) {
    if (!enabled) return 0;
    installStyle();

    const generating = composerGenerating();
    const messages = collectMessages(records);
    const viewers = collectViewers(records);
    const blocks = collectEditableBlocks(records);

    // A contained message that is mutating must be revealed before any child
    // measurement, otherwise a skipped subtree may only expose its old intrinsic
    // size. Current streaming/focused messages remain revealed until settled.
    for (const message of messages) clearMessage(message);

    let changed = 0;
    for (const viewer of viewers) changed += prepareViewer(viewer) ? 1 : 0;
    const editableRootUnsafeCache = new Map();
    for (const block of blocks) {
      const root = block.parentElement;
      if (!isEditableRoot(root)) continue;
      if (!editableRootUnsafeCache.has(root)) {
        editableRootUnsafeCache.set(root, editableRootUnsafe(root, generating));
      }
      changed += prepareEditableBlock(block, generating, editableRootUnsafeCache.get(root)) ? 1 : 0;
    }
    for (const message of messages) changed += prepareMessage(message, generating) ? 1 : 0;

    stats.observer_batches += 1;
    stats.last_batch_viewers = viewers.size;
    stats.last_batch_messages = messages.size;
    stats.last_batch_editable_blocks = blocks.size;

    if (lastGenerating && !generating) queueMicrotask?.(scan);
    lastGenerating = generating;
    return changed;
  }

  function scan() {
    if (!enabled) return 0;
    installStyle();
    const generating = composerGenerating();
    let changed = 0;

    for (const viewer of document.querySelectorAll?.(VIEWER_SELECTOR) || []) {
      changed += prepareViewer(viewer) ? 1 : 0;
    }
    for (const root of document.querySelectorAll?.(EDITABLE_ROOT_SELECTOR) || []) {
      changed += scanEditableRoot(root, generating);
    }
    for (const message of document.querySelectorAll?.(MESSAGE_SELECTOR) || []) {
      changed += prepareMessage(message, generating) ? 1 : 0;
    }

    lastGenerating = generating;
    return changed;
  }

  function startObserver() {
    if (!enabled || observer || typeof MutationObserver !== "function") return false;
    observer = new MutationObserver(handleMutations);
    observer.observe(document, { childList: true, subtree: true, characterData: true });
    return true;
  }

  function handleFocusIn(event) {
    const message = messageFor(event?.target);
    if (message) clearMessage(message);
    const root = editableRootFor(event?.target);
    if (root) clearEditableRoot(root);
  }

  function handleFocusOut(event) {
    const message = messageFor(event?.target);
    const root = editableRootFor(event?.target);
    queueMicrotask?.(() => {
      if (!enabled) return;
      const generating = composerGenerating();
      if (root && !containsActiveElement(root)) scanEditableRoot(root, generating);
      if (message && !containsActiveElement(message)) prepareMessage(message, generating);
    });
  }

  function scheduleSelectionRefresh() {
    if (selectionRefreshScheduled) return;
    selectionRefreshScheduled = true;
    const run = () => {
      selectionRefreshScheduled = false;
      if (enabled) scan();
    };
    if (typeof requestAnimationFrame === "function") requestAnimationFrame(run);
    else setTimeout(run, 0);
  }

  function handleSelectionChange() {
    for (const element of selectionElements()) {
      const message = messageFor(element);
      if (message) clearMessage(message);
      const root = editableRootFor(element);
      if (root) clearEditableRoot(root);
    }
    scheduleSelectionRefresh();
  }

  function installListeners() {
    if (listenersInstalled || typeof document.addEventListener !== "function") return false;
    document.addEventListener("focusin", handleFocusIn, true);
    document.addEventListener("focusout", handleFocusOut, true);
    document.addEventListener("selectionchange", handleSelectionChange, true);
    listenersInstalled = true;
    return true;
  }

  function removeListeners() {
    if (!listenersInstalled || typeof document.removeEventListener !== "function") return false;
    document.removeEventListener("focusin", handleFocusIn, true);
    document.removeEventListener("focusout", handleFocusOut, true);
    document.removeEventListener("selectionchange", handleSelectionChange, true);
    listenersInstalled = false;
    return true;
  }

  function cleanCurrentViewers() {
    for (const viewer of document.querySelectorAll?.(`${VIEWER_SELECTOR}[${VIEWER_ATTR}="1"]`) || []) {
      try {
        viewer.removeAttribute(VIEWER_ATTR);
        viewer.style.removeProperty(VIEWER_INTRINSIC_VAR);
      } catch (_) {}
    }
  }

  function cleanCurrentMessages() {
    for (const message of document.querySelectorAll?.(`${MESSAGE_SELECTOR}[${MESSAGE_ATTR}="1"]`) || []) {
      try {
        message.removeAttribute(MESSAGE_ATTR);
        message.style.removeProperty(MESSAGE_INTRINSIC_VAR);
      } catch (_) {}
    }
  }

  function cleanCurrentEditableBlocks() {
    for (const block of document.querySelectorAll?.(`[${EDITABLE_BLOCK_ATTR}="1"]`) || []) {
      try {
        block.removeAttribute(EDITABLE_BLOCK_ATTR);
        block.style.removeProperty(EDITABLE_BLOCK_INTRINSIC_VAR);
      } catch (_) {}
    }
  }

  installStyle();
  startObserver();
  installListeners();
  queueMicrotask?.(scan);

  const api = {
    version: VERSION,
    stats,
    scan,
    disable() {
      enabled = false;
      observer?.disconnect();
      observer = null;
      removeListeners();
      styleElement?.remove?.();
      styleElement = null;
      cleanCurrentViewers();
      cleanCurrentMessages();
      cleanCurrentEditableBlocks();
      return { enabled };
    },
    enable() {
      enabled = true;
      installStyle();
      startObserver();
      installListeners();
      scan();
      return { enabled };
    },
    get enabled() { return enabled; },
  };

  publish(Object.freeze(api));
})();
