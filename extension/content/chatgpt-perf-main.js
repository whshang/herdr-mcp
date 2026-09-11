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
//      introduced by MutationObserver;
//   3. settled, heavy tool-call clusters are fully skipped while they are outside
//      the viewport. Their exact committed height comes from IntersectionObserver,
//      so scroll geometry is preserved without reading layout in the mutation path.
(() => {
  "use strict";

  const VERSION = "9";
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

  const TOOL_MESSAGE_SELECTOR = '[class~="group/tool-message"]';
  const TOOL_CLUSTER_ATTR = "data-herdr-tool-cluster-hidden";
  const TOOL_CLUSTER_OBSERVED_ATTR = "data-herdr-tool-cluster-observed";
  const TOOL_CLUSTER_INTRINSIC_VAR = "--herdr-tool-cluster-intrinsic-size";
  const TOOL_CLUSTER_NEAREST_COUNT = 2;
  const TOOL_CLUSTER_MIN_MESSAGES = 8;
  const TOOL_CLUSTER_MIN_HEIGHT_PX = 600;
  const TOOL_CLUSTER_MAX_HEIGHT_PX = 500000;
  const TOOL_CLUSTER_ANCESTOR_DEPTH = 12;
  const TOOL_FIND_SUSPEND_MS = 30000;
  const TOOL_RUN_SUMMARY_ATTR = "data-herdr-tool-run-summary";
  const TOOL_RUN_HIDDEN_ATTR = "data-herdr-tool-run-hidden";
  const TOOL_RUN_MIN_MESSAGES = 2;

  const QUIET_MS = 300;
  const MAX_DISCOVERY_LATENCY_MS = 1000;
  const DISCOVERY_IDLE_TIMEOUT_MS = 100;
  const STOP_SELECTORS = [
    'button[data-testid="stop-button"]',
    '[role="button"][data-testid="stop-button"]',
    'button[aria-label="Stop generating" i]',
    'button[aria-label="Stop streaming" i]',
    'button[aria-label="停止生成"]',
    'button[aria-label="停止流式"]',
  ];
  const DYNAMIC_MEDIA_SELECTOR = "img,video,iframe,canvas";

  if (window[API_NAME]) return;

  const stats = {
    observer_batches: 0,
    mutation_records: 0,
    idle_scans: 0,
    forced_scans: 0,
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
    tool_clusters_observed: 0,
    tool_clusters_hidden: 0,
    tool_clusters_updated: 0,
    tool_clusters_revealed: 0,
    tool_clusters_skipped: 0,
    tool_discovery_batches: 0,
    tool_runs_folded: 0,
    tool_run_messages_hidden: 0,
    tool_runs_expanded: 0,
    last_batch_records: 0,
    last_scan_ms: 0,
    last_intrinsic_height_px: 0,
    max_intrinsic_height_px: 0,
    last_editable_block_height_px: 0,
    max_editable_block_height_px: 0,
    last_tool_cluster_height_px: 0,
    max_tool_cluster_height_px: 0,
  };

  let enabled = true;
  let observer = null;
  let blockObserver = null;
  let toolClusterObserver = null;
  let styleElement = null;
  let listenersInstalled = false;
  let quietTimer = null;
  let maxDiscoveryTimer = null;
  let idleHandle = null;
  let lastMutationAt = 0;
  let discoveryPending = false;
  let toolFindSuspendUntil = 0;
  let toolFindResumeTimer = null;

  const viewerHeights = new WeakMap();
  const pendingViewers = new Set();
  const observedRoots = new Set();
  const observedBlocks = new Set();
  const blockHeights = new WeakMap();
  const observedToolClusters = new Set();
  const toolClusterHeights = new WeakMap();
  const foldedToolRuns = new WeakMap();

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

  function toolClusterFor(node) {
    const element = elementForNode(node);
    if (!element) return null;
    const cluster = element.closest?.(
      `[${TOOL_CLUSTER_OBSERVED_ATTR}="1"], [${TOOL_CLUSTER_ATTR}="1"]`,
    );
    return cluster instanceof Element ? cluster : null;
  }

  function nodeHasToolMessage(node) {
    const element = elementForNode(node);
    if (!element) return false;
    try {
      return element.matches?.(TOOL_MESSAGE_SELECTOR)
        || Boolean(element.querySelector?.(TOOL_MESSAGE_SELECTOR));
    } catch (_) {
      return false;
    }
  }

  function clearToolCluster(cluster) {
    if (!(cluster instanceof Element) || cluster.getAttribute(TOOL_CLUSTER_ATTR) !== "1") return false;
    try {
      cluster.removeAttribute(TOOL_CLUSTER_ATTR);
      cluster.style.removeProperty(TOOL_CLUSTER_INTRINSIC_VAR);
      stats.tool_clusters_revealed += 1;
      return true;
    } catch (_) {
      return false;
    }
  }

  function clearAllToolClusters() {
    let cleared = 0;
    for (const cluster of observedToolClusters) cleared += clearToolCluster(cluster) ? 1 : 0;
    return cleared;
  }

  function unobserveToolCluster(cluster, clear = true) {
    if (!(cluster instanceof Element)) return false;
    if (clear) clearToolCluster(cluster);
    try { toolClusterObserver?.unobserve?.(cluster); } catch (_) {}
    observedToolClusters.delete(cluster);
    try { cluster.removeAttribute(TOOL_CLUSTER_OBSERVED_ATTR); } catch (_) {}
    return true;
  }

  function toolClusterUnsafe(cluster, safety) {
    if (!(cluster instanceof Element)) return true;
    if (safety.active && safety.active !== document.body && cluster.contains?.(safety.active)) return true;
    for (const element of safety.selected) {
      if (cluster.contains?.(element)) return true;
    }
    return false;
  }

  function toolHidingSuspended() {
    const now = performance.now?.() || Date.now();
    return now < toolFindSuspendUntil;
  }

  function hideToolCluster(cluster, height) {
    if (!enabled || !(cluster instanceof Element)) return false;
    const cssHeight = cssPixels(height, TOOL_CLUSTER_MAX_HEIGHT_PX);
    const previous = toolClusterHeights.get(cluster);
    if (previous === cssHeight && cluster.getAttribute(TOOL_CLUSTER_ATTR) === "1") return false;
    try {
      cluster.style.setProperty(TOOL_CLUSTER_INTRINSIC_VAR, cssHeight);
      cluster.setAttribute(TOOL_CLUSTER_ATTR, "1");
    } catch (_) {
      stats.tool_clusters_skipped += 1;
      return false;
    }

    if (previous == null) stats.tool_clusters_hidden += 1;
    else stats.tool_clusters_updated += 1;
    toolClusterHeights.set(cluster, cssHeight);
    stats.last_tool_cluster_height_px = height;
    stats.max_tool_cluster_height_px = Math.max(stats.max_tool_cluster_height_px, height);
    return true;
  }

  function handleToolClusterEntries(entries) {
    if (!enabled) return;
    const safety = safetySnapshot();
    const suspended = toolHidingSuspended();
    for (const entry of entries) {
      const cluster = entry?.target;
      if (!(cluster instanceof Element) || !cluster.isConnected || !observedToolClusters.has(cluster)) {
        unobserveToolCluster(cluster);
        continue;
      }
      if (entry.isIntersecting || suspended || toolClusterUnsafe(cluster, safety)) {
        clearToolCluster(cluster);
        continue;
      }
      const height = Number(entry.boundingClientRect?.height || 0);
      if (!Number.isFinite(height) || height < TOOL_CLUSTER_MIN_HEIGHT_PX) {
        clearToolCluster(cluster);
        stats.tool_clusters_skipped += 1;
        continue;
      }
      hideToolCluster(cluster, height);
    }
  }

  function installToolClusterObserver() {
    if (toolClusterObserver || typeof IntersectionObserver !== "function") return Boolean(toolClusterObserver);
    toolClusterObserver = new IntersectionObserver(handleToolClusterEntries, {
      root: null,
      rootMargin: "0px",
      threshold: 0,
    });
    return true;
  }

  function observeToolCluster(cluster) {
    if (!enabled || !(cluster instanceof Element) || observedToolClusters.has(cluster)) return false;
    if (!installToolClusterObserver()) return false;
    observedToolClusters.add(cluster);
    try {
      cluster.setAttribute(TOOL_CLUSTER_OBSERVED_ATTR, "1");
      toolClusterObserver.observe(cluster);
    } catch (_) {
      observedToolClusters.delete(cluster);
      try { cluster.removeAttribute(TOOL_CLUSTER_OBSERVED_ATTR); } catch (_) {}
      return false;
    }
    stats.tool_clusters_observed += 1;
    return true;
  }

  function discoverToolClusters() {
    if (!enabled || !installToolClusterObserver()) return 0;
    const tools = Array.from(document.querySelectorAll?.(TOOL_MESSAGE_SELECTOR) || []);
    const counts = new Map();

    for (const tool of tools) {
      let ancestor = tool.parentElement;
      for (let depth = 0; ancestor && depth < TOOL_CLUSTER_ANCESTOR_DEPTH; depth += 1) {
        counts.set(ancestor, (counts.get(ancestor) || 0) + 1);
        ancestor = ancestor.parentElement;
      }
    }

    const nearest = new Set();
    for (const tool of tools) {
      let ancestor = tool.parentElement;
      for (let depth = 0; ancestor && depth < TOOL_CLUSTER_ANCESTOR_DEPTH; depth += 1) {
        if ((counts.get(ancestor) || 0) >= TOOL_CLUSTER_NEAREST_COUNT) {
          nearest.add(ancestor);
          break;
        }
        ancestor = ancestor.parentElement;
      }
    }

    const heavy = Array.from(nearest).filter(
      (cluster) => (counts.get(cluster) || 0) >= TOOL_CLUSTER_MIN_MESSAGES,
    );
    const minimal = heavy.filter(
      (cluster) => !heavy.some((other) => other !== cluster && cluster.contains?.(other)),
    );
    const keep = new Set(minimal);

    for (const cluster of Array.from(observedToolClusters)) {
      if (!cluster?.isConnected || !keep.has(cluster)) unobserveToolCluster(cluster);
    }
    for (const cluster of minimal) observeToolCluster(cluster);
    return minimal.length;
  }

  function toolRunLabel(count, expanded) {
    const lang = String(document.documentElement?.lang || "").toLowerCase();
    const label = lang.startsWith("zh")
      ? "工具调用"
      : (lang.startsWith("ja") ? "ツール呼び出し" : "Tool calls");
    return `${expanded ? "−" : "+"} ${label} × ${count}`;
  }

  function isDirectToolWrapper(child) {
    if (!(child instanceof Element) || child.getAttribute(TOOL_RUN_SUMMARY_ATTR) === "1") return false;
    try {
      return child.matches?.(TOOL_MESSAGE_SELECTOR)
        || Boolean(child.querySelector?.(TOOL_MESSAGE_SELECTOR));
    } catch (_) {
      return false;
    }
  }

  function isToolRunBarrier(child) {
    if (!(child instanceof Element) || child.getAttribute(TOOL_RUN_SUMMARY_ATTR) === "1") return false;
    if (isDirectToolWrapper(child)) return false;
    if (child.getAttribute("data-message-author-role")) return true;
    const text = String(child.textContent || child.innerText || "").trim();
    if (text) return true;
    try {
      return Boolean(child.querySelector?.("button,a,input,textarea,select,[role=\"button\"]"));
    } catch (_) {
      return false;
    }
  }

  function setToolRunExpanded(summary, expanded) {
    const wrappers = foldedToolRuns.get(summary) || [];
    summary.setAttribute("aria-expanded", expanded ? "true" : "false");
    summary.textContent = toolRunLabel(wrappers.length, expanded);
    for (const wrapper of wrappers) {
      if (!(wrapper instanceof Element) || !wrapper.isConnected) continue;
      if (expanded) wrapper.removeAttribute(TOOL_RUN_HIDDEN_ATTR);
      else wrapper.setAttribute(TOOL_RUN_HIDDEN_ATTR, "1");
    }
  }

  function lowestCommonAncestor(nodes, boundary) {
    if (!nodes.length) return null;
    let candidate = nodes[0] instanceof Element ? nodes[0] : null;
    while (candidate && candidate !== boundary?.parentElement) {
      if (nodes.every((node) => candidate.contains(node))) return candidate;
      candidate = candidate.parentElement;
    }
    return boundary instanceof Element ? boundary : null;
  }

  function toolRunStacks() {
    const stacks = new Set();
    for (const message of document.querySelectorAll?.('[data-message-author-role="assistant"]') || []) {
      if (message.parentElement instanceof Element) stacks.add(message.parentElement);
    }
    for (const section of document.querySelectorAll?.("section") || []) {
      const turnId = String(section.getAttribute?.("data-testid") || "");
      if (!turnId.startsWith("conversation-turn-")) continue;
      if (section.querySelector?.('[data-message-author-role="assistant"]')) continue;
      const tools = Array.from(section.querySelectorAll?.(TOOL_MESSAGE_SELECTOR) || []);
      if (tools.length < TOOL_RUN_MIN_MESSAGES) continue;
      const stack = lowestCommonAncestor(tools, section);
      if (stack instanceof Element) stacks.add(stack);
    }
    return stacks;
  }

  function foldToolRuns() {
    if (!enabled) return 0;
    let folded = 0;
    for (const stack of toolRunStacks()) {
      if (!(stack instanceof Element)) continue;
      if (stack.querySelector?.('[data-testid="tool-approval-card"]')) continue;

      let run = [];
      let activeSummary = null;
      const flush = () => {
        if (run.length < TOOL_RUN_MIN_MESSAGES) {
          run = [];
          activeSummary = null;
          return;
        }

        if (activeSummary instanceof Element) {
          const previous = foldedToolRuns.get(activeSummary) || [];
          const expanded = activeSummary.getAttribute("aria-expanded") === "true";
          const newlyTracked = run.filter((wrapper) => !previous.includes(wrapper)).length;
          foldedToolRuns.set(activeSummary, run.slice());
          setToolRunExpanded(activeSummary, expanded);
          if (!expanded) stats.tool_run_messages_hidden += newlyTracked;
        } else {
          const summary = document.createElement("button");
          summary.setAttribute("type", "button");
          summary.setAttribute(TOOL_RUN_SUMMARY_ATTR, "1");
          summary.setAttribute("aria-expanded", "false");
          foldedToolRuns.set(summary, run.slice());
          setToolRunExpanded(summary, false);
          stack.insertBefore(summary, run[0]);
          stats.tool_runs_folded += 1;
          stats.tool_run_messages_hidden += run.length;
          folded += 1;
        }
        run = [];
        activeSummary = null;
      };

      for (const child of Array.from(stack.children || [])) {
        if (!(child instanceof Element)) continue;
        if (child.getAttribute(TOOL_RUN_SUMMARY_ATTR) === "1") {
          if (run.length) flush();
          activeSummary = child;
          continue;
        }
        if (isDirectToolWrapper(child)) {
          run.push(child);
          continue;
        }
        if (isToolRunBarrier(child)) flush();
      }
      flush();
    }
    return folded;
  }

  function toolRunSummaryFor(node) {
    let element = elementForNode(node);
    while (element) {
      if (element.getAttribute?.(TOOL_RUN_SUMMARY_ATTR) === "1") return element;
      element = element.parentElement;
    }
    return null;
  }

  function handleToolRunClick(event) {
    if (!enabled) return;
    const summary = toolRunSummaryFor(event?.target);
    if (!summary) return;
    const expanded = summary.getAttribute("aria-expanded") !== "true";
    setToolRunExpanded(summary, expanded);
    if (expanded) stats.tool_runs_expanded += 1;
  }

  function cleanupToolRuns() {
    for (const summary of document.querySelectorAll?.(`[${TOOL_RUN_SUMMARY_ATTR}="1"]`) || []) {
      if (!(summary instanceof Element)) continue;
      const wrappers = foldedToolRuns.get(summary) || [];
      for (const wrapper of wrappers) wrapper?.removeAttribute?.(TOOL_RUN_HIDDEN_ATTR);
      foldedToolRuns.delete(summary);
      summary.remove?.();
    }
    for (const wrapper of document.querySelectorAll?.(`[${TOOL_RUN_HIDDEN_ATTR}="1"]`) || []) {
      wrapper?.removeAttribute?.(TOOL_RUN_HIDDEN_ATTR);
    }
  }

  function rearmToolClusters() {
    if (!enabled || !toolClusterObserver) return;
    for (const cluster of observedToolClusters) {
      try {
        toolClusterObserver.unobserve(cluster);
        toolClusterObserver.observe(cluster);
      } catch (_) {}
    }
  }

  function suspendToolHiding(ms = TOOL_FIND_SUSPEND_MS) {
    const now = performance.now?.() || Date.now();
    toolFindSuspendUntil = Math.max(toolFindSuspendUntil, now + ms);
    clearAllToolClusters();
    if (toolFindResumeTimer != null) clearTimeout(toolFindResumeTimer);
    toolFindResumeTimer = setTimeout(() => {
      toolFindResumeTimer = null;
      if (!enabled) return;
      toolFindSuspendUntil = 0;
      rearmToolClusters();
    }, ms);
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
        [${TOOL_CLUSTER_ATTR}="1"] {
          content-visibility: hidden;
          contain-intrinsic-size: var(${TOOL_CLUSTER_INTRINSIC_VAR}, 600px);
        }
      }
      [${TOOL_RUN_HIDDEN_ATTR}="1"] {
        display: none !important;
      }
      [${TOOL_RUN_SUMMARY_ATTR}="1"] {
        width: fit-content;
        border: 0;
        background: transparent;
        padding: 2px 0;
        color: inherit;
        font: inherit;
        font-size: 0.8125rem;
        opacity: 0.68;
        cursor: pointer;
      }
      [${TOOL_RUN_SUMMARY_ATTR}="1"]:hover {
        opacity: 1;
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

    const toolCluster = toolClusterFor(element);
    if (toolCluster) unobserveToolCluster(toolCluster);

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

    let toolStructureChanged = false;
    for (const record of records) {
      processMutationNode(record.target);
      for (const node of record.addedNodes || []) {
        processMutationNode(node);
        toolStructureChanged ||= nodeHasToolMessage(node);
      }
      for (const node of record.removedNodes || []) {
        toolStructureChanged ||= nodeHasToolMessage(node);
        const element = elementForNode(node);
        if (element && observedBlocks.has(element)) {
          try { blockObserver?.unobserve?.(element); } catch (_) {}
          observedBlocks.delete(element);
        }
      }
    }

    if (toolStructureChanged) {
      stats.tool_discovery_batches += 1;
      discoverToolClusters();
      foldToolRuns();
    }

    scheduleSettledScan();
  }

  function scan() {
    if (!enabled) return 0;
    installStyle();
    installBlockObserver();
    installToolClusterObserver();
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

    discovered += discoverToolClusters();
    discovered += foldToolRuns();

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
    queueDiscoveryScan(false);
  }

  function forceDiscoveryScan() {
    maxDiscoveryTimer = null;
    if (!enabled || !discoveryPending) return;
    queueDiscoveryScan(true);
  }

  function queueDiscoveryScan(forced) {
    discoveryPending = false;
    if (quietTimer != null) {
      clearTimeout(quietTimer);
      quietTimer = null;
    }
    if (maxDiscoveryTimer != null) {
      clearTimeout(maxDiscoveryTimer);
      maxDiscoveryTimer = null;
    }
    if (forced) stats.forced_scans += 1;

    const run = () => {
      idleHandle = null;
      if (enabled) scan();
    };
    if (typeof requestIdleCallback === "function") {
      idleHandle = requestIdleCallback(run, { timeout: DISCOVERY_IDLE_TIMEOUT_MS });
    } else {
      idleHandle = setTimeout(run, 0);
    }
  }

  function scheduleSettledScan() {
    const firstPending = !discoveryPending;
    discoveryPending = true;
    lastMutationAt = performance.now?.() || Date.now();
    if (firstPending && maxDiscoveryTimer == null) {
      maxDiscoveryTimer = setTimeout(forceDiscoveryScan, MAX_DISCOVERY_LATENCY_MS);
    }
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
    const toolCluster = toolClusterFor(event?.target);
    if (toolCluster) unobserveToolCluster(toolCluster);
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
    const toolClusters = new Set();
    for (const node of [selection.anchorNode, selection.focusNode]) {
      const root = editableRootFor(node);
      if (root) roots.add(root);
      const cluster = toolClusterFor(node);
      if (cluster) toolClusters.add(cluster);
    }
    for (const root of roots) clearEditableRoot(root);
    for (const cluster of toolClusters) unobserveToolCluster(cluster);
  }

  function handleKeyDown(event) {
    if (!enabled) return;
    if ((event?.metaKey || event?.ctrlKey) && String(event?.key || "").toLowerCase() === "f") {
      suspendToolHiding();
    }
  }

  function handleBeforeMatch(event) {
    if (!enabled) return;
    const cluster = toolClusterFor(event?.target);
    if (cluster) suspendToolHiding();
  }

  function installListeners() {
    if (listenersInstalled || typeof document.addEventListener !== "function") return;
    document.addEventListener("focusin", handleFocusIn, true);
    document.addEventListener("focusout", handleFocusOut, true);
    document.addEventListener("selectionchange", handleSelectionChange, true);
    document.addEventListener("keydown", handleKeyDown, true);
    document.addEventListener("beforematch", handleBeforeMatch, true);
    document.addEventListener("click", handleToolRunClick, true);
    listenersInstalled = true;
  }

  function removeListeners() {
    if (!listenersInstalled || typeof document.removeEventListener !== "function") return;
    document.removeEventListener("focusin", handleFocusIn, true);
    document.removeEventListener("focusout", handleFocusOut, true);
    document.removeEventListener("selectionchange", handleSelectionChange, true);
    document.removeEventListener("keydown", handleKeyDown, true);
    document.removeEventListener("beforematch", handleBeforeMatch, true);
    document.removeEventListener("click", handleToolRunClick, true);
    listenersInstalled = false;
  }

  function cleanupContainment() {
    cleanupToolRuns();
    for (const viewer of document.querySelectorAll?.(`${VIEWER_SELECTOR}[${VIEWER_ATTR}="1"]`) || []) {
      clearViewer(viewer);
    }
    for (const block of document.querySelectorAll?.(`[${EDITABLE_BLOCK_ATTR}="1"]`) || []) {
      clearEditableBlock(block);
    }
    for (const cluster of Array.from(observedToolClusters)) {
      clearToolCluster(cluster);
      try { cluster.removeAttribute(TOOL_CLUSTER_OBSERVED_ATTR); } catch (_) {}
    }
  }

  function cancelScheduledWork() {
    if (quietTimer != null) {
      clearTimeout(quietTimer);
      quietTimer = null;
    }
    if (maxDiscoveryTimer != null) {
      clearTimeout(maxDiscoveryTimer);
      maxDiscoveryTimer = null;
    }
    if (idleHandle != null) {
      if (typeof cancelIdleCallback === "function") cancelIdleCallback(idleHandle);
      else clearTimeout(idleHandle);
      idleHandle = null;
    }
    if (toolFindResumeTimer != null) {
      clearTimeout(toolFindResumeTimer);
      toolFindResumeTimer = null;
    }
    toolFindSuspendUntil = 0;
    discoveryPending = false;
  }

  installStyle();
  installBlockObserver();
  installToolClusterObserver();
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
      toolClusterObserver?.disconnect?.();
      toolClusterObserver = null;
      cancelScheduledWork();
      removeListeners();
      cleanupContainment();
      pendingViewers.clear();
      observedRoots.clear();
      observedBlocks.clear();
      observedToolClusters.clear();
      styleElement?.remove?.();
      styleElement = null;
      return { enabled };
    },
    enable() {
      if (enabled) return { enabled };
      enabled = true;
      installStyle();
      installBlockObserver();
      installToolClusterObserver();
      startObserver();
      installListeners();
      scheduleSettledScan();
      return { enabled };
    },
    get enabled() { return enabled; },
  };

  publish(Object.freeze(api));
})();
