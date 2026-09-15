import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { chatGptConversationInfo } from "../extension/continuity-core.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const backgroundSource = readFileSync(path.join(__dirname, "..", "extension", "background.js"), "utf8");
const wakeSource = readFileSync(path.join(__dirname, "..", "extension", "content", "wake.js"), "utf8");
const chatGptAdapterSource = readFileSync(path.join(__dirname, "..", "extension", "content", "injector", "chatgpt.js"), "utf8");

const recoveryStart = backgroundSource.indexOf("async function recoverBrowserSessionTarget(");
const recoveryEnd = backgroundSource.indexOf("\nasync function handleBrowserActuation", recoveryStart);
assert.ok(recoveryStart >= 0 && recoveryEnd > recoveryStart, "recovery helper must remain extractable");
const recoverySource = backgroundSource.slice(recoveryStart, recoveryEnd);

const archiveCleanupStart = backgroundSource.indexOf("function shouldCloseArchivedChatGptTab(");
const archiveCleanupEnd = backgroundSource.indexOf("\nasync function recoverBrowserSessionTarget", archiveCleanupStart);
assert.ok(archiveCleanupStart >= 0 && archiveCleanupEnd > archiveCleanupStart, "archive tab cleanup helpers must remain extractable");
const archiveCleanupSource = backgroundSource.slice(archiveCleanupStart, archiveCleanupEnd);

const registrationStart = wakeSource.indexOf('async function registerCurrentConversation(reason = "startup")');
const registrationEnd = wakeSource.indexOf("\n  function startConversationRouteWatch()", registrationStart);
assert.ok(registrationStart >= 0 && registrationEnd > registrationStart, "registration helper must remain extractable");
const registrationSource = wakeSource.slice(registrationStart, registrationEnd);

function recoveryHarness(tabRecords) {
  const browserSessionTargets = new Map();
  const chrome = {
    tabs: {
      async query() {
        return tabRecords.map(({ id, url, status = "complete" }) => ({ id, url, status }));
      },
      async sendMessage(tabId, message) {
        assert.equal(message?.type, "h2w_get_convkey");
        const record = tabRecords.find((item) => item.id === tabId);
        if (!record || record.error) throw new Error("missing content listener");
        return record.live;
      },
    },
  };
  const activeH2WTabUrls = () => ["https://chatgpt.com/*"];
  const browserConversationInfoFromSupportedUrl = (rawUrl) => {
    const match = String(rawUrl || "").match(/^https:\/\/chatgpt\.com\/c\/([^/?#]+)/);
    if (!match) return null;
    return {
      site: "chatgpt",
      conversation_id: match[1],
      project_id: null,
      convKey: `https://chatgpt.com/c/${match[1]}`,
    };
  };
  const recover = new Function(
    "chrome",
    "activeH2WTabUrls",
    "browserConversationInfoFromSupportedUrl",
    "browserSessionTargets",
    `${recoverySource}; return recoverBrowserSessionTarget;`,
  )(chrome, activeH2WTabUrls, browserConversationInfoFromSupportedUrl, browserSessionTargets);
  return { recover, browserSessionTargets };
}

function archiveCleanupHarness({
  tabUrl,
  sessionRef = "br_archive_cleanup",
  generation = 7,
  projectId = "g-p-6a89c078669481918c8eb70fdfd3d978",
  target = null,
  scope = null,
} = {}) {
  const tabId = 41;
  const tabs = new Map([[tabId, { id: tabId, url: tabUrl }]]);
  const removeCalls = [];
  const chrome = {
    tabs: {
      async get(id) {
        const tab = tabs.get(id);
        if (!tab) throw new Error(`tab ${id} missing`);
        return { ...tab };
      },
      async remove(id) {
        removeCalls.push(id);
        tabs.delete(id);
      },
    },
  };
  const browserSessionTargets = new Map([[sessionRef, target || {
    provider: "chatgpt",
    tabId,
    projectId,
    observationGeneration: generation,
  }]]);
  const browserTabScopes = new Map([[tabId, scope || {
    provider: "chatgpt",
    projectId,
    observationGeneration: generation,
  }]]);
  const archiveTabCloseInFlight = new Set();
  const browserConversationInfo = (provider, url) => provider === "chatgpt"
    ? chatGptConversationInfo(url)
    : null;
  const api = new Function(
    "chrome",
    "browserConversationInfo",
    "browserSessionTargets",
    "browserTabScopes",
    "archiveTabCloseInFlight",
    `${archiveCleanupSource}; return { shouldCloseArchivedChatGptTab, closeArchivedChatGptTabAfterProjectHome };`,
  )(chrome, browserConversationInfo, browserSessionTargets, browserTabScopes, archiveTabCloseInFlight);
  return {
    ...api,
    tabId,
    sessionRef,
    generation,
    projectId,
    tabs,
    removeCalls,
    browserSessionTargets,
    browserTabScopes,
  };
}

function registrationHarness(initialConvKey = "https://claude.ai/chat/aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa") {
  return new Function(`
    let currentConvKey = ${JSON.stringify(initialConvKey)};
    let currentUrl = currentConvKey;
    let registeredConvKey = currentConvKey;
    let registeredBrowserSessionRef = "br_initial";
    let registeredBrowserGeneration = 1;
    let browserRegistrationAttempt = 0;
    const pending = [];
    const ADAPTER = { name: "claude", getConversationKey: () => currentConvKey };
    const location = { get href() { return currentUrl; } };
    const runtimeAlive = () => true;
    const browserAccountNativeIdentity = async () => "opaque-account";
    const chatGptProjectCatalog = async () => [];
    const currentChatGptProjectFromCatalog = () => null;
    const sendBg = async (payload) => new Promise((resolve) => pending.push({ payload, resolve }));
    const ensureConversationHealth = async () => {};
    const chatGptConversationId = () => null;
    const refreshQueuedInsertStatus = () => {};
    const backfillCurrentChatGptContinuity = () => {};
    const restoreBrowserResultAssignment = () => {};
    const CONTEXT_PRESSURE = false;
    const usesOperationalHud = () => false;
    const refreshPageHud = () => {};
    let conversationHealth = null;
    let contextPressureRecord = null;
    let queuedInsertCount = 0;
    const removeQueuedInsertButton = () => {};
    ${registrationSource}
    return {
      register: registerCurrentConversation,
      pending,
      setRoute(value) { currentConvKey = value; currentUrl = value; },
      state() {
        return {
          convKey: registeredConvKey,
          sessionRef: registeredBrowserSessionRef,
          generation: registeredBrowserGeneration,
          attempt: browserRegistrationAttempt,
        };
      },
    };
  `)();
}

async function flushRegistrationToSend(harness) {
  for (let i = 0; i < 4; i += 1) await Promise.resolve();
  assert.ok(harness.pending.length > 0, "registration must reach background send");
}

test("archive cleanup closes only the exact ChatGPT tab after proven archive reaches the same Project home", async () => {
  const harness = archiveCleanupHarness({
    tabUrl: "https://chatgpt.com/g/g-p-6a89c078669481918c8eb70fdfd3d978-herdr-mcp/project",
  });
  const appliedEvidence = {
    command_accepted: true,
    resource_available: true,
    rejected: false,
    stable_resource_ref_observed: true,
    lifecycle_observed: true,
  };
  assert.equal(harness.shouldCloseArchivedChatGptTab(
    "herdr_mcp.browser_session.archive",
    appliedEvidence,
    "chatgpt",
    harness.projectId,
  ), true);
  const result = await harness.closeArchivedChatGptTabAfterProjectHome({
    tabId: harness.tabId,
    sessionRef: harness.sessionRef,
    expectedGeneration: harness.generation,
    projectId: harness.projectId,
    timeoutMs: 0,
  });
  assert.deepEqual(result, { closed: true, reason: "archived_project_home" });
  assert.deepEqual(harness.removeCalls, [harness.tabId]);
  assert.equal(harness.tabs.has(harness.tabId), false);
  assert.equal(harness.browserSessionTargets.has(harness.sessionRef), false);
  assert.equal(harness.browserTabScopes.has(harness.tabId), false);
});

test("archive cleanup keeps the tab when archive is uncertain or the exact Project home is not proven", async () => {
  const conversationHarness = archiveCleanupHarness({
    tabUrl: "https://chatgpt.com/g/g-p-6a89c078669481918c8eb70fdfd3d978/c/still-open",
  });
  const uncertainEvidence = {
    command_accepted: true,
    resource_available: true,
    rejected: false,
    stable_resource_ref_observed: true,
    lifecycle_observed: false,
  };
  assert.equal(conversationHarness.shouldCloseArchivedChatGptTab(
    "herdr_mcp.browser_session.archive",
    uncertainEvidence,
    "chatgpt",
    conversationHarness.projectId,
  ), false);
  const stillConversation = await conversationHarness.closeArchivedChatGptTabAfterProjectHome({
    tabId: conversationHarness.tabId,
    sessionRef: conversationHarness.sessionRef,
    expectedGeneration: conversationHarness.generation,
    projectId: conversationHarness.projectId,
    timeoutMs: 0,
  });
  assert.deepEqual(stillConversation, { closed: false, reason: "project_home_not_observed" });
  assert.deepEqual(conversationHarness.removeCalls, []);

  const wrongProjectHarness = archiveCleanupHarness({
    tabUrl: "https://chatgpt.com/g/g-p-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-other/project",
  });
  const wrongProject = await wrongProjectHarness.closeArchivedChatGptTabAfterProjectHome({
    tabId: wrongProjectHarness.tabId,
    sessionRef: wrongProjectHarness.sessionRef,
    expectedGeneration: wrongProjectHarness.generation,
    projectId: wrongProjectHarness.projectId,
    timeoutMs: 0,
  });
  assert.deepEqual(wrongProject, { closed: false, reason: "wrong_project" });
  assert.deepEqual(wrongProjectHarness.removeCalls, []);
});

test("archive cleanup is exact-generation fenced and duplicate close attempts are idempotent", async () => {
  const mismatched = archiveCleanupHarness({
    tabUrl: "https://chatgpt.com/g/g-p-6a89c078669481918c8eb70fdfd3d978/project",
    target: {
      provider: "chatgpt",
      tabId: 41,
      projectId: "g-p-6a89c078669481918c8eb70fdfd3d978",
      observationGeneration: 8,
    },
  });
  const fenced = await mismatched.closeArchivedChatGptTabAfterProjectHome({
    tabId: mismatched.tabId,
    sessionRef: mismatched.sessionRef,
    expectedGeneration: mismatched.generation,
    projectId: mismatched.projectId,
    timeoutMs: 0,
  });
  assert.deepEqual(fenced, { closed: false, reason: "session_identity_changed" });
  assert.deepEqual(mismatched.removeCalls, []);

  const duplicate = archiveCleanupHarness({
    tabUrl: "https://chatgpt.com/g/g-p-6a89c078669481918c8eb70fdfd3d978/project",
  });
  const [first, second] = await Promise.all([
    duplicate.closeArchivedChatGptTabAfterProjectHome({
      tabId: duplicate.tabId,
      sessionRef: duplicate.sessionRef,
      expectedGeneration: duplicate.generation,
      projectId: duplicate.projectId,
      timeoutMs: 0,
    }),
    duplicate.closeArchivedChatGptTabAfterProjectHome({
      tabId: duplicate.tabId,
      sessionRef: duplicate.sessionRef,
      expectedGeneration: duplicate.generation,
      projectId: duplicate.projectId,
      timeoutMs: 0,
    }),
  ]);
  assert.equal(first.closed || second.closed, true);
  assert.deepEqual(duplicate.removeCalls, [duplicate.tabId]);
});

test("service-worker recovery rebuilds exactly one stable session target without page mutation", async () => {
  const sessionRef = "br_session_recovery";
  const { recover, browserSessionTargets } = recoveryHarness([{
    id: 41,
    url: "https://chatgpt.com/c/recovery-1",
    live: {
      convKey: "https://chatgpt.com/c/recovery-1",
      url: "https://chatgpt.com/c/recovery-1",
      site: "chatgpt",
      browserSessionRef: sessionRef,
      browserGeneration: 7,
    },
  }]);

  const recovered = await recover(sessionRef, 7);
  assert.equal(recovered.ambiguous, false);
  assert.equal(recovered.observedGeneration, 7);
  assert.equal(recovered.target?.tabId, 41);
  assert.equal(recovered.target?.conversationId, "recovery-1");
  assert.equal(recovered.target?.observationGeneration, 7);
  assert.deepEqual(browserSessionTargets.get(sessionRef), recovered.target);
  assert.doesNotMatch(recoverySource, /tabs\.reload|scripting\.executeScript|tabs\.update/);
});

test("service-worker recovery fails closed when the same logical session is open twice", async () => {
  const sessionRef = "br_duplicate_session";
  const live = {
    convKey: "https://chatgpt.com/c/duplicate",
    url: "https://chatgpt.com/c/duplicate",
    site: "chatgpt",
    browserSessionRef: sessionRef,
    browserGeneration: 7,
  };
  const { recover, browserSessionTargets } = recoveryHarness([
    { id: 51, url: live.url, live },
    { id: 52, url: live.url, live },
  ]);

  const recovered = await recover(sessionRef, 7);
  assert.equal(recovered.target, null);
  assert.equal(recovered.ambiguous, true);
  assert.equal(browserSessionTargets.has(sessionRef), false);
});

test("service-worker recovery reports a newer generation instead of reusing a stale target", async () => {
  const sessionRef = "br_stale_session";
  const { recover, browserSessionTargets } = recoveryHarness([{
    id: 61,
    url: "https://chatgpt.com/c/stale",
    live: {
      convKey: "https://chatgpt.com/c/stale",
      url: "https://chatgpt.com/c/stale",
      site: "chatgpt",
      browserSessionRef: sessionRef,
      browserGeneration: 8,
    },
  }]);

  const recovered = await recover(sessionRef, 7);
  assert.equal(recovered.target, null);
  assert.equal(recovered.ambiguous, false);
  assert.equal(recovered.observedGeneration, 8);
  assert.equal(browserSessionTargets.has(sessionRef), false);
});

test("page identity handshake lazily recovers only opaque Browser Registry identity", () => {
  const identitySegment = wakeSource.slice(
    wakeSource.indexOf('if (msg?.type === "h2w_get_convkey")'),
    wakeSource.indexOf('if (msg?.type === "h2w_snapshot_turn")'),
  );
  assert.match(identitySegment, /registerCurrentConversation\("identity-recovery"\)/);
  assert.match(identitySegment, /return true;/);
  assert.match(identitySegment, /const identityMatchesRoute = Boolean\(convKey && registeredConvKey === convKey\)/);
  assert.match(identitySegment, /browserSessionRef:\s*identityMatchesRoute \? registeredBrowserSessionRef : null/);
  assert.match(identitySegment, /browserGeneration:\s*identityMatchesRoute \? registeredBrowserGeneration : null/);
  assert.match(wakeSource, /registeredBrowserSessionRef\s*=\s*null;\s*\n\s*registeredBrowserGeneration\s*=\s*null;/);
  assert.doesNotMatch(identitySegment, /accountNativeIdentity|email|userId/i);
});

test("terminal stale session reservations do not block ordinary browser identity recovery", () => {
  const registerStart = backgroundSource.indexOf('if (msg?.type === "h2w_register")');
  const registerEnd = backgroundSource.indexOf('if (msg?.type === "h2w_insert_main")', registerStart);
  const segment = backgroundSource.slice(registerStart, registerEnd);
  for (const code of [
    "browser_session_reservation_not_found",
    "browser_session_reservation_not_pending",
    "browser_session_reservation_expired",
    "stale_capability_generation",
  ]) {
    assert.match(segment, new RegExp(code));
  }
  assert.match(segment, /reservationRef:\s*null/);
  assert.doesNotMatch(segment, /browser_session_materialization_conflict[\s\S]*reservationRef:\s*null/);
});

test("conversation registration fences route changes before and after async background registration", () => {
  const send = registrationSource.indexOf("const response = await sendBg({");
  const fences = [...registrationSource.matchAll(/registrationAttempt !== browserRegistrationAttempt \|\| ADAPTER\.getConversationKey\(\) !== convKey/g)].map((match) => match.index);
  assert.equal(fences.length, 2, "registration must fence route drift on both sides of sendBg");
  assert.ok(fences[0] < send, "route drift during account lookup must stop before registration send");
  assert.ok(fences[1] > send, "stale registration response must not overwrite the current route identity");
});

test("newer same-route browser registration wins when responses arrive out of order", async () => {
  const harness = registrationHarness();
  const first = harness.register("first");
  await flushRegistrationToSend(harness);
  const second = harness.register("second");
  for (let i = 0; i < 4; i += 1) await Promise.resolve();
  assert.equal(harness.pending.length, 2);

  harness.pending[1].resolve({ browser_session_ref: "br_new", browser_generation: 9, bound: false });
  await second;
  assert.deepEqual(harness.state(), {
    convKey: "https://claude.ai/chat/aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
    sessionRef: "br_new",
    generation: 9,
    attempt: 2,
  });

  harness.pending[0].resolve({ browser_session_ref: "br_old_late", browser_generation: 8, bound: false });
  await first;
  assert.equal(harness.state().sessionRef, "br_new");
  assert.equal(harness.state().generation, 9);
});

test("A to B to A registration cannot resurrect an older A response", async () => {
  const a = "https://claude.ai/chat/aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
  const b = "https://claude.ai/chat/bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
  const harness = registrationHarness(a);

  const oldA = harness.register("old-a");
  await flushRegistrationToSend(harness);
  harness.setRoute(b);
  const pendingB = harness.register("b");
  for (let i = 0; i < 4; i += 1) await Promise.resolve();
  harness.setRoute(a);
  const newA = harness.register("new-a");
  for (let i = 0; i < 4; i += 1) await Promise.resolve();
  assert.equal(harness.pending.length, 3);

  harness.pending[2].resolve({ browser_session_ref: "br_a_new", browser_generation: 12, bound: false });
  await newA;
  harness.pending[0].resolve({ browser_session_ref: "br_a_old", browser_generation: 10, bound: false });
  harness.pending[1].resolve({ browser_session_ref: "br_b_late", browser_generation: 11, bound: false });
  await Promise.all([oldA, pendingB]);
  assert.equal(harness.state().convKey, a);
  assert.equal(harness.state().sessionRef, "br_a_new");
  assert.equal(harness.state().generation, 12);
});

test("ChatGPT session.open is the only supported existing-view open", () => {
  assert.match(backgroundSource, /const capabilities = browserProviderCapabilities\(provider\)/);
  assert.match(backgroundSource, /capabilities,\s*observed_at:/);
  assert.match(backgroundSource, /input_modalities:\s*\["text"\]/);
  assert.match(backgroundSource, /attachment_count:\s*\{\s*status:\s*"known",\s*max:\s*0/);
  assert.match(backgroundSource, /provider_message_chars:\s*\{\s*status:\s*"unknown"/);
  assert.match(backgroundSource, /provider_model_reasoning_combinations:\s*\{\s*status:\s*"unknown"/);
  assert.match(backgroundSource, /"session\.open"/);
  assert.match(wakeSource, /herdr_mcp\.browser_session\.open/);
  // No provider except chatgpt should ever reach the open postcondition.
  assert.match(wakeSource, /if \(ADAPTER\.name !== "chatgpt"\)\s*\{\s*return \{[^}]*resource_available:\s*false/);
});

test("adapter capability reprobe advances observation generation on snapshot change", () => {
  assert.match(backgroundSource, /const browserCapabilitySnapshots = new Map\(\)/);
  assert.match(backgroundSource, /getBrowserObservationGeneration\(provider, capabilities\)/);
  assert.match(backgroundSource, /const previous = browserCapabilitySnapshots\.get\(provider\)/);
  assert.match(backgroundSource, /previous !== undefined && previous !== snapshot/);
  assert.match(
    backgroundSource,
    /browserObservationGeneration = Math\.max\(browserObservationGeneration \+ 1, Date\.now\(\)\)/,
  );
  assert.match(backgroundSource, /browserCapabilitySnapshots\.set\(provider, snapshot\)/);
});

test("background session.open recovers unique target via recoverBrowserSessionTarget and activates without message insert", () => {
  const start = backgroundSource.indexOf('if (operation === "herdr_mcp.browser_session.open")');
  assert.ok(start >= 0, "session.open branch must exist in handleBrowserActuation");
  const segment = backgroundSource.slice(start, backgroundSource.indexOf("\n  const sessionRef = String(params.session_ref", start));
  assert.match(segment, /recoverBrowserSessionTarget/);
  assert.match(segment, /chrome\.tabs\.update.*active:\s*true.*autoDiscardable:\s*false/);
  assert.match(segment, /protectBoundTab/);
  assert.doesNotMatch(segment, /insertMainWorld|performWake|tabs\.reload|executeScript/);
  // Must fail closed on missing/duplicate/stale/provider mismatch.
  assert.match(segment, /observedGeneration/);
  assert.match(segment, /providerOpen !== "chatgpt"/);
});

test("content script session.open verifies identity, generation, route, canonical readiness and never submits", () => {
  const start = wakeSource.indexOf('if (command?.operation === "herdr_mcp.browser_session.open")');
  assert.ok(start >= 0, "wake must handle session.open");
  const stopStart = wakeSource.indexOf('if (command?.operation === "herdr_mcp.browser_dispatch.stop")', start);
  const segment = wakeSource.slice(start, stopStart >= 0 ? stopStart : start + 3000);
  assert.match(segment, /registeredBrowserSessionRef/);
  assert.match(segment, /registeredBrowserGeneration/);
  assert.match(segment, /registeredConvKey/);
  assert.match(segment, /providerCanonicalConversationObserved/);
  assert.match(segment, /ADAPTER\.getConversationKey/);
  assert.doesNotMatch(segment, /performWake|insertMainWorld|dispatchEnterSubmit|findSendButton|isTurnInProgress/);
  // Must set the session.open postcondition evidence without message fields.
  assert.match(segment, /stable_resource_ref_observed\s*=\s*true/);
  assert.match(segment, /lifecycle_observed\s*=\s*true/);
  assert.match(segment, /canonical_url_observed\s*=\s*true/);
  assert.match(segment, /command_accepted\s*=\s*true/);
});

test("accepted ChatGPT user turns register a current-source identity before continuity binding", () => {
  const start = backgroundSource.indexOf('if (msg?.type === "h2w_turn_started")');
  const end = backgroundSource.indexOf('if (msg?.type === "h2w_turn_ended")', start);
  assert.ok(start >= 0 && end > start, "turn-start handler must remain extractable");
  const segment = backgroundSource.slice(start, end);
  const sourceObserve = segment.indexOf('operation: "source_turn.observe"');
  const bindingLookup = segment.indexOf('loadBindings()');
  assert.ok(sourceObserve >= 0, "accepted turn must register current-source identity");
  assert.ok(bindingLookup > sourceObserve, "source identity must not depend on continuity binding");
  assert.match(segment, /canonical_url:\s*convKey/);
  assert.match(segment, /user_text:\s*userText/);
});

test("ChatGPT session.archive targets the exact registered session and verifies provider archive state", () => {
  assert.match(backgroundSource, /"session\.archive"/);
  const start = wakeSource.indexOf("const PENDING_SELF_ARCHIVE_STORAGE_KEY");
  const end = wakeSource.indexOf("async function performBrowserActuationCommand", start);
  assert.ok(start >= 0 && end > start, "deferred self-archive helper must exist before browser actuation");
  const segment = wakeSource.slice(start, end);
  assert.match(segment, /sessionRef !== registeredBrowserSessionRef/);
  assert.match(segment, /registeredBrowserGeneration/);
  assert.match(segment, /isTurnInProgress\(\)/);
  assert.match(segment, /openChatGptArchiveMenu/);
  assert.match(segment, /is_archived === true/);
  assert.match(segment, /stable_resource_ref_observed = true/);
  assert.match(segment, /lifecycle_observed = true/);
  assert.doesNotMatch(segment, /performWake|dispatchEnterSubmit|findSendButton|delete/);
});

test("durable self-archive is runtime-claimed after turn end or idle reload", () => {
  const drainStart = backgroundSource.indexOf("async function drainDurableSelfArchive");
  const drainEnd = backgroundSource.indexOf("\nfunction browserProviderCapabilities", drainStart);
  assert.ok(drainStart >= 0 && drainEnd > drainStart, "durable archive drain helper must exist");
  const drain = backgroundSource.slice(drainStart, drainEnd);
  assert.match(drain, /operation:\s*"archive\.claim"/);
  assert.match(drain, /claimAttempt = Number\(claim\.claim_attempt/);
  const beginStart = backgroundSource.indexOf("async function beginDurableSelfArchive");
  const completeStart = backgroundSource.indexOf("async function completeDurableSelfArchive");
  const completeEnd = backgroundSource.indexOf("\nasync function drainDurableSelfArchive", completeStart);
  assert.ok(beginStart >= 0 && beginStart < completeStart, "durable archive begin helper must precede completion helper");
  assert.match(backgroundSource.slice(beginStart, completeStart), /operation:\s*"archive\.begin"/);
  assert.ok(completeStart >= 0 && completeEnd > completeStart, "durable archive completion helper must exist");
  assert.match(backgroundSource.slice(completeStart, completeEnd), /operation:\s*"archive\.complete"/);
  assert.match(drain, /operation:\s*"herdr_mcp\.browser_session\.archive"/);
  assert.match(drain, /durable_archive:\s*true/);
  assert.match(drain, /idempotency_key:\s*archiveRef/);
  const beginCall = drain.indexOf("await beginDurableSelfArchive(");
  const sendCall = drain.indexOf("sendBrowserActuationTabMessage(tabId");
  assert.ok(beginCall >= 0 && sendCall > beginCall, "durable no-replay fence must commit before browser send");
  assert.match(drain, /command_accepted:\s*true[\s\S]*resource_available:\s*true/);
  assert.match(drain, /DURABLE_ARCHIVE_CLAIM_RECOVERY_MS/);
  assert.match(drain, /DURABLE_ARCHIVE_RETRY_MS/);
  assert.doesNotMatch(drain, /setInterval|setTimeout/);

  const turnStart = backgroundSource.indexOf('if (msg?.type === "h2w_turn_ended")');
  const turnEnd = backgroundSource.indexOf('if (msg?.type === "h2w_handoff_start")', turnStart);
  const turnSegment = backgroundSource.slice(turnStart, turnEnd);
  assert.match(turnSegment, /drainDurableSelfArchive\(msg\?\.convKey \|\| "", sender\.tab\?\.id, "turn-ended"\)/);
  assert.ok(
    turnSegment.indexOf("drainDurableSelfArchive") < turnSegment.indexOf("handleHandoffTurnEnded"),
    "handoff completion must not short-circuit the durable archive drain",
  );

  const archiveStart = wakeSource.indexOf("async function performChatGptSessionArchive");
  const archiveEnd = wakeSource.indexOf("\n  async function performBrowserActuationCommand", archiveStart);
  const archiveSegment = wakeSource.slice(archiveStart, archiveEnd);
  assert.match(archiveSegment, /durableAuthority = command\?\.durable_archive === true/);
  assert.match(archiveSegment, /if \(durableAuthority\)[\s\S]*command_accepted:\s*false/);
  const durableBranch = archiveSegment.slice(
    archiveSegment.indexOf("if (durableAuthority)"),
    archiveSegment.indexOf("// The authoritative self-archive request"),
  );
  assert.doesNotMatch(durableBranch, /enqueuePendingSelfArchive/);

  assert.match(wakeSource, /h2w_schedule_durable_archive_retry/);
  assert.match(wakeSource, /trigger:\s*"bounded-retry"/);

  const startupStart = wakeSource.lastIndexOf("(async () => {");
  const startupEnd = wakeSource.indexOf("// ---- Idle nudge", startupStart);
  const startup = wakeSource.slice(startupStart, startupEnd);
  assert.match(startup, /!isTurnInProgress\(\)/);
  assert.match(startup, /type:\s*"h2w_durable_archive_ready"/);
  assert.match(startup, /trigger:\s*"startup-idle"/);
});

const SELF_ARCHIVE_KEY = "herdrPendingSelfArchiveV1";

async function flushUntil(predicate, turns = 24) {
  for (let i = 0; i < turns; i += 1) {
    if (predicate()) return;
    await Promise.resolve();
  }
}

function withTimeout(promise, ms, label) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`bounded-wait timeout: ${label}`)), ms);
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

function selfArchiveHarness(overrides = {}) {
  const { storage: sharedStorage, failWrites = false, ...ctxOverrides } = overrides;
  const ctx = {
    turnInProgress: true,
    generation: 7,
    sessionRef: "br_self_archive",
    conversationId: "conv-self-archive",
    archiveAvailable: true,
    archiveVerifies: true,
    archiveClickThrows: false,
    onArchiveClick: null,
    verifyGate: null,
    menuGate: null,
    clicks: [],
    ...ctxOverrides,
  };
  const storage = sharedStorage instanceof Map ? sharedStorage : new Map();
  const writeState = { fail: failWrites };
  const sessionStorage = {
    getItem: (key) => (storage.has(key) ? storage.get(key) : null),
    setItem: (key, value) => {
      if (writeState.fail) throw new Error("sessionStorage write failed");
      storage.set(key, String(value));
    },
    removeItem: (key) => {
      if (writeState.fail) throw new Error("sessionStorage write failed");
      storage.delete(key);
    },
  };
  // Virtual clock: the unverified archive path polls to a 6s deadline, which
  // would otherwise make every uncertainty test take real seconds.
  const clock = { value: 1_000_000 };
  const dateShim = {
    now: () => clock.value,
    advance: (ms) => { clock.value += Number(ms) || 0; },
  };
  const start = wakeSource.indexOf("const PENDING_SELF_ARCHIVE_STORAGE_KEY");
  const end = wakeSource.indexOf("async function performBrowserActuationCommand", start);
  assert.ok(start >= 0 && end > start, "deferred self-archive helper must remain extractable");
  const segment = wakeSource.slice(start, end);
  const api = new Function("ctx", "sessionStorage", "setTimeout", "Date", `
    const ADAPTER = { name: "chatgpt" };
    const chatGptConversationId = () => ctx.conversationId;
    const isTurnInProgress = () => ctx.turnInProgress;
    const openChatGptArchiveMenu = async () => {
      if (ctx.menuGate) await ctx.menuGate;
      return ctx.archiveAvailable
        ? { click: () => {
            ctx.clicks.push(Date.now());
            if (typeof ctx.onArchiveClick === "function") ctx.onArchiveClick();
            if (ctx.archiveClickThrows) throw new Error("archive click failed after dispatch");
          } }
        : null;
    };
    const fetchChatGptConversation = async () => {
      if (ctx.verifyGate) await ctx.verifyGate;
      return ctx.archiveVerifies ? { ok: true, body: { is_archived: true } } : { ok: false };
    };
    const wait = (ms) => { Date.advance(ms); return Promise.resolve(); };
    let registeredBrowserSessionRef = ctx.sessionRef;
    let registeredBrowserGeneration = ctx.generation;
    ${segment}
    return {
      performChatGptSessionArchive,
      drain: drainPendingSelfArchives,
      pendingCount: () => readPendingSelfArchives().length,
      setGeneration: (value) => { registeredBrowserGeneration = value; },
      setSession: (value) => { registeredBrowserSessionRef = value; },
    };
  `)(ctx, sessionStorage, () => 0, dateShim);
  return { ctx, api, storage, writeState };
}

test("deferred self-archive waits for idle, dedupes idempotently, and fails closed on generation drift", async () => {
  const sessionRef = "br_self_archive";
  const { ctx, api } = selfArchiveHarness();
  const command = {
    operation: "herdr_mcp.browser_session.archive",
    expected_generation: 7,
    params: { session_ref: sessionRef, expected_generation: 7, idempotency_key: "archive-key-1" },
  };
  const evidence = () => ({ observed_generation: 7 });

  // While the assistant's own turn is active the request is accepted but must
  // not archive early.
  const accepted = await api.performChatGptSessionArchive(command, evidence());
  assert.equal(accepted.command_accepted, true);
  assert.equal(accepted.stable_resource_ref_observed, true);
  assert.equal(accepted.lifecycle_observed, false);
  assert.notEqual(accepted.rejected, true);
  assert.equal(ctx.clicks.length, 0);
  assert.equal(api.pendingCount(), 1);

  // A duplicate idempotent request must not enqueue or actuate a second archive.
  const duplicate = await api.performChatGptSessionArchive(command, evidence());
  assert.equal(duplicate.command_accepted, true);
  assert.equal(ctx.clicks.length, 0);
  assert.equal(api.pendingCount(), 1);

  // Once the source turn is idle the persisted archive executes exactly once.
  ctx.turnInProgress = false;
  await api.drain();
  assert.equal(ctx.clicks.length, 1);
  assert.equal(api.pendingCount(), 0);

  // A stale generation drops the persisted intent without actuation.
  ctx.turnInProgress = true;
  await api.performChatGptSessionArchive(command, evidence());
  assert.equal(api.pendingCount(), 1);
  api.setGeneration(8);
  ctx.turnInProgress = false;
  await api.drain();
  assert.equal(ctx.clicks.length, 1);
  assert.equal(api.pendingCount(), 0);
});

test("an unverified self-archive click is terminal and is never replayed by later drains", async () => {
  const { ctx, api } = selfArchiveHarness({ archiveVerifies: false });
  const command = {
    operation: "herdr_mcp.browser_session.archive",
    expected_generation: 7,
    params: { session_ref: ctx.sessionRef, expected_generation: 7, idempotency_key: "archive-uncertain-1" },
  };
  const evidence = () => ({ observed_generation: 7 });

  ctx.turnInProgress = true;
  const accepted = await api.performChatGptSessionArchive(command, evidence());
  assert.equal(accepted.command_accepted, true);
  assert.equal(accepted.lifecycle_observed, false);
  assert.equal(api.pendingCount(), 1);

  ctx.turnInProgress = false;
  await api.drain();
  assert.equal(ctx.clicks.length, 1, "the first drain dispatches exactly one archive click");
  // The provider never confirmed the archive, so the outcome stays unverified
  // and the intent stays terminal instead of being dropped as applied.
  assert.equal(api.pendingCount(), 1);

  await api.drain();
  await api.drain();
  assert.equal(ctx.clicks.length, 1, "no later drain may replay an already-clicked archive");

  // The same idempotency key re-entering the intent queue stays idempotent.
  ctx.turnInProgress = true;
  const reentry = await api.performChatGptSessionArchive(command, evidence());
  assert.equal(reentry.command_accepted, true);
  assert.equal(reentry.lifecycle_observed, false);
  ctx.turnInProgress = false;
  await api.drain();
  assert.equal(ctx.clicks.length, 1, "same-key re-entry must not dispatch a second click");

  // The existing identity fence still drops a terminal intent when the
  // generation is superseded, without actuating it.
  api.setGeneration(8);
  await api.drain();
  assert.equal(ctx.clicks.length, 1);
  assert.equal(api.pendingCount(), 0);
});

test("a post-click exception and a reload leave the archive intent terminal, not retryable", async () => {
  const storage = new Map();
  const command = {
    operation: "herdr_mcp.browser_session.archive",
    expected_generation: 7,
    params: { session_ref: "br_self_archive", expected_generation: 7, idempotency_key: "archive-throw-1" },
  };
  const evidence = () => ({ observed_generation: 7 });

  const throwing = selfArchiveHarness({ archiveVerifies: false, archiveClickThrows: true, storage });
  throwing.ctx.turnInProgress = true;
  await throwing.api.performChatGptSessionArchive(command, evidence());
  throwing.ctx.turnInProgress = false;
  await throwing.api.drain();
  assert.equal(throwing.ctx.clicks.length, 1, "the archive click is dispatched exactly once");
  await throwing.api.drain();
  assert.equal(throwing.ctx.clicks.length, 1, "a click that threw after dispatch must not be replayed");

  // Reload: a fresh page instance restores the same sessionStorage intent and
  // must not actuate it again.
  const reloaded = selfArchiveHarness({ archiveVerifies: false, storage });
  reloaded.ctx.turnInProgress = false;
  await reloaded.api.drain();
  assert.equal(reloaded.ctx.clicks.length, 0, "a persisted delivered intent must survive reload without replay");
});

test("a self-archive that is clearly not delivered before the click stays retryable", async () => {
  const { ctx, api } = selfArchiveHarness({ archiveAvailable: false, archiveVerifies: false });
  const command = {
    operation: "herdr_mcp.browser_session.archive",
    expected_generation: 7,
    params: { session_ref: ctx.sessionRef, expected_generation: 7, idempotency_key: "archive-retry-1" },
  };
  const evidence = () => ({ observed_generation: 7 });

  ctx.turnInProgress = true;
  await api.performChatGptSessionArchive(command, evidence());
  ctx.turnInProgress = false;
  await api.drain();
  assert.equal(ctx.clicks.length, 0);
  assert.equal(api.pendingCount(), 1, "a rejected pre-click attempt must stay retryable");

  ctx.archiveAvailable = true;
  await api.drain();
  assert.equal(ctx.clicks.length, 1);
  assert.equal(api.pendingCount(), 1, "the retried click is now terminal and unverified");
  await api.drain();
  assert.equal(ctx.clicks.length, 1, "a terminal retry is not replayed either");
});

test("the delivery claim is durable before the archive click is dispatched", { timeout: 3000 }, async () => {
  const storage = new Map();
  let persistedAtClick = null;
  const harness = selfArchiveHarness({
    archiveVerifies: false,
    storage,
    onArchiveClick: () => { persistedAtClick = storage.get(SELF_ARCHIVE_KEY); },
  });
  const command = {
    operation: "herdr_mcp.browser_session.archive",
    expected_generation: 7,
    params: { session_ref: harness.ctx.sessionRef, expected_generation: 7, idempotency_key: "archive-preclick-1" },
  };
  const evidence = () => ({ observed_generation: 7 });

  harness.ctx.turnInProgress = true;
  await harness.api.performChatGptSessionArchive(command, evidence());
  harness.ctx.turnInProgress = false;
  await harness.api.drain();
  assert.equal(harness.ctx.clicks.length, 1);

  // The click callback observes sessionStorage as it is at dispatch time.
  const atClick = JSON.parse(String(persistedAtClick || "[]"));
  assert.equal(atClick.length, 1);
  assert.equal(atClick[0].idempotencyKey, "archive-preclick-1");
  assert.equal(atClick[0].deliveryAttempted, true, "the delivery claim must persist before the click");

  // Reload inside the click window: a fresh page reading the same
  // sessionStorage must not replay the already-clicked intent.
  const reloaded = selfArchiveHarness({ archiveVerifies: false, storage });
  reloaded.ctx.turnInProgress = false;
  await reloaded.api.drain();
  assert.equal(reloaded.ctx.clicks.length, 0, "a click-window reload must not replay the intent");
});

test("a same-key re-entry during the in-flight archive poll cannot click again", { timeout: 3000 }, async () => {
  let releasePoll;
  const pollGate = new Promise((resolve) => { releasePoll = resolve; });
  const { ctx, api, storage } = selfArchiveHarness({ archiveVerifies: false, verifyGate: pollGate });
  const command = {
    operation: "herdr_mcp.browser_session.archive",
    expected_generation: 7,
    params: { session_ref: ctx.sessionRef, expected_generation: 7, idempotency_key: "archive-inflight-1" },
  };
  const evidence = () => ({ observed_generation: 7 });

  ctx.turnInProgress = true;
  await api.performChatGptSessionArchive(command, evidence());
  ctx.turnInProgress = false;

  const draining = withTimeout(api.drain(), 2000, "in-flight drain");
  await flushUntil(() => ctx.clicks.length > 0);
  assert.equal(ctx.clicks.length, 1, "the first drain dispatches one click before the poll settles");

  // The click is mid-verification. The direct path for the same key must see the
  // durable claim and must not dispatch a second click. The poll gate is always
  // released so a failing assertion here cannot leave the gated drain pending.
  const reentryPromise = api.performChatGptSessionArchive(command, evidence());
  try {
    await flushUntil(() => ctx.clicks.length > 1);
    assert.equal(ctx.clicks.length, 1, "in-flight same-key re-entry must not click");
  } finally {
    releasePoll();
  }
  const reentry = await withTimeout(reentryPromise, 1000, "in-flight same-key re-entry");
  assert.equal(reentry.command_accepted, true);
  assert.equal(reentry.lifecycle_observed, false);
  await draining;
  assert.equal(ctx.clicks.length, 1);
  const records = JSON.parse(String(storage.get(SELF_ARCHIVE_KEY)));
  assert.equal(records[0].deliveryAttempted, true, "the claim survives same-key re-entry");
});

test("two concurrent same-key calls cannot both click the archive", { timeout: 3000 }, async () => {
  let releaseMenu;
  const menuGate = new Promise((resolve) => { releaseMenu = resolve; });
  const { ctx, api } = selfArchiveHarness({ archiveVerifies: false, menuGate });
  const command = {
    operation: "herdr_mcp.browser_session.archive",
    expected_generation: 7,
    params: { session_ref: ctx.sessionRef, expected_generation: 7, idempotency_key: "archive-concurrent-1" },
  };
  const evidence = () => ({ observed_generation: 7 });

  ctx.turnInProgress = true;
  await api.performChatGptSessionArchive(command, evidence());
  ctx.turnInProgress = false;

  // Both calls pass the pre-menu guard, then park on the gated menu await.
  const draining = api.drain();
  const direct = api.performChatGptSessionArchive(command, evidence());
  await flushUntil(() => ctx.clicks.length > 0, 8);
  assert.equal(ctx.clicks.length, 0, "neither call may click while the menu is unresolved");

  let directResult;
  try {
    releaseMenu();
    directResult = await withTimeout(direct, 1000, "concurrent direct call");
    await withTimeout(draining, 1000, "concurrent drain");
  } finally {
    releaseMenu();
  }
  assert.equal(ctx.clicks.length, 1, "exactly one concurrent same-key call may click");
  assert.equal(directResult.command_accepted, true);
  assert.notEqual(directResult.rejected, true);
  assert.equal(directResult.lifecycle_observed, false);
});

test("an unpersistable delivery claim suppresses the archive click", { timeout: 3000 }, async () => {
  const { ctx, api, writeState } = selfArchiveHarness({ archiveVerifies: false });
  const command = {
    operation: "herdr_mcp.browser_session.archive",
    expected_generation: 7,
    params: { session_ref: ctx.sessionRef, expected_generation: 7, idempotency_key: "archive-storage-1" },
  };
  const evidence = () => ({ observed_generation: 7 });

  ctx.turnInProgress = true;
  await api.performChatGptSessionArchive(command, evidence());
  assert.equal(api.pendingCount(), 1);
  ctx.turnInProgress = false;

  writeState.fail = true;
  await api.drain();
  assert.equal(ctx.clicks.length, 0, "a delivery claim that cannot be persisted must not click");
  assert.equal(api.pendingCount(), 1);

  writeState.fail = false;
  await api.drain();
  assert.equal(ctx.clicks.length, 1, "the archive proceeds once the claim is durable");

  // The direct path must also refuse an unpersistable mutation.
  const direct = selfArchiveHarness({ archiveVerifies: false, failWrites: true });
  const rejected = await direct.api.performChatGptSessionArchive(command, evidence());
  assert.equal(rejected.rejected, true);
  assert.equal(direct.ctx.clicks.length, 0);
});

test("ChatGPT session.open can restore a disposable view from a local canonical locator", () => {
  const start = backgroundSource.indexOf('if (operation === "herdr_mcp.browser_session.open")');
  assert.ok(start >= 0, "session.open branch must exist in handleBrowserActuation");
  const end = backgroundSource.indexOf("\n  const sessionRef = String(params.session_ref", start);
  const segment = backgroundSource.slice(start, end >= 0 ? end : start + 9000);
  assert.match(segment, /recoverBrowserSessionTarget/);
  assert.match(segment, /canonical_url/);
  assert.match(segment, /chrome\.tabs\.create\(\{ url: canonicalUrl, active: true \}\)/);
  assert.match(segment, /chrome\.tabs\.update\(targetOpen\.tabId, \{ active: true, autoDiscardable: false \}\)/);
  assert.doesNotMatch(segment, /performWake|insertMainWorld|dispatchEnterSubmit|findSendButton/);

  const contentStart = wakeSource.indexOf('if (command?.operation === "herdr_mcp.browser_session.open")');
  const contentEnd = wakeSource.indexOf('if (command?.operation === "herdr_mcp.browser_dispatch.stop")', contentStart);
  const contentSegment = wakeSource.slice(contentStart, contentEnd);
  assert.match(contentSegment, /registeredBrowserSessionRef/);
  assert.match(contentSegment, /registeredBrowserGeneration/);
  assert.match(contentSegment, /providerCanonicalConversationObserved/);
  assert.match(contentSegment, /canonical_url_observed\s*=\s*true/);
  assert.doesNotMatch(contentSegment, /performWake|findSendButton|dispatchEnterSubmit/);
});

test("ChatGPT session.archive can reopen its durable canonical URL when the target tab is closed", () => {
  const start = backgroundSource.indexOf('const sessionRef = String(params.session_ref || "")');
  const end = backgroundSource.indexOf('const response = await sendBrowserActuationTabMessage(target.tabId', start);
  assert.ok(start >= 0 && end > start, "archive target routing block must remain extractable");
  const segment = backgroundSource.slice(start, end);
  assert.match(segment, /operation === "herdr_mcp\.browser_session\.archive"/);
  assert.match(segment, /const canonicalUrl = String\(params\.canonical_url \|\| ""\)/);
  assert.match(segment, /browserConversationInfo\(providerArchive, canonicalUrl\)/);
  assert.match(segment, /chrome\.tabs\.create\(\{ url: canonicalUrl, active: true \}\)/);
  assert.match(segment, /browserSessionTargets\.get\(sessionRef\)/);
  assert.match(segment, /Date\.now\(\) \+ 8000/);
  assert.match(segment, /createdTab\?\.id/);
});

test("browser dispatch evicts a stale cached target before exact recovery", () => {
  const start = backgroundSource.indexOf('const sessionRef = String(params.session_ref || "")');
  const end = backgroundSource.indexOf('const response = await sendBrowserActuationTabMessage(target.tabId', start);
  assert.ok(start >= 0 && end > start, "dispatch target routing block must remain extractable");
  const segment = backgroundSource.slice(start, end);
  assert.match(segment, /cachedTargetCurrent/);
  assert.match(segment, /cachedLive\?\.convKey === target\.convKey/);
  assert.match(segment, /browserSessionTargets\.delete\(sessionRef\);\s*target = null;/);
  const staleEviction = segment.indexOf("browserSessionTargets.delete(sessionRef)");
  const recovery = segment.indexOf("recoverBrowserSessionTarget(sessionRef, expectedGeneration)", staleEviction);
  assert.ok(staleEviction >= 0 && recovery > staleEviction, "stale cached target must be evicted before one exact recovery");
  assert.match(segment, /live\?\.convKey !== target\.convKey/);
});

test("shared native push stream self-heals when heartbeat bytes stall", () => {
  assert.match(backgroundSource, /const PUSH_STREAM_STALL_MS = 22000/);
  const start = backgroundSource.indexOf("async function runPushStream(ctrl)");
  const end = backgroundSource.indexOf("\nasync function postBrowserActuationEvidence", start);
  assert.ok(start >= 0 && end > start, "push stream loop must remain extractable");
  const segment = backgroundSource.slice(start, end);
  assert.match(segment, /const armStallWatchdog = \(\) =>/);
  assert.match(segment, /stallTimer = setTimeout\(\(\) => \{[\s\S]*?stream\?\.close\(\)/);
  assert.match(segment, /onChunk: \(bytes\) => \{\s*armStallWatchdog\(\);/);
  assert.match(segment, /noteLocalRuntimeReachability\(true\);\s*armStallWatchdog\(\);/);
  assert.match(segment, /await stream\.done;\s*disarmStallWatchdog\(\);/);
  assert.match(segment, /finally \{[\s\S]*?if \(stallTimer\) clearTimeout\(stallTimer\)/);
});

test("ChatGPT session.create carries one durable reservation across the new-conversation route", () => {
  const start = backgroundSource.indexOf('if (operation === "herdr_mcp.browser_session.create")');
  const end = backgroundSource.indexOf('if (operation === "herdr_mcp.browser_session.open")', start);
  assert.ok(start >= 0 && end > start, "session.create branch must precede session.open");
  const segment = backgroundSource.slice(start, end);
  assert.match(segment, /browserTabScopes\.get\(createdTab\.id\)/);
  assert.match(segment, /scope\.accountRef === accountRefCreate/);
  assert.match(segment, /scope\.spaceRef === spaceRefCreate/);
  assert.match(segment, /chrome\.tabs\.create\(\{ url: launchUrl, active: true \}\)/);
  assert.match(segment, /reservationRef/);

  const createStart = wakeSource.indexOf('const creatingSession = command?.operation === "herdr_mcp.browser_session.create"');
  const createEnd = wakeSource.indexOf("\n  // Browser Registry identity cached by the page script", createStart);
  const createSegment = wakeSource.slice(createStart, createEnd);
  assert.match(createSegment, /sessionStorage\.setItem\(BROWSER_SESSION_RESERVATION_STORAGE_KEY, reservationRef\)/);
  assert.match(createSegment, /const composerReadyDeadline = Date\.now\(\) \+ 8000/);
  assert.match(createSegment, /while \(!ADAPTER\.getInputEl\(\) && Date\.now\(\) < composerReadyDeadline\)/);
  assert.match(createSegment, /await wait\(200\)/);
  assert.ok(
    createSegment.indexOf("const composerReadyDeadline") < createSegment.indexOf("if (isTurnInProgress() || ADAPTER.inputHasContent())"),
    "fresh-session composer readiness must settle before the normal busy guard",
  );
  assert.match(createSegment, /registerCurrentConversation\("browser-session-create"\)/);
  assert.match(createSegment, /registeredBrowserSessionRef/);
  const createRegistration = createSegment.indexOf("registerCurrentConversation(\"browser-session-create\")");
  const createSettlementAssignment = createSegment.indexOf("acceptedDispatchAssignments.set(registeredBrowserSessionRef");
  assert.ok(
    createRegistration >= 0 && createSettlementAssignment > createRegistration,
    "session.create must register its reservation-backed session before storing the accepted dispatch settlement assignment",
  );
  assert.doesNotMatch(createSegment, /reasoning != null \|\| requiredApps\.length > 0/);
  assert.match(createSegment, /ensureRequiredComposerApps\(requiredApps\)/);
  assert.match(createSegment, /evidence\.required_apps_readback = appSelection\.apps/);
  assert.match(createSegment, /requiredApps,/);

  const registrationStart = wakeSource.indexOf('async function registerCurrentConversation');
  const registrationSegment = wakeSource.slice(registrationStart, registrationStart + 3500);
  assert.match(registrationSegment, /browserSessionReservationRef/);
  assert.match(registrationSegment, /sessionStorage\.removeItem\(BROWSER_SESSION_RESERVATION_STORAGE_KEY\)/);
});

test("ChatGPT required_apps selects a real composer app pill and fails closed on ambiguity", () => {
  assert.match(backgroundSource, /"composer\.select_tool"/);
  assert.match(chatGptAdapterSource, /#composer-plus-btn/);
  assert.match(chatGptAdapterSource, /data-testid="composer-plus-btn"/);
  assert.match(chatGptAdapterSource, /data-inline-selection-pill/);
  assert.match(chatGptAdapterSource, /data-symbol="ecosystemMention"/);
  assert.match(chatGptAdapterSource, /data-keyword/);
  assert.match(chatGptAdapterSource, /leaf\.closest\('\[tabindex="0"\]'\)/);
  assert.match(wakeSource, /candidates\.length !== 1/);
  assert.match(wakeSource, /required-app-ambiguous/);
  assert.match(wakeSource, /required-app-not-found/);
  assert.match(wakeSource, /composerHasOnlyAppPills\(data\.requiredApps\)/);
});
