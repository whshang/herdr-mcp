// performance-core.js — small classic-script helpers for bounded browser work.
//
// Two capabilities:
//  - createCoalescedScheduler: throttle high-frequency MutationObserver-driven
//    callbacks to at most one run per minIntervalMs (default 400ms).
//  - createUiPressureMeter / classifyUiPressure: bounded, fixed-window rolling
//    aggregation of UI pressure signals (mutation callback rate, sampling/tick
//    rate, timer drift). Only per-window counters are kept — never a timeline.
//    Long Task samples are optional and informational only: they never affect
//    the healthy/warning/high classification, which stays purely aggregate.
(function initBrowserPerformance(global) {
  "use strict";

  const DEFAULT_MUTATION_COALESCE_MS = 400;
  const DEFAULT_PERMISSION_COALESCE_MS = 150;
  const UI_PRESSURE_WINDOW_MS = 60000;

  const DEFAULT_UI_PRESSURE_POLICY = Object.freeze({
    windowMs: UI_PRESSURE_WINDOW_MS,
    // MutationObserver callbacks per minute sustained.
    warningMutationRatePerMin: 600,
    highMutationRatePerMin: 2400,
    // Sampling/tick executions per minute sustained.
    warningTickRatePerMin: 300,
    highTickRatePerMin: 900,
    // Max observed setTimeout drift in ms (throttling/contention fallback).
    warningMaxTimerDriftMs: 1500,
    highMaxTimerDriftMs: 5000,
  });

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

  /**
   * Pure three-band UI pressure classifier. Input carries per-minute rates and
   * max timer drift; any "high" signal raises the level to "high", otherwise
   * any "warning". Echoes the measured inputs and the contributing reasons.
   */
  function classifyUiPressure(input = {}, policy = DEFAULT_UI_PRESSURE_POLICY) {
    const mutationRatePerMin = Math.max(0, Number(input?.mutationRatePerMin) || 0);
    const ticksPerMin = Math.max(0, Number(input?.ticksPerMin) || 0);
    const maxTimerDriftMs = Math.max(0, Number(input?.maxTimerDriftMs) || 0);
    let level = "healthy";
    const reasons = [];
    const consider = (measured, warning, high, label) => {
      if (measured >= high) {
        if (level !== "high") level = "high";
        reasons.push(`${label}:${Math.round(measured)}`);
      } else if (measured >= warning && level !== "high") {
        if (level === "healthy") level = "warning";
        reasons.push(`${label}:${Math.round(measured)}`);
      }
    };
    consider(mutationRatePerMin, policy.warningMutationRatePerMin, policy.highMutationRatePerMin, "mutation_rate_min");
    consider(ticksPerMin, policy.warningTickRatePerMin, policy.highTickRatePerMin, "tick_rate_min");
    consider(maxTimerDriftMs, policy.warningMaxTimerDriftMs, policy.highMaxTimerDriftMs, "timer_drift_ms");
    return {
      level,
      reasons,
      mutation_rate_per_min: Math.round(mutationRatePerMin * 10) / 10,
      ticks_per_min: Math.round(ticksPerMin * 10) / 10,
      max_timer_drift_ms: Math.round(maxTimerDriftMs),
    };
  }

  /**
   * Fixed-window rolling meter. Counters accumulate within windowMs and reset
   * when the window elapses, so memory stays O(1) — no timeline is retained.
   * Long-task samples are recorded but are informational only: they appear in
   * evaluate() output without affecting the classification level.
   */
  function createUiPressureMeter({
    windowMs = UI_PRESSURE_WINDOW_MS,
    now = () => Date.now(),
  } = {}) {
    const interval = Math.max(1000, Number(windowMs) || UI_PRESSURE_WINDOW_MS);
    let windowStart = 0;
    let mutationCount = 0;
    let tickCount = 0;
    let driftCount = 0;
    let maxTimerDriftMs = 0;
    let longTaskCount = 0;
    let maxLongTaskMs = 0;

    const openWindow = (current) => {
      if (!windowStart || current - windowStart >= interval) {
        windowStart = current;
        mutationCount = 0;
        tickCount = 0;
        driftCount = 0;
        maxTimerDriftMs = 0;
        longTaskCount = 0;
        maxLongTaskMs = 0;
      }
    };

    const evaluate = (at) => {
      const current = Number(at) || now();
      openWindow(current);
      const elapsedMs = Math.max(1000, current - windowStart);
      const scale = 60000 / elapsedMs;
      return {
        ...classifyUiPressure({
          mutationRatePerMin: mutationCount * scale,
          ticksPerMin: tickCount * scale,
          maxTimerDriftMs,
        }),
        window_ms: interval,
        window_start: windowStart,
        window_elapsed_ms: Math.round(current - windowStart),
        drift_samples: driftCount,
        long_task_count: longTaskCount,
        long_task_max_ms: Math.round(maxLongTaskMs),
        sampled_at: current,
      };
    };

    return Object.freeze({
      recordMutation(count = 1, at) {
        const current = Number(at) || now();
        openWindow(current);
        mutationCount += Math.max(1, Number(count) || 1);
      },
      recordTick(count = 1, at) {
        const current = Number(at) || now();
        openWindow(current);
        tickCount += Math.max(1, Number(count) || 1);
      },
      recordTimerDrift(driftMs, at) {
        const value = Math.max(0, Number(driftMs) || 0);
        if (!value) return;
        openWindow(Number(at) || now());
        driftCount += 1;
        if (value > maxTimerDriftMs) maxTimerDriftMs = value;
      },
      recordLongTask(durationMs, at) {
        const value = Math.max(0, Number(durationMs) || 0);
        if (!value) return;
        openWindow(Number(at) || now());
        longTaskCount += 1;
        if (value > maxLongTaskMs) maxLongTaskMs = value;
      },
      evaluate,
      reset() {
        windowStart = 0;
        mutationCount = 0;
        tickCount = 0;
        driftCount = 0;
        maxTimerDriftMs = 0;
        longTaskCount = 0;
        maxLongTaskMs = 0;
      },
    });
  }

  global.H2W_BROWSER_PERFORMANCE = Object.freeze({
    DEFAULT_MUTATION_COALESCE_MS,
    DEFAULT_PERMISSION_COALESCE_MS,
    UI_PRESSURE_WINDOW_MS,
    DEFAULT_UI_PRESSURE_POLICY,
    createCoalescedScheduler,
    createUiPressureMeter,
    classifyUiPressure,
  });
})(typeof globalThis !== "undefined" ? globalThis : window);
