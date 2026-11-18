// Hidden-tab idle timer suspension (#1039): idle hidden AI tabs must stop the
// 1s route-watch and 800ms turn-watch intervals, and any wake signal
// (visibility, SPA history route, background command) must re-arm them.
//
// The harness evaluates the real suspension block and the real route watch
// sliced from extension/content/wake.js on top of the real performance-core.js,
// with a fake clock and fake interval timers at the browser boundary.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const wakeSource = readFileSync(new URL("../extension/content/wake.js", import.meta.url), "utf8");
const performanceSource = readFileSync(new URL("../extension/performance-core.js", import.meta.url), "utf8");

function sliceSource(startMarker, endMarker, what) {
  const start = wakeSource.indexOf(startMarker);
  const end = wakeSource.indexOf(endMarker, start);
  assert.ok(start >= 0 && end > start, `${what} must remain extractable`);
  return wakeSource.slice(start, end);
}

const suspensionSource = sliceSource(
  "  // ---- Hidden-tab idle timer suspension (#1039) ----",
  "  // ---- Message listener ----",
  "hidden-tab idle timer suspension",
);
const routeWatchSource = sliceSource(
  "  function startConversationRouteWatch()",
  "\n  function assistantSignature(",
  "conversation route watch",
);

const KEY = "https://chatgpt.com/c/idle-1039";
const WORKER_KEY = "https://chatgpt.com/c/worker-1039";

function fakeTimers() {
  let nextId = 1;
  const active = new Map();
  return {
    setInterval(fn, ms) { const id = nextId++; active.set(id, { fn, ms }); return id; },
    clearInterval(id) { active.delete(id); },
    periods(ms) { return [...active.values()].filter((t) => t.ms === ms).length; },
    count() { return active.size; },
    // One period elapses for every armed interval: returns the wake-ups.
    elapse() {
      const due = [...active.entries()];
      let wakeups = 0;
      for (const [id, timer] of due) {
        if (!active.has(id)) continue;
        wakeups += 1;
        timer.fn();
      }
      return wakeups;
    },
  };
}

function eventTarget(extra = {}) {
  const listeners = new Map();
  return {
    ...extra,
    addEventListener(type, fn) {
      if (!listeners.has(type)) listeners.set(type, []);
      listeners.get(type).push(fn);
    },
    dispatch(type) { for (const fn of listeners.get(type) || []) fn({ type }); },
  };
}

function makeHarness({
  hidden = true,
  convKey = KEY,
  registeredConvKey = KEY,
  submitAt = 0,
  pendingRefresh = null,
  assignments = [],
  probeState = null,
} = {}) {
  const clock = { now: 1_000_000 };
  const timers = fakeTimers();
  const perfContext = vm.createContext({
    console,
    Math,
    setInterval: timers.setInterval,
    clearInterval: timers.clearInterval,
    setTimeout,
    clearTimeout,
  });
  new vm.Script(performanceSource, { filename: "performance-core.js" }).runInContext(perfContext);
  const document = eventTarget({ hidden });
  const window = eventTarget();
  const messageListeners = [];
  const chrome = { runtime: { onMessage: { addListener: (fn) => messageListeners.push(fn) } } };
  const calls = { observe: 0, refresh: 0, registrations: [], turnTicks: 0, hiddenCaptures: 0, resumes: 0 };
  const accepted = new Map(assignments);
  const state = {
    convKey,
    pendingRefresh,
    probeState,
    health: { last_user_submit_at: submitAt },
  };
  const api = new Function(
    "Date", "document", "window", "chrome", "setInterval", "BROWSER_PERFORMANCE", "ADAPTER",
    "acceptedDispatchAssignments", "registrationInFlight", "state", "calls", "initialRegisteredConvKey",
    "registerCurrentConversation", "observeBrowserResultSettlement",
    "maybeRefreshBrowserPendingDispatchAssignment",
    `
    let registeredConvKey = initialRegisteredConvKey;
    let registeredConversationBound = false;
    let registeredHerdrAppKeyword = null;
    const latestTurnForRole = () => null;
    const ensureQueuedInsertButton = () => {};
    const permObs = null;
    const permScheduler = null;
    const permissionTryClick = () => {};
    let browserPendingDispatchRefresh;
    let browserResultProbeState;
    let conversationHealth;
    const sync = () => {
      browserPendingDispatchRefresh = state.pendingRefresh;
      browserResultProbeState = state.probeState;
      conversationHealth = state.health;
    };
    sync();
    ${suspensionSource}
    ${routeWatchSource}
    const wrap = (fn) => (...args) => { sync(); return fn(...args); };
    return {
      startRouteWatch: wrap(startConversationRouteWatch),
      mountTurnWatch: wrap(() => mountHiddenIdleInterval(() => {
        if (document.hidden) { calls.hiddenCaptures += 1; return; }
        calls.turnTicks += 1;
      }, 800, { onResume: () => { calls.resumes += 1; } })),
      commitRegistration(key) { registeredConvKey = key; },
      sync,
    };
    `,
  )(
    { now: () => clock.now },
    document,
    window,
    chrome,
    timers.setInterval,
    perfContext.H2W_BROWSER_PERFORMANCE,
    { name: "chatgpt", getConversationKey: () => state.convKey },
    accepted,
    new Map(),
    state,
    calls,
    registeredConvKey,
    (reason) => { calls.registrations.push(reason); return new Promise(() => {}); },
    async () => { calls.observe += 1; },
    () => { calls.refresh += 1; },
  );
  api.startRouteWatch();
  api.mountTurnWatch();
  return {
    api,
    timers,
    calls,
    state,
    accepted,
    document,
    window,
    advance(ms) { clock.now += ms; },
    elapse() { api.sync(); return timers.elapse(); },
    setHidden(value) {
      document.hidden = value;
      document.dispatch("visibilitychange");
    },
    deliver(msg) { for (const fn of messageListeners) fn(msg, {}, () => {}); },
  };
}

test("user keeps idle background AI tabs asleep | Given a hidden registered tab with no dispatch, refresh, submit anchor, or route drift | When one poll period elapses | Then both the 1s and 800ms intervals are cleared and later periods cause zero wake-ups", () => {
  const h = makeHarness();
  assert.equal(h.timers.periods(1000), 1);
  assert.equal(h.timers.periods(800), 1);
  h.elapse();
  assert.equal(h.timers.count(), 0, "idle hidden tab must hold no interval timers");
  assert.equal(h.calls.observe, 0, "a suspending tick does no settlement work");
  assert.equal(h.calls.hiddenCaptures, 0);
  for (let i = 0; i < 5; i += 1) assert.equal(h.elapse(), 0);
});

test("user returns to a suspended tab without delay | Given an idle hidden tab whose intervals were suspended | When the tab becomes visible | Then both intervals re-arm and the next period runs the route poll and the turn tick", () => {
  const h = makeHarness();
  h.elapse();
  assert.equal(h.timers.count(), 0);
  h.setHidden(false);
  assert.equal(h.timers.periods(1000), 1);
  assert.equal(h.timers.periods(800), 1);
  assert.equal(h.calls.resumes, 1, "a resumed interval restarts its drift baseline");
  assert.equal(h.elapse(), 2);
  assert.equal(h.calls.observe, 1);
  assert.equal(h.calls.refresh, 1);
  assert.equal(h.calls.turnTicks, 1);
  // Visible tabs never suspend.
  for (let i = 0; i < 3; i += 1) assert.equal(h.elapse(), 2);
});

test("user dispatches to a suspended background tab | Given an idle hidden tab whose intervals were suspended | When the background sends a dispatch command | Then the intervals re-arm and stay armed through the activity grace before suspending again", () => {
  const h = makeHarness();
  h.elapse();
  assert.equal(h.timers.count(), 0);
  h.deliver({ type: "h2w_browser_actuation", command: {} });
  assert.equal(h.timers.count(), 2);
  h.advance(1000);
  assert.equal(h.elapse(), 2);
  assert.equal(h.calls.observe, 1);
  assert.equal(h.calls.hiddenCaptures, 1);
  h.advance(15_000);
  h.elapse();
  assert.equal(h.timers.count(), 0, "with no follow-up work the tab suspends again after the grace");
});

test("user navigates a suspended tab by SPA history | Given an idle hidden tab whose intervals were suspended | When popstate or hashchange fires | Then the intervals re-arm", () => {
  for (const type of ["popstate", "hashchange"]) {
    const h = makeHarness();
    h.elapse();
    assert.equal(h.timers.count(), 0);
    h.window.dispatch(type);
    assert.equal(h.timers.count(), 2, `${type} must re-arm`);
  }
});

test("user still gets a hidden dispatch result | Given a hidden tab holding an accepted but unsettled browser dispatch | When poll periods elapse | Then the route poll keeps observing settlement until the result is reported, then suspends", () => {
  const assignment = { dispatchId: "bd_x", reportedAssistantRef: null };
  const h = makeHarness({ assignments: [["br_worker", assignment]] });
  for (let i = 0; i < 4; i += 1) {
    h.advance(1000);
    assert.equal(h.elapse(), 2);
  }
  assert.equal(h.calls.observe, 4);
  h.accepted.set("br_worker", { ...assignment, reportedAssistantRef: "assistant-1" });
  h.elapse();
  assert.equal(h.timers.count(), 0);
});

test("user is not pinned awake by a dead result probe | Given a hidden tab whose only unsettled dispatch exhausted its result rejections | When a poll period elapses | Then the intervals suspend", () => {
  const assignment = { dispatchId: "bd_x", reportedAssistantRef: null };
  const h = makeHarness({
    assignments: [["br_worker", assignment]],
    probeState: { pending: assignment, rejected: 3 },
  });
  h.elapse();
  assert.equal(h.timers.count(), 0);
});

test("user still binds a hidden session.create worker | Given a hidden worker tab with a pending dispatch refresh | When poll periods elapse | Then the refresh keeps running until it clears, then the intervals suspend", () => {
  const h = makeHarness({ pendingRefresh: { reservationRef: "bsr_x" } });
  for (let i = 0; i < 3; i += 1) {
    h.advance(1000);
    assert.equal(h.elapse(), 2);
  }
  assert.equal(h.calls.refresh, 3);
  h.state.pendingRefresh = null;
  h.elapse();
  assert.equal(h.timers.count(), 0);
});

test("user still captures a hidden first turn | Given a hidden tab with a fresh pending-submit anchor | When poll periods elapse within 30s | Then the 800ms turn watch keeps sampling, and suspends once the anchor goes stale", () => {
  const h = makeHarness({ submitAt: 1_000_000 });
  for (let i = 0; i < 3; i += 1) {
    h.advance(800);
    assert.equal(h.elapse(), 2);
  }
  assert.equal(h.calls.hiddenCaptures, 3);
  h.advance(30_000);
  h.elapse();
  assert.equal(h.timers.count(), 0);
});

test("user still re-registers a hidden worker route | Given a hidden worker tab whose route moved to a new conversation without a history event | When poll periods elapse | Then the hidden poll re-registers and the intervals stay armed until the new key is registered", () => {
  const h = makeHarness({ convKey: WORKER_KEY, registeredConvKey: KEY });
  h.advance(1000);
  assert.equal(h.elapse(), 2);
  assert.deepEqual(h.calls.registrations, ["poll-hidden"]);
  h.advance(1000);
  assert.equal(h.elapse(), 2, "route drift keeps the poll alive");
  h.api.commitRegistration(WORKER_KEY);
  h.elapse();
  assert.equal(h.timers.count(), 0);
});

test("user sees no early tick from a re-arm | Given a suspendable interval that suspended itself | When it is re-armed repeatedly | Then it runs nothing synchronously, holds one timer, and resumes once", () => {
  const timers = fakeTimers();
  const context = vm.createContext({ console, Math, setInterval: timers.setInterval, clearInterval: timers.clearInterval });
  new vm.Script(performanceSource, { filename: "performance-core.js" }).runInContext(context);
  let keep = false;
  let runs = 0;
  let resumes = 0;
  const interval = context.H2W_BROWSER_PERFORMANCE.createSuspendableInterval(() => { runs += 1; }, {
    intervalMs: 800,
    keepActive: () => keep,
    onResume: () => { resumes += 1; },
  });
  assert.equal(interval.rearm(), false, "an unstarted interval is not armed by a wake signal");
  assert.equal(timers.count(), 0);
  interval.start();
  timers.elapse();
  assert.equal(interval.active(), false);
  assert.equal(runs, 0);
  keep = true;
  assert.equal(interval.rearm(), true);
  assert.equal(interval.rearm(), false);
  assert.equal(timers.count(), 1);
  assert.equal(runs, 0);
  assert.equal(resumes, 1);
  timers.elapse();
  assert.equal(runs, 1);
  interval.stop();
  assert.equal(interval.rearm(), false);
  assert.equal(timers.count(), 0);
});
