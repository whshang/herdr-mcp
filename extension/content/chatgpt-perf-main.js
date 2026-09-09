// chatgpt-perf-main.js — MAIN-world, document-start ChatGPT render hot-path patch.
//
// CodeMirror 6 mounts an uninitialized editor root before assigning .cm-editor.
// Holding only that exact root detached until the next task lets many editors
// initialize off-document and amortizes page-wide style invalidation. Any
// fingerprint mismatch passes through unchanged. If the known userscript owns
// the same hook, Herdr yields instead of stacking two interceptors.
(() => {
  "use strict";

  const VERSION = "1";
  const API_NAME = "__HERDR_CHATGPT_PERF__";
  const EXTERNAL_API = "__CHATGPT_CM_PERF_FIX__";
  if (window[API_NAME]) return;

  const nodePrototype = window.Node?.prototype;
  const descriptor = nodePrototype
    ? Object.getOwnPropertyDescriptor(nodePrototype, "appendChild")
    : null;
  const nativeAppendChild = descriptor?.value;

  const stats = {
    intercepted: 0,
    mounted: 0,
    skipped: 0,
    batches: 0,
    last_batch_size: 0,
    last_batch_insert_ms: 0,
  };

  const externalActive = () => Boolean(window[EXTERNAL_API]);
  const publish = (value) => {
    try {
      Object.defineProperty(window, API_NAME, {
        configurable: false,
        enumerable: false,
        writable: false,
        value,
      });
    } catch (_) {}
  };

  if (typeof nativeAppendChild !== "function" || !window.Element) {
    publish(Object.freeze({ version: VERSION, enabled: false, install_error: "appendChild-unavailable", stats }));
    return;
  }
  if (externalActive()) {
    publish(Object.freeze({ version: VERSION, enabled: false, external: true, stats }));
    return;
  }

  const pending = [];
  const pendingNodes = new WeakSet();
  let enabled = true;
  let flushing = false;
  let timer = null;

  function isCodeMirrorMount(parent, child) {
    if (!enabled || flushing || externalActive()) return false;
    if (!(parent instanceof Element) || !(child instanceof Element)) return false;
    if (!parent.isConnected || child.isConnected || child.ownerDocument !== document) return false;
    if (child.classList.contains("cm-editor")) return false;
    const announced = child.firstElementChild;
    const scroller = announced?.nextElementSibling;
    return child.childElementCount === 2
      && announced?.classList.contains("cm-announced")
      && scroller?.classList.contains("cm-scroller")
      && scroller.nextElementSibling === null;
  }

  function flush() {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    if (!pending.length) return 0;
    const batch = pending.splice(0, pending.length);
    const started = performance.now();
    let mounted = 0;
    flushing = true;
    try {
      for (const { parent, child } of batch) {
        pendingNodes.delete(child);
        if (!parent.isConnected || child.isConnected || child.parentNode) {
          stats.skipped += 1;
          continue;
        }
        nativeAppendChild.call(parent, child);
        mounted += 1;
      }
    } finally {
      flushing = false;
    }
    stats.mounted += mounted;
    stats.batches += 1;
    stats.last_batch_size = batch.length;
    stats.last_batch_insert_ms = Math.round((performance.now() - started) * 100) / 100;
    return mounted;
  }

  function schedule() {
    if (timer !== null) return;
    timer = setTimeout(flush, 0);
  }

  function patchedAppendChild(child) {
    // A userscript may load after this document-start script. In that case its
    // wrapper can still call ours as the captured native appendChild. Always
    // pass through immediately so only one implementation owns CM batching.
    if (externalActive()) return nativeAppendChild.call(this, child);
    if (isCodeMirrorMount(this, child)) {
      if (!pendingNodes.has(child)) {
        pendingNodes.add(child);
        pending.push({ parent: this, child });
        stats.intercepted += 1;
      }
      schedule();
      return child;
    }
    return nativeAppendChild.call(this, child);
  }

  try {
    Object.defineProperty(patchedAppendChild, "name", { configurable: true, value: "appendChild" });
    Object.defineProperty(nodePrototype, "appendChild", { ...descriptor, value: patchedAppendChild });
  } catch (_) {
    publish(Object.freeze({ version: VERSION, enabled: false, install_error: "defineProperty-failed", stats }));
    return;
  }

  const api = {
    version: VERSION,
    stats,
    flush,
    disable() {
      enabled = false;
      flush();
      return { enabled };
    },
    enable() {
      enabled = true;
      return { enabled };
    },
    get enabled() { return enabled; },
    get external() { return externalActive(); },
    get queued() { return pending.length; },
  };
  publish(Object.freeze(api));
})();
