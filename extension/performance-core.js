// performance-core.js — small classic-script helpers for bounded browser work.
//
// Two capabilities:
//  - createCoalescedScheduler: throttle high-frequency MutationObserver-driven
//    callbacks to at most one run per minIntervalMs (default 400ms).
//  - createUiPressureMeter / classifyUiPressure: bounded, fixed-window rolling
//    aggregation of UI pressure signals (mutation callback rate, sampling/tick
//    rate, timer drift). Only per-window counters are kept — never a timeline.
//    Long Task and HTTP 429 samples stay bounded and informational; page-health
//    recovery consumes them conservatively and never treats 429 as a reload cue.
(function initBrowserPerformance(global) {
  "use strict";

  const DEFAULT_MUTATION_COALESCE_MS = 400;
  const DEFAULT_PERMISSION_COALESCE_MS = 150;
  const UI_PRESSURE_WINDOW_MS = 60000;
  const DEFAULT_TURN_MUTATION_SELECTOR = [
    '[data-message-author-role="user"]',
    '[data-message-author-role="assistant"]',
    '[data-testid^="conversation-turn-"]',
  ].join(", ");
  const DEFAULT_IGNORED_TURN_MUTATION_SELECTOR = [
    '[class*="group/tool-message"]',
    '#h2w-page-hud',
  ].join(", ");
  const DEFAULT_MESSAGE_SAMPLE_CHARS = 64 * 1024;
  const DEFAULT_MESSAGE_TAIL_CHARS = 16 * 1024;
  const DEFAULT_IGNORED_MESSAGE_TEXT_SELECTOR = [
    '[class*="group/tool-message"]',
    '#h2w-page-hud',
    'script',
    'style',
    'noscript',
    'template',
  ].join(", ");

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

  const DEFAULT_PAGE_HEALTH_POLICY = Object.freeze({
    minSampleWindowMs: 15000,
    sustainedPressureMs: 30000,
    activeTurnStallMs: 30000,
    memoryQuiescentMs: 30000,
    memoryWarningBytes: 1024 * 1024 * 1024,
    memoryCriticalBytes: 1536 * 1024 * 1024,
    memoryWarningRatio: 0.60,
    memoryCriticalRatio: 0.78,
    severeLongTaskMs: 2000,
    severeLongTaskCount: 4,
    rateLimitBackoffBaseMs: 30000,
    rateLimitBackoffMaxMs: 120000,
    backgroundEscalationDelayMs: 30000,
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

  function asElement(node) {
    if (!node) return null;
    if (node.nodeType === 1) return node;
    return node.parentElement || null;
  }

  function matchesOrContains(element, selector) {
    if (!element || !selector) return false;
    try {
      if (typeof element.matches === "function" && element.matches(selector)) return true;
      if (typeof element.querySelector === "function" && element.querySelector(selector)) return true;
    } catch (_) {}
    return false;
  }

  function insideSelector(element, selector) {
    if (!element || !selector || typeof element.closest !== "function") return false;
    try { return Boolean(element.closest(selector)); } catch (_) { return false; }
  }

  /**
   * Decide whether structural mutation records can invalidate the cached latest
   * ChatGPT user/assistant turn. Mutations wholly inside tool-card/HUD subtrees
   * are ignored so expanding tool details does not force a full conversation
   * rediscovery. The caller may still schedule its normal coalesced tick.
   */
  function mutationTouchesConversationTurns(records, {
    turnSelector = DEFAULT_TURN_MUTATION_SELECTOR,
    ignoredSelector = DEFAULT_IGNORED_TURN_MUTATION_SELECTOR,
  } = {}) {
    for (const record of records || []) {
      const changed = [
        ...Array.from(record?.addedNodes || []),
        ...Array.from(record?.removedNodes || []),
      ];
      let sawUnignoredChangedNode = false;
      for (const node of changed) {
        const element = asElement(node);
        if (insideSelector(element, ignoredSelector) || matchesOrContains(element, ignoredSelector)) continue;
        sawUnignoredChangedNode = true;
        if (insideSelector(element, turnSelector) || matchesOrContains(element, turnSelector)) return true;
      }
      if (changed.length > 0 && !sawUnignoredChangedNode) continue;

      const target = asElement(record?.target);
      if (insideSelector(target, ignoredSelector)) continue;
      if (insideSelector(target, turnSelector) || matchesOrContains(target, turnSelector)) return true;
    }
    return false;
  }

  /**
   * Read a bounded text sample without forcing layout. The complete text length
   * is still counted so streaming growth remains observable after the sample is
   * truncated. Tool/HUD/script subtrees are rejected before their text is read.
   */
  function sampleBoundedMessageText(root, {
    maxChars = DEFAULT_MESSAGE_SAMPLE_CHARS,
    tailChars = DEFAULT_MESSAGE_TAIL_CHARS,
    ignoredSelector = DEFAULT_IGNORED_MESSAGE_TEXT_SELECTOR,
  } = {}) {
    if (!root) {
      return { text: "", total_chars: 0, truncated: false, text_nodes: 0, skipped_subtrees: 0 };
    }
    const limit = Math.max(1024, Math.floor(Number(maxChars) || DEFAULT_MESSAGE_SAMPLE_CHARS));
    const tailLimit = Math.min(
      Math.floor(limit / 2),
      Math.max(0, Math.floor(Number(tailChars) || DEFAULT_MESSAGE_TAIL_CHARS)),
    );
    const marker = "\n…\n";
    const stack = [root];
    let prefix = "";
    let tail = "";
    let totalChars = 0;
    let textNodes = 0;
    let skippedSubtrees = 0;

    while (stack.length > 0) {
      const node = stack.pop();
      if (!node) continue;
      if (node.nodeType === 3) {
        const value = String(node.nodeValue || "");
        if (!value) continue;
        textNodes += 1;
        totalChars += value.length;
        if (prefix.length < limit) prefix += value.slice(0, limit - prefix.length);
        if (tailLimit > 0) {
          tail += value;
          if (tail.length > tailLimit * 2) tail = tail.slice(-tailLimit);
        }
        continue;
      }
      if (node.nodeType !== 1 && node !== root) continue;
      const element = node.nodeType === 1 ? node : null;
      if (element && insideSelector(element, ignoredSelector)) {
        skippedSubtrees += 1;
        continue;
      }
      const children = node.childNodes;
      if (!children) continue;
      for (let index = children.length - 1; index >= 0; index -= 1) stack.push(children[index]);
    }

    const truncated = totalChars > limit;
    if (!truncated) {
      return {
        text: prefix.trim(),
        total_chars: totalChars,
        truncated: false,
        text_nodes: textNodes,
        skipped_subtrees: skippedSubtrees,
      };
    }
    const headLimit = Math.max(0, limit - tailLimit - marker.length);
    return {
      text: `${prefix.slice(0, headLimit)}${marker}${tail.slice(-tailLimit)}`.trim(),
      total_chars: totalChars,
      truncated: true,
      text_nodes: textNodes,
      skipped_subtrees: skippedSubtrees,
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
    let http429Count = 0;
    let lastHttp429At = 0;

    const openWindow = (current) => {
      if (!windowStart || current - windowStart >= interval) {
        windowStart = current;
        mutationCount = 0;
        tickCount = 0;
        driftCount = 0;
        maxTimerDriftMs = 0;
        longTaskCount = 0;
        maxLongTaskMs = 0;
        http429Count = 0;
        lastHttp429At = 0;
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
        http_429_count: http429Count,
        last_http_429_at: lastHttp429At || null,
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
      recordHttpStatus(status, at) {
        if (Number(status) !== 429) return;
        const current = Number(at) || now();
        openWindow(current);
        http429Count += 1;
        lastHttp429At = current;
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
        http429Count = 0;
        lastHttp429At = 0;
      },
    });
  }

  function classifyMemoryPressure(memory = {}, policy = DEFAULT_PAGE_HEALTH_POLICY) {
    const usedBytes = Math.max(0, Number(memory?.usedJSHeapSize || memory?.used_bytes || 0) || 0);
    const limitBytes = Math.max(0, Number(memory?.jsHeapSizeLimit || memory?.limit_bytes || 0) || 0);
    const ratio = limitBytes > 0 ? usedBytes / limitBytes : null;
    let level = "healthy";
    const reasons = [];
    if (usedBytes >= policy.memoryCriticalBytes
      || (ratio != null && ratio >= policy.memoryCriticalRatio)) {
      level = "critical";
      reasons.push(`heap_bytes:${Math.round(usedBytes)}`);
      if (ratio != null) reasons.push(`heap_ratio:${Math.round(ratio * 1000) / 1000}`);
    } else if (usedBytes >= policy.memoryWarningBytes
      || (ratio != null && ratio >= policy.memoryWarningRatio)) {
      level = "warning";
      reasons.push(`heap_bytes:${Math.round(usedBytes)}`);
      if (ratio != null) reasons.push(`heap_ratio:${Math.round(ratio * 1000) / 1000}`);
    }
    return {
      level,
      reasons,
      used_bytes: Math.round(usedBytes),
      limit_bytes: Math.round(limitBytes),
      ratio: ratio == null ? null : Math.round(ratio * 10000) / 10000,
    };
  }

  function rateLimitBackoffMs(attempt, policy = DEFAULT_PAGE_HEALTH_POLICY) {
    const n = Math.max(1, Math.floor(Number(attempt) || 1));
    return Math.min(
      policy.rateLimitBackoffMaxMs,
      policy.rateLimitBackoffBaseMs * (2 ** Math.min(8, n - 1)),
    );
  }

  /**
   * Pure page-health decision. HTTP 429 is always backoff-only: it never
   * recommends reload/retry. Render-pressure reloads require an active stalled
   * turn, a mature high-pressure sample, and server confirmation that the
   * assistant turn has already settled. Critical JS heap pressure can request
   * an idle maintenance reload only after a quiescent interval.
   */
  function classifyPageHealth(input = {}, policy = DEFAULT_PAGE_HEALTH_POLICY) {
    const now = Number(input?.now) || Date.now();
    const backoffUntil = Math.max(0, Number(input?.backoffUntil || 0) || 0);
    const memory = classifyMemoryPressure(input?.memory || {}, policy);
    const ui = input?.ui || {};
    const uiReady = Number(ui?.window_elapsed_ms || 0) >= policy.minSampleWindowMs;
    const uiHigh = uiReady && ui?.level === "high";
    const longTaskSevere = Number(ui?.long_task_max_ms || 0) >= policy.severeLongTaskMs
      || Number(ui?.long_task_count || 0) >= policy.severeLongTaskCount;
    const activeTurn = input?.activeTurn === true;
    const serverSettled = input?.serverSettled === true;
    const stallMs = Math.max(0, Number(input?.stallMs || 0) || 0);
    const quiescentMs = Math.max(0, Number(input?.quiescentMs || 0) || 0);
    const highSince = Math.max(0, Number(input?.highSince || 0) || 0);

    if (backoffUntil > now) {
      return {
        state: "rate_limited",
        action: "backoff",
        reason: "http_429_backoff",
        candidate: false,
        sustained: false,
        backoff_until: backoffUntil,
        memory,
        ui_high: uiHigh,
        long_task_severe: longTaskSevere,
      };
    }

    const renderCandidate = activeTurn
      && serverSettled
      && stallMs >= policy.activeTurnStallMs
      && uiHigh;
    const memoryCandidate = memory.level === "critical"
      && quiescentMs >= policy.memoryQuiescentMs;
    const candidate = renderCandidate || memoryCandidate;
    const sustained = Boolean(candidate && highSince && now - highSince >= policy.sustainedPressureMs);
    const warning = uiHigh || longTaskSevere || memory.level !== "healthy";
    const reason = renderCandidate
      ? "render_stall"
      : memoryCandidate
        ? "memory_pressure"
        : longTaskSevere
          ? "long_task_pressure"
          : memory.level !== "healthy"
            ? "memory_warning"
            : uiHigh
              ? "ui_pressure"
              : null;

    return {
      state: sustained ? "reload_recommended" : candidate ? "degraded" : warning ? "warning" : "healthy",
      action: sustained ? "reload" : candidate ? "observe" : "none",
      reason,
      candidate,
      sustained,
      backoff_until: null,
      memory,
      ui_high: uiHigh,
      long_task_severe: longTaskSevere,
      active_turn: activeTurn,
      server_settled: serverSettled,
      stall_ms: Math.round(stallMs),
      quiescent_ms: Math.round(quiescentMs),
    };
  }

  global.H2W_BROWSER_PERFORMANCE = Object.freeze({
    DEFAULT_MUTATION_COALESCE_MS,
    DEFAULT_PERMISSION_COALESCE_MS,
    DEFAULT_TURN_MUTATION_SELECTOR,
    DEFAULT_IGNORED_TURN_MUTATION_SELECTOR,
    DEFAULT_MESSAGE_SAMPLE_CHARS,
    DEFAULT_MESSAGE_TAIL_CHARS,
    DEFAULT_IGNORED_MESSAGE_TEXT_SELECTOR,
    UI_PRESSURE_WINDOW_MS,
    DEFAULT_UI_PRESSURE_POLICY,
    DEFAULT_PAGE_HEALTH_POLICY,
    createCoalescedScheduler,
    mutationTouchesConversationTurns,
    sampleBoundedMessageText,
    createUiPressureMeter,
    classifyUiPressure,
    classifyMemoryPressure,
    classifyPageHealth,
    rateLimitBackoffMs,
  });
})(typeof globalThis !== "undefined" ? globalThis : window);
