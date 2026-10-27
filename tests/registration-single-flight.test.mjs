// Single-flight conversation registration (bug: registry_handshake_delay).
//
// registerCurrentConversation used to bump a newest-attempt-wins counter on
// every call, so repeated handshake probes and the 1s route poll cancelled the
// in-flight run before h2w_register was ever sent. These tests slice the real
// registration, route-watch, and project-catalog code out of
// extension/content/wake.js and exercise the shipped implementation with
// controllable promises instead of re-implementations.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const wakeSource = readFileSync(new URL("../extension/content/wake.js", import.meta.url), "utf8");

const KEY_A = "https://claude.ai/chat/aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const KEY_B = "https://claude.ai/chat/bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";

function sliceSource(startMarker, endMarker, what) {
  const start = wakeSource.indexOf(startMarker);
  const end = wakeSource.indexOf(endMarker, start);
  assert.ok(start >= 0 && end > start, `${what} must remain extractable`);
  return wakeSource.slice(start, end);
}

const registrationSource = sliceSource(
  "  async function currentAdapterProjectIdentity(convKey)",
  "\n  function startConversationRouteWatch()",
  "conversation registration helpers",
);
const routeWatchSource = sliceSource(
  "  function startConversationRouteWatch()",
  "\n  function assistantSignature(",
  "conversation route watch",
);
const catalogSource = sliceSource(
  "  async function chatGptProjectCatalog(accountNativeIdentity)",
  "\n  function currentChatGptProjectFromCatalog(",
  "ChatGPT project catalog",
);

async function flush(turns = 12) {
  for (let i = 0; i < turns; i += 1) await Promise.resolve();
}

function registrationHarness(initialConvKey = KEY_A, { site = "claude", projectSequence = null } = {}) {
  const sends = [];
  let resolveAccount;
  let resolveCatalog;
  const accountBox = { promise: new Promise((resolve) => { resolveAccount = resolve; }) };
  const catalogBox = { promise: new Promise((resolve) => { resolveCatalog = resolve; }) };
  const api = new Function(
    "accountBox",
    "catalogBox",
    "sends",
    `
    let currentConvKey = ${JSON.stringify(initialConvKey)};
    let currentUrl = currentConvKey;
    let registeredConvKey = null;
    let registeredBrowserSessionRef = null;
    let registeredBrowserGeneration = null;
    let registeredHerdrAppKeyword = null;
    let registeredConversationBound = false;
    let browserRegistrationAttempt = 0;
    const pending = sends;
    const ADAPTER = {
      name: ${JSON.stringify(site)},
      getConversationKey: () => currentConvKey,
      resolveProjectIdentity: async () => null,
    };
    const location = { get href() { return currentUrl; } };
    const runtimeAlive = () => true;
    const browserAccountNativeIdentity = () => accountBox.promise.then((value) => value);
    const chatGptProjectCatalog = () => catalogBox.promise.then((projects) => projects);
    const projectSequence = ${JSON.stringify(projectSequence)};
    let projectCalls = 0;
    const currentChatGptProjectFromCatalog = () => {
      if (!projectSequence) return null;
      const value = projectSequence[Math.min(projectCalls, projectSequence.length - 1)];
      projectCalls += 1;
      return value;
    };
    const sendBg = async (payload) => new Promise((resolve) => pending.push({ payload, resolve }));
    const ensureConversationHealth = async () => {};
    const chatGptConversationId = () => null;
    const refreshQueuedInsertStatus = () => {};
    const backfillCurrentChatGptContinuity = () => {};
    const restoreBrowserResultAssignment = () => {};
    const armBrowserPendingDispatchRefresh = () => {};
    const clearBrowserPendingDispatchRefresh = () => {};
    const acceptedDispatchAssignments = new Map();
    const observeBrowserResultSettlement = async () => {};
    let browserResultProbeState = null;
    let conversationHealth = null;
    let contextPressureRecord = null;
    const CONTEXT_PRESSURE = false;
    const loadContextPressure = async () => null;
    const updateContextPressure = async () => null;
    const paintPageHud = () => {};
    let queuedInsertCount = 0;
    const removeQueuedInsertButton = () => {};
    const latestTurnForRole = () => null;
    const usesOperationalHud = () => false;
    const refreshPageHud = () => {};
    ${registrationSource}
    return {
      register: registerCurrentConversation,
      setRoute(value) { currentConvKey = value; currentUrl = value; },
      status() { return lastBrowserRegistrationStatus; },
      state() {
        return {
          convKey: registeredConvKey,
          sessionRef: registeredBrowserSessionRef,
          generation: registeredBrowserGeneration,
          bound: registeredConversationBound,
          attempt: browserRegistrationAttempt,
        };
      },
    };
    `,
  )(accountBox, catalogBox, sends);
  return {
    register: (reason, options) => api.register(reason, options),
    setRoute: (value) => api.setRoute(value),
    state: () => api.state(),
    status: () => api.status(),
    sends,
    // Resolve the stalled pre-registration work and make later runs flow.
    releaseAccount(identity = "opaque-account") {
      resolveAccount(identity);
      accountBox.promise = Promise.resolve(identity);
    },
    changeAccount(identity) { accountBox.promise = Promise.resolve(identity); },
    releaseCatalog() {
      resolveCatalog([]);
      catalogBox.promise = Promise.resolve([]);
    },
    releasePreWork() {
      this.releaseAccount();
      this.releaseCatalog();
    },
  };
}

function routeWatchHarness({ convKey = KEY_A, registeredConvKey = null, hidden = false } = {}) {
  const ticks = [];
  const registrationReasons = [];
  let currentRegistration = null;
  let currentConvKey = convKey;
  const registerCurrentConversation = (reason) => {
    registrationReasons.push(reason);
    currentRegistration = {};
    currentRegistration.promise = new Promise((resolve) => { currentRegistration.resolve = resolve; });
    return currentRegistration.promise;
  };
  const documentStub = { hidden, addEventListener: () => {} };
  const windowStub = { addEventListener: () => {} };
  const adapterStub = {
    name: "claude",
    getConversationKey: () => currentConvKey,
  };
  const api = new Function(
    "setInterval",
    "window",
    "document",
    "ADAPTER",
    "registerCurrentConversation",
    "registeredConversationBound",
    "registeredHerdrAppKeyword",
    "latestTurnForRole",
    "ensureQueuedInsertButton",
    "observeBrowserResultSettlement",
    "maybeRefreshBrowserPendingDispatchAssignment",
    "permObs",
    "permScheduler",
    "permissionTryClick",
    "initialRegisteredConvKey",
    `
    let registeredConvKey = initialRegisteredConvKey;
    ${routeWatchSource}
    return {
      start: startConversationRouteWatch,
      commitRegistration(key) { registeredConvKey = key; },
    };
    `,
  )(
    (fn) => { ticks.push(fn); return ticks.length; },
    windowStub,
    documentStub,
    adapterStub,
    registerCurrentConversation,
    false,
    null,
    () => null,
    () => {},
    async () => {},
    () => {},
    null,
    null,
    () => {},
    registeredConvKey,
  );
  return {
    start: api.start,
    commitRegistration: (key) => api.commitRegistration(key),
    tick() { for (const fn of ticks) fn(); },
    registrationReasons,
    settleRegistration() { currentRegistration.resolve({ ok: true }); },
    setConvKey(value) { currentConvKey = value; },
  };
}

function catalogHarness(fetchHandlers = []) {
  const fetches = [];
  const adapterStub = { name: "chatgpt" };
  const cache = { accountNativeIdentity: null, fetchedAt: 0, projects: [] };
  const handlers = [...fetchHandlers];
  const hangUntilAbort = (init) => new Promise((resolve, reject) => {
    init?.signal?.addEventListener("abort", () => reject(new Error("aborted")));
  });
  const fetchStub = (url, init) => {
    fetches.push({ url, init });
    return (handlers.shift() || hangUntilAbort)(init);
  };
  const catalog = new Function(
    "ADAPTER",
    "chatGptProjectCatalogCache",
    "readChatGptAccessToken",
    "fetch",
    `${catalogSource}
    return chatGptProjectCatalog;`,
  )(adapterStub, cache, async () => "token", fetchStub);
  return { catalog, fetches };
}

async function abortFirstFetch(harness) {
  await flush();
  const signal = harness.fetches[0].init?.signal;
  assert.ok(signal, "sidebar fetch must carry an abort signal");
  assert.ok(signal instanceof AbortSignal, "the abort signal must come from the shipped AbortSignal.timeout source");
  // AbortSignal.timeout uses an unref'd timer; drive the abort deterministically
  // instead of waiting for the real 3s timeout (test-design policy).
  signal.dispatchEvent(new Event("abort"));
}

test("user gets exactly one registration per conversation | Given slow pre-registration work and repeated handshake probes | When registration is requested four times for the same conversation | Then one h2w_register is sent and every caller shares the same result", async () => {
  const harness = registrationHarness(KEY_A);
  const callers = [
    harness.register("startup"),
    harness.register("identity-recovery"),
    harness.register("poll"),
    harness.register("identity-recovery"),
  ];
  harness.releasePreWork();
  await flush();
  assert.equal(harness.sends.length, 1, "shared callers must not stack registration attempts");
  assert.equal(harness.sends[0].payload.type, "h2w_register");
  assert.equal(harness.sends[0].payload.convKey, KEY_A);
  harness.sends[0].resolve({ browser_session_ref: "br_shared", browser_generation: 7, bound: true });
  const results = await Promise.all(callers);
  assert.ok(results.every((result) => result === results[0]), "all callers share one result");
  assert.equal(results[0]?.browser_session_ref, "br_shared");
  assert.deepEqual(harness.state(), {
    convKey: KEY_A,
    sessionRef: "br_shared",
    generation: 7,
    bound: true,
    attempt: 1,
  });
});

test("user cannot register a stale account | Given an account switch during project discovery | When registration is about to publish | Then no old-account identity is sent and the stale tab scope is cleared", async () => {
  const harness = registrationHarness(KEY_A);
  const registration = harness.register("startup");
  harness.releaseAccount("account-one");
  await flush();
  harness.changeAccount("account-two");
  harness.releaseCatalog();
  const result = await registration;
  assert.equal(result, null);
  assert.deepEqual(harness.sends.map((send) => send.payload.type), ["h2w_clear_scope"],
    "the old account must never reach h2w_register and the stale scope must be dropped");
  assert.equal(harness.status(), "account_changed", "the refusal reason stays a fixed enum");
});

test("user cannot register a switched project | Given the conversation's project changes during discovery | When registration is about to publish | Then no registration is sent and the stale tab scope is cleared", async () => {
  const harness = registrationHarness(KEY_A, {
    site: "chatgpt",
    projectSequence: [{ id: "g-p-expected", name: "Expected" }, { id: "g-p-other", name: "Other" }],
  });
  const registration = harness.register("startup");
  harness.releaseAccount("opaque-account");
  await flush();
  harness.releaseCatalog();
  const result = await registration;
  assert.equal(result, null);
  assert.deepEqual(harness.sends.map((send) => send.payload.type), ["h2w_clear_scope"],
    "a project change must refuse registration and drop the stale tab scope");
  assert.equal(harness.status(), "project_changed", "the refusal reason stays a fixed enum");
});

test("user clears a scope it cannot re-verify | Given a ChatGPT tab without an authenticated identity | When registration runs | Then nothing is published and the tab scope is cleared", async () => {
  const harness = registrationHarness(KEY_A, { site: "chatgpt" });
  const registration = harness.register("startup");
  harness.releaseAccount(null);
  await flush();
  const result = await registration;
  assert.equal(result, null);
  assert.deepEqual(harness.sends.map((send) => send.payload.type), ["h2w_clear_scope"],
    "an unauthenticated ChatGPT tab must not keep its scope");
  assert.equal(harness.status(), "identity_unavailable");
});

test("user refreshes registration observation without cancelling the in-flight run | Given one slow in-flight registration | When an identity-recovery handshake requests refreshObservation | Then the in-flight run still lands and a fresh registration follows it", async () => {
  const harness = registrationHarness(KEY_A);
  const first = harness.register("startup");
  const refresh = harness.register("identity-recovery", { refreshObservation: true });
  harness.releasePreWork();
  await flush();
  assert.equal(harness.sends.length, 1);
  harness.sends[0].resolve({ browser_session_ref: "br_first", browser_generation: 3, bound: true });
  const firstResult = await first;
  assert.equal(firstResult?.browser_session_ref, "br_first", "the in-flight run must not be cancelled");
  assert.equal(harness.state().sessionRef, "br_first");
  await flush();
  assert.equal(harness.sends.length, 2, "refreshObservation must chain one fresh registration");
  assert.equal(harness.sends[1].payload.convKey, KEY_A);
  harness.sends[1].resolve({ browser_session_ref: "br_second", browser_generation: 4, bound: true });
  const refreshResult = await refresh;
  assert.equal(refreshResult?.browser_session_ref, "br_second");
  assert.equal(harness.state().sessionRef, "br_second");
  assert.equal(harness.state().attempt, 2);
});

test("user skips the chained refresh when the route moved on | Given a refreshObservation chained behind conversation A's in-flight registration | When the route changes to B before A settles | Then no redundant registration runs for the new route", async () => {
  const harness = registrationHarness(KEY_A);
  const first = harness.register("startup");
  const refresh = harness.register("identity-recovery", { refreshObservation: true });
  harness.releasePreWork();
  await flush();
  assert.equal(harness.sends.length, 1);
  harness.setRoute(KEY_B);
  harness.sends[0].resolve({ browser_session_ref: "br_a", browser_generation: 1, bound: true });
  await first;
  const refreshResult = await refresh;
  assert.equal(refreshResult, null, "the chained refresh must be skipped for a changed route");
  assert.equal(harness.sends.length, 1, "no redundant registration may start for the new route");
});

test("user supersedes an in-flight registration on route change | Given registration in flight for conversation A | When the route changes to B and registration restarts | Then B registers and A's late response is discarded", async () => {
  const harness = registrationHarness(KEY_A);
  const oldRun = harness.register("startup");
  harness.releasePreWork();
  await flush();
  assert.equal(harness.sends.length, 1);
  harness.setRoute(KEY_B);
  const bRun = harness.register("poll");
  await flush();
  assert.equal(harness.sends.length, 2);
  assert.equal(harness.sends[0].payload.convKey, KEY_A);
  assert.equal(harness.sends[1].payload.convKey, KEY_B);
  harness.sends[1].resolve({ browser_session_ref: "br_b", browser_generation: 5, bound: true });
  await bRun;
  assert.equal(harness.state().convKey, KEY_B);
  assert.equal(harness.state().sessionRef, "br_b");
  harness.sends[0].resolve({ browser_session_ref: "br_a_late", browser_generation: 4, bound: true });
  const oldResult = await oldRun;
  assert.equal(oldResult, null, "a superseded run must not apply its late response");
  assert.equal(harness.state().sessionRef, "br_b");
  assert.equal(harness.state().generation, 5);
});

test("user never advertises the previous conversation identity while a new route registers | Given conversation A registered | When the route changes to B and B's registration is in flight | Then the advertised refs are cleared before B lands", async () => {
  const harness = registrationHarness(KEY_A);
  const aRun = harness.register("startup");
  harness.releasePreWork();
  await flush();
  harness.sends[0].resolve({ browser_session_ref: "br_a", browser_generation: 2, bound: true });
  await aRun;
  assert.equal(harness.state().sessionRef, "br_a");
  harness.setRoute(KEY_B);
  const bRun = harness.register("poll");
  assert.equal(harness.state().sessionRef, null, "route change must clear the previous identity immediately");
  assert.equal(harness.state().generation, null);
  await flush();
  harness.sends[1].resolve({ browser_session_ref: "br_b", browser_generation: 6, bound: true });
  await bRun;
  assert.equal(harness.state().convKey, KEY_B);
  assert.equal(harness.state().sessionRef, "br_b");
});

test("user keeps the visible route poll from stacking registrations | Given the visible-tab 1s poll with a registration in flight | When the poll fires again for the same conversation | Then no second attempt starts until the first settles", async () => {
  const harness = routeWatchHarness({ convKey: KEY_A, registeredConvKey: null, hidden: false });
  harness.start();
  harness.tick();
  harness.tick();
  harness.tick();
  assert.equal(harness.registrationReasons.length, 1, "poll must not stack attempts for the same conversation");
  harness.settleRegistration();
  await flush();
  harness.tick();
  assert.equal(harness.registrationReasons.length, 2, "poll may register again after the previous run settles");
  // Once the settled registration commits its identity, registeredConvKey is
  // a live cell: the poll must now stay quiet for the registered conversation.
  harness.commitRegistration(KEY_A);
  harness.tick();
  harness.tick();
  assert.equal(harness.registrationReasons.length, 2, "poll must stay quiet once the conversation is registered");
  // A different conversation key may still register while the old one is in flight.
  harness.setConvKey(KEY_B);
  harness.tick();
  assert.equal(harness.registrationReasons.length, 3);
  assert.equal(harness.registrationReasons[2], "poll");
});

test("user cannot hang registration on a stuck sidebar catalog fetch | Given the ChatGPT project-catalog fetch never responds | When the abort signal fires | Then the catalog read fails fast and returns no projects", async () => {
  const harness = catalogHarness();
  const result = harness.catalog("opaque-account");
  await abortFirstFetch(harness);
  assert.deepEqual(await result, []);
  assert.ok(harness.fetches.length >= 1, "sidebar fetch must be attempted");
  assert.match(wakeSource, /signal:\s*AbortSignal\.timeout\(3000\)/, "the shipped source must arm the 3s abort timeout");
});

test("user does not cache an aborted catalog read | Given a fresh cache and a sidebar fetch that aborts | When the catalog is read again after the fetch recovers | Then the catalog refetches instead of serving the poisoned empty cache", async () => {
  const okPage = {
    ok: true,
    json: async () => ({
      items: [{ gizmo: { gizmo: { id: "g-p-herdr", display: { name: "herdr" } } } }],
      cursor: null,
    }),
  };
  const harness = catalogHarness([null, () => Promise.resolve(okPage)]);
  // First read: the fetch hangs, then the abort signal fires (as the 3s
  // AbortSignal.timeout would in production).
  const first = harness.catalog("opaque-account");
  await abortFirstFetch(harness);
  assert.deepEqual(await first, []);
  // The aborted read must not have refreshed the 5-minute cache with []:
  // the retry fetches again and returns the real projects.
  const second = await harness.catalog("opaque-account");
  assert.equal(harness.fetches.length, 2, "an aborted read must not be cached as fresh");
  assert.deepEqual(second, [{ id: "g-p-herdr", name: "herdr" }]);
  // A completed read is cached normally: the next read does not refetch.
  const third = await harness.catalog("opaque-account");
  assert.equal(harness.fetches.length, 2, "a completed read must be cached");
  assert.deepEqual(third, [{ id: "g-p-herdr", name: "herdr" }]);
});

test("user ships a version-consistent extension bundle | Given the manifest and script version constants | When the extension is assembled | Then all version markers agree", () => {
  const manifest = JSON.parse(readFileSync(new URL("../extension/manifest.json", import.meta.url), "utf8"));
  const background = readFileSync(new URL("../extension/background.js", import.meta.url), "utf8");
  assert.match(background, /const H2W_SCRIPT_VERSION = "([0-9]+\.[0-9]+\.[0-9]+)"/);
  const backgroundVersion = background.match(/const H2W_SCRIPT_VERSION = "([0-9]+\.[0-9]+\.[0-9]+)"/)[1];
  const contentVersion = wakeSource.match(/const H2W_CONTENT_VERSION = "([0-9]+\.[0-9]+\.[0-9]+)"/)[1];
  assert.equal(backgroundVersion, manifest.version);
  assert.equal(contentVersion, manifest.version);
});