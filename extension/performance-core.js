// performance-core.js — small classic-script helpers for bounded browser work.
(function initBrowserPerformance(global) {
  "use strict";

  const DEFAULT_MUTATION_COALESCE_MS = 400;
  const DEFAULT_PERMISSION_COALESCE_MS = 150;

  function createCoalescedScheduler(run, {
    minIntervalMs = DEFAULT_MUTATION_COALESCE_MS,
    now = () => Date.now(),
    setTimer = (fn, delay) => setTimeout(fn, delay),
    clearTimer = (id) => clearTimeout(id),
    isSuspended = () => false,
  } = {}) {
    if (typeof run !== "function") throw new TypeError("run must be a function");
    const interval = Math.max(0, Number(minIntervalMs) || 0);
    let timer = null;
    let lastRunAt = 0;

    const fire = () => {
      timer = null;
      if (isSuspended()) return false;
      lastRunAt = Number(now()) || Date.now();
      run();
      return true;
    };

    return {
      schedule() {
        if (timer !== null || isSuspended()) return false;
        const current = Number(now()) || Date.now();
        const elapsed = lastRunAt > 0 ? Math.max(0, current - lastRunAt) : interval;
        const delay = Math.max(0, interval - elapsed);
        timer = setTimer(fire, delay);
        return true;
      },
      flush() {
        if (timer !== null) {
          clearTimer(timer);
          timer = null;
        }
        return fire();
      },
      cancel() {
        if (timer !== null) clearTimer(timer);
        timer = null;
      },
      pending() { return timer !== null; },
    };
  }

  global.H2W_BROWSER_PERFORMANCE = Object.freeze({
    DEFAULT_MUTATION_COALESCE_MS,
    DEFAULT_PERMISSION_COALESCE_MS,
    createCoalescedScheduler,
  });
})(typeof globalThis !== "undefined" ? globalThis : window);
