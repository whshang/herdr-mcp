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

const projectPageInfoStart = backgroundSource.indexOf("function browserProjectPageInfoFromSupportedUrl(");
const projectPageInfoEnd = backgroundSource.indexOf("\nfunction browserPageContextInfoFromSupportedUrl", projectPageInfoStart);
assert.ok(projectPageInfoStart >= 0 && projectPageInfoEnd > projectPageInfoStart, "provider project page helper must remain extractable");
const projectPageInfoSource = backgroundSource.slice(projectPageInfoStart, projectPageInfoEnd);
const browserProjectPageInfoFromSupportedUrl = new Function(
  `${projectPageInfoSource}; return browserProjectPageInfoFromSupportedUrl;`,
)();

const canonicalRecoveryStart = backgroundSource.indexOf("async function findBrowserSessionTargetByCanonicalIdentity(");
const recoveryStart = backgroundSource.indexOf("async function recoverBrowserSessionTarget(");
const recoveryEnd = backgroundSource.indexOf("\nasync function handleBrowserActuation", recoveryStart);
assert.ok(canonicalRecoveryStart >= 0 && recoveryStart > canonicalRecoveryStart, "canonical identity recovery helper must remain extractable");
assert.ok(recoveryStart >= 0 && recoveryEnd > recoveryStart, "recovery helper must remain extractable");
const canonicalRecoverySource = backgroundSource.slice(canonicalRecoveryStart, recoveryStart);
const recoverySource = backgroundSource.slice(recoveryStart, recoveryEnd);

const createAnchorStart = backgroundSource.indexOf("async function resolveBrowserCreateAnchorWindow(");
const createAnchorEnd = backgroundSource.indexOf("\nasync function handleBrowserActuation", createAnchorStart);
assert.ok(createAnchorStart >= 0 && createAnchorEnd > createAnchorStart, "create anchor helper must remain extractable");
const createAnchorSource = backgroundSource.slice(createAnchorStart, createAnchorEnd);

const archiveCleanupStart = backgroundSource.indexOf("function shouldCloseArchivedChatGptTab(");
const archiveCleanupEnd = backgroundSource.indexOf("\nasync function recoverBrowserSessionTarget", archiveCleanupStart);
assert.ok(archiveCleanupStart >= 0 && archiveCleanupEnd > archiveCleanupStart, "archive tab cleanup helpers must remain extractable");
const archiveCleanupSource = backgroundSource.slice(archiveCleanupStart, archiveCleanupEnd);

const projectIdentityStart = wakeSource.indexOf("async function currentAdapterProjectIdentity(convKey)");
const registrationStart = projectIdentityStart;
const registrationEnd = wakeSource.indexOf("\n  function startConversationRouteWatch()", registrationStart);
assert.ok(registrationStart >= 0 && registrationEnd > registrationStart, "registration helper must remain extractable");
const registrationSource = wakeSource.slice(registrationStart, registrationEnd);
const projectIdentityEnd = wakeSource.indexOf(
  '\n  async function registerCurrentConversation(reason = "startup")',
  projectIdentityStart,
);
assert.ok(projectIdentityStart >= 0 && projectIdentityEnd > projectIdentityStart, "provider project identity helper must remain extractable");
const projectIdentitySource = wakeSource.slice(projectIdentityStart, projectIdentityEnd);

function delayedClaudeProjectIdentityHarness(sequence) {
  let index = 0;
  const convKey = "https://claude.ai/chat/aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
  const ADAPTER = {
    name: "claude",
    getConversationKey: () => convKey,
    getProjectIdentity: () => sequence[Math.min(index, sequence.length - 1)] ?? null,
  };
  const wait = async () => { index += 1; };
  const observe = new Function(
    "ADAPTER",
    "wait",
    `${projectIdentitySource}; return currentAdapterProjectIdentity;`,
  )(ADAPTER, wait);
  return { observe, convKey, calls: () => index + 1 };
}

function canonicalIdentityRecoveryHarness(tabRecords, scopeRecords = new Map()) {
  const queryArgs = [];
  const chrome = {
    tabs: {
      async query(args) {
        queryArgs.push(args);
        return tabRecords.map(({ id, url, status = "complete" }) => ({ id, url, status }));
      },
    },
  };
  const hostPermissionPatternForUrl = (rawUrl) => new URL(rawUrl).origin + "/*";
  const browserTabScopes = scopeRecords;
  const browserConversationInfo = (provider, rawUrl) => provider === "chatgpt"
    ? chatGptConversationInfo(rawUrl)
    : null;
  const find = new Function(
    "chrome",
    "hostPermissionPatternForUrl",
    "browserConversationInfo",
    "browserTabScopes",
    `${canonicalRecoverySource}; return findBrowserSessionTargetByCanonicalIdentity;`,
  )(chrome, hostPermissionPatternForUrl, browserConversationInfo, browserTabScopes);
  return { find, queryArgs };
}

function claudeCanonicalIdentityRecoveryHarness(tabRecords) {
  const queryArgs = [];
  const chrome = {
    tabs: {
      async query(args) {
        queryArgs.push(args);
        return tabRecords.map(({ id, url, status = "complete" }) => ({ id, url, status }));
      },
    },
  };
  const hostPermissionPatternForUrl = (rawUrl) => new URL(rawUrl).origin + "/*";
  const browserTabScopes = new Map(tabRecords
    .filter((record) => record.scope)
    .map((record) => [record.id, record.scope]));
  const browserConversationInfo = (provider, rawUrl) => {
    if (provider !== "claude") return null;
    const match = String(rawUrl || "").match(
      /^https:\/\/claude\.ai\/chat\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\/?$/i,
    );
    if (!match) return null;
    const conversationId = match[1].toLowerCase();
    return {
      site: "claude",
      conversation_id: conversationId,
      project_id: null,
      convKey: `https://claude.ai/chat/${conversationId}`,
    };
  };
  const find = new Function(
    "chrome",
    "hostPermissionPatternForUrl",
    "browserConversationInfo",
    "browserTabScopes",
    `${canonicalRecoverySource}; return findBrowserSessionTargetByCanonicalIdentity;`,
  )(chrome, hostPermissionPatternForUrl, browserConversationInfo, browserTabScopes);
  return { find, queryArgs };
}

function grokCanonicalIdentityRecoveryHarness(tabRecords) {
  const queryArgs = [];
  const chrome = {
    tabs: {
      async query(args) {
        queryArgs.push(args);
        return tabRecords.map(({ id, url, status = "complete" }) => ({ id, url, status }));
      },
    },
  };
  const hostPermissionPatternForUrl = (rawUrl) => new URL(rawUrl).origin + "/*";
  const browserTabScopes = new Map();
  const browserConversationInfo = (provider, rawUrl) => {
    if (provider !== "grok") return null;
    const url = new URL(String(rawUrl || ""));
    const match = url.pathname.match(/^\/c\/([0-9a-f-]{36})\/?$/i);
    if (!match) return null;
    const conversationId = match[1].toLowerCase();
    return {
      site: "grok",
      conversation_id: conversationId,
      project_id: null,
      convKey: url.origin + "/c/" + conversationId,
    };
  };
  const find = new Function(
    "chrome",
    "hostPermissionPatternForUrl",
    "browserConversationInfo",
    "browserTabScopes",
    `${canonicalRecoverySource}; return findBrowserSessionTargetByCanonicalIdentity;`,
  )(chrome, hostPermissionPatternForUrl, browserConversationInfo, browserTabScopes);
  return { find, queryArgs };
}

function recoveryHarness(tabRecords) {
  const browserSessionTargets = new Map();
  const queryArgs = [];
  const chrome = {
    tabs: {
      async query(args = {}) {
        queryArgs.push(args);
        const patterns = Array.isArray(args.url) ? args.url : args.url ? [args.url] : [];
        return tabRecords
          .filter(({ url }) => {
            if (!patterns.length) return true;
            const host = new URL(url).host;
            return patterns.some((pattern) => String(pattern).includes(host));
          })
          .map(({ id, url, status = "complete" }) => ({ id, url, status }));
      },
      async sendMessage(tabId, message) {
        assert.equal(message?.type, "h2w_get_convkey");
        const record = tabRecords.find((item) => item.id === tabId);
        if (!record || record.error) throw new Error("missing content listener");
        if (record.hang) return new Promise(() => {});
        return record.live;
      },
    },
  };
  const activeH2WTabUrlsForProvider = async (provider) => {
    if (provider === "claude") return ["https://claude.ai/*"];
    if (provider === "grok") return ["https://grok.com/*"];
    return ["https://chatgpt.com/*"];
  };
  const browserConversationInfoFromSupportedUrl = (rawUrl) => {
    const url = new URL(String(rawUrl || ""));
    const match = url.pathname.match(/^\/c\/([^/?#]+)/);
    if (url.hostname === "chatgpt.com" && match) {
      return {
        site: "chatgpt",
        conversation_id: match[1],
        project_id: null,
        convKey: `https://chatgpt.com/c/${match[1]}`,
      };
    }
    const claudeMatch = url.pathname.match(/^\/chat\/([^/?#]+)/);
    if (url.hostname === "claude.ai" && claudeMatch) {
      return {
        site: "claude",
        conversation_id: claudeMatch[1],
        project_id: null,
        convKey: `https://claude.ai/chat/${claudeMatch[1]}`,
      };
    }
    if (!match) return null;
    return {
      site: "grok",
      conversation_id: match[1],
      project_id: null,
      convKey: `https://grok.com/c/${match[1]}`,
    };
  };
  const sendTabMessageWithTimeout = async (tabId, message) => {
    const record = tabRecords.find((item) => item.id === tabId);
    if (record?.hang) throw new Error("tab_message_timeout");
    return chrome.tabs.sendMessage(tabId, message);
  };
  const recover = new Function(
    "chrome",
    "activeH2WTabUrlsForProvider",
    "browserConversationInfoFromSupportedUrl",
    "browserSessionTargets",
    "sendTabMessageWithTimeout",
    `${recoverySource}; return recoverBrowserSessionTarget;`,
  )(
    chrome,
    activeH2WTabUrlsForProvider,
    browserConversationInfoFromSupportedUrl,
    browserSessionTargets,
    sendTabMessageWithTimeout,
  );
  return { recover, browserSessionTargets, queryArgs };
}

test("user sees WebChat control on Claude and Grok project homes | Given supported provider project URLs without conversations | When project page identity is parsed | Then project context exists without a conversation key", () => {
  const claude = browserProjectPageInfoFromSupportedUrl(
    "https://claude.ai/project/01a0606c-0d44-773b-b0b5-f4ed8ebf78c4",
  );
  assert.deepEqual(claude, {
    site: "claude",
    project_id: "01a0606c-0d44-773b-b0b5-f4ed8ebf78c4",
    conversation_id: null,
    convKey: null,
    pageKey: "https://claude.ai/project/01a0606c-0d44-773b-b0b5-f4ed8ebf78c4",
  });

  const grok = browserProjectPageInfoFromSupportedUrl(
    "https://grok.com/project/eacfb5b0-1ce3-4724-8b10-8d323896ffec",
  );
  assert.deepEqual(grok, {
    site: "grok",
    project_id: "eacfb5b0-1ce3-4724-8b10-8d323896ffec",
    conversation_id: null,
    convKey: null,
    pageKey: "https://grok.com/project/eacfb5b0-1ce3-4724-8b10-8d323896ffec",
  });

  assert.equal(
    browserProjectPageInfoFromSupportedUrl(
      "https://grok.com/project/eacfb5b0-1ce3-4724-8b10-8d323896ffec?chat=cd60accd-c663-4a40-a292-53a192996423",
    ),
    null,
    "a real Grok project chat remains owned by the session parser",
  );
  assert.equal(
    browserProjectPageInfoFromSupportedUrl(
      "https://claude.ai/chat/ace3312e-1eac-424f-8363-ad0ee0f6b24d",
    ),
    null,
    "a real Claude chat remains owned by the session parser",
  );
});

function createAnchorHarness({ tabs = [], scopes = [], targets = [], recovered = null } = {}) {
  const tabMap = new Map(tabs.map((tab) => [tab.id, { ...tab }]));
  const browserTabScopes = new Map(scopes);
  const browserSessionTargets = new Map(targets);
  const chrome = {
    tabs: {
      async get(tabId) {
        const tab = tabMap.get(tabId);
        if (!tab) throw new Error(`tab ${tabId} missing`);
        return { ...tab };
      },
    },
  };
  const recoverCalls = [];
  const recoverBrowserSessionTarget = async (sessionRef, expectedGeneration) => {
    recoverCalls.push({ sessionRef, expectedGeneration });
    return recovered || { target: null, observedGeneration: expectedGeneration, ambiguous: false };
  };
  const resolve = new Function(
    "chrome",
    "browserTabScopes",
    "browserSessionTargets",
    "recoverBrowserSessionTarget",
    `${createAnchorSource}; return resolveBrowserCreateAnchorWindow;`,
  )(chrome, browserTabScopes, browserSessionTargets, recoverBrowserSessionTarget);
  return { resolve, recoverCalls };
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
  const updateCalls = [];
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
      async update(id, update) {
        updateCalls.push({ id, update: { ...update } });
        const tab = tabs.get(id);
        if (!tab) throw new Error(`tab ${id} missing`);
        tabs.set(id, { ...tab, ...update });
        return { ...tabs.get(id) };
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
    updateCalls,
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

test("canonical ChatGPT session recovery reuses one slugged Project alias instead of opening a duplicate tab", async () => {
  const conversationId = "6aa8f808-4a18-83e9-8919-ef5ed2acdfeb";
  const projectId = "g-p-6a89c078669481918c8eb70fdfd3d978";
  const canonical = `https://chatgpt.com/g/${projectId}/c/${conversationId}`;
  const slugged = `https://chatgpt.com/g/${projectId}-herdr-mcp/c/${conversationId}`;
  const scopes = new Map([[71, {
    provider: "chatgpt",
    projectId,
    observationGeneration: 17,
    accountRef: "br_account",
    spaceRef: "br_space",
  }]]);
  const harness = canonicalIdentityRecoveryHarness([{ id: 71, url: slugged }], scopes);
  const result = await harness.find("chatgpt", canonical, 17);
  assert.equal(result.ambiguous, false);
  assert.equal(result.target?.tabId, 71);
  assert.equal(result.target?.conversationId, conversationId);
  assert.equal(result.target?.projectId, projectId);
  assert.equal(result.target?.convKey, canonical);
  assert.equal(result.target?.observationGeneration, 17);
});

test("user recovers a Grok direct session | Given service-worker target cache is lost | When canonical recovery runs | Then only the Grok origin is queried and the exact existing tab is reused", async () => {
  const url = "https://grok.com/c/5fe91b62-b7f0-4e7a-8e4d-fac53208475b";
  const harness = grokCanonicalIdentityRecoveryHarness([{ id: 81, url }]);
  const result = await harness.find("grok", url, 7);
  assert.equal(result.ambiguous, false);
  assert.equal(result.target?.provider, "grok");
  assert.equal(result.target?.tabId, 81);
  assert.equal(result.target?.convKey, url);
  assert.deepEqual(harness.queryArgs, [{ url: ["https://grok.com/*"] }]);
});

test("canonical ChatGPT session recovery fails closed when slugged and unslugged aliases are both open", async () => {
  const conversationId = "6aa8f808-4a18-83e9-8919-ef5ed2acdfeb";
  const projectId = "g-p-6a89c078669481918c8eb70fdfd3d978";
  const canonical = `https://chatgpt.com/g/${projectId}/c/${conversationId}`;
  const slugged = `https://chatgpt.com/g/${projectId}-herdr-mcp/c/${conversationId}`;
  const scopes = new Map([
    [71, { provider: "chatgpt", projectId, observationGeneration: 17 }],
    [72, { provider: "chatgpt", projectId, observationGeneration: 17 }],
  ]);
  const harness = canonicalIdentityRecoveryHarness([
    { id: 71, url: slugged },
    { id: 72, url: canonical },
  ], scopes);
  const result = await harness.find("chatgpt", canonical, 17);
  assert.deepEqual(result, { target: null, ambiguous: true });
});

test("canonical ChatGPT session recovery rejects a stale browser scope generation", async () => {
  const conversationId = "6aa8f808-4a18-83e9-8919-ef5ed2acdfeb";
  const projectId = "g-p-6a89c078669481918c8eb70fdfd3d978";
  const canonical = `https://chatgpt.com/g/${projectId}/c/${conversationId}`;
  const slugged = `https://chatgpt.com/g/${projectId}-herdr-mcp/c/${conversationId}`;
  const scopes = new Map([[71, {
    provider: "chatgpt",
    projectId,
    observationGeneration: 16,
  }]]);
  const harness = canonicalIdentityRecoveryHarness([{ id: 71, url: slugged }], scopes);
  const result = await harness.find("chatgpt", canonical, 17);
  assert.deepEqual(result, { target: null, ambiguous: false });
});

test("ChatGPT session.create anchors to the exact source session window across matching Project windows", async () => {
  const sourceSessionRef = "br_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
  const harness = createAnchorHarness({
    tabs: [
      { id: 71, windowId: 11, url: "https://chatgpt.com/c/other-conv" },
      { id: 72, windowId: 22, url: "https://chatgpt.com/c/source-conv" },
    ],
    scopes: [
      [71, { provider: "chatgpt", accountRef: "br_account", spaceRef: "br_space", observationGeneration: 17 }],
      [72, { provider: "chatgpt", accountRef: "br_account", spaceRef: "br_space", observationGeneration: 17 }],
    ],
    targets: [[sourceSessionRef, {
      provider: "chatgpt",
      tabId: 72,
      conversationId: "source-conv",
      observationGeneration: 17,
    }]],
  });
  const result = await harness.resolve({
    provider: "chatgpt",
    accountRef: "br_account",
    spaceRef: "br_space",
    expectedGeneration: 17,
    sourceSessionRef,
  });
  assert.deepEqual(result, { windowId: 22, unavailable: false, reason: "source_session" });
  assert.deepEqual(harness.recoverCalls, []);
});

test("ChatGPT session.create recovers instead of trusting a stale cached source tab route", async () => {
  const sourceSessionRef = "br_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
  const harness = createAnchorHarness({
    tabs: [
      { id: 71, windowId: 11, url: "https://chatgpt.com/c/source-conv" },
      { id: 72, windowId: 22, url: "https://chatgpt.com/g/g-p-project/project" },
    ],
    targets: [[sourceSessionRef, {
      provider: "chatgpt",
      tabId: 72,
      conversationId: "source-conv",
      observationGeneration: 17,
    }]],
    recovered: {
      target: {
        provider: "chatgpt",
        tabId: 71,
        conversationId: "source-conv",
        observationGeneration: 17,
      },
      observedGeneration: 17,
      ambiguous: false,
    },
  });
  const result = await harness.resolve({
    provider: "chatgpt",
    accountRef: "br_account",
    spaceRef: "br_space",
    expectedGeneration: 17,
    sourceSessionRef,
  });
  assert.deepEqual(result, { windowId: 11, unavailable: false, reason: "source_session" });
  assert.deepEqual(harness.recoverCalls, [{ sessionRef: sourceSessionRef, expectedGeneration: 17 }]);
});

test("user keeps exact Project affinity | Given the same authorized Project is open in multiple windows | When session.create needs a host window | Then it deterministically selects one exact-scope window", async () => {
  const harness = createAnchorHarness({
    tabs: [
      { id: 71, windowId: 11 },
      { id: 72, windowId: 22 },
    ],
    scopes: [
      [71, { provider: "chatgpt", accountRef: "br_account", spaceRef: "br_space", observationGeneration: 17 }],
      [72, { provider: "chatgpt", accountRef: "br_account", spaceRef: "br_space", observationGeneration: 17 }],
    ],
  });
  const result = await harness.resolve({
    provider: "chatgpt",
    accountRef: "br_account",
    spaceRef: "br_space",
    expectedGeneration: 17,
  });
  assert.deepEqual(result, { windowId: 11, unavailable: false, reason: "exact_scope_window" });
});

test("user can fan out after source route drift | Given the registered source tab is unavailable but exact account Project generation scope remains | When session.create resolves its anchor | Then it uses the exact scope without reusing a worker tab", async () => {
  const sourceSessionRef = "br_cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
  const harness = createAnchorHarness({
    tabs: [
      { id: 71, windowId: 22, url: "https://chatgpt.com/c/worker-a" },
      { id: 72, windowId: 11, url: "https://chatgpt.com/c/worker-b" },
    ],
    scopes: [
      [71, {
        provider: "chatgpt", accountRef: "br_account", spaceRef: "br_space",
        observationGeneration: 17, executionState: "generating",
      }],
      [72, {
        provider: "chatgpt", accountRef: "br_account", spaceRef: "br_space",
        observationGeneration: 17, executionState: "generating",
      }],
    ],
  });
  const result = await harness.resolve({
    provider: "chatgpt",
    accountRef: "br_account",
    spaceRef: "br_space",
    expectedGeneration: 17,
    sourceSessionRef,
  });
  assert.deepEqual(result, { windowId: 11, unavailable: false, reason: "source_scope_window" });
  assert.deepEqual(harness.recoverCalls, [{ sessionRef: sourceSessionRef, expectedGeneration: 17 }]);
});

test("ChatGPT session.create without source affinity keeps single-window compatibility", async () => {
  const harness = createAnchorHarness({
    tabs: [
      { id: 71, windowId: 11 },
      { id: 72, windowId: 11 },
    ],
    scopes: [
      [71, { provider: "chatgpt", accountRef: "br_account", spaceRef: "br_space", observationGeneration: 17 }],
      [72, { provider: "chatgpt", accountRef: "br_account", spaceRef: "br_space", observationGeneration: 17 }],
    ],
  });
  const result = await harness.resolve({
    provider: "chatgpt",
    accountRef: "br_account",
    spaceRef: "br_space",
    expectedGeneration: 17,
  });
  assert.deepEqual(result, { windowId: 11, unavailable: false, reason: "unique_scope_window" });
});

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

test("archive cleanup restores a proven archived Project tab from ChatGPT root before closing", async () => {
  const harness = archiveCleanupHarness({
    tabUrl: "https://chatgpt.com/",
  });
  const result = await harness.closeArchivedChatGptTabAfterProjectHome({
    tabId: harness.tabId,
    sessionRef: harness.sessionRef,
    expectedGeneration: harness.generation,
    projectId: harness.projectId,
    timeoutMs: 0,
  });
  assert.deepEqual(harness.updateCalls, [{
    id: harness.tabId,
    update: { url: `https://chatgpt.com/g/${harness.projectId}` },
  }]);
  assert.deepEqual(result, { closed: true, reason: "archived_project_home" });
  assert.deepEqual(harness.removeCalls, [harness.tabId]);
  assert.equal(harness.tabs.has(harness.tabId), false);
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

test("user recovers an exact browser session | Given another supported tab never answers identity probing | When service-worker target recovery scans the browser | Then the exact live session is returned without waiting on the stalled tab", async () => {
  const sessionRef = "br_exact";
  const generation = 17;
  const { recover } = recoveryHarness([
    {
      id: 71,
      url: "https://chatgpt.com/c/exact",
      live: {
        convKey: "https://chatgpt.com/c/exact",
        url: "https://chatgpt.com/c/exact",
        site: "chatgpt",
        browserSessionRef: sessionRef,
        browserGeneration: generation,
      },
    },
    {
      id: 72,
      url: "https://chatgpt.com/c/unrelated",
      hang: true,
    },
  ]);

  const recovered = await recover(sessionRef, generation);
  assert.equal(recovered.ambiguous, false);
  assert.equal(recovered.target?.tabId, 71);
  assert.equal(recovered.target?.conversationId, "exact");
});

test("user recovers the requested provider session | Given an unrelated provider tab is stalled | When exact service-worker recovery runs | Then only the requested provider origin is scanned", async () => {
  const sessionRef = "br_claude_exact";
  const generation = 19;
  const { recover, queryArgs } = recoveryHarness([
    {
      id: 81,
      url: "https://chatgpt.com/c/stalled",
      hang: true,
    },
    {
      id: 82,
      url: "https://claude.ai/chat/claude-exact",
      live: {
        convKey: "https://claude.ai/chat/claude-exact",
        url: "https://claude.ai/chat/claude-exact",
        site: "claude",
        browserSessionRef: sessionRef,
        browserGeneration: generation,
      },
    },
  ]);

  const recovered = await recover(sessionRef, generation, "claude");
  assert.equal(recovered.ambiguous, false);
  assert.equal(recovered.target?.provider, "claude");
  assert.equal(recovered.target?.tabId, 82);
  assert.equal(recovered.target?.conversationId, "claude-exact");
  assert.deepEqual(queryArgs, [{ url: ["https://claude.ai/*"] }]);
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

test("user keeps the ordinary ChatGPT submit path bounded | Given a ready composer | When fallback ordering is inspected | Then MAIN-world send-button click precedes isolated-world fallbacks", () => {
  assert.match(wakeSource, /function submitMainWorld\(selector\)/);
  assert.match(wakeSource, /type:\s*"h2w_submit_main"/);
  assert.match(backgroundSource, /msg\?\.type === "h2w_submit_main"/);
  assert.match(backgroundSource, /sendButton\.click\(\)/);
  assert.doesNotMatch(backgroundSource, /form\.requestSubmit\(sendButton\)/);
  const submitStart = wakeSource.indexOf("async function submit() {");
  const submitEnd = wakeSource.indexOf("// ---- Auto-allow", submitStart);
  const submitSegment = wakeSource.slice(submitStart, submitEnd);
  assert.match(submitSegment, /await submitMainWorld\(selector\)/);
  assert.match(submitSegment, /ADAPTER\.name === "chatgpt" && ADAPTER\.inputHasContent\(\)/);
  assert.doesNotMatch(submitSegment, /ADAPTER\.name === "chatgpt" && attempt === 0/);
  const mainFallbackStart = submitSegment.indexOf("const mainSubmit = selector ? await submitMainWorld(selector) : null;");
  const mainFallbackEnd = submitSegment.indexOf("const baseline = captureSubmitAckBaseline(btn);", mainFallbackStart);
  assert.ok(mainFallbackStart >= 0 && mainFallbackEnd > mainFallbackStart, "MAIN submit fallback must remain bounded");
  assert.doesNotMatch(submitSegment.slice(mainFallbackStart, mainFallbackEnd), /return false;/);
  assert.match(submitSegment.slice(mainFallbackStart, mainFallbackEnd), /waitForSubmitAck\(mainBaseline, 4000\)/);
  const clickIndex = submitSegment.indexOf("btn.click();", mainFallbackEnd);
  const enterIndex = submitSegment.indexOf("dispatchEnterSubmit(el)", clickIndex);
  assert.ok(clickIndex > mainFallbackEnd && enterIndex > clickIndex, "DOM click and Enter remain ordered fallbacks");

  const ackStart = wakeSource.indexOf("function submitWasAccepted(baseline) {");
  const ackEnd = wakeSource.indexOf("async function waitForSubmitAck", ackStart);
  const ackSegment = wakeSource.slice(ackStart, ackEnd);
  assert.match(ackSegment, /ADAPTER\.name !== "chatgpt"\) return !ADAPTER\.inputHasContent\(\)/);
  assert.doesNotMatch(ackSegment, /if \(!ADAPTER\.inputHasContent\(\)\) return true/);
  assert.match(ackSegment, /location\.href !== baseline\.href/);
  assert.doesNotMatch(ackSegment, /baseline\?\.generating|isComposerGenerating\(\)/);
  assert.doesNotMatch(ackSegment, /sendButton\.isConnected|isSendButton\(baseline\.sendButton\)/);
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

test("user keeps one Claude Browser Registry session across reload | Given the Project breadcrumb renders after the chat route | When registration resolves the provider scope | Then it waits for the Project identity before choosing the parent", async () => {
  const project = {
    id: "01a0606c-0d44-773b-b0b5-f4ed8ebf78c4",
    name: "herdr-mcp",
    key: "https://claude.ai/project/01a0606c-0d44-773b-b0b5-f4ed8ebf78c4",
  };
  const harness = delayedClaudeProjectIdentityHarness([null, null, project]);
  assert.deepEqual(await harness.observe(harness.convKey), project);
  assert.equal(harness.calls(), 3);
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
});

test("user Given unavailable browser actuation When the content script rejects it Then the exact machine reason is returned", async () => {
  const evidenceStart = wakeSource.indexOf("  function browserActuationEvidence(");
  const evidenceEnd = wakeSource.indexOf("  function providerMessageSnapshot(", evidenceStart);
  const commandStart = wakeSource.indexOf("  async function performBrowserActuationCommand(");
  const commandEnd = wakeSource.indexOf("  async function reportBrowserResultSettlement(", commandStart);
  assert.ok(evidenceStart >= 0 && evidenceEnd > evidenceStart, "browser evidence helpers must remain extractable");
  assert.ok(commandStart >= 0 && commandEnd > commandStart, "browser actuation command must remain extractable");
  const evidenceSource = wakeSource.slice(evidenceStart, evidenceEnd);
  const commandSource = wakeSource.slice(commandStart, commandEnd);

  function makeActuator(ctx = {}) {
    return new Function("ctx", `
      const ADAPTER = {
        name: ctx.adapterName || "chatgpt",
        getConversationKey: () => ctx.currentConvKey || "conv-current",
        getCanonicalConversationUrl: () => ctx.canonicalObserved === false ? "" : "https://chatgpt.com/c/current",
      };
      const chatGptConversationId = () => "current";
      const sessionStorage = {
        setItem: () => { if (ctx.storageFails) throw new Error("storage unavailable"); },
        removeItem: () => {},
      };
      const BROWSER_SESSION_RESERVATION_STORAGE_KEY = "herdrBrowserSessionReservationV1";
      let registeredBrowserSessionRef = ctx.registeredSessionRef || "br_${"a".repeat(64)}";
      let registeredBrowserGeneration = ctx.registeredGeneration || 17;
      let registeredConvKey = ctx.registeredConvKey || "conv-current";
      const providerCanonicalConversationObserved = () => ctx.canonicalObserved !== false;
      const document = { hidden: ctx.hidden === true };
      ${evidenceSource}
      ${commandSource}
      return performBrowserActuationCommand;
    `)(ctx);
  }

  const cases = [
    {
      name: "unsupported provider context",
      ctx: { adapterName: "z.ai" },
      command: { operation: "herdr_mcp.browser_dispatch.submit", expected_generation: 17 },
      reason: "browser_actuation_context_unavailable",
    },
    {
      name: "non-ChatGPT session.open",
      ctx: { adapterName: "gemini" },
      command: { operation: "herdr_mcp.browser_session.open", expected_generation: 17 },
      reason: "browser_open_provider_unavailable",
    },
    {
      name: "wrong session",
      ctx: {},
      command: { operation: "herdr_mcp.browser_session.open", expected_generation: 17, params: { session_ref: "br_" + "b".repeat(64) } },
      reason: "browser_open_session_unavailable",
    },
    {
      name: "generation drift",
      ctx: { registeredSessionRef: "br_" + "b".repeat(64), registeredGeneration: 18 },
      command: { operation: "herdr_mcp.browser_session.open", expected_generation: 17, params: { session_ref: "br_" + "b".repeat(64) } },
      reason: "browser_open_generation_unavailable",
    },
    {
      name: "conversation drift",
      ctx: { registeredSessionRef: "br_" + "b".repeat(64), registeredConvKey: "conv-old", currentConvKey: "conv-current" },
      command: { operation: "herdr_mcp.browser_session.open", expected_generation: 17, params: { session_ref: "br_" + "b".repeat(64) } },
      reason: "browser_open_conversation_unavailable",
    },
    {
      name: "canonical URL unavailable",
      ctx: { registeredSessionRef: "br_" + "b".repeat(64), canonicalObserved: false },
      command: { operation: "herdr_mcp.browser_session.open", expected_generation: 17, params: { session_ref: "br_" + "b".repeat(64) } },
      reason: "browser_open_canonical_url_unavailable",
    },
    {
      name: "hidden page",
      ctx: { registeredSessionRef: "br_" + "b".repeat(64), hidden: true },
      command: { operation: "herdr_mcp.browser_session.open", expected_generation: 17, params: { session_ref: "br_" + "b".repeat(64) } },
      reason: "browser_open_page_hidden",
    },
    {
      name: "invalid create reservation",
      ctx: {},
      command: { operation: "herdr_mcp.browser_session.create", expected_generation: 17, params: { reservation_ref: "invalid" } },
      reason: "browser_create_reservation_invalid",
    },
    {
      name: "create reservation storage unavailable",
      ctx: { storageFails: true },
      command: { operation: "herdr_mcp.browser_session.create", expected_generation: 17, params: { reservation_ref: "bsr_" + "c".repeat(64) } },
      reason: "browser_create_reservation_storage_unavailable",
    },
  ];

  for (const item of cases) {
    const result = await makeActuator(item.ctx)(item.command);
    assert.equal(result.resource_available, false, item.name);
    assert.equal(result.command_accepted, false, item.name);
    assert.equal(result.result?.error, item.reason, item.name);
  }
});

test("user can stop a live answer | Given an explicit visible stop control while the generic turn probe lags | When stop is requested | Then Herdr clicks once and verifies the stopped postcondition", async () => {
  const evidenceStart = wakeSource.indexOf("  function browserActuationEvidence(");
  const evidenceEnd = wakeSource.indexOf("  function providerMessageSnapshot(", evidenceStart);
  const commandStart = wakeSource.indexOf("  async function performBrowserActuationCommand(");
  const commandEnd = wakeSource.indexOf("  async function reportBrowserResultSettlement(", commandStart);
  assert.ok(evidenceStart >= 0 && evidenceEnd > evidenceStart);
  assert.ok(commandStart >= 0 && commandEnd > commandStart);
  const evidenceSource = wakeSource.slice(evidenceStart, evidenceEnd);
  const commandSource = wakeSource.slice(commandStart, commandEnd);
  const ctx = { clicked: false };
  const act = new Function("ctx", `
    const stopButton = {
      disabled: false,
      getAttribute: (name) => name === "aria-disabled" ? "false" : null,
      click: () => { ctx.clicked = true; },
    };
    const ADAPTER = {
      name: "chatgpt",
      getConversationKey: () => "conv-current",
      getCanonicalConversationUrl: () => "https://chatgpt.com/c/current",
      getStopButtonCandidates: () => ctx.clicked ? [] : [stopButton],
      elementVisible: () => true,
    };
    const chatGptConversationId = () => "current";
    const sessionStorage = { setItem: () => {}, removeItem: () => {} };
    const BROWSER_SESSION_RESERVATION_STORAGE_KEY = "herdrBrowserSessionReservationV1";
    let registeredBrowserSessionRef = "br_${"a".repeat(64)}";
    let registeredBrowserGeneration = 17;
    let registeredConvKey = "conv-current";
    const providerCanonicalConversationObserved = () => true;
    const document = { hidden: false };
    const isTurnInProgress = () => false;
    const wait = async () => {};
    ${evidenceSource}
    ${commandSource}
    return performBrowserActuationCommand;
  `)(ctx);

  const result = await act({
    operation: "herdr_mcp.browser_dispatch.stop",
    expected_generation: 17,
    params: {},
  });
  assert.equal(ctx.clicked, true);
  assert.equal(result.command_accepted, true);
  assert.equal(result.generation_owner, 17);
  assert.equal(result.generation_status_observed, true);
  assert.equal(result.generation_stopped, true);
});

test("user never duplicates a Browser Actuation submit | Given ChatGPT acknowledgement is delayed | When Herdr submits once | Then one Enter submit is attempted and uncertain delivery never falls through to another submit", async () => {
  const start = wakeSource.indexOf("  async function submitBrowserActuationOnce()");
  const end = wakeSource.indexOf("  // ---- Submission ----", start);
  assert.ok(start >= 0 && end > start, "bounded Browser Actuation submit helper must remain extractable");
  const helperSource = wakeSource.slice(start, end);

  async function run({ busy = false, ack = false } = {}) {
    const ctx = { busy, ack, clicks: 0, mainSubmits: 0, enterSubmits: 0 };
    ctx.input = { innerText: "next turn" };
    ctx.button = {
      disabled: false,
      click() { ctx.clicks += 1; },
    };
    const submitOnce = new Function("ctx", `
      const isComposerGenerating = () => ctx.busy;
      const ADAPTER = {
        name: "chatgpt",
        needsMainWorldInsert: true,
        inputHasContent: () => true,
        getInputEl: () => ctx.input,
        getWatchMainWorldSelector: () => "#prompt-textarea",
      };
      const wait = async () => {};
      const findSendButton = () => ctx.button;
      const isSendButton = (button) => Boolean(button) && button.disabled !== true;
      const captureSubmitAckBaseline = () => ({});
      const waitForSubmitAck = async () => ctx.ack;
      const submitMainWorld = async () => {
        ctx.mainSubmits += 1;
        return { ok: true, submitted: true };
      };
      const dispatchEnterSubmit = () => { ctx.enterSubmits += 1; };
      ${helperSource}
      return submitBrowserActuationOnce;
    `)(ctx);
    return {
      result: await submitOnce(),
      clicks: ctx.clicks,
      mainSubmits: ctx.mainSubmits,
      enterSubmits: ctx.enterSubmits,
    };
  }

  const delayed = await run({ ack: false });
  assert.equal(delayed.enterSubmits, 1);
  assert.equal(delayed.mainSubmits, 0);
  assert.equal(delayed.clicks, 0);
  assert.equal(delayed.result.ok, false);
  assert.equal(delayed.result.attempted, true);
  assert.equal(delayed.result.uncertain, true);

  const accepted = await run({ ack: true });
  assert.equal(accepted.enterSubmits, 1);
  assert.equal(accepted.mainSubmits, 0);
  assert.equal(accepted.clicks, 0);
  assert.equal(accepted.result.ok, true);
  assert.equal(accepted.result.attempted, true);

  const busy = await run({ busy: true });
  assert.equal(busy.enterSubmits, 0);
  assert.equal(busy.mainSubmits, 0);
  assert.equal(busy.clicks, 0);
  assert.equal(busy.result.ok, false);
  assert.equal(busy.result.attempted, false);
});

test("user keeps an unconfirmed ChatGPT submit fail-closed | Given one dispatch or fresh-create attempt cannot be proven | When Browser Actuation returns | Then it stays uncertain without replaying or dropping the create reservation", async () => {
  const evidenceStart = wakeSource.indexOf("  function browserActuationEvidence(");
  const evidenceEnd = wakeSource.indexOf("  function providerMessageSnapshot(", evidenceStart);
  const commandStart = wakeSource.indexOf("  async function performBrowserActuationCommand(");
  const commandEnd = wakeSource.indexOf("  async function reportBrowserResultSettlement(", commandStart);
  assert.ok(evidenceStart >= 0 && evidenceEnd > evidenceStart);
  assert.ok(commandStart >= 0 && commandEnd > commandStart);
  const evidenceSource = wakeSource.slice(evidenceStart, evidenceEnd);
  const commandSource = wakeSource.slice(commandStart, commandEnd);
  const ctx = {
    snapshotTimeouts: [],
    wakeArgs: [],
    reservationWrites: [],
    reservationRemovals: 0,
  };

  const act = new Function("ctx", `
    const ADAPTER = {
      name: "chatgpt",
      getConversationKey: () => "https://chatgpt.com/c/current",
      getCanonicalConversationUrl: () => "https://chatgpt.com/c/current",
      getInputEl: () => ({}),
      inputHasContent: () => false,
      getMessageSnapshot: () => ({}),
    };
    const chatGptConversationId = () => "current";
    const sessionStorage = {
      setItem: (key, value) => ctx.reservationWrites.push([key, value]),
      removeItem: () => { ctx.reservationRemovals += 1; },
    };
    const BROWSER_SESSION_RESERVATION_STORAGE_KEY = "herdrBrowserSessionReservationV1";
    let registeredBrowserSessionRef = "br_${"a".repeat(64)}";
    let registeredBrowserGeneration = 17;
    let registeredConvKey = "https://chatgpt.com/c/current";
    const currentHerdrRequiredApps = () => ["herdr"];
    const providerCanonicalConversationObserved = () => true;
    const document = { hidden: false };
    const ensureChatGptChatMode = async () => ({ ok: true, switched: false });
    const isTurnInProgress = () => false;
    const runtimeAlive = () => true;
    const wait = async () => {};
    const ensureRequiredComposerApps = async () => ({ ok: true, apps: [] });
    const fetchChatGptConversationSnapshot = async (timeoutMs) => {
      ctx.snapshotTimeouts.push(timeoutMs);
      return { ok: false };
    };
    const performWake = async (args) => {
      ctx.wakeArgs.push(args);
      return { ok: false, attempted: true, uncertain: true, error: "submit-unconfirmed" };
    };
    const providerMessageSnapshot = () => ({ messageId: null, text: "", count: 0 });
    ${evidenceSource}
    ${commandSource}
    return performBrowserActuationCommand;
  `)(ctx);

  const result = await act({
    operation: "herdr_mcp.browser_dispatch.submit",
    expected_generation: 17,
    params: { message: "next turn", required_apps: [] },
  });

  assert.equal(ctx.wakeArgs[0].browserActuation, true);
  assert.deepEqual(ctx.snapshotTimeouts, [1200]);
  assert.equal(result.command_accepted, true);
  assert.equal(result.rejected, false);
  assert.equal(result.accepted_message_observed, false);
  assert.equal(result.generation_owner, null);

  const reservationRef = "bsr_" + "c".repeat(64);
  const create = await act({
    operation: "herdr_mcp.browser_session.create",
    expected_generation: 17,
    params: {
      reservation_ref: reservationRef,
      message: "first worker turn",
      required_apps: [],
    },
  });

  assert.equal(ctx.wakeArgs[1].browserActuation, true);
  assert.deepEqual(ctx.snapshotTimeouts, [1200, 1200]);
  assert.equal(create.command_accepted, true);
  assert.equal(create.rejected, false);
  assert.equal(create.stable_resource_ref_observed, false);
  assert.deepEqual(ctx.reservationWrites, [["herdrBrowserSessionReservationV1", reservationRef]]);
  assert.equal(ctx.reservationRemovals, 0);
});

test("user waits through transient fresh ChatGPT composer busy without a second submit | Given the new Project tab is scoped but still hydrating | When composer readiness changes from busy to idle | Then the same actuation submits once and keeps its reservation", async () => {
  const evidenceStart = wakeSource.indexOf("  function browserActuationEvidence(");
  const evidenceEnd = wakeSource.indexOf("  function providerMessageSnapshot(", evidenceStart);
  const commandStart = wakeSource.indexOf("  async function performBrowserActuationCommand(");
  const commandEnd = wakeSource.indexOf("  async function reportBrowserResultSettlement(", commandStart);
  assert.ok(evidenceStart >= 0 && evidenceEnd > evidenceStart);
  assert.ok(commandStart >= 0 && commandEnd > commandStart);
  const evidenceSource = wakeSource.slice(evidenceStart, evidenceEnd);
  const commandSource = wakeSource.slice(commandStart, commandEnd);
  const ctx = {
    busyChecks: 0,
    busySequence: [true, true, false, true, false, false, false, true],
    waits: 0,
    wakeCalls: 0,
    reservationRemovals: 0,
  };

  const act = new Function("ctx", `
    const ADAPTER = {
      name: "chatgpt",
      getConversationKey: () => "https://chatgpt.com/g/g-p-test/project",
      getCanonicalConversationUrl: () => "",
      getInputEl: () => ({}),
      inputHasContent: () => false,
      getMessageSnapshot: () => ({}),
    };
    const chatGptConversationId = () => null;
    const sessionStorage = {
      setItem: () => {},
      removeItem: () => { ctx.reservationRemovals += 1; },
    };
    const BROWSER_SESSION_RESERVATION_STORAGE_KEY = "herdrBrowserSessionReservationV1";
    let registeredBrowserSessionRef = null;
    let registeredBrowserGeneration = 17;
    let registeredConvKey = "https://chatgpt.com/g/g-p-test/project";
    const currentHerdrRequiredApps = () => [];
    const providerCanonicalConversationObserved = () => false;
    const document = { hidden: false };
    const ensureChatGptChatMode = async () => ({ ok: true, switched: false });
    const isTurnInProgress = () => {
      const value = ctx.busySequence[ctx.busyChecks] ?? false;
      ctx.busyChecks += 1;
      return value;
    };
    const runtimeAlive = () => true;
    const wait = async () => { ctx.waits += 1; };
    const ensureRequiredComposerApps = async () => ({ ok: true, apps: [] });
    const fetchChatGptConversationSnapshot = async () => ({ ok: false });
    const performWake = async () => {
      ctx.wakeCalls += 1;
      return { ok: false, attempted: true, uncertain: true, error: "submit-unconfirmed" };
    };
    const providerMessageSnapshot = () => ({ messageId: null, text: "", count: 0 });
    ${evidenceSource}
    ${commandSource}
    return performBrowserActuationCommand;
  `)(ctx);

  const reservationRef = "bsr_" + "d".repeat(64);
  const result = await act({
    operation: "herdr_mcp.browser_session.create",
    expected_generation: 17,
    params: {
      reservation_ref: reservationRef,
      message: "fresh worker turn",
      required_apps: [],
    },
  });

  assert.ok(ctx.waits >= 2, "fresh create should wait for transient composer busy state to clear");
  assert.equal(ctx.wakeCalls, 1, "the provider submit path must be entered exactly once");
  assert.equal(result.command_accepted, true);
  assert.equal(result.rejected, false);
  assert.equal(ctx.reservationRemovals, 0, "uncertain delivery keeps the reservation for reconciliation");
});

test("user receives exact content rejection reasons | Given browser controls reject before provider mutation | When create dispatch or stop is attempted | Then each result has a bounded machine reason", async () => {
  const evidenceStart = wakeSource.indexOf("  function browserActuationEvidence(");
  const evidenceEnd = wakeSource.indexOf("  function providerMessageSnapshot(", evidenceStart);
  const commandStart = wakeSource.indexOf("  async function performBrowserActuationCommand(");
  const commandEnd = wakeSource.indexOf("  async function reportBrowserResultSettlement(", commandStart);
  assert.ok(evidenceStart >= 0 && evidenceEnd > evidenceStart);
  assert.ok(commandStart >= 0 && commandEnd > commandStart);
  const evidenceSource = wakeSource.slice(evidenceStart, evidenceEnd);
  const commandSource = wakeSource.slice(commandStart, commandEnd);

  function makeActuator(ctx = {}) {
    return new Function("ctx", `
      const ADAPTER = {
        name: "chatgpt",
        getConversationKey: () => "conv-current",
        getCanonicalConversationUrl: () => "https://chatgpt.com/c/current",
        getInputEl: () => (ctx.inputAvailable === false ? null : {}),
        inputHasContent: () => ctx.inputHasContent === true,
        getStopButtonCandidates: () => [],
        getMessageSnapshot: () => ({}),
      };
      const chatGptConversationId = () => "current";
      const sessionStorage = { setItem: () => {}, removeItem: () => {} };
      const BROWSER_SESSION_RESERVATION_STORAGE_KEY = "herdrBrowserSessionReservationV1";
      let registeredBrowserSessionRef = "br_${"a".repeat(64)}";
      let registeredBrowserGeneration = 17;
      let registeredConvKey = "conv-current";
      const currentHerdrRequiredApps = () => ["herdr"];
      const providerCanonicalConversationObserved = () => true;
      const document = { hidden: false };
      const ensureChatGptChatMode = async () => ({ ok: true, switched: false });
      const isTurnInProgress = () => ctx.turnInProgress === true;
      const runtimeAlive = () => true;
      const Date = { now: () => ctx.now || 0 };
      const wait = async (ms = 0) => { ctx.now = (ctx.now || 0) + Math.max(1, ms); };
      const ensureRequiredComposerApps = async () => ({ ok: true, apps: [] });
      const fetchChatGptConversationSnapshot = async () => ({ ok: false });
      const performWake = async () => ({ ok: ctx.wakeOk !== false });
      const providerMessageSnapshot = () => ({ messageId: null, text: "", count: 0 });
      ${evidenceSource}
      ${commandSource}
      return performBrowserActuationCommand;
    `)(ctx);
  }

  const reservation = "bsr_" + "c".repeat(64);
  const cases = [
    {
      name: "create composer busy",
      ctx: { turnInProgress: true },
      command: {
        operation: "herdr_mcp.browser_session.create",
        expected_generation: 17,
        params: { reservation_ref: reservation, message: "worker A" },
      },
      reason: "browser_create_composer_busy",
    },
    {
      name: "create submit failure",
      ctx: { wakeOk: false },
      command: {
        operation: "herdr_mcp.browser_session.create",
        expected_generation: 17,
        params: { reservation_ref: reservation, message: "worker A" },
      },
      reason: "browser_create_submit_failed",
    },
    {
      name: "dispatch composer busy",
      ctx: { inputHasContent: true },
      command: {
        operation: "herdr_mcp.browser_dispatch.submit",
        expected_generation: 17,
        params: { message: "next turn" },
      },
      reason: "browser_dispatch_composer_busy",
    },
    {
      name: "stop control unavailable",
      ctx: {},
      command: {
        operation: "herdr_mcp.browser_dispatch.stop",
        expected_generation: 17,
        params: {},
      },
      reason: "browser_stop_control_unavailable",
    },
  ];

  for (const item of cases) {
    const result = await makeActuator(item.ctx)(item.command);
    assert.equal(result.command_accepted, false, item.name);
    assert.equal(result.rejected, true, item.name);
    assert.equal(result.result?.error, item.reason, item.name);
  }
});

test("user keeps shared browser control responsive | Given one stale content view never answers | When Herdr sends a browser actuation command | Then one tab message times out without reload or resend", async () => {
  const start = backgroundSource.indexOf("async function sendTabMessageWithTimeout(");
  const end = backgroundSource.indexOf("async function sendHandoffTabMessage(", start);
  assert.ok(start >= 0 && end > start, "bounded tab-message helpers must remain extractable");
  const helperSource = backgroundSource.slice(start, end);
  const ctx = { sends: 0, reloads: 0 };
  const send = new Function("ctx", `
    const chrome = {
      tabs: {
        sendMessage: () => {
          ctx.sends += 1;
          return new Promise(() => {});
        },
        reload: async () => { ctx.reloads += 1; },
      },
    };
    const setTimeout = (fn) => { fn(); return 1; };
    const clearTimeout = () => {};
    const waitForTabComplete = async () => null;
    const missingReceiverError = () => false;
    const sleep = async () => {};
    ${helperSource}
    return sendBrowserActuationTabMessage;
  `)(ctx);

  await assert.rejects(
    () => send(41, { type: "h2w_browser_actuation" }),
    /browser-actuation-content-timeout/,
  );
  assert.equal(ctx.sends, 1);
  assert.equal(ctx.reloads, 0);
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

test("user resumes an exact ChatGPT session | Given service-worker target cache is lost | When session.open has a canonical locator | Then Herdr reuses that view before broad probing and caches the verified target", () => {
  const start = backgroundSource.indexOf('if (operation === "herdr_mcp.browser_session.open")');
  assert.ok(start >= 0, "session.open branch must exist in handleBrowserActuation");
  const segment = backgroundSource.slice(start, backgroundSource.indexOf("\n  const sessionRef = String(params.session_ref", start));
  assert.match(segment, /recoverBrowserSessionTarget/);
  assert.match(segment, /findBrowserSessionTargetByCanonicalIdentity/);
  assert.match(segment, /recovered\.ambiguous/);
  assert.ok(
    segment.indexOf("findBrowserSessionTargetByCanonicalIdentity") < segment.indexOf("recoverBrowserSessionTarget"),
    "session.open must reuse the durable canonical locator before broad identity probing",
  );
  assert.ok(
    segment.indexOf("findBrowserSessionTargetByCanonicalIdentity") < segment.indexOf("chrome.tabs.create"),
    "session.open must try canonical-identity tab reuse before creating a new view",
  );
  assert.match(segment, /browserSessionTargets\.set\(sessionRefOpen/);
  assert.match(segment, /stable_resource_ref_observed === true/);
  assert.match(segment, /sendChatGptTabMessage\(targetOpen\.tabId/);
  assert.match(segment, /\}, 2000\)/);
  assert.doesNotMatch(segment, /sendBrowserActuationTabMessage\(targetOpen\.tabId/);
  assert.match(segment, /chrome\.tabs\.update.*active:\s*true.*autoDiscardable:\s*false/);
  assert.match(segment, /protectBoundTab/);
  assert.doesNotMatch(segment, /insertMainWorld|performWake|executeScript/);
  assert.match(segment, /observedGenerationOpen/);
  assert.match(segment, /providerOpen !== "chatgpt"/);
});

test("user archive recovery reuses canonical aliases | Given an exact session target is absent | When archive or archive-status resolves its canonical route | Then one existing alias is reused before any disposable view", () => {
  const openStart = backgroundSource.indexOf('if (operation === "herdr_mcp.browser_session.open")');
  const archiveStart = backgroundSource.indexOf('if (!target && (', openStart);
  assert.ok(archiveStart > openStart, "archive fallback must exist after session.open");
  const segment = backgroundSource.slice(archiveStart, archiveStart + 5000);
  assert.match(segment, /operation === "herdr_mcp\.browser_session\.archive"/);
  assert.match(segment, /operation === "herdr_mcp\.browser_session\.archive_status"/);
  assert.match(segment, /findBrowserSessionTargetByCanonicalIdentity/);
  assert.match(segment, /existing\.ambiguous/);
  assert.ok(
    segment.indexOf("findBrowserSessionTargetByCanonicalIdentity") < segment.indexOf("chrome.tabs.create"),
    "archive recovery must reuse one logical alias before creating a new view",
  );
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
    archiveListContains: false,
    providerArchived: false,
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
            if (ctx.archiveVerifies) ctx.providerArchived = true;
            if (ctx.archiveClickThrows) throw new Error("archive click failed after dispatch");
          } }
        : null;
    };
    const fetchChatGptConversation = async () => {
      if (ctx.verifyGate && ctx.clicks.length > 0) await ctx.verifyGate;
      return ctx.archiveVerifies
        ? { ok: true, body: { is_archived: ctx.providerArchived === true } }
        : { ok: false };
    };
    const fetchChatGptArchivedConversationList = async () => ({
      ok: true,
      items: ctx.archiveListContains
        ? [{ id: ctx.conversationId, is_archived: true }]
        : [],
    });
    const wait = (ms) => { Date.advance(ms); return Promise.resolve(); };
    const browserRejectedEvidence = (evidence, reason) => ({
      ...evidence,
      rejected: true,
      result: { error: reason },
    });
    let registeredBrowserSessionRef = ctx.sessionRef;
    let registeredBrowserGeneration = ctx.generation;
    ${segment}
    return {
      performChatGptSessionArchive,
      performChatGptSessionArchiveStatus,
      drain: drainPendingSelfArchives,
      pendingCount: () => readPendingSelfArchives().length,
      setGeneration: (value) => { registeredBrowserGeneration = value; },
      setSession: (value) => { registeredBrowserSessionRef = value; },
    };
  `)(ctx, sessionStorage, () => 0, dateShim);
  return { ctx, api, storage, writeState };
}

test("user archive status reads provider state without clicking Archive | Given an active then archived conversation | When status is read | Then state changes and click count stays zero", async () => {
  const { ctx, api } = selfArchiveHarness();
  const command = {
    operation: "herdr_mcp.browser_session.archive_status",
    expected_generation: 7,
    params: { session_ref: ctx.sessionRef, expected_generation: 7 },
  };
  const evidence = () => ({ observed_generation: 7 });

  const active = await api.performChatGptSessionArchiveStatus(command, evidence());
  assert.equal(active.command_accepted, true);
  assert.equal(active.lifecycle_observed, true);
  assert.equal(active.result?.is_archived, false);
  assert.equal(active.result?.archive_state, "active");
  assert.equal(ctx.clicks.length, 0);

  ctx.providerArchived = true;
  const archived = await api.performChatGptSessionArchiveStatus(command, evidence());
  assert.equal(archived.command_accepted, true);
  assert.equal(archived.lifecycle_observed, true);
  assert.equal(archived.result?.is_archived, true);
  assert.equal(archived.result?.archive_state, "archived");
  assert.equal(ctx.clicks.length, 0);
});

test("user archive status stays unknown on readback failure and never clicks | Given provider readback unavailable | When status is read | Then lifecycle is unconfirmed", async () => {
  const { ctx, api } = selfArchiveHarness({ archiveVerifies: false });
  const command = {
    operation: "herdr_mcp.browser_session.archive_status",
    expected_generation: 7,
    params: { session_ref: ctx.sessionRef, expected_generation: 7 },
  };
  const result = await api.performChatGptSessionArchiveStatus(command, { observed_generation: 7 });
  assert.equal(result.command_accepted, true);
  assert.equal(result.lifecycle_observed, false);
  assert.equal(result.result?.error, "browser_archive_status_readback_unavailable");
  assert.equal(ctx.clicks.length, 0);
});

test("user archive status reconciles from the archived list without mutation | Given direct conversation readback is unavailable but the exact id is in the archived list | When status is read | Then archived is proven and Archive is never clicked", async () => {
  const { ctx, api } = selfArchiveHarness({
    archiveVerifies: false,
    archiveListContains: true,
  });
  const command = {
    operation: "herdr_mcp.browser_session.archive_status",
    expected_generation: 7,
    params: { session_ref: ctx.sessionRef, expected_generation: 7 },
  };
  const result = await api.performChatGptSessionArchiveStatus(command, { observed_generation: 7 });
  assert.equal(result.command_accepted, true);
  assert.equal(result.lifecycle_observed, true);
  assert.equal(result.result?.is_archived, true);
  assert.equal(result.result?.archive_state, "archived");
  assert.equal(result.result?.readback_source, "archive_list");
  assert.equal(ctx.clicks.length, 0);
});

test("user re-archive of an already archived conversation confirms lifecycle without a second click | Given provider state already archived | When archive runs | Then it is applied readback-only", async () => {
  const { ctx, api } = selfArchiveHarness({ providerArchived: true, turnInProgress: false });
  const command = {
    operation: "herdr_mcp.browser_session.archive",
    expected_generation: 7,
    params: { session_ref: ctx.sessionRef, expected_generation: 7, idempotency_key: "archive-known-1" },
  };
  const result = await api.performChatGptSessionArchive(command, { observed_generation: 7 });
  assert.equal(result.command_accepted, true);
  assert.equal(result.lifecycle_observed, true);
  assert.equal(result.result?.is_archived, true);
  assert.equal(result.result?.already_archived, true);
  assert.equal(ctx.clicks.length, 0);
});

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

test("user archive reconciliation uses bounded temporary views | Given the exact session tab is closed | When archive or archive-status restores the canonical URL | Then mutation is visible while read-only status uses and closes an inactive temporary tab", () => {
  const start = backgroundSource.indexOf('const sessionRef = String(params.session_ref || "")');
  const end = backgroundSource.indexOf('const evidence = response?.evidence', start);
  assert.ok(start >= 0 && end > start, "archive target routing block must remain extractable");
  const segment = backgroundSource.slice(start, end);
  assert.match(segment, /operation === "herdr_mcp\.browser_session\.archive"/);
  assert.match(segment, /operation === "herdr_mcp\.browser_session\.archive_status"/);
  assert.match(segment, /const canonicalUrl = String\(params\.canonical_url \|\| ""\)/);
  assert.match(segment, /browserConversationInfo\(providerArchive, canonicalUrl\)/);
  assert.match(
    segment,
    /chrome\.tabs\.create\(\{\s*url: canonicalUrl,\s*active: operation !== "herdr_mcp\.browser_session\.archive_status"/,
  );
  assert.match(segment, /temporaryArchiveStatusTabId = createdTab\.id/);
  assert.match(segment, /chrome\.tabs\.remove\(temporaryArchiveStatusTabId\)/);
  assert.match(segment, /let actuationSessionRef = sessionRef/);
  assert.match(segment, /for \(const \[candidateRef, candidateTarget\] of browserSessionTargets\.entries\(\)\)/);
  assert.match(segment, /candidateTarget\?\.convKey === canonicalInfo\.convKey/);
  assert.match(segment, /actuationSessionRef = candidateRef/);
  assert.match(segment, /params: actuationParams/);
  assert.match(segment, /browserSessionTargets\.get\(sessionRef\)/);
  assert.match(segment, /Date\.now\(\) \+ 8000/);
  assert.match(segment, /createdTab\?\.id/);
});

test("user recovers a Claude dispatch target | Given one canonical Claude tab after service-worker target loss | When background performs canonical recovery | Then the unique tab is restored and duplicate views stay ambiguous", async () => {
  const canonical = "https://claude.ai/chat/46ea4d77-ef82-4ef6-a8f0-46c27f7593d0";
  const scope = {
    provider: "claude",
    observationGeneration: 17,
    accountRef: "br_claude_account",
    spaceRef: null,
  };
  const unique = claudeCanonicalIdentityRecoveryHarness([
    { id: 71, url: canonical, scope },
  ]);
  const recovered = await unique.find("claude", canonical, 17);
  assert.equal(recovered.ambiguous, false);
  assert.equal(recovered.target?.tabId, 71);
  assert.equal(recovered.target?.provider, "claude");
  assert.equal(recovered.target?.convKey, canonical);

  const loading = claudeCanonicalIdentityRecoveryHarness([
    { id: 73, url: canonical, status: "loading", scope },
  ]);
  const recoveredLoading = await loading.find("claude", canonical, 17);
  assert.equal(recoveredLoading.ambiguous, false);
  assert.equal(recoveredLoading.target?.tabId, 73);
  assert.equal(recoveredLoading.target?.convKey, canonical);

  const duplicate = claudeCanonicalIdentityRecoveryHarness([
    { id: 71, url: canonical, scope },
    { id: 72, url: canonical, scope },
  ]);
  const ambiguous = await duplicate.find("claude", canonical, 17);
  assert.equal(ambiguous.target, null);
  assert.equal(ambiguous.ambiguous, true);
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
  const recovery = segment.indexOf("recoverBrowserSessionTarget(", staleEviction);
  const canonicalRecovery = segment.indexOf(
    "findBrowserSessionTargetByCanonicalIdentity(",
    recovery,
  );
  assert.ok(staleEviction >= 0 && recovery > staleEviction, "stale cached target must be evicted before one exact recovery");
  assert.ok(canonicalRecovery > recovery, "canonical recovery must remain a bounded fallback after exact session-ref recovery");
  assert.match(
    segment,
    /recoverBrowserSessionTarget\(\s*sessionRef,\s*expectedGeneration,\s*String\(params\.provider \|\| ""\),\s*\)/,
  );
  assert.match(segment, /browserSessionTargets\.set\(sessionRef, target\)/);
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

test("shared native push stream fences retired runtime boots and reconnects", () => {
  assert.match(backgroundSource, /let pushStream = null; \/\/ \{ ctrl, bootId \}/);
  assert.match(backgroundSource, /function reconcilePushStreamRuntime\(state\)/);
  assert.match(backgroundSource, /currentBootId === streamBootId/);
  assert.match(backgroundSource, /push stream runtime changed .* forcing reconnect/);
  assert.match(backgroundSource, /stopPushStream\(\);\s*return true;/);
  const pushStart = backgroundSource.indexOf("async function runPushStream(ctrl)");
  const pushEnd = backgroundSource.indexOf("\nasync function postBrowserActuationEvidence", pushStart);
  const pushSegment = backgroundSource.slice(pushStart, pushEnd);
  assert.match(pushSegment, /handlePushBlock\(block, ctrl\)/);
  const blockStart = backgroundSource.indexOf("async function handlePushBlock(block, streamCtrl = null)");
  const blockEnd = backgroundSource.indexOf("\nasync function onPushHello", blockStart);
  const blockSegment = backgroundSource.slice(blockStart, blockEnd);
  assert.match(blockSegment, /pushStream\?\.ctrl === streamCtrl/);
  assert.match(blockSegment, /pushStream\.bootId = String\(data\.boot_id \|\| ""\)/);
  const aliveStart = backgroundSource.indexOf("async function ensureAlive(preloaded, runtimeState = null)");
  const aliveEnd = backgroundSource.indexOf("\n// ---- Install, browser startup", aliveStart);
  const aliveSegment = backgroundSource.slice(aliveStart, aliveEnd);
  assert.match(aliveSegment, /runtimeState\?\.ok === true \? runtimeState : await fetchStateFresh\(\)/);
  assert.match(aliveSegment, /reconcilePushStreamRuntime\(currentState\)/);
  const agentsStart = backgroundSource.indexOf('if (msg?.type === "h2w_agents")');
  const agentsEnd = backgroundSource.indexOf('if (msg?.type === "h2w_bind")', agentsStart);
  const agentsSegment = backgroundSource.slice(agentsStart, agentsEnd);
  assert.match(agentsSegment, /const state = await fetchStateFresh\(\)/);
  assert.match(agentsSegment, /await ensureAlive\(undefined, state\)/);
});

test("ChatGPT session.create carries one durable reservation across the new-conversation route", () => {
  const start = backgroundSource.indexOf('if (operation === "herdr_mcp.browser_session.create")');
  const end = backgroundSource.indexOf('if (operation === "herdr_mcp.browser_session.open")', start);
  assert.ok(start >= 0 && end > start, "session.create branch must precede session.open");
  const segment = backgroundSource.slice(start, end);
  assert.match(segment, /browserTabScopes\.get\(createdTab\.id\)/);
  assert.match(segment, /scope\.accountRef === accountRefCreate/);
  assert.match(segment, /scope\.spaceRef === spaceRefCreate/);
  assert.match(segment, /source_session_ref/);
  assert.match(segment, /resolveBrowserCreateAnchorWindow/);
  assert.match(segment, /scope\.observationGeneration === expectedGeneration/);
  assert.match(segment, /windowId: anchorWindowId/);
  assert.match(segment, /chrome\.tabs\.create\(\{ url: launchUrl, active: true \}\)/);
  assert.doesNotMatch(segment, /lastSeenAt/);
  assert.match(segment, /reservationRef/);
  assert.match(segment, /void sendBrowserActuationTabMessage\(createdTab\.id,/);
  assert.doesNotMatch(segment, /const response = await sendBrowserActuationTabMessage\(createdTab\.id,/);
  assert.match(segment, /\.then\(async \(response\) => \{/);
  assert.match(segment, /postBrowserActuationEvidence\(actuationId, evidence\)/);

  const createStart = wakeSource.indexOf('const creatingSession = command?.operation === "herdr_mcp.browser_session.create"');
  const createEnd = wakeSource.indexOf("\n  // Browser Registry identity cached by the page script", createStart);
  const createSegment = wakeSource.slice(createStart, createEnd);
  assert.match(createSegment, /sessionStorage\.setItem\(BROWSER_SESSION_RESERVATION_STORAGE_KEY, reservationRef\)/);
  assert.match(createSegment, /const chatMode = await ensureChatGptChatMode\(\)/);
  assert.match(createSegment, /result: \{ error: chatMode\.error \}/);
  assert.ok(
    createSegment.indexOf("const chatMode = await ensureChatGptChatMode()") < createSegment.indexOf("const composerReadyDeadline"),
    "fresh-session actuation must enter Chat mode before waiting for the composer",
  );
  assert.match(createSegment, /const composerReadyDeadline = Date\.now\(\) \+ 20000/);
  assert.match(createSegment, /let freshCreateStableIdleSamples = 0/);
  assert.match(createSegment, /const idleAndEmpty = inputMounted/);
  assert.match(createSegment, /&& !isTurnInProgress\(\)/);
  assert.match(createSegment, /&& !ADAPTER\.inputHasContent\(\)/);
  assert.match(createSegment, /freshCreateStableIdleSamples \+= 1/);
  assert.match(createSegment, /freshCreateStableIdleSamples >= 3/);
  assert.match(createSegment, /freshCreateStableIdleSamples = 0/);
  assert.match(createSegment, /if \(\(!creatingSession && isTurnInProgress\(\)\) \|\| ADAPTER\.inputHasContent\(\)\)/);
  assert.match(createSegment, /await wait\(200\)/);
  assert.ok(
    createSegment.indexOf("const composerReadyDeadline") < createSegment.indexOf("if ((!creatingSession && isTurnInProgress()) || ADAPTER.inputHasContent())"),
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
  const registrationEnd = wakeSource.indexOf("\n  function startConversationRouteWatch", registrationStart);
  assert.ok(registrationStart >= 0 && registrationEnd > registrationStart);
  const registrationSegment = wakeSource.slice(registrationStart, registrationEnd);
  assert.match(registrationSegment, /chatGptProjectRoute/);
  assert.match(registrationSegment, /chatGptProjectRoute\s*\?\s*\[\]\s*:\s*await chatGptProjectCatalog\(accountNativeIdentity\)/);
  assert.match(registrationSegment, /browserSessionReservationRef/);
  assert.match(registrationSegment, /clearBrowserPendingDispatchRefresh\(true\)/);
  const refreshClearStart = wakeSource.indexOf("function clearBrowserPendingDispatchRefresh");
  const refreshClearSegment = wakeSource.slice(refreshClearStart, refreshClearStart + 500);
  assert.match(refreshClearSegment, /sessionStorage\.removeItem\(BROWSER_SESSION_RESERVATION_STORAGE_KEY\)/);
});

test("ChatGPT browser actuation switches Work mode to Chat mode through the visible radio control", () => {
  const visibleHelperStart = wakeSource.indexOf("function visibleChatGptModeRadio(pattern)");
  const helperStart = wakeSource.indexOf("async function ensureChatGptChatMode()");
  const helperEnd = wakeSource.indexOf("\n  async function performBrowserActuationCommand", helperStart);
  assert.ok(visibleHelperStart >= 0 && helperStart > visibleHelperStart && helperEnd > helperStart, "Chat mode helpers must exist before browser actuation");
  const helper = wakeSource.slice(visibleHelperStart, helperEnd);
  assert.match(helper, /button\[role="radio"\]/);
  assert.match(helper, /getBoundingClientRect\(\)/);
  assert.match(helper, /rect\.width > 0/);
  assert.match(helper, /rect\.height > 0/);
  assert.match(helper, /聊天\|Chat\|チャット/);
  assert.match(helper, /工作\|Work\|作業/);
  assert.match(helper, /work\.getAttribute\("aria-checked"\) !== "true"/);
  assert.match(helper, /chat\.click\(\)/);
  assert.match(helper, /chat\.getAttribute\("aria-checked"\) === "true"/);
  assert.match(helper, /work\.getAttribute\("aria-checked"\) === "false"/);
  assert.match(helper, /chat_mode_switch_timeout/);
});

test("ChatGPT required_apps selects a real composer app pill and fails closed on ambiguity", () => {
  assert.match(backgroundSource, /"composer\.select_tool"/);
  assert.match(chatGptAdapterSource, /#composer-plus-btn/);
  assert.match(chatGptAdapterSource, /data-testid="composer-plus-btn"/);
  assert.match(chatGptAdapterSource, /data-inline-selection-pill/);
  assert.match(chatGptAdapterSource, /data-symbol="ecosystemMention"/);
  assert.match(chatGptAdapterSource, /data-keyword/);
  // The generalized picker resolves the pill from visible, scored candidates in
  // the opened menu; ambiguity stays fail-closed via candidates.length !== 1 in
  // wake.js (asserted below).
  assert.match(chatGptAdapterSource, /getComposerAppCandidates\(keyword\)/);
  assert.match(chatGptAdapterSource, /\[data-keyword\], \[data-value\]/);
  assert.match(chatGptAdapterSource, /keywordMatches\(node\)/);
  assert.match(chatGptAdapterSource, /visible\(node\)/);
  assert.match(chatGptAdapterSource, /if \(!menuRoots\.length\) return \[\]/);
  assert.match(chatGptAdapterSource, /for \(const root of menuRoots\)/);
  assert.match(chatGptAdapterSource, /return matches\.sort/);
  assert.match(wakeSource, /candidates\.length !== 1/);
  assert.match(wakeSource, /required-app-ambiguous/);
  assert.match(wakeSource, /required-app-not-found/);
  assert.match(wakeSource, /!composerModelVisibleText\(\)/);
  assert.match(wakeSource, /const search = selector \? await insertMainWorld\(app, selector\) : null/);
  assert.match(wakeSource, /if \(searchInserted\) await clearComposer\(\)/);
  assert.match(wakeSource, /const requestedApps = Array\.isArray\(params\.required_apps\)/);
  assert.match(wakeSource, /if \(!registeredHerdrAppKeyword\) return \[\]/);
  assert.match(wakeSource, /const observedHerdrApps = ADAPTER\.name === "chatgpt" \? currentHerdrRequiredApps\(\) : \[\]/);
  assert.match(wakeSource, /\.\.\.observedHerdrApps, \.\.\.requestedApps/);
  assert.doesNotMatch(wakeSource, /defaultChatGptApps/);
  assert.match(wakeSource, /composerHasOnlyAppPills\(requiredApps\)/);
});

test("user never gets a guessed ChatGPT app | Given no learned Herdr app identity | When recovery handoff or fresh session creation prepares another turn | Then only observed or inherited provider-owned identities are used", () => {
  assert.match(wakeSource, /function currentHerdrRequiredApps\(\)[\s\S]*if \(!registeredHerdrAppKeyword\) return \[\]/);
  assert.doesNotMatch(wakeSource, /defaultChatGptApps/);
  assert.doesNotMatch(wakeSource, /registeredHerdrAppKeyword \|\| "herdr"/);
  assert.doesNotMatch(wakeSource, /return alias \|\| "herdr"/);
  assert.match(backgroundSource, /function learnedBindingRequiredApps\(bindings\)/);
  assert.match(backgroundSource, /async function handoffMessageWithRequiredApps/);
  assert.match(backgroundSource, /createParams = \{ \.\.\.params, required_apps: inheritedRequiredApps \}/);
  assert.match(backgroundSource, /targetRow\.herdr_app_keyword = inheritedAppKeyword/);
});

test("user keeps Herdr attached across auto turns | Given one Herdr-enabled conversation | When Auto wakes continue the thread | Then only Herdr-generated turns reassert the app requirement", () => {
  const routeStart = backgroundSource.indexOf("async function routeWakeAttempt(");
  const routeEnd = backgroundSource.indexOf("async function deliverWakeToTab(", routeStart);
  assert.ok(routeStart >= 0 && routeEnd > routeStart);
  const route = backgroundSource.slice(routeStart, routeEnd);
  assert.match(route, /requiredApps:\s*bindingRequiredApps\(b\)/);
  assert.match(backgroundSource, /function bindingRequiredApps\(binding\)/);
  assert.match(backgroundSource, /herdr_app_keyword/);
  assert.match(backgroundSource, /b\.herdr_app_keyword = browserAppKeywords\[0\]/);
  assert.match(wakeSource, /browserAppKeywords/);
  assert.match(chatGptAdapterSource, /getLatestUserAppKeywords\(latestUser = null\)/);
  assert.match(wakeSource, /registeredConversationBound/);
  assert.match(wakeSource, /getLatestUserAppKeywords\(latestTurnForRole\("user"\)\)/);
  assert.match(wakeSource, /shouldLearnAppIdentity/);
  assert.match(wakeSource, /registerCurrentConversation\(shouldLearnAppIdentity \? "app-identity" : "poll"\)/);
  assert.match(wakeSource, /registerCurrentConversation\("binding-changed"\)/);
  assert.doesNotMatch(backgroundSource, /requiredApps:\s*\["herdr"\]/);
  assert.doesNotMatch(wakeSource, /requiredApps:\s*\["herdr"\]/);

  const wakeStart = wakeSource.indexOf("async function performWake(data)");
  const wakeEnd = wakeSource.indexOf("\n  // ---- Delivery confirmation", wakeStart);
  assert.ok(wakeStart >= 0 && wakeEnd > wakeStart);
  const wake = wakeSource.slice(wakeStart, wakeEnd);
  assert.match(wake, /const requiredApps =/);
  assert.match(wake, /if \(requiredApps\.length > 0\)/);
  assert.match(wake, /ensureRequiredComposerApps\(requiredApps\)/);
  assert.doesNotMatch(wake, /ensureHerdrComposerReference/);
  const resumeStart = wake.indexOf("if (resumeOnly) {");
  const resumeEnd = wake.indexOf("if (clearBeforeInsert) await clearComposer();", resumeStart);
  assert.ok(resumeStart >= 0 && resumeEnd > resumeStart);
  const resume = wake.slice(resumeStart, resumeEnd);
  assert.match(resume, /ensureRequiredComposerApps\(requiredApps\)/);
  assert.ok(resume.indexOf("ensureRequiredComposerApps(requiredApps)") < resume.indexOf("await submit()"),
    "resume-only submission must restore the required App before submit");
  const clearBeforeInsert = wake.indexOf("if (clearBeforeInsert) await clearComposer()");
  const mainAppSelection = wake.indexOf("let appSelection = { ok: true, apps: [] }");
  assert.ok(clearBeforeInsert >= 0 && mainAppSelection > clearBeforeInsert,
    "normal wake replacement clears stale text before selecting the required App");
});

test("user keeps Herdr attached on queued next-turn delivery | Given a bound ChatGPT conversation with an observed Herdr app keyword | When Queue delivers the next user turn | Then the delivery reuses only the observed app identity and never guesses an unknown keyword", () => {
  const queueStart = wakeSource.indexOf('if (msg?.type === "h2w_queue_deliver")');
  const queueEnd = wakeSource.indexOf('if (msg?.type === "h2w_wake")', queueStart);
  assert.ok(queueStart >= 0 && queueEnd > queueStart);
  const queue = wakeSource.slice(queueStart, queueEnd);
  assert.match(queue, /const queuedSelectedApps =/);
  assert.match(queue, /composerHasOnlyAppPills\(queuedSelectedApps\)/);
  assert.match(queue, /requiredApps:\s*queuedSelectedApps\.length > 0 \? queuedSelectedApps : currentHerdrRequiredApps\(\)/);
  assert.match(backgroundSource, /function bindingRequiredApps\(binding\)[\s\S]*return keyword \? \[keyword\] : \[\]/);
  const enqueueStart = wakeSource.indexOf("async function queueCurrentComposerMessage()");
  const enqueueEnd = wakeSource.indexOf("\n  function ensureQueuedInsertButton", enqueueStart);
  assert.ok(enqueueStart >= 0 && enqueueEnd > enqueueStart);
  const enqueue = wakeSource.slice(enqueueStart, enqueueEnd);
  assert.match(enqueue, /getComposerTextWithoutAppPills/);
  assert.match(enqueue, /selectedAppsBeforeQueue/);
  assert.match(enqueue, /ensureRequiredComposerApps\(selectedAppsBeforeQueue\)/);
  assert.ok(enqueue.indexOf("await clearComposer()") < enqueue.indexOf("ensureRequiredComposerApps(selectedAppsBeforeQueue)"),
    "queue must restore provider-owned App pills after clearing queued text");
});

// Regression: session.create must not passively deadlock on a Browser Registry
// scope only a freshly-created ChatGPT tab's content script can publish. After
// a worker reload or a slow Project mount the fresh tab's registration can lag
// the create scope-gate, so the handler used to time out its passive wait and
// misreport a healthy create as resource_unavailable — with a visible new tab
// already open but no authoritative create command ever delivered. The fix
// actively probes the exact tab with the non-mutating h2w_get_convkey identity
// handshake (which drives lazy registerCurrentConversation registration) while
// retaining the account/space/generation equality gate. Old code never probes,
// so the scope never materializes and the create still fails closed.
const contentActuationEvidenceWithReasonSource = (() => {
  const start = backgroundSource.indexOf("function contentActuationEvidenceWithReason(");
  const end = backgroundSource.indexOf("function unavailableBrowserActuationEvidence(", start);
  assert.ok(start >= 0 && end > start, "content evidence reason helper must remain extractable");
  return backgroundSource.slice(start, end);
})();

const recoverBrowserSessionTargetSource = (() => {
  const start = backgroundSource.indexOf("async function recoverBrowserSessionTarget(");
  const end = backgroundSource.indexOf("async function resolveBrowserCreateAnchorWindow({", start);
  assert.ok(start >= 0 && end > start, "session target recovery helper must remain extractable");
  return backgroundSource.slice(start, end);
})();
const createAnchorSource2 = (() => {
  const start = backgroundSource.indexOf("async function resolveBrowserCreateAnchorWindow({");
  const end = backgroundSource.indexOf("async function handleBrowserActuation", start);
  assert.ok(start >= 0 && end > start, "create anchor helper must remain extractable");
  return backgroundSource.slice(start, end);
})();
const createBranchSource = (() => {
  const start = backgroundSource.indexOf('if (operation === "herdr_mcp.browser_session.create")');
  const end = backgroundSource.indexOf('if (operation === "herdr_mcp.browser_session.open")', start);
  assert.ok(start >= 0 && end > start, "create branch must remain extractable");
  return backgroundSource.slice(start, end);
})();

function createActuationBranchHarness({
  publishOnProbe = true,
  publishOnProbeAttempt = 1,
  contentMode = "success",
} = {}) {
  // Virtual clock: the old 8s scope-gate deadline must elapse immediately so a
  // harness that models the no-probe failure returns fast.
  const clock = { value: 1_000_000 };
  const dateShim = { now: () => clock.value };
  const setTimeoutShim = (fn) => { clock.value += 200; fn(); return 0; };

  const tabs = new Map();
  const browserTabScopes = new Map();
  const browserSessionTargets = new Map();
  const postCalls = [];
  const actuationMessages = [];
  const removedTabs = [];
  let nextTabId = 100;
  let identityProbeAttempts = 0;
  const liveScopesByTab = new Map();

  const chrome = {
    tabs: {
      async get(tabId) {
        const t = tabs.get(tabId);
        if (!t) throw new Error(`tab ${tabId} missing`);
        return { ...t };
      },
      async query() {
        return [...tabs.values()].map((t) => ({ id: t.id, url: t.url, status: "complete" }));
      },
      async create(info) {
        const id = nextTabId++;
        const tab = { id, url: info.url, windowId: info.windowId ?? null };
        tabs.set(id, tab);
        return { ...tab };
      },
      async remove(tabId) {
        removedTabs.push(tabId);
        if (!tabs.delete(tabId)) throw new Error(`tab ${tabId} missing`);
      },
      async sendMessage(tabId, message) {
        if (message?.type === "h2w_get_convkey") {
          identityProbeAttempts += 1;
          if (publishOnProbe && identityProbeAttempts >= publishOnProbeAttempt) {
            // The content-script identity handshake drives lazy registration,
            // which publishes the tab's Browser Registry scope on the next poll.
            if (liveScopesByTab.has(tabId)) browserTabScopes.set(tabId, liveScopesByTab.get(tabId));
          }
        }
        return {};
      },
    },
  };
  const activeH2WTabUrlsForProvider = async () => ["https://chatgpt.com/*"];
  const browserConversationInfo = (provider, url) => provider === "chatgpt" ? chatGptConversationInfo(url) : null;
  const browserConversationInfoFromSupportedUrl = (rawUrl) => {
    const info = chatGptConversationInfo(rawUrl);
    return info ? { ...info } : null;
  };
  const sendTabMessageWithTimeout = async (tabId, message) => chrome.tabs.sendMessage(tabId, message);
  const unavailable = (expectedGeneration, reason, observedGeneration = expectedGeneration) => ({
    observed_generation: Math.max(1, Number(observedGeneration) || Number(expectedGeneration) || 1),
    command_accepted: false, browser_online: false, resource_available: false, rejected: false,
    stable_resource_ref_observed: false, lifecycle_observed: false, canonical_url_observed: false,
    accepted_message_observed: false, message_baseline_advanced: false,
    reasoning_effort_readback: null, required_apps_readback: [],
    generation_owner: null, generation_status_observed: false, generation_stopped: false,
    result: { error: reason },
  });
  const postBrowserActuationEvidence = async (id, evidence) => { postCalls.push({ id, evidence }); };
  const protectBoundTab = async () => {};
  const sendBrowserActuationTabMessage = async (tabId, message) => {
    actuationMessages.push({ tabId, message });
    if (contentMode === "throw") throw new Error("content dispatch failed");
    if (contentMode === "missing") return {};
    if (contentMode === "unavailable-missing-reason") {
      return {
        evidence: {
          observed_generation: 17,
          command_accepted: false,
          browser_online: true,
          resource_available: false,
          rejected: false,
          stable_resource_ref_observed: false,
          lifecycle_observed: false,
          canonical_url_observed: false,
          accepted_message_observed: false,
          message_baseline_advanced: false,
          reasoning_effort_readback: null,
          required_apps_readback: [],
          generation_owner: null,
          generation_status_observed: false,
          generation_stopped: false,
          result: null,
        },
      };
    }
    return {
      evidence: {
        observed_generation: 17,
        command_accepted: true,
        browser_online: true,
        resource_available: true,
        rejected: false,
        stable_resource_ref_observed: true,
        lifecycle_observed: true,
        canonical_url_observed: true,
        accepted_message_observed: true,
        message_baseline_advanced: false,
        reasoning_effort_readback: null,
        required_apps_readback: [],
        generation_owner: 17,
        generation_status_observed: true,
        generation_stopped: false,
        result: null,
      },
    };
  };

  const actuate = new Function(
    "chrome", "browserTabScopes", "browserSessionTargets", "activeH2WTabUrlsForProvider",
    "browserConversationInfo", "browserConversationInfoFromSupportedUrl",
    "sendTabMessageWithTimeout",
    "postBrowserActuationEvidence", "protectBoundTab", "sendBrowserActuationTabMessage",
    "unavailableBrowserActuationEvidence", "BROWSER_CREATE_CONTENT_TIMEOUT_MS", "Date", "setTimeout",
    `async function __actuate(command) {\n` +
    `const actuationId = String(command?.actuation_id || "");\n` +
    `const dispatchId = String(command?.dispatch_id || "");\n` +
    `const operation = String(command?.operation || "");\n` +
    `const expectedGeneration = Number(command?.expected_generation || 0);\n` +
    `const params = command?.params && typeof command.params === "object" ? command.params : {};\n` +
    `${contentActuationEvidenceWithReasonSource}\n${recoverBrowserSessionTargetSource}\n${createAnchorSource2}\n${createBranchSource}\n}\nreturn __actuate;`,
  )(chrome, browserTabScopes, browserSessionTargets, activeH2WTabUrlsForProvider,
    browserConversationInfo, browserConversationInfoFromSupportedUrl,
    sendTabMessageWithTimeout,
    postBrowserActuationEvidence, protectBoundTab, sendBrowserActuationTabMessage,
    unavailable, 43_000, dateShim, setTimeoutShim);

  return {
    actuate,
    postCalls,
    actuationMessages,
    browserTabScopes,
    browserSessionTargets,
    tabs,
    removedTabs,
    liveScopesByTab,
    identityProbeAttempts: () => identityProbeAttempts,
    dateShim,
  };
}

test("session.create closes the startup scope deadlock with a non-mutating identity probe", async () => {
  const accountRef = "br_account_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
  const spaceRef = "br_space_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
  const projectId = "g-p-6a89c078669481918c8eb70fdfd3d978";
  const harness = createActuationBranchHarness();
  const sourceTabId = 71;
  harness.browserTabScopes.set(sourceTabId, {
    provider: "chatgpt", accountRef, spaceRef, observationGeneration: 17,
  });
  harness.tabs.set(sourceTabId, { id: sourceTabId, url: `https://chatgpt.com/g/${projectId}/project`, windowId: 11 });
  // The freshly-created tab (id 100) will only publish its scope on the
  // non-mutating h2w_get_convkey probe, not on a passive poll.
  harness.liveScopesByTab.set(100, {
    provider: "chatgpt", accountRef, spaceRef, observationGeneration: 17,
  });

  const actuate = harness.actuate;
  await actuate({
    protocol: "herdr-browser-actuation/v1",
    actuation_id: "ba_" + "0".repeat(16),
    operation: "herdr_mcp.browser_session.create",
    expected_generation: 17,
    params: {
      provider: "chatgpt",
      account_ref: accountRef,
      space_ref: spaceRef,
      reservation_ref: "bsr_" + "1".repeat(64),
      launch_url: `https://chatgpt.com/g/${projectId}`,
    },
  });
  for (let i = 0; i < 8; i += 1) await Promise.resolve();

  // The gate must not deadlock: the authoritative create command is delivered
  // to the exact freshly-created tab (window affinity preserved, new tab id 100).
  assert.deepEqual(harness.actuationMessages.map((m) => m.tabId), [100]);
  assert.equal(
    harness.actuationMessages[0].message.command.operation,
    "herdr_mcp.browser_session.create",
  );
  // On probe-driven scope success the create proceeds and reports resource_available.
  assert.ok(harness.postCalls.length > 0, "create must post actuation evidence");
  const success = harness.postCalls.find((c) => c.evidence?.resource_available === true);
  assert.ok(success, "probe-driven create must complete with resource_available=true");
  assert.equal(success.evidence.command_accepted, true);
  assert.equal(harness.postCalls.some((c) => c.evidence?.resource_available === false), false);
});

test("user Given a late ChatGPT content script When create retries read-only identity probes Then the matching scope is accepted without retrying the mutation", async () => {
  const accountRef = "br_account_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
  const spaceRef = "br_space_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
  const projectId = "g-p-6a89c078669481918c8eb70fdfd3d978";
  const harness = createActuationBranchHarness({ publishOnProbeAttempt: 3 });
  harness.browserTabScopes.set(71, {
    provider: "chatgpt", accountRef, spaceRef, observationGeneration: 17,
  });
  harness.tabs.set(71, { id: 71, url: `https://chatgpt.com/g/${projectId}/project`, windowId: 11 });
  harness.liveScopesByTab.set(100, {
    provider: "chatgpt", accountRef, spaceRef, observationGeneration: 17,
  });

  await harness.actuate({
    protocol: "herdr-browser-actuation/v1",
    actuation_id: "ba_" + "9".repeat(16),
    operation: "herdr_mcp.browser_session.create",
    expected_generation: 17,
    params: {
      provider: "chatgpt",
      account_ref: accountRef,
      space_ref: spaceRef,
      reservation_ref: "bsr_" + "a".repeat(64),
      launch_url: `https://chatgpt.com/g/${projectId}`,
    },
  });
  for (let i = 0; i < 12; i += 1) await Promise.resolve();

  assert.ok(
    harness.identityProbeAttempts() >= 3,
    "late content injection must trigger another read-only identity probe",
  );
  assert.deepEqual(
    harness.actuationMessages.map((message) => message.tabId),
    [100],
    "the create mutation must still be delivered exactly once",
  );
  const success = harness.postCalls.find((call) => call.evidence?.resource_available === true);
  assert.ok(success, "late scope registration must still complete the create actuation");
  assert.equal(success.evidence.command_accepted, true);
  assert.equal(
    harness.postCalls.some((call) => call.evidence?.result?.error === "browser_create_scope_unavailable"),
    false,
  );
});

test("user receives a precise boundary reason | Given fresh create content reports unavailable without a reason | When background settles the actuation | Then the missing content reason is classified", async () => {
  const accountRef = "br_account_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
  const spaceRef = "br_space_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
  const projectId = "g-p-6a89c078669481918c8eb70fdfd3d978";
  const harness = createActuationBranchHarness({ contentMode: "unavailable-missing-reason" });
  harness.browserTabScopes.set(71, {
    provider: "chatgpt", accountRef, spaceRef, observationGeneration: 17,
  });
  harness.tabs.set(71, { id: 71, url: `https://chatgpt.com/g/${projectId}/project`, windowId: 11 });
  harness.liveScopesByTab.set(100, {
    provider: "chatgpt", accountRef, spaceRef, observationGeneration: 17,
  });

  await harness.actuate({
    protocol: "herdr-browser-actuation/v1",
    actuation_id: "ba_" + "9".repeat(16),
    operation: "herdr_mcp.browser_session.create",
    expected_generation: 17,
    params: {
      provider: "chatgpt",
      account_ref: accountRef,
      space_ref: spaceRef,
      reservation_ref: "bsr_" + "9".repeat(64),
      launch_url: `https://chatgpt.com/g/${projectId}`,
    },
  });
  for (let i = 0; i < 12; i += 1) await Promise.resolve();

  const evidence = harness.postCalls.at(-1)?.evidence;
  assert.equal(evidence?.resource_available, false);
  assert.equal(evidence?.result?.error, "browser_create_content_reason_missing");
});

test("user Given a fresh ChatGPT create When the content response is missing Then the exact machine reason is preserved", async () => {
  const accountRef = "br_account_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
  const spaceRef = "br_space_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
  const projectId = "g-p-6a89c078669481918c8eb70fdfd3d978";
  const harness = createActuationBranchHarness({ contentMode: "missing" });
  harness.browserTabScopes.set(71, {
    provider: "chatgpt", accountRef, spaceRef, observationGeneration: 17,
  });
  harness.tabs.set(71, { id: 71, url: `https://chatgpt.com/g/${projectId}/project`, windowId: 11 });
  harness.liveScopesByTab.set(100, {
    provider: "chatgpt", accountRef, spaceRef, observationGeneration: 17,
  });

  await harness.actuate({
    protocol: "herdr-browser-actuation/v1",
    actuation_id: "ba_" + "5".repeat(16),
    operation: "herdr_mcp.browser_session.create",
    expected_generation: 17,
    params: {
      provider: "chatgpt",
      account_ref: accountRef,
      space_ref: spaceRef,
      reservation_ref: "bsr_" + "6".repeat(64),
      launch_url: `https://chatgpt.com/g/${projectId}`,
    },
  });
  for (let i = 0; i < 12; i += 1) await Promise.resolve();

  const evidence = harness.postCalls.at(-1)?.evidence;
  assert.equal(evidence?.command_accepted, true);
  assert.equal(evidence?.resource_available, true);
  assert.equal(evidence?.result?.error, "browser_create_content_response_missing");
});

test("user Given a fresh ChatGPT create When content dispatch throws Then the exact machine reason is preserved", async () => {
  const accountRef = "br_account_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
  const spaceRef = "br_space_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
  const projectId = "g-p-6a89c078669481918c8eb70fdfd3d978";
  const harness = createActuationBranchHarness({ contentMode: "throw" });
  harness.browserTabScopes.set(71, {
    provider: "chatgpt", accountRef, spaceRef, observationGeneration: 17,
  });
  harness.tabs.set(71, { id: 71, url: `https://chatgpt.com/g/${projectId}/project`, windowId: 11 });
  harness.liveScopesByTab.set(100, {
    provider: "chatgpt", accountRef, spaceRef, observationGeneration: 17,
  });

  await harness.actuate({
    protocol: "herdr-browser-actuation/v1",
    actuation_id: "ba_" + "7".repeat(16),
    operation: "herdr_mcp.browser_session.create",
    expected_generation: 17,
    params: {
      provider: "chatgpt",
      account_ref: accountRef,
      space_ref: spaceRef,
      reservation_ref: "bsr_" + "8".repeat(64),
      launch_url: `https://chatgpt.com/g/${projectId}`,
    },
  });
  for (let i = 0; i < 12; i += 1) await Promise.resolve();

  const evidence = harness.postCalls.at(-1)?.evidence;
  assert.equal(evidence?.command_accepted, true);
  assert.equal(evidence?.resource_available, true);
  assert.equal(evidence?.result?.error, "browser_create_content_dispatch_failed");
  assert.equal(evidence?.result?.tab_opened, true);
  assert.equal(evidence?.result?.tab_closed, true);
  assert.equal(evidence?.result?.tab_cleanup_verified, true);
  assert.deepEqual(harness.removedTabs, [100]);
  assert.equal(harness.tabs.has(71), true, "content failure cleanup must preserve the user's anchor tab");
});

test("user can create two independent workers | Given one exact Project scope spans multiple windows and the original source tab is unavailable | When two session.create mutations run consecutively while sibling workers are generating | Then each mutation opens and delivers only to its own fresh tab", async () => {
  const accountRef = "br_account_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
  const spaceRef = "br_space_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
  const projectId = "g-p-6a89c078669481918c8eb70fdfd3d978";
  const sourceSessionRef = "br_" + "3".repeat(64);
  const harness = createActuationBranchHarness();
  harness.browserTabScopes.set(71, {
    provider: "chatgpt", accountRef, spaceRef, observationGeneration: 17, executionState: "generating",
  });
  harness.browserTabScopes.set(72, {
    provider: "chatgpt", accountRef, spaceRef, observationGeneration: 17, executionState: "generating",
  });
  harness.tabs.set(71, { id: 71, url: `https://chatgpt.com/g/${projectId}/c/worker-a`, windowId: 22 });
  harness.tabs.set(72, { id: 72, url: `https://chatgpt.com/g/${projectId}/c/worker-b`, windowId: 11 });
  harness.liveScopesByTab.set(100, {
    provider: "chatgpt", accountRef, spaceRef, observationGeneration: 17,
  });
  harness.liveScopesByTab.set(101, {
    provider: "chatgpt", accountRef, spaceRef, observationGeneration: 17,
  });

  for (const [suffix, reservationDigit] of [["a", "4"], ["b", "5"]]) {
    await harness.actuate({
      protocol: "herdr-browser-actuation/v1",
      actuation_id: "ba_" + suffix.repeat(16),
      operation: "herdr_mcp.browser_session.create",
      expected_generation: 17,
      params: {
        provider: "chatgpt",
        account_ref: accountRef,
        space_ref: spaceRef,
        source_session_ref: sourceSessionRef,
        reservation_ref: "bsr_" + reservationDigit.repeat(64),
        launch_url: `https://chatgpt.com/g/${projectId}`,
      },
    });
    for (let i = 0; i < 8; i += 1) await Promise.resolve();
  }

  assert.deepEqual(harness.actuationMessages.map((message) => message.tabId), [100, 101]);
  assert.equal(harness.tabs.get(100)?.windowId, 11);
  assert.equal(harness.tabs.get(101)?.windowId, 11);
  assert.equal(harness.postCalls.filter((call) => call.evidence?.command_accepted === true).length, 2);
  assert.equal(
    harness.postCalls.some((call) => call.evidence?.result?.error === "source_session_unavailable"),
    false,
  );
});

test("session.create still fails closed when the fresh tab can never be identified", async () => {
  const accountRef = "br_account_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
  const spaceRef = "br_space_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
  const projectId = "g-p-6a89c078669481918c8eb70fdfd3d978";
  const harness = createActuationBranchHarness({ publishOnProbe: false });
  const sourceTabId = 71;
  harness.browserTabScopes.set(sourceTabId, {
    provider: "chatgpt", accountRef, spaceRef, observationGeneration: 17,
  });
  harness.tabs.set(sourceTabId, { id: sourceTabId, url: `https://chatgpt.com/g/${projectId}/project`, windowId: 11 });

  const actuate = harness.actuate;
  await actuate({
    protocol: "herdr-browser-actuation/v1",
    actuation_id: "ba_" + "0".repeat(16),
    operation: "herdr_mcp.browser_session.create",
    expected_generation: 17,
    params: {
      provider: "chatgpt",
      account_ref: accountRef,
      space_ref: spaceRef,
      reservation_ref: "bsr_" + "1".repeat(64),
      launch_url: `https://chatgpt.com/g/${projectId}`,
    },
  });
  for (let i = 0; i < 60; i += 1) await Promise.resolve();

  assert.equal(harness.actuationMessages.length, 0, "no create command may be delivered without a matching scope");
  const failure = harness.postCalls.find((c) => c.evidence?.resource_available === false);
  assert.ok(failure, "unidentifiable fresh tab must fail closed with resource_available=false");
  assert.equal(failure.evidence.command_accepted, false);
  assert.equal(failure.evidence.result?.error, "browser_create_scope_unavailable");
  assert.equal(failure.evidence.browser_online, true);
  assert.equal(failure.evidence.result?.phase, "scope_handshake");
  assert.equal(failure.evidence.result?.message_submitted, false);
  assert.equal(failure.evidence.result?.retry_safe, true);
  assert.equal(failure.evidence.result?.tab_opened, true);
  assert.equal(failure.evidence.result?.tab_closed, true);
  assert.equal(failure.evidence.result?.tab_cleanup_verified, true);
  assert.deepEqual(harness.removedTabs, [100]);
  assert.equal(harness.tabs.has(100), false);
  assert.equal(harness.tabs.has(sourceTabId), true, "scope cleanup must not close the user's anchor tab");
});

test("user still fails closed after source recovery | Given source affinity falls back to an exact Project scope | When the fresh tab never proves that scope | Then no create command is delivered", async () => {
  const accountRef = "br_account_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
  const spaceRef = "br_space_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
  const projectId = "g-p-6a89c078669481918c8eb70fdfd3d978";
  const harness = createActuationBranchHarness();
  const sourceTabId = 71;
  harness.browserTabScopes.set(sourceTabId, {
    provider: "chatgpt", accountRef, spaceRef, observationGeneration: 17,
  });
  harness.tabs.set(sourceTabId, { id: sourceTabId, url: `https://chatgpt.com/g/${projectId}/project`, windowId: 11 });

  await harness.actuate({
    protocol: "herdr-browser-actuation/v1",
    actuation_id: "ba_" + "2".repeat(16),
    operation: "herdr_mcp.browser_session.create",
    expected_generation: 17,
    params: {
      provider: "chatgpt",
      account_ref: accountRef,
      space_ref: spaceRef,
      source_session_ref: "br_" + "3".repeat(64),
      reservation_ref: "bsr_" + "4".repeat(64),
      launch_url: `https://chatgpt.com/g/${projectId}`,
    },
  });

  const failure = harness.postCalls.find((c) => c.evidence?.resource_available === false);
  assert.ok(failure, "unverified fresh tab must fail closed");
  assert.equal(failure.evidence.command_accepted, false);
  assert.equal(failure.evidence.result?.error, "browser_create_scope_unavailable");
  assert.equal(harness.actuationMessages.length, 0);
});
