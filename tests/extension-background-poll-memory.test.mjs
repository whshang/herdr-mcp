// #1036 / #1037 behaviour tests for the long-lived extension service worker.
// They slice the shipped background.js regions (runtime reachability gate,
// /push/state single-flight, tab-close and unbind reclamation) and drive them
// through their real entry points with fakes only at the Native Messaging and
// chrome.* event boundaries.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const backgroundSource = readFileSync(new URL("../extension/background.js", import.meta.url), "utf8");

function slice(startMarker, endMarker, what) {
  const start = backgroundSource.indexOf(startMarker);
  const end = backgroundSource.indexOf(endMarker, start);
  assert.ok(start >= 0 && end > start, `${what} must remain extractable from extension/background.js`);
  return backgroundSource.slice(start, end);
}

function declaration(pattern, what) {
  const match = backgroundSource.match(pattern);
  assert.ok(match, `${what} must remain declared in extension/background.js`);
  return match[0];
}

async function flush(turns = 20) {
  for (let i = 0; i < turns; i += 1) await Promise.resolve();
}

function runtimeHarness({ autoOn = false } = {}) {
  const gateSource = slice(
    "function noteLocalRuntimeReachability(",
    "\nfunction cachePushWorkspaceCatalog(",
    "automation runtime gate",
  );
  const fetchSource = slice("async function fetchStateFresh()", "\nasync function extensionOwnerAllowsControl(", "state fetch");
  const cacheWindow = declaration(/const MANUAL_RUNTIME_REACHABILITY_CACHE_MS = \d+;/, "manual reachability cache window");
  const env = {
    now: 1_700_000_000_000,
    autoOn,
    runtimeUp: true,
    holdFetch: false,
    pending: [],
    pushStateRequests: 0,
  };
  const api = new Function(
    "env",
    `
    const Date = { now: () => env.now };
    let localRuntimeReachable = null;
    let localRuntimeCheckedAt = 0;
    let stateFetchInFlight = null;
    let pushStream = null;
    ${cacheWindow}
    const CFG = { herdrMcpUrl: "http://127.0.0.1:8772" };
    const STATE_FETCH_MS = 1000;
    const broadcastControlMessage = () => {};
    const cachePushWorkspaceCatalog = () => {};
    const hudLabels = () => ({});
    const automationScopeForConversation = (convKey) => ({ convKey, enabled: env.autoOn });
    const localHerdrFetch = async (url) => {
      if (!url.endsWith("/push/state")) throw new Error("unexpected " + url);
      env.pushStateRequests += 1;
      const answer = () => {
        if (!env.runtimeUp) throw new Error("connect ECONNREFUSED 127.0.0.1:8772");
        return { ok: true, status: 200, json: async () => ({ workspaces: [] }) };
      };
      if (!env.holdFetch) return answer();
      return new Promise((resolve, reject) => env.pending.push(() => {
        try { resolve(answer()); } catch (error) { reject(error); }
      }));
    };
    ${gateSource}
    ${fetchSource}
    return {
      poll: (convKey) => automationStateForConversation(convKey),
      connectPushStream() {
        pushStream = { ctrl: {}, bootId: "boot", connected: true };
        noteLocalRuntimeReachability(true);
      },
      dropPushStream() {
        if (pushStream) pushStream.connected = false;
        noteLocalRuntimeReachability(false);
      },
      cacheWindowMs: MANUAL_RUNTIME_REACHABILITY_CACHE_MS,
    };
    `,
  )(env);
  const release = () => { for (const resolve of env.pending.splice(0)) resolve(); };
  return { api, env, release };
}

test("user manual-mode tabs stop re-pulling runtime state | Given Auto is off and the resident push stream is connected | When five visible tabs poll h2w_automation_state every 5s for ten minutes | Then no /push/state request is made and every poll reports the runtime available", async () => {
  const { api, env } = runtimeHarness({ autoOn: false });
  api.connectPushStream();
  for (let tick = 0; tick < 120; tick += 1) {
    env.now += 5000;
    const states = await Promise.all([1, 2, 3, 4, 5].map((tab) => api.poll(`https://chatgpt.com/c/tab-${tab}`)));
    for (const state of states) {
      assert.equal(state.runtime_available, true);
      assert.equal(state.preference_enabled, false);
      assert.equal(state.effective_enabled, false);
    }
  }
  assert.equal(env.pushStateRequests, 0);
});

test("user manual-mode polls share one runtime check | Given Auto is off with no connected push stream and no recent check | When three tabs poll at once, again inside the cache window, and again after it | Then one /push/state request serves the first burst, the second burst uses the cache, and the third refreshes once", async () => {
  const { api, env, release } = runtimeHarness({ autoOn: false });
  env.holdFetch = true;
  const burst = Promise.all(["a", "b", "c"].map((id) => api.poll(`https://chatgpt.com/c/${id}`)));
  await flush();
  assert.equal(env.pushStateRequests, 1);
  release();
  assert.deepEqual((await burst).map((state) => state.runtime_available), [true, true, true]);

  env.holdFetch = false;
  env.now += api.cacheWindowMs - 1000;
  await Promise.all(["a", "b", "c"].map((id) => api.poll(`https://chatgpt.com/c/${id}`)));
  assert.equal(env.pushStateRequests, 1);

  env.now += 2000;
  env.runtimeUp = false;
  const stale = await api.poll("https://chatgpt.com/c/a");
  assert.equal(env.pushStateRequests, 2);
  assert.equal(stale.runtime_available, false);
});

test("user manual mode reports a recent outage without probing again | Given Auto is off and the push stream just dropped | When a tab polls inside the cache window | Then runtime_available is false and no /push/state request is made", async () => {
  const { api, env } = runtimeHarness({ autoOn: false });
  api.connectPushStream();
  env.now += 1000;
  api.dropPushStream();
  env.now += 5000;
  const state = await api.poll("https://chatgpt.com/c/manual");
  assert.equal(state.runtime_available, false);
  assert.equal(env.pushStateRequests, 0);
});

test("user Auto mode still fails closed immediately | Given Auto is on and the push stream reported the runtime reachable | When the runtime stops answering and two tabs poll concurrently | Then one shared live /push/state check runs and both polls report effective automation off", async () => {
  const { api, env, release } = runtimeHarness({ autoOn: true });
  api.connectPushStream();
  const online = await api.poll("https://chatgpt.com/c/auto");
  assert.equal(online.effective_enabled, true);
  assert.equal(env.pushStateRequests, 1);

  env.runtimeUp = false;
  env.holdFetch = true;
  env.now += 1000;
  const polls = Promise.all([api.poll("https://chatgpt.com/c/auto"), api.poll("https://chatgpt.com/c/auto-2")]);
  await flush();
  assert.equal(env.pushStateRequests, 2);
  release();
  for (const state of await polls) {
    assert.equal(state.preference_enabled, true);
    assert.equal(state.enabled, false);
    assert.equal(state.effective_enabled, false);
    assert.equal(state.runtime_available, false);
  }
});

function memoryHarness() {
  const reclaimSource = slice("function reclaimClosedTabState(", "\n// ---- Toolbar badge", "tab-close and unbind reclamation");
  const nudgeSource = slice("const CONVERSATION_NUDGE_STATE_MAX", "\nconst LLM_JUDGE_TIMEOUT_MS", "conversation nudge state bounds");
  const cooldown = declaration(/const PAGE_HEALTH_FORCE_RELOAD_COOLDOWN_MS = \d+;/, "page-health reload cooldown");
  const events = { tabRemoved: [], storageChanged: [] };
  const state = {
    tabVersions: new Map(),
    reloadedTabs: new Set(),
    tabRecoveryAttemptAt: new Map(),
    pageHealthForceReloadAt: new Map(),
    browserTabScopes: new Map(),
    lastIdleNudgeAt: new Map(),
    lastJudgedAssistantFp: new Map(),
    lastIdleNudgeResult: new Map(),
    lastTurnEndedPayload: new Map(),
  };
  const chrome = {
    tabs: { onRemoved: { addListener: (fn) => events.tabRemoved.push(fn) } },
    storage: { onChanged: { addListener: (fn) => events.storageChanged.push(fn) } },
  };
  const api = new Function(
    "chrome",
    "state",
    `
    const { tabVersions, reloadedTabs, tabRecoveryAttemptAt, pageHealthForceReloadAt, browserTabScopes,
      lastIdleNudgeAt, lastJudgedAssistantFp, lastIdleNudgeResult, lastTurnEndedPayload } = state;
    ${cooldown}
    ${reclaimSource}
    ${nudgeSource}
    return { rememberConversationNudgeState, cap: CONVERSATION_NUDGE_STATE_MAX, cooldownMs: PAGE_HEALTH_FORCE_RELOAD_COOLDOWN_MS };
    `,
  )(chrome, state);
  assert.equal(events.tabRemoved.length, 1, "chrome.tabs.onRemoved must be registered");
  assert.equal(events.storageChanged.length, 1, "chrome.storage.onChanged must be registered");
  return {
    api,
    state,
    closeTab: (tabId) => events.tabRemoved[0](tabId, { windowId: 1, isWindowClosing: false }),
    storageChange: (changes, area = "local") => events.storageChanged[0](changes, area),
  };
}

test("user closing an AI tab releases its per-tab memory | Given worker state recorded for two open tabs | When Chrome reports one tab removed | Then only that tab's version, reload, recovery and scope entries are gone and in-cooldown page-health reservations survive", () => {
  const { api, state, closeTab } = memoryHarness();
  const now = Date.now();
  for (const tabId of [11, 12]) {
    state.tabVersions.set(tabId, "0.1.0");
    state.reloadedTabs.add(tabId);
    state.tabRecoveryAttemptAt.set(tabId, now);
    state.browserTabScopes.set(tabId, { provider: "chatgpt" });
  }
  state.pageHealthForceReloadAt.set("https://chatgpt.com/c/recent", now);
  state.pageHealthForceReloadAt.set("https://chatgpt.com/c/expired", now - api.cooldownMs - 1);

  closeTab(11);

  assert.deepEqual([...state.tabVersions.keys()], [12]);
  assert.deepEqual([...state.reloadedTabs], [12]);
  assert.deepEqual([...state.tabRecoveryAttemptAt.keys()], [12]);
  assert.deepEqual([...state.browserTabScopes.keys()], [12]);
  assert.deepEqual([...state.pageHealthForceReloadAt.keys()], ["https://chatgpt.com/c/recent"]);
});

test("user unbinding a conversation releases its nudge memory | Given nudge state for three conversations and bindings that still reference two of them | When stored wake bindings drop every binding for one conversation and one of two for another | Then only the fully unbound conversation's nudge state is cleared", () => {
  const { state, storageChange } = memoryHarness();
  const gone = "https://chatgpt.com/c/gone";
  const shared = "https://chatgpt.com/c/shared";
  const project = "https://chatgpt.com/g/g-p-abc";
  const projectActive = "https://chatgpt.com/g/g-p-abc/c/active";
  for (const convKey of [gone, shared, projectActive]) {
    state.lastIdleNudgeAt.set(convKey, 1);
    state.lastJudgedAssistantFp.set(convKey, "fp");
    state.lastIdleNudgeResult.set(convKey, { reason: "same_assistant" });
    state.lastTurnEndedPayload.set(convKey, { convKey });
  }
  const oldValue = {
    [`${gone}::w1`]: { convKey: gone },
    [`${shared}::w1`]: { convKey: shared },
    [`${shared}::w2`]: { convKey: shared },
    [`${project}::w3`]: { convKey: project, active_conv_key: projectActive },
  };
  const newValue = {
    [`${shared}::w2`]: { convKey: shared },
    [`${project}::w3`]: { convKey: project, active_conv_key: projectActive },
  };

  storageChange({ herdrWakeBindings: { oldValue, newValue } }, "sync");
  assert.equal(state.lastIdleNudgeAt.has(gone), true, "non-local storage areas are ignored");

  storageChange({ herdrWakeBindings: { oldValue, newValue } });
  for (const map of [state.lastIdleNudgeAt, state.lastJudgedAssistantFp, state.lastIdleNudgeResult, state.lastTurnEndedPayload]) {
    assert.deepEqual([...map.keys()].sort(), [projectActive, shared].sort());
  }

  storageChange({ herdrWakeBindings: { oldValue: newValue, newValue: {} } });
  assert.equal(state.lastJudgedAssistantFp.size, 0);
});

test("user visiting many conversations keeps nudge memory bounded | Given the per-conversation nudge cap | When more conversations than the cap record judge results and an early one is touched again | Then the map never exceeds the cap and keeps the most recently written conversations", () => {
  const { api, state } = memoryHarness();
  api.rememberConversationNudgeState(state.lastIdleNudgeResult, "https://chatgpt.com/c/keep", { reason: "unbound" });
  for (let i = 0; i < api.cap + 44; i += 1) {
    api.rememberConversationNudgeState(state.lastIdleNudgeResult, `https://chatgpt.com/c/${i}`, { reason: "unbound" });
    if (i === 50) api.rememberConversationNudgeState(state.lastIdleNudgeResult, "https://chatgpt.com/c/keep", { reason: "disabled" });
    assert.ok(state.lastIdleNudgeResult.size <= api.cap);
  }
  assert.equal(state.lastIdleNudgeResult.size, api.cap);
  assert.equal(state.lastIdleNudgeResult.has("https://chatgpt.com/c/0"), false);
  assert.equal(state.lastIdleNudgeResult.get("https://chatgpt.com/c/keep")?.reason, "disabled");
  assert.equal(state.lastIdleNudgeResult.has(`https://chatgpt.com/c/${api.cap + 43}`), true);
});
