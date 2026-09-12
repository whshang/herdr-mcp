import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const backgroundSource = readFileSync(path.join(__dirname, "..", "extension", "background.js"), "utf8");
const wakeSource = readFileSync(path.join(__dirname, "..", "extension", "content", "wake.js"), "utf8");
const chatGptAdapterSource = readFileSync(path.join(__dirname, "..", "extension", "content", "injector", "chatgpt.js"), "utf8");

const recoveryStart = backgroundSource.indexOf("async function recoverBrowserSessionTarget(");
const recoveryEnd = backgroundSource.indexOf("\nasync function handleBrowserActuation", recoveryStart);
assert.ok(recoveryStart >= 0 && recoveryEnd > recoveryStart, "recovery helper must remain extractable");
const recoverySource = backgroundSource.slice(recoveryStart, recoveryEnd);

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

test("page identity handshake exposes only opaque Browser Registry recovery identity", () => {
  assert.match(wakeSource, /const identityMatchesRoute = Boolean\(convKey && registeredConvKey === convKey\)/);
  assert.match(wakeSource, /browserSessionRef:\s*identityMatchesRoute \? registeredBrowserSessionRef : null/);
  assert.match(wakeSource, /browserGeneration:\s*identityMatchesRoute \? registeredBrowserGeneration : null/);
  assert.match(wakeSource, /registeredBrowserSessionRef\s*=\s*null;\s*\n\s*registeredBrowserGeneration\s*=\s*null;/);
  assert.doesNotMatch(
    wakeSource.slice(wakeSource.indexOf('if \(msg?.type === "h2w_get_convkey"\)'), wakeSource.indexOf('if \(msg?.type === "h2w_snapshot_turn"\)')),
    /accountNativeIdentity|email|userId/i,
  );
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
  assert.match(backgroundSource, /capabilities:\s*\{\s*operations:\s*provider === "chatgpt"/);
  assert.match(backgroundSource, /"session\.open"/);
  assert.match(wakeSource, /herdr_mcp\.browser_session\.open/);
  // No provider except chatgpt should ever reach the open postcondition.
  assert.match(wakeSource, /if \(ADAPTER\.name !== "chatgpt"\)\s*\{\s*return \{[^}]*resource_available:\s*false/);
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

test("ChatGPT session.archive targets the exact registered session and verifies provider archive state", () => {
  assert.match(backgroundSource, /"session\.archive"/);
  const start = wakeSource.indexOf("async function performChatGptSessionArchive");
  const end = wakeSource.indexOf("async function performBrowserActuationCommand", start);
  assert.ok(start >= 0 && end > start, "session.archive helper must exist before browser actuation");
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
