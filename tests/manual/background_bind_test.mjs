#!/usr/bin/env node
/**
 * Drive extension/background.js with a Chrome API mock and verify the full
 * h2w_bind flow: binding creation, push stream, and message routing.
 *
 * Usage: node tests/manual/background_bind_test.mjs
 */
import { pathToFileURL } from "node:url";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { readFileSync } from "node:fs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const EXT = path.join(__dirname, "..", "..", "extension");
const CONV = "https://chatgpt.com/c/abc123";
const CHATGPT_AUTO_CONV = "https://chatgpt.com/c/plain-auto-123";
const SK_WH = `${CONV}::wH`;
const PROJECT_ID = "g-p-6a89c078669481918c8eb70fdfd3d978";
const PROJECT_KEY = `https://chatgpt.com/g/${PROJECT_ID}`;
const PROJECT_HOME_URL = `https://chatgpt.com/g/${PROJECT_ID}-herdr-mcp/project`;
const PROJECT_SOURCE = `${PROJECT_KEY}/c/source123`;
const PROJECT_SOURCE_URL = `https://chatgpt.com/g/${PROJECT_ID}-herdr-mcp/c/source123`;
const PROJECT_TARGET = `https://chatgpt.com/g/${PROJECT_ID}/c/target456`;
const PROJECT_TARGET_URL = `https://chatgpt.com/g/${PROJECT_ID}-herdr-mcp/c/target456`;
const ZAI_CONV = "https://chat.z.ai/c/json-bridge-test";
const ZAI_ROOT = "https://chat.z.ai";
const ZAI_NEW = "https://chat.z.ai/c/new-chat-123";
const ZAI_OTHER = "https://chat.z.ai/c/history-chat-456";
const ZAI_SOURCE = "https://chat.z.ai/c/handoff-source";
const ZAI_TARGET = "https://chat.z.ai/c/handoff-target";

let failures = 0;
function ok(cond, label, detail = "") {
  if (cond) console.log(`  ✅ ${label}`);
  else { failures++; console.error(`  ❌ ${label} ${detail}`); }
}

async function waitForTest(predicate, timeoutMs = 5000, pollMs = 20) {
  const deadline = Date.now() + timeoutMs;
  do {
    if (predicate()) return true;
    await new Promise((resolve) => setTimeout(resolve, pollMs));
  } while (Date.now() < deadline);
  return Boolean(predicate());
}

// ---- chrome mock ----
const storage = { herdrWakeBindings: {}, herdrMcpUrl: "http://127.0.0.1:8772", token: "test-token", enabled: true, wakeTemplate: "a {status}" };
const listeners = { onMessage: [], onStartup: [], onInstalled: [], onActivated: [] };
const sentMessages = []; // Messages from background to content.
const tabs = new Map();   // tabId -> { url, listener }.
let nextTabId = 500;
let handoffSeedMode = "uncertain";
let targetSeeded = false;
let zaiTargetSeeded = false;
let handoffPrompt = "";
let targetComposerReady = true;
let targetComposerReadyAfter = 0;
let targetProbeCount = 0;
let targetSeedCount = 0;
let tabCreateCount = 0;
let tabUpdateCount = 0;
let lastTabUpdate = null;
let mockStateWorkspaces = [];
let mockLocalRuntimeAvailable = true;
let hangAutomationNotifications = false;

const nativeFetch = globalThis.fetch;
globalThis.fetch = async (input, init) => {
  const url = String(input || "");
  if (url.startsWith("chrome-extension://test-ext/")) {
    const rel = url.slice("chrome-extension://test-ext/".length);
    return new Response(readFileSync(path.join(EXT, rel), "utf8"), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  }
  if (url.endsWith("/push/state")) {
    return new Response(JSON.stringify({ workspaces: mockStateWorkspaces, panes: [], agents: [] }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  }
  if (url.endsWith("/mcp")) {
    const body = JSON.parse(init?.body || "{}");
    const result = body.method === "tools/list"
      ? { tools: [{ name: "herdr_inspect", description: "inspect", inputSchema: { type: "object" } }] }
      : { content: [{ type: "text", text: "ok" }] };
    return new Response(JSON.stringify({ jsonrpc: "2.0", id: body.id, result }), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  }
  return nativeFetch(input, init);
};

function targetListener(tab) {
  return (msg, _sender, sendResponse) => {
    if (String(tab.url || "").startsWith("https://chat.z.ai")) {
      if (msg?.type === "h2w_get_convkey") {
        sendResponse({
          convKey: zaiTargetSeeded ? ZAI_TARGET : ZAI_ROOT,
          url: tab.url,
          site: "z.ai",
        });
        return;
      }
      if (msg?.type === "h2w_handoff_probe") {
        sendResponse({
          ok: true,
          targetConvKey: zaiTargetSeeded ? ZAI_TARGET : ZAI_ROOT,
          targetUrl: tab.url,
          seedConfirmed: zaiTargetSeeded,
          composerReady: true,
        });
        return;
      }
      if (msg?.type === "h2w_handoff_seed") {
        zaiTargetSeeded = true;
        tab.url = ZAI_TARGET;
        sendResponse({
          ok: true,
          targetConvKey: ZAI_TARGET,
          targetUrl: ZAI_TARGET,
          seedConfirmed: true,
        });
        return;
      }
      sendResponse({ ok: true });
      return;
    }
    if (msg?.type === "h2w_get_convkey") {
      sendResponse({
        convKey: targetSeeded ? PROJECT_TARGET : null,
        url: tab.url,
        site: "chatgpt",
      });
      return;
    }
    if (msg?.type === "h2w_handoff_probe") {
      targetProbeCount += 1;
      const composerReady = targetComposerReady && targetProbeCount > targetComposerReadyAfter;
      sendResponse({
        ok: true,
        targetConvKey: targetSeeded ? PROJECT_TARGET : null,
        targetUrl: tab.url,
        seedConfirmed: targetSeeded,
        composerReady,
      });
      return;
    }
    if (msg?.type === "h2w_handoff_seed") {
      targetSeedCount += 1;
      if (handoffSeedMode === "confirmed") {
        targetSeeded = true;
        tab.url = PROJECT_TARGET_URL;
        sendResponse({
          ok: true,
          targetConvKey: PROJECT_TARGET,
          targetUrl: PROJECT_TARGET_URL,
          seedConfirmed: true,
        });
      } else if (handoffSeedMode === "insert_failed_but_committed") {
        targetSeeded = true;
        tab.url = PROJECT_TARGET_URL;
        sendResponse({ ok: false, error: "insert-failed" });
      } else {
        sendResponse({ ok: true, targetConvKey: null, targetUrl: tab.url, seedConfirmed: false });
      }
      return;
    }
    sendResponse({ ok: true });
  };
}

globalThis.chrome = {
  runtime: {
    id: "test-ext",
    getURL: (rel) => `chrome-extension://test-ext/${rel}`,
    lastError: null,
    onMessage: { addListener: (fn) => listeners.onMessage.push(fn) },
    onStartup: { addListener: (fn) => listeners.onStartup.push(fn) },
    onInstalled: { addListener: (fn) => listeners.onInstalled.push(fn) },
    openOptionsPage: () => {},
    sendNativeMessage(_host, message, callback) {
      if (message?.type !== "request") {
        callback({ ok: false, error: "unsupported-native-test-message" });
        return;
      }
      if (message.path === "/push/state") {
        if (!mockLocalRuntimeAvailable) {
          callback({ ok: false, error: "connect ECONNREFUSED 127.0.0.1:8772" });
          return;
        }
        callback({
          ok: true,
          transport: "ipc",
          status: 200,
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ workspaces: mockStateWorkspaces, panes: [], agents: [] }),
        });
        return;
      }
      if (message.path === "/mcp") {
        const body = JSON.parse(message.body || "{}");
        const result = body.method === "tools/list"
          ? { tools: [{ name: "herdr_inspect", description: "inspect", inputSchema: { type: "object" } }] }
          : { content: [{ type: "text", text: "ok" }] };
        callback({
          ok: true,
          transport: "ipc",
          status: 200,
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ jsonrpc: "2.0", id: body.id, result }),
        });
        return;
      }
      callback({ ok: false, error: `unexpected-native-path:${message.path}` });
    },
    connectNative() {
      const messageListeners = [];
      const disconnectListeners = [];
      let disconnected = false;
      return {
        onMessage: { addListener(fn) { messageListeners.push(fn); } },
        onDisconnect: { addListener(fn) { disconnectListeners.push(fn); } },
        postMessage(message) {
          if (message?.type === "stream" && message.path === "/push/events") {
            queueMicrotask(() => {
              if (disconnected) return;
              for (const fn of messageListeners) fn({ type: "stream_open", status: 200, transport: "ipc" });
            });
          }
        },
        disconnect() {
          if (disconnected) return;
          disconnected = true;
          for (const fn of disconnectListeners) fn();
        },
      };
    },
  },
  storage: {
    local: {
      async get(keys) {
        if (typeof keys === "string") keys = [keys];
        const out = {};
        for (const k of keys) out[k] = storage[k];
        return out;
      },
      async set(obj) { Object.assign(storage, obj); },
      async remove(keys) {
        for (const key of Array.isArray(keys) ? keys : [keys]) delete storage[key];
      },
    },
  },
  tabs: {
    async query({ url }) {
      const glob = url.replace("*", "");
      return [...tabs.values()].filter((t) => t.url.startsWith(glob)).map((t) => ({ id: t.id, url: t.url }));
    },
    async sendMessage(tabId, msg) {
      if (hangAutomationNotifications && msg?.type === "h2w_automation_changed") {
        return new Promise(() => {});
      }
      const t = tabs.get(tabId);
      if (!t?.listener) throw new Error(`no content script in tab ${tabId}`);
      sentMessages.push({ tabId, msg });
      let resp;
      let sync = false;
      await new Promise((resolve) => {
        t.listener(msg, { tab: { id: tabId } }, (r) => { resp = r; sync = true; resolve(); });
        // Resolve immediately for synchronous sendResponse, otherwise time out.
        setTimeout(resolve, 200);
      });
      return resp;
    },
    async get(tabId) {
      const t = tabs.get(tabId);
      if (!t) throw new Error(`tab ${tabId} missing`);
      return { id: t.id, url: t.url, status: t.status || "complete", active: t.active === true, windowId: 1 };
    },
    async create({ url }) {
      tabCreateCount += 1;
      const id = ++nextTabId;
      const tab = { id, url, status: "complete", active: true, listener: null };
      tab.listener = targetListener(tab);
      tabs.set(id, tab);
      return { id, url, status: "complete" };
    },
    async update(tabId, props = {}) {
      const tab = tabs.get(tabId);
      if (!tab) throw new Error(`tab ${tabId} missing`);
      if (props.url) {
        tabUpdateCount += 1;
        lastTabUpdate = { tabId, ...props };
        tab.url = props.url;
        // Navigation destroys the source content script and loads the target
        // page in the same tab. Attribute-only updates must preserve it.
        tab.listener = targetListener(tab);
      }
      if (Object.prototype.hasOwnProperty.call(props, "active")) tab.active = props.active === true;
      tab.status = "complete";
      return { id: tab.id, url: tab.url, status: tab.status, active: tab.active };
    },
    reload: () => {},
    onActivated: { addListener: (fn) => listeners.onActivated.push(fn) },
  },
  scripting: { executeScript: async () => [{ result: { ok: true } }] },
  alarms: { create: () => {}, onAlarm: { addListener: () => {} } },
};

// Content-script stub for wake.js h2w_get_convkey responses.
function installContentScript(tabId, url, convKey, site = "chatgpt") {
  tabs.set(tabId, {
    url, listener: (msg, _sender, sendResponse) => {
      if (msg?.type === "h2w_get_convkey") { sendResponse({ convKey, url, site }); return; }
      if (msg?.type === "h2w_wake") { sendResponse({}); return; }
      if (msg?.type === "h2w_handoff_prompt") { handoffPrompt = msg.template || ""; sendResponse({ ok: true }); return; }
      sendResponse({});
    },
  });
}

// ---- Load background.js ----
await import(pathToFileURL(path.join(__dirname, "..", "..", "extension", "background.js")).href);
const onMsg = listeners.onMessage[0];
ok(!!onMsg, "background onMessage listener registered");

// ---- Scenario 1: successful binding on a supported active tab ----
console.log("\n[binding flow]");
{
  installContentScript(101, CONV, CONV);
  let resolveP;
  const p = new Promise((r) => { resolveP = r; });
  onMsg({ type: "h2w_bind", tabId: 101, pane: "wH:p1", agent: "omp", workspace_id: "wH", workspace_label: "herdr-mcp (wH)", workspace_label_raw: "herdr-mcp" }, { tab: { id: 101 } }, (r) => resolveP(r));
  const r = await p;
  ok(r?.ok === true && r.convKey === CONV, "h2w_bind creates a binding", JSON.stringify(r));
  ok(!!storage.herdrWakeBindings[SK_WH], "binding persisted with convKey::workspace_id key");
  const b = storage.herdrWakeBindings[SK_WH];
  ok(
    b.workspace_id === "wH"
      && b.workspace_label.includes("herdr-mcp")
      && b.agent == null
      && b.persistence === "explicit"
      && b.expires_at == null
      && typeof b.last_seen_at === "number"
      && typeof b.continuity_id === "string"
      && b.continuity_id.startsWith("hc:")
      && !!b.revision,
    "binding is explicit, persistent and carries a continuity id",
    JSON.stringify(b),
  );
  const bound = sentMessages.find((m) => m.msg?.type === "h2w_bound");
  ok(!!bound, "content script receives h2w_bound");
}

// ---- Scenario 2: active tab without a content script ----
{
  const before = Object.keys(storage.herdrWakeBindings).length;
  let resolveP;
  const p = new Promise((r) => { resolveP = r; });
  onMsg({ type: "h2w_bind", tabId: 999, pane: "wH:p1", agent: "omp" }, { tab: { id: 999 } }, (r) => resolveP(r));
  const r = await p;
  ok(r?.ok === false && r.error === "conversation-unavailable", "unsupported tab returns conversation-unavailable", JSON.stringify(r));
  ok(Object.keys(storage.herdrWakeBindings).length === before, "failed binding creates no entry");
}

// ---- Scenario 3: duplicate binding same workspace ----
{
  let resolveP;
  const p = new Promise((r) => { resolveP = r; });
  onMsg({ type: "h2w_bind", tabId: 101, pane: "wH:p1", agent: "omp", workspace_id: "wH" }, { tab: { id: 101 } }, (r) => resolveP(r));
  const r = await p;
  ok(r?.ok === false && r.error === "already-bound", "duplicate workspace binding is rejected", JSON.stringify(r));
}

// ---- Scenario 3b: second workspace on same conversation ----
{
  let resolveP;
  const p = new Promise((r) => { resolveP = r; });
  onMsg({ type: "h2w_bind", tabId: 101, pane: "w2Y:p1", agent: "omp", workspace_id: "w2Y", workspace_label: "other (w2Y)" }, { tab: { id: 101 } }, (r) => resolveP(r));
  const r = await p;
  ok(r?.ok === true && r.workspace_id === "w2Y", "second workspace on same conversation binds", JSON.stringify(r));
  ok(!!storage.herdrWakeBindings[`${CONV}::w2Y`], "second binding persisted");
}

// ---- Scenario 4: register restores tabId after refresh ----
{
  let resolveP;
  const p = new Promise((r) => { resolveP = r; });
  onMsg({ type: "h2w_register", convKey: CONV, url: CONV, site: "chatgpt" }, { tab: { id: 202 } }, (r) => resolveP(r));
  const r = await p;
  ok(r?.bound === true && (r.workspace_id === "wH" || r.pane === "wH:p1"), "register restores the binding", JSON.stringify(r));
  ok(storage.herdrWakeBindings[SK_WH].tabId === 202, "tabId updated on first binding");
  ok(storage.herdrWakeBindings[`${CONV}::w2Y`].tabId === 202, "tabId updated on second binding");
}

// ---- Scenario 5: unbind one workspace ----
{
  let resolveP;
  const p = new Promise((r) => { resolveP = r; });
  onMsg({ type: "h2w_unbind", convKey: CONV, workspace_id: "wH" }, {}, (r) => resolveP(r));
  const r = await p;
  ok(r?.ok === true && !storage.herdrWakeBindings[SK_WH], "unbind removes one workspace binding", JSON.stringify(r));
  ok(!!storage.herdrWakeBindings[`${CONV}::w2Y`], "other workspace binding remains");
}

// ---- Scenario 5b: unbind remaining ----
{
  let resolveP;
  const p = new Promise((r) => { resolveP = r; });
  onMsg({ type: "h2w_unbind", convKey: CONV, workspace_id: "w2Y" }, {}, (r) => resolveP(r));
  const r = await p;
  ok(r?.ok === true && !Object.keys(storage.herdrWakeBindings).length, "unbind last workspace clears storage", JSON.stringify(r));
}

// ---- Scenario 6: state for popup rendering ----
{
  installContentScript(303, "https://chatgpt.com/c/xyz", "https://chatgpt.com/c/xyz");
  let resolveP;
  const p = new Promise((r) => { resolveP = r; });
  onMsg({ type: "h2w_state", tabId: 303 }, { tab: { id: 303 } }, (r) => resolveP(r));
  const r = await p;
  ok(!!r.convInfo && r.convInfo.convKey === "https://chatgpt.com/c/xyz" && Array.isArray(r.bindings) && Array.isArray(r.sessionBindings), "h2w_state returns convInfo, bindings, sessionBindings", JSON.stringify(r).slice(0, 120));
}

// ---- Scenario 6b: stale workspace label is reconciled from live catalog ----
{
  const staleKey = `${CONV}::w68`;
  storage.herdrWakeBindings[staleKey] = {
    convKey: CONV,
    workspace_id: "w68",
    workspace_label: "integrate-dsh-enterprise-agent-20260823 (w68)",
    persistence: "explicit",
    created_at: Date.now(),
    last_seen_at: Date.now(),
  };
  mockStateWorkspaces = [{
    id: "w68",
    label: "herdr-mcp",
    roots: ["/Users/qingxian/Documents/herdr-mcp"],
  }];
  let resolveP;
  const p = new Promise((r) => { resolveP = r; });
  onMsg({ type: "h2w_page_hud", convKey: CONV }, {}, (r) => resolveP(r));
  const r = await p;
  ok(r?.workspace_label?.includes("herdr-mcp"), "HUD prefers live workspace label over stale binding label", JSON.stringify(r));
  ok(storage.herdrWakeBindings[staleKey]?.workspace_label?.includes("herdr-mcp"), "stale binding label is repaired");
  mockStateWorkspaces = [];
}

// ---- Scenario 6c: JSON-bridge conversations get isolated automation and bound-only MCP access ----
console.log("\n[json bridge automation]");
{
  installContentScript(350, ZAI_CONV, ZAI_CONV, "z.ai");

  let resolveBootstrapCatalog;
  const bootstrapCatalogP = new Promise((r) => { resolveBootstrapCatalog = r; });
  onMsg({ type: "h2w_json_bridge_catalog", site: "z.ai", convKey: ZAI_CONV }, { tab: { id: 350, url: ZAI_CONV } }, (r) => resolveBootstrapCatalog(r));
  const bootstrapCatalog = await bootstrapCatalogP;
  ok(bootstrapCatalog?.ok === true && bootstrapCatalog.tools?.[0]?.name === "herdr_inspect",
    "unbound z.ai conversation can bootstrap the manual Herdr JSON bridge", JSON.stringify(bootstrapCatalog));

  let resolveUnboundAuto;
  const unboundAutoP = new Promise((r) => { resolveUnboundAuto = r; });
  onMsg({
    type: "h2w_set_project_automation",
    project_id: null,
    site: "z.ai",
    convKey: ZAI_CONV,
    enabled: true,
  }, { tab: { id: 350, url: ZAI_CONV } }, (r) => resolveUnboundAuto(r));
  const unboundAuto = await unboundAutoP;
  ok(unboundAuto?.ok === true
      && unboundAuto?.conversation_automation_enabled === true
      && storage.herdrConversationAutomation?.[ZAI_CONV] === true,
    "unbound z.ai conversation can save its automation preference", JSON.stringify(unboundAuto));

  let resolveUnboundAutoOff;
  const unboundAutoOffP = new Promise((r) => { resolveUnboundAutoOff = r; });
  onMsg({
    type: "h2w_set_project_automation",
    project_id: null,
    site: "z.ai",
    convKey: ZAI_CONV,
    enabled: false,
  }, { tab: { id: 350, url: ZAI_CONV } }, (r) => resolveUnboundAutoOff(r));
  const unboundAutoOff = await unboundAutoOffP;
  ok(unboundAutoOff?.ok === true
      && unboundAutoOff?.conversation_automation_enabled === false
      && storage.herdrConversationAutomation?.[ZAI_CONV] !== true,
    "unbound z.ai conversation can turn automation back off before binding", JSON.stringify(unboundAutoOff));

  let resolveBind;
  const bindP = new Promise((r) => { resolveBind = r; });
  onMsg({
    type: "h2w_bind",
    tabId: 350,
    workspace_id: "w68",
    workspace_label: "herdr-mcp (w68)",
  }, { tab: { id: 350, url: ZAI_CONV } }, (r) => resolveBind(r));
  const bound = await bindP;
  ok(bound?.ok === true, "z.ai conversation can bind before JSON tool access", JSON.stringify(bound));

  let resolveHud;
  const hudP = new Promise((r) => { resolveHud = r; });
  onMsg({ type: "h2w_page_hud", convKey: ZAI_CONV }, { tab: { id: 350, url: ZAI_CONV } }, (r) => resolveHud(r));
  const hud = await hudP;
  ok(hud?.conversation_automation_available === true
      && hud?.conversation_automation_enabled === false
      && hud?.manual_handoff_available === true
      && hud?.can_handoff === true,
    "persisted z.ai HUD exposes automation off plus manual handoff when bound", JSON.stringify(hud));

  let resolveAuto;
  const autoP = new Promise((r) => { resolveAuto = r; });
  onMsg({
    type: "h2w_set_project_automation",
    project_id: null,
    site: "z.ai",
    convKey: ZAI_CONV,
    enabled: true,
  }, { tab: { id: 350, url: ZAI_CONV } }, (r) => resolveAuto(r));
  const auto = await autoP;
  ok(auto?.ok === true && auto?.conversation_automation_enabled === true && storage.herdrConversationAutomation?.[ZAI_CONV] === true,
    "z.ai HUD can enable automation for only the current conversation", JSON.stringify(auto));

  let resolveManualMode;
  const manualModeP = new Promise((r) => { resolveManualMode = r; });
  onMsg({ type: "h2w_set_config", config: { automationMode: "manual" } }, {}, (r) => resolveManualMode(r));
  await manualModeP;
  let resolveManualHud;
  const manualHudP = new Promise((r) => { resolveManualHud = r; });
  onMsg({ type: "h2w_page_hud", convKey: ZAI_CONV }, { tab: { id: 350, url: ZAI_CONV } }, (r) => resolveManualHud(r));
  const manualHud = await manualHudP;
  ok(manualHud?.conversation_automation_available === true && manualHud?.enabled === true,
    "global Project-manual mode does not disable z.ai conversation automation", JSON.stringify(manualHud));
  ok(storage.herdrConversationAutomation?.[ZAI_CONV] === true,
    "z.ai conversation automation remains independently persisted");

  hangAutomationNotifications = true;
  let resolveNonblockingAuto;
  const nonblockingAutoP = new Promise((r) => { resolveNonblockingAuto = r; });
  onMsg({
    type: "h2w_set_project_automation",
    project_id: null,
    site: "z.ai",
    convKey: ZAI_CONV,
    enabled: false,
  }, { tab: { id: 350, url: ZAI_CONV } }, (r) => resolveNonblockingAuto(r));
  const nonblockingAuto = await Promise.race([
    nonblockingAutoP,
    new Promise((resolve) => setTimeout(() => resolve({ timeout: true }), 100)),
  ]);
  ok(nonblockingAuto?.ok === true
      && nonblockingAuto?.conversation_automation_enabled === false
      && storage.herdrConversationAutomation?.[ZAI_CONV] !== true,
    "conversation Auto responds after persistence without waiting for stale-tab broadcasts", JSON.stringify(nonblockingAuto));
  hangAutomationNotifications = false;

  let resolveRestoreConversationAuto;
  const restoreConversationAutoP = new Promise((r) => { resolveRestoreConversationAuto = r; });
  onMsg({
    type: "h2w_set_project_automation",
    project_id: null,
    site: "z.ai",
    convKey: ZAI_CONV,
    enabled: true,
  }, { tab: { id: 350, url: ZAI_CONV } }, (r) => resolveRestoreConversationAuto(r));
  await restoreConversationAutoP;

  let resolveProjectMode;
  const projectModeP = new Promise((r) => { resolveProjectMode = r; });
  onMsg({ type: "h2w_set_config", config: { automationMode: "project_auto" } }, {}, (r) => resolveProjectMode(r));
  await projectModeP;
  let resolveRestoredHud;
  const restoredHudP = new Promise((r) => { resolveRestoredHud = r; });
  onMsg({ type: "h2w_page_hud", convKey: ZAI_CONV }, { tab: { id: 350, url: ZAI_CONV } }, (r) => resolveRestoredHud(r));
  const restoredHud = await restoredHudP;
  ok(restoredHud?.conversation_automation_available === true && restoredHud?.enabled === true,
    "enabling Project automation leaves z.ai conversation automation unchanged", JSON.stringify(restoredHud));

  let resolveCatalog;
  const catalogP = new Promise((r) => { resolveCatalog = r; });
  onMsg({ type: "h2w_json_bridge_catalog", site: "z.ai", convKey: ZAI_CONV }, { tab: { id: 350, url: ZAI_CONV } }, (r) => resolveCatalog(r));
  const catalog = await catalogP;
  ok(catalog?.ok === true && catalog.tools?.[0]?.name === "herdr_inspect",
    "bound z.ai conversation keeps access to the Herdr tool catalog", JSON.stringify(catalog));

  let resolveMismatch;
  const mismatchP = new Promise((r) => { resolveMismatch = r; });
  onMsg({ type: "h2w_json_bridge_catalog", site: "deepseek", convKey: ZAI_CONV }, { tab: { id: 350, url: ZAI_CONV } }, (r) => resolveMismatch(r));
  const mismatch = await mismatchP;
  ok(mismatch?.ok === false && mismatch?.error === "json-bridge-site-mismatch",
    "JSON bridge rejects a mismatched site identity", JSON.stringify(mismatch));
}

// ---- Scenario 6cc: plain ChatGPT conversations get isolated conversation-scoped automation ----
console.log("\n[plain ChatGPT conversation automation]");
{
  installContentScript(355, CHATGPT_AUTO_CONV, CHATGPT_AUTO_CONV, "chatgpt");

  let resolveHudOff;
  const hudOffP = new Promise((r) => { resolveHudOff = r; });
  onMsg({ type: "h2w_page_hud", convKey: CHATGPT_AUTO_CONV }, { tab: { id: 355, url: CHATGPT_AUTO_CONV } }, (r) => resolveHudOff(r));
  const hudOff = await hudOffP;
  ok(hudOff?.conversation_automation_available === true
      && hudOff?.conversation_automation_enabled === false
      && hudOff?.project_automation_available === false
      && !hudOff?.project_id,
    "plain ChatGPT /c/<id> exposes conversation-scoped automation instead of requiring a Project", JSON.stringify(hudOff));

  let resolveAuto;
  const autoP = new Promise((r) => { resolveAuto = r; });
  onMsg({
    type: "h2w_set_project_automation",
    project_id: null,
    site: "chatgpt",
    convKey: CHATGPT_AUTO_CONV,
    enabled: true,
  }, { tab: { id: 355, url: CHATGPT_AUTO_CONV } }, (r) => resolveAuto(r));
  const auto = await autoP;
  ok(auto?.ok === true
      && auto?.conversation_automation_enabled === true
      && storage.herdrConversationAutomation?.[CHATGPT_AUTO_CONV] === true,
    "plain ChatGPT conversation can save Auto on without joining a Project", JSON.stringify(auto));

  mockLocalRuntimeAvailable = false;
  let resolveOfflineState;
  const offlineStateP = new Promise((r) => { resolveOfflineState = r; });
  onMsg({ type: "h2w_automation_state", convKey: CHATGPT_AUTO_CONV },
    { tab: { id: 355, url: CHATGPT_AUTO_CONV } }, (r) => resolveOfflineState(r));
  const offlineState = await offlineStateP;
  ok(offlineState?.preference_enabled === true
      && offlineState?.enabled === false
      && offlineState?.effective_enabled === false
      && offlineState?.runtime_available === false,
    "Auto preference stays saved but effective automation fails closed when 127.0.0.1:8772 is unavailable",
    JSON.stringify(offlineState));

  let resolveOfflineHud;
  const offlineHudP = new Promise((r) => { resolveOfflineHud = r; });
  onMsg({ type: "h2w_page_hud", convKey: CHATGPT_AUTO_CONV },
    { tab: { id: 355, url: CHATGPT_AUTO_CONV } }, (r) => resolveOfflineHud(r));
  const offlineHud = await offlineHudP;
  ok(offlineHud?.enabled === true
      && offlineHud?.effective_enabled === false
      && offlineHud?.runtime_available === false,
    "HUD distinguishes saved Auto-on from an offline local runtime", JSON.stringify(offlineHud));

  mockLocalRuntimeAvailable = true;
  let resolveOnlineState;
  const onlineStateP = new Promise((r) => { resolveOnlineState = r; });
  onMsg({ type: "h2w_automation_state", convKey: CHATGPT_AUTO_CONV },
    { tab: { id: 355, url: CHATGPT_AUTO_CONV } }, (r) => resolveOnlineState(r));
  const onlineState = await onlineStateP;
  ok(onlineState?.enabled === true && onlineState?.runtime_available === true,
    "effective automation resumes after the local runtime is reachable again", JSON.stringify(onlineState));

  let resolveOtherHud;
  const otherHudP = new Promise((r) => { resolveOtherHud = r; });
  onMsg({ type: "h2w_page_hud", convKey: CONV }, { tab: { id: 202, url: CONV } }, (r) => resolveOtherHud(r));
  const otherHud = await otherHudP;
  ok(otherHud?.conversation_automation_available === true && otherHud?.conversation_automation_enabled === false,
    "plain ChatGPT automation preference does not leak into another conversation", JSON.stringify(otherHud));

  let resolveMismatch;
  const mismatchP = new Promise((r) => { resolveMismatch = r; });
  onMsg({
    type: "h2w_set_project_automation",
    project_id: null,
    site: "chatgpt",
    convKey: CHATGPT_AUTO_CONV,
    enabled: false,
  }, { tab: { id: 355, url: "https://chatgpt.com/c/different-chat" } }, (r) => resolveMismatch(r));
  const mismatch = await mismatchP;
  ok(mismatch?.ok === false && mismatch?.error === "conversation-automation-sender-mismatch",
    "plain ChatGPT automation rejects a sender from a different conversation", JSON.stringify(mismatch));
}

// ---- Scenario 6d: z.ai new-chat root state follows the first persisted /c/<chat_id> only ----
console.log("\n[z.ai root migration]");
{
  installContentScript(360, `${ZAI_ROOT}/`, ZAI_ROOT, "z.ai");
  let resolveBind;
  const bindP = new Promise((r) => { resolveBind = r; });
  onMsg({
    type: "h2w_bind",
    tabId: 360,
    workspace_id: "wZ",
    workspace_label: "zai-work (wZ)",
  }, { tab: { id: 360, url: `${ZAI_ROOT}/` } }, (r) => resolveBind(r));
  const bound = await bindP;
  ok(bound?.ok === true, "z.ai root launcher can hold a temporary workspace binding", JSON.stringify(bound));

  let resolveAuto;
  const autoP = new Promise((r) => { resolveAuto = r; });
  onMsg({
    type: "h2w_set_project_automation",
    project_id: null,
    site: "z.ai",
    convKey: ZAI_ROOT,
    enabled: true,
  }, { tab: { id: 360, url: `${ZAI_ROOT}/` } }, (r) => resolveAuto(r));
  const auto = await autoP;
  ok(auto?.ok === true && storage.herdrConversationAutomation?.[ZAI_ROOT] === true,
    "z.ai root launcher can remember temporary automation until a chat id exists", JSON.stringify(auto));

  installContentScript(360, ZAI_NEW, ZAI_NEW, "z.ai");
  let resolveRegister;
  const registerP = new Promise((r) => { resolveRegister = r; });
  onMsg({ type: "h2w_register", convKey: ZAI_NEW, url: ZAI_NEW, site: "z.ai" }, { tab: { id: 360, url: ZAI_NEW } }, (r) => resolveRegister(r));
  const registered = await registerP;
  ok(registered?.bound === true, "first z.ai /c/<chat_id> route inherits the temporary root binding", JSON.stringify(registered));
  ok(!storage.herdrWakeBindings[`${ZAI_ROOT}::wZ`] && !!storage.herdrWakeBindings[`${ZAI_NEW}::wZ`],
    "z.ai root binding migrates exactly to the persisted chat key");
  ok(storage.herdrConversationAutomation?.[ZAI_NEW] === true && storage.herdrConversationAutomation?.[ZAI_ROOT] !== true,
    "z.ai automation preference migrates from root to the persisted chat key");

  installContentScript(360, ZAI_OTHER, ZAI_OTHER, "z.ai");
  let resolveHistory;
  const historyP = new Promise((r) => { resolveHistory = r; });
  onMsg({ type: "h2w_register", convKey: ZAI_OTHER, url: ZAI_OTHER, site: "z.ai" }, { tab: { id: 360, url: ZAI_OTHER } }, (r) => resolveHistory(r));
  const history = await historyP;
  ok(history?.bound === false && !!storage.herdrWakeBindings[`${ZAI_NEW}::wZ`] && !storage.herdrWakeBindings[`${ZAI_OTHER}::wZ`],
    "switching between existing z.ai chats never drags a workspace binding along", JSON.stringify(history));
}

// ---- Scenario 6e: ChatGPT can bind before a conversation exists ----
console.log("\n[ChatGPT pre-conversation Project binding]");
{
  installContentScript(365, PROJECT_HOME_URL, PROJECT_KEY);
  tabs.get(365).active = true;
  let resolveProjectBind;
  const projectBindP = new Promise((r) => { resolveProjectBind = r; });
  onMsg({
    type: "h2w_bind",
    tabId: 365,
    workspace_id: "wP",
    workspace_label: "project-home (wP)",
  }, { tab: { id: 365, url: PROJECT_HOME_URL } }, (r) => resolveProjectBind(r));
  const projectBound = await projectBindP;
  const projectStoreKey = `${PROJECT_KEY}::wP`;
  ok(projectBound?.ok === true && projectBound?.binding_scope === "project"
      && !!storage.herdrWakeBindings[projectStoreKey],
    "Project home binds the workspace directly to the stable project key", JSON.stringify(projectBound));
  ok(storage.herdrWakeBindings[projectStoreKey]?.active_conv_key == null
      && storage.herdrWakeBindings[projectStoreKey]?.tabId == null,
    "Project-home binding has no delivery target before a conversation exists");

  let resolveProjectHud;
  const projectHudP = new Promise((r) => { resolveProjectHud = r; });
  onMsg({ type: "h2w_page_hud", convKey: PROJECT_KEY }, { tab: { id: 365, url: PROJECT_HOME_URL } }, (r) => resolveProjectHud(r));
  const projectHud = await projectHudP;
  ok(projectHud?.bound === true && projectHud?.project_id === PROJECT_ID && projectHud?.manual_handoff_available === false,
    "Project home HUD sees the persistent binding without pretending a conversation exists", JSON.stringify(projectHud));

  installContentScript(365, PROJECT_SOURCE_URL, PROJECT_SOURCE);
  tabs.get(365).active = true;
  let resolveProjectRegister;
  const projectRegisterP = new Promise((r) => { resolveProjectRegister = r; });
  onMsg({ type: "h2w_register", convKey: PROJECT_SOURCE, url: PROJECT_SOURCE_URL, site: "chatgpt" }, { tab: { id: 365, url: PROJECT_SOURCE_URL } }, (r) => resolveProjectRegister(r));
  const projectRegistered = await projectRegisterP;
  ok(projectRegistered?.bound === true
      && storage.herdrWakeBindings[projectStoreKey]?.active_conv_key === PROJECT_SOURCE
      && storage.herdrWakeBindings[projectStoreKey]?.tabId === 365,
    "first Project conversation automatically becomes the delivery target", JSON.stringify(projectRegistered));

  installContentScript(366, PROJECT_TARGET_URL, PROJECT_TARGET);
  tabs.get(366).active = false;
  let resolveBackgroundRegister;
  const backgroundRegisterP = new Promise((r) => { resolveBackgroundRegister = r; });
  onMsg({ type: "h2w_register", convKey: PROJECT_TARGET, url: PROJECT_TARGET_URL, site: "chatgpt" }, { tab: { id: 366, url: PROJECT_TARGET_URL } }, (r) => resolveBackgroundRegister(r));
  await backgroundRegisterP;
  ok(storage.herdrWakeBindings[projectStoreKey]?.active_conv_key === PROJECT_SOURCE,
    "an inactive Project conversation does not steal the delivery target");
  tabs.get(365).active = false;
  tabs.get(366).active = true;
  for (const fn of listeners.onActivated) fn({ tabId: 366, windowId: 1 });
  await new Promise((r) => setTimeout(r, 20));
  ok(storage.herdrWakeBindings[projectStoreKey]?.active_conv_key === PROJECT_TARGET
      && storage.herdrWakeBindings[projectStoreKey]?.tabId === 366,
    "activating another Project conversation switches only the delivery target");

  let resolveProjectUnbind;
  const projectUnbindP = new Promise((r) => { resolveProjectUnbind = r; });
  onMsg({ type: "h2w_unbind", convKey: PROJECT_TARGET, workspace_id: "wP" }, { tab: { id: 366, url: PROJECT_TARGET_URL } }, (r) => resolveProjectUnbind(r));
  await projectUnbindP;
  ok(!storage.herdrWakeBindings[projectStoreKey], "Project binding can be removed from any conversation in that Project");

  const rootKey = "https://chatgpt.com";
  installContentScript(367, `${rootKey}/`, rootKey);
  tabs.get(367).active = true;
  let resolveRootBind;
  const rootBindP = new Promise((r) => { resolveRootBind = r; });
  onMsg({ type: "h2w_bind", tabId: 367, workspace_id: "wR", workspace_label: "root-pending (wR)" }, { tab: { id: 367, url: `${rootKey}/` } }, (r) => resolveRootBind(r));
  const rootBound = await rootBindP;
  ok(rootBound?.ok === true && rootBound?.binding_scope === "pending"
      && storage.herdrWakeBindings[`${rootKey}::wR`]?.tabId === 367,
    "ChatGPT root can hold a tab-scoped pending binding", JSON.stringify(rootBound));

  installContentScript(367, PROJECT_HOME_URL, PROJECT_KEY);
  tabs.get(367).active = true;
  let resolveRootMigration;
  const rootMigrationP = new Promise((r) => { resolveRootMigration = r; });
  onMsg({ type: "h2w_register", convKey: PROJECT_KEY, url: PROJECT_HOME_URL, site: "chatgpt" }, { tab: { id: 367, url: PROJECT_HOME_URL } }, (r) => resolveRootMigration(r));
  const rootMigrated = await rootMigrationP;
  ok(rootMigrated?.bound === true
      && !storage.herdrWakeBindings[`${rootKey}::wR`]
      && storage.herdrWakeBindings[`${PROJECT_KEY}::wR`]?.binding_scope === "project",
    "root pending binding migrates once into the Project when that tab enters Project scope", JSON.stringify(rootMigrated));

  let resolveRootCleanup;
  const rootCleanupP = new Promise((r) => { resolveRootCleanup = r; });
  onMsg({ type: "h2w_unbind", convKey: PROJECT_KEY, workspace_id: "wR" }, { tab: { id: 367, url: PROJECT_HOME_URL } }, (r) => resolveRootCleanup(r));
  await rootCleanupP;
}

// ---- Scenario 6f: z.ai manual handoff uses a raw summary/seed and cuts over only after confirmation ----
console.log("\n[z.ai manual handoff]");
{
  tabs.set(370, {
    url: ZAI_SOURCE,
    listener: (msg, _sender, sendResponse) => {
      if (msg?.type === "h2w_get_convkey") {
        sendResponse({ convKey: ZAI_SOURCE, url: ZAI_SOURCE, site: "z.ai" });
        return;
      }
      if (msg?.type === "h2w_handoff_prompt") {
        const match = String(msg.template || "").match(/<<<HERDR_HANDOFF_V1 id=([^>]+)>>>/);
        const id = match?.[1] || "missing";
        handoffPrompt = msg.template || "";
        sendResponse({
          ok: true,
          assistantText: [
            `<<<HERDR_HANDOFF_V1 id=${id}>>>`,
            "# Project handoff",
            "Current objective: continue the z.ai task in a fresh chat.",
            "Next: verify live state before mutation.",
            "<<<END_HERDR_HANDOFF_V1>>>",
          ].join("\n"),
        });
        return;
      }
      if (msg?.type === "h2w_snapshot_turn") {
        sendResponse({ assistantText: "", turnInProgress: false, generating: false });
        return;
      }
      sendResponse({ ok: true });
    },
  });
  let resolveBind;
  const bindP = new Promise((r) => { resolveBind = r; });
  onMsg({
    type: "h2w_bind",
    tabId: 370,
    workspace_id: "wY",
    workspace_label: "zai-handoff (wY)",
  }, { tab: { id: 370, url: ZAI_SOURCE } }, (r) => resolveBind(r));
  const bound = await bindP;
  ok(bound?.ok === true, "persisted z.ai source binds before manual handoff", JSON.stringify(bound));
  const sourceKey = `${ZAI_SOURCE}::wY`;
  const continuityId = storage.herdrWakeBindings[sourceKey]?.continuity_id;

  zaiTargetSeeded = false;
  let resolveStart;
  const startP = new Promise((r) => { resolveStart = r; });
  onMsg({ type: "h2w_handoff_start", tabId: 370 }, { tab: { id: 370, url: ZAI_SOURCE } }, (r) => resolveStart(r));
  const started = await startP;
  ok(started?.ok === true && started?.pending === true,
    "z.ai manual handoff accepts the immediate marked summary", JSON.stringify(started));
  ok(handoffPrompt.includes("z.ai") && handoffPrompt.includes("HERDR_HANDOFF_V1"),
    "z.ai handoff uses the z.ai-specific raw summary template", JSON.stringify({ handoffPrompt }));

  const targetKey = `${ZAI_TARGET}::wY`;
  await waitForTest(() => !storage.herdrWakeBindings[sourceKey] && !!storage.herdrWakeBindings[targetKey]);
  ok(!storage.herdrWakeBindings[sourceKey] && !!storage.herdrWakeBindings[targetKey],
    "z.ai binding moves only after the fresh target confirms its seed");
  ok(storage.herdrWakeBindings[targetKey]?.continuity_id === continuityId,
    "z.ai manual handoff preserves continuity id across chat ids");
  ok(storage.herdrWakeBindings[targetKey]?.handoff_from === ZAI_SOURCE,
    "z.ai target binding records the predecessor chat");
  ok(storage.herdrConversationAutomation?.[ZAI_TARGET] !== true,
    "z.ai manual handoff preserves Auto off in the target chat");

  // Repeat from the same source with conversation Auto explicitly on. Manual
  // handoff must stay available and the fresh target must inherit Auto on.
  delete storage.herdrWakeBindings[targetKey];
  zaiTargetSeeded = false;
  let resolveRebind;
  const rebindP = new Promise((r) => { resolveRebind = r; });
  onMsg({
    type: "h2w_bind",
    tabId: 370,
    workspace_id: "wY",
    workspace_label: "zai-handoff (wY)",
  }, { tab: { id: 370, url: ZAI_SOURCE } }, (r) => resolveRebind(r));
  await rebindP;
  let resolveAutoOn;
  const autoOnP = new Promise((r) => { resolveAutoOn = r; });
  onMsg({
    type: "h2w_set_project_automation",
    convKey: ZAI_SOURCE,
    site: "z.ai",
    enabled: true,
  }, { tab: { id: 370, url: ZAI_SOURCE } }, (r) => resolveAutoOn(r));
  const autoOn = await autoOnP;
  ok(autoOn?.ok === true && autoOn?.enabled === true,
    "z.ai source can enable conversation Auto before manual handoff", JSON.stringify(autoOn));

  let resolveAutoHandoff;
  const autoHandoffP = new Promise((r) => { resolveAutoHandoff = r; });
  onMsg({ type: "h2w_handoff_start", tabId: 370, trigger: "manual" }, { tab: { id: 370, url: ZAI_SOURCE } }, (r) => resolveAutoHandoff(r));
  const autoHandoff = await autoHandoffP;
  ok(autoHandoff?.ok === true,
    "z.ai manual handoff remains available while Auto is on", JSON.stringify(autoHandoff));
  await waitForTest(() => !!storage.herdrWakeBindings[targetKey]
    && storage.herdrConversationAutomation?.[ZAI_TARGET] === true);
  ok(!!storage.herdrWakeBindings[targetKey], "z.ai Auto-on handoff still moves the workspace binding");
  ok(storage.herdrConversationAutomation?.[ZAI_TARGET] === true,
    "z.ai manual handoff preserves Auto on in the target chat");
}

// ---- Scenario 7: Project handoff keeps source authoritative until target seed is confirmed ----
console.log("\n[project handoff]");
{
  targetComposerReady = true;
  targetComposerReadyAfter = 0;
  targetProbeCount = 0;
  targetSeedCount = 0;
  const manualCreateBefore = tabCreateCount;
  const manualUpdateBefore = tabUpdateCount;
  installContentScript(401, PROJECT_SOURCE_URL, PROJECT_SOURCE);
  let resolveBind;
  const bindP = new Promise((r) => { resolveBind = r; });
  onMsg({
    type: "h2w_bind",
    tabId: 401,
    workspace_id: "wH",
    workspace_label: "herdr-mcp (wH)",
  }, { tab: { id: 401 } }, (r) => resolveBind(r));
  const bound = await bindP;
  ok(bound?.ok === true, "Project source binds before rollover", JSON.stringify(bound));
  const sourceKey = `${PROJECT_KEY}::wH`;
  ok(!!storage.herdrWakeBindings[sourceKey]
      && storage.herdrWakeBindings[sourceKey]?.binding_scope === "project"
      && storage.herdrWakeBindings[sourceKey]?.active_conv_key === PROJECT_SOURCE,
    "Project binding is stable while the source conversation is the active target");
  const continuityId = storage.herdrWakeBindings[sourceKey].continuity_id;

  let resolveHudOff;
  const hudOffP = new Promise((r) => { resolveHudOff = r; });
  onMsg({ type: "h2w_page_hud", convKey: PROJECT_SOURCE }, { tab: { id: 401 } }, (r) => resolveHudOff(r));
  const hudOff = await hudOffP;
  ok(hudOff?.automation_mode === "project_auto" && hudOff?.project_automation_available === true && hudOff?.enabled === false,
    "Project-auto global mode keeps a new Project explicitly off", JSON.stringify(hudOff));

  let resolveProjectOn;
  const projectOnP = new Promise((r) => { resolveProjectOn = r; });
  onMsg({ type: "h2w_set_project_automation", project_id: PROJECT_ID, convKey: PROJECT_SOURCE, enabled: true }, { tab: { id: 401 } }, (r) => resolveProjectOn(r));
  const projectOn = await projectOnP;
  ok(projectOn?.ok === true && storage.herdrProjectAutomation?.[PROJECT_ID] === true,
    "Project HUD can explicitly enable automation for one Project", JSON.stringify(projectOn));

  let resolveManualMode;
  const manualModeP = new Promise((r) => { resolveManualMode = r; });
  onMsg({ type: "h2w_set_config", config: { automationMode: "manual" } }, {}, (r) => resolveManualMode(r));
  await manualModeP;
  let resolveHudManual;
  const hudManualP = new Promise((r) => { resolveHudManual = r; });
  onMsg({ type: "h2w_page_hud", convKey: PROJECT_SOURCE }, { tab: { id: 401 } }, (r) => resolveHudManual(r));
  const hudManual = await hudManualP;
  ok(hudManual?.project_automation_available === false && hudManual?.enabled === false,
    "global manual mode hides/disables Project automation without deleting the Project preference", JSON.stringify(hudManual));

  let resolveProjectMode;
  const projectModeP = new Promise((r) => { resolveProjectMode = r; });
  onMsg({ type: "h2w_set_config", config: { automationMode: "project_auto" } }, {}, (r) => resolveProjectMode(r));
  await projectModeP;
  let resolveHudRestored;
  const hudRestoredP = new Promise((r) => { resolveHudRestored = r; });
  onMsg({ type: "h2w_page_hud", convKey: PROJECT_SOURCE }, { tab: { id: 401 } }, (r) => resolveHudRestored(r));
  const hudRestored = await hudRestoredP;
  ok(hudRestored?.project_automation_available === true && hudRestored?.enabled === true,
    "returning to Project-auto restores the saved Project preference", JSON.stringify(hudRestored));

  let resolveLegacyAllow;
  const legacyAllowP = new Promise((r) => { resolveLegacyAllow = r; });
  onMsg({ type: "h2w_set_config", config: { autoAllow: false } }, {}, (r) => resolveLegacyAllow(r));
  await legacyAllowP;
  let resolveHudAllow;
  const hudAllowP = new Promise((r) => { resolveHudAllow = r; });
  onMsg({ type: "h2w_page_hud", convKey: PROJECT_SOURCE }, { tab: { id: 401 } }, (r) => resolveHudAllow(r));
  const hudAllow = await hudAllowP;
  ok(hudAllow?.enabled === true && hudAllow?.autoAllow === true,
    "legacy autoAllow=false cannot disable permission handling inside Project automation", JSON.stringify(hudAllow));

  let resolveProjectOff;
  const projectOffP = new Promise((r) => { resolveProjectOff = r; });
  onMsg({ type: "h2w_set_project_automation", project_id: PROJECT_ID, convKey: PROJECT_SOURCE, enabled: false }, { tab: { id: 401 } }, (r) => resolveProjectOff(r));
  const projectOff = await projectOffP;
  ok(projectOff?.ok === true && projectOff?.enabled === false,
    "Project automation can be turned off before an explicit manual handoff", JSON.stringify(projectOff));

  let resolveStart;
  const startP = new Promise((r) => { resolveStart = r; });
  onMsg({ type: "h2w_handoff_start", tabId: 401 }, { tab: { id: 401 } }, (r) => resolveStart(r));
  const started = await startP;
  ok(started?.ok === true && started.pending === true, "rollover requests a handoff summary", JSON.stringify(started));
  const transferId = Object.values(storage.herdrConversationTransfers || {})
    .filter((transfer) => transfer?.source_conv_key === PROJECT_SOURCE)
    .sort((a, b) => Number(b.created_at || 0) - Number(a.created_at || 0))[0]?.id;
  ok(!!transferId && handoffPrompt.includes(`id=${transferId}`), "source prompt carries the persisted transfer id");

  const assistantText = [
    `<<<HERDR_HANDOFF_V1 id=${transferId}>>>`,
    "# Project handoff",
    "Current objective: verify browser continuity.",
    "Next: verify live state before mutation.",
    "<<<END_HERDR_HANDOFF_V1>>>",
  ].join("\n");
  let resolveEnded;
  const endedP = new Promise((r) => { resolveEnded = r; });
  onMsg({ type: "h2w_turn_ended", convKey: PROJECT_SOURCE, assistantText, userText: "roll over" }, { tab: { id: 401 } }, (r) => resolveEnded(r));
  const ended = await endedP;
  ok(ended?.handled === true && ended?.ok === true, "marked assistant packet is accepted", JSON.stringify(ended));

  await waitForTest(() => storage.herdrConversationTransfers?.[transferId]?.status === "seed_uncertain");
  const uncertain = storage.herdrConversationTransfers[transferId];
  ok(uncertain?.status === "seed_uncertain", "unconfirmed target seed is recorded as delivery-uncertain", JSON.stringify(uncertain));
  ok(uncertain?.target_tab_id === 401,
    "manual ChatGPT Project handoff reuses the source tab as the target", JSON.stringify(uncertain));
  ok(tabCreateCount === manualCreateBefore,
    "manual ChatGPT Project handoff does not create a new tab", `creates=${tabCreateCount - manualCreateBefore}`);
  ok(tabUpdateCount === manualUpdateBefore + 1
      && lastTabUpdate?.tabId === 401
      && lastTabUpdate?.url === PROJECT_KEY,
    "manual ChatGPT Project handoff navigates the current tab to the stable Project entry",
    JSON.stringify(lastTabUpdate));
  ok(targetProbeCount >= 1, "current-tab handoff waits for the Project composer before seeding");
  ok(!!storage.herdrWakeBindings[sourceKey]
      && storage.herdrWakeBindings[sourceKey]?.active_conv_key === PROJECT_SOURCE,
    "Project binding stays on the source target while target delivery is uncertain");
  ok(!storage.herdrWakeBindings[`${PROJECT_TARGET}::wH`],
    "Project handoff never creates a conversation-scoped target binding");

  handoffSeedMode = "confirmed";
  let resolveResume;
  const resumeP = new Promise((r) => { resolveResume = r; });
  onMsg({ type: "h2w_handoff_start", tabId: 401 }, { tab: { id: 401 } }, (r) => resolveResume(r));
  const resumed = await resumeP;
  ok(resumed?.ok === true, "explicit resume completes the target seed and cutover", JSON.stringify(resumed));
  ok(tabCreateCount === manualCreateBefore && tabUpdateCount === manualUpdateBefore + 1,
    "uncertain resume reuses the already-navigated current tab");
  ok(tabs.has(401), "committing a same-tab handoff does not retire the current tab");
  const targetKey = sourceKey;
  ok(!!storage.herdrWakeBindings[targetKey]
      && storage.herdrWakeBindings[targetKey]?.active_conv_key === PROJECT_TARGET,
    "committed rollover keeps the Project binding and switches only its active conversation target");
  ok(storage.herdrWakeBindings[targetKey]?.continuity_id === continuityId,
    "continuity id survives the Project target switch");
  ok(storage.herdrWakeBindings[targetKey]?.handoff_from === PROJECT_SOURCE,
    "Project binding records its predecessor conversation");
  let resolveTargetHud;
  const targetHudP = new Promise((r) => { resolveTargetHud = r; });
  onMsg({ type: "h2w_page_hud", convKey: PROJECT_TARGET }, { tab: { id: storage.herdrWakeBindings[targetKey]?.tabId } }, (r) => resolveTargetHud(r));
  const targetHud = await targetHudP;
  ok(targetHud?.enabled === false && targetHud?.project_id === PROJECT_ID,
    "rolled-over conversation preserves the Project automation-off setting used for manual handoff", JSON.stringify(targetHud));
  ok(storage.herdrConversationTransfers[transferId]?.status === "committed", "transfer metadata records committed state");
  ok(storage.herdrConversationTransfers[transferId]?.handoff_text == null, "committed transfer clears the temporary handoff packet");

  // Repeat with Project Auto on. Manual handoff must remain available and the
  // target must preserve the source Auto-on snapshot.
  delete storage.herdrWakeBindings[targetKey];
  targetSeeded = false;
  handoffSeedMode = "confirmed";
  handoffPrompt = "";
  targetProbeCount = 0;
  targetSeedCount = 0;
  installContentScript(401, PROJECT_SOURCE_URL, PROJECT_SOURCE);
  tabs.get(401).active = true;
  let resolveRebind;
  const rebindP = new Promise((r) => { resolveRebind = r; });
  onMsg({ type: "h2w_bind", tabId: 401, workspace_id: "wH", workspace_label: "herdr-mcp (wH)" }, { tab: { id: 401 } }, (r) => resolveRebind(r));
  await rebindP;

  let resolveAutoOn;
  const autoOnP = new Promise((r) => { resolveAutoOn = r; });
  onMsg({ type: "h2w_set_project_automation", project_id: PROJECT_ID, convKey: PROJECT_SOURCE, enabled: true }, { tab: { id: 401 } }, (r) => resolveAutoOn(r));
  const autoOn = await autoOnP;
  ok(autoOn?.ok === true && autoOn?.enabled === true,
    "Project source can enable Auto before manual handoff", JSON.stringify(autoOn));

  const transferCountBeforeOfflineAuto = Object.keys(storage.herdrConversationTransfers || {}).length;
  mockLocalRuntimeAvailable = false;
  let resolveOfflineAutoStart;
  const offlineAutoStartP = new Promise((r) => { resolveOfflineAutoStart = r; });
  onMsg({ type: "h2w_handoff_start", tabId: 401, trigger: "context_pressure" },
    { tab: { id: 401, url: PROJECT_SOURCE_URL } }, (r) => resolveOfflineAutoStart(r));
  const offlineAutoStart = await offlineAutoStartP;
  ok(offlineAutoStart?.ok === false
      && offlineAutoStart?.error === "local_runtime_unavailable"
      && Object.keys(storage.herdrConversationTransfers || {}).length === transferCountBeforeOfflineAuto,
    "automatic Project handoff does not begin while the local 8772 runtime is unavailable",
    JSON.stringify(offlineAutoStart));
  mockLocalRuntimeAvailable = true;

  const autoCreateBefore = tabCreateCount;
  const autoUpdateBefore = tabUpdateCount;
  let resolveAutoStart;
  const autoStartP = new Promise((r) => { resolveAutoStart = r; });
  onMsg({ type: "h2w_handoff_start", tabId: 401, trigger: "manual" }, { tab: { id: 401 } }, (r) => resolveAutoStart(r));
  const autoStart = await autoStartP;
  ok(autoStart?.ok === true && autoStart?.pending === true,
    "manual Project handoff remains available while Auto is on", JSON.stringify(autoStart));
  const autoTransfer = Object.values(storage.herdrConversationTransfers || {})
    .filter((transfer) => transfer?.source_conv_key === PROJECT_SOURCE)
    .sort((a, b) => Number(b.created_at || 0) - Number(a.created_at || 0))[0];
  ok(autoTransfer?.source_automation_scope === "project" && autoTransfer?.source_automation_enabled === true,
    "Project handoff snapshots Auto on before cutover", JSON.stringify(autoTransfer));
  const autoAssistantText = [
    `<<<HERDR_HANDOFF_V1 id=${autoTransfer?.id}>>>`,
    "# Project handoff",
    "Current objective: verify Auto-on inheritance.",
    "Next: verify the target keeps automation enabled.",
    "<<<END_HERDR_HANDOFF_V1>>>",
  ].join("\n");
  let resolveAutoEnded;
  const autoEndedP = new Promise((r) => { resolveAutoEnded = r; });
  onMsg({ type: "h2w_turn_ended", convKey: PROJECT_SOURCE, assistantText: autoAssistantText, userText: "roll over" }, { tab: { id: 401 } }, (r) => resolveAutoEnded(r));
  await autoEndedP;
  await waitForTest(() => storage.herdrConversationTransfers?.[autoTransfer?.id]?.status === "committed");
  ok(storage.herdrConversationTransfers[autoTransfer?.id]?.status === "committed",
    "Auto-on Project handoff commits normally");
  ok(tabCreateCount === autoCreateBefore && tabUpdateCount === autoUpdateBefore + 1,
    "Auto-on manual Project handoff still reuses the current tab");
  ok(storage.herdrProjectAutomation?.[PROJECT_ID] === true,
    "Project handoff preserves Auto on in shared Project state");
  ok(storage.herdrWakeBindings[targetKey]?.active_conv_key === PROJECT_TARGET,
    "Auto-on Project handoff switches only the active conversation target");
  let resolveAutoTargetHud;
  const autoTargetHudP = new Promise((r) => { resolveAutoTargetHud = r; });
  onMsg({ type: "h2w_page_hud", convKey: PROJECT_TARGET }, { tab: { id: storage.herdrWakeBindings[targetKey]?.tabId } }, (r) => resolveAutoTargetHud(r));
  const autoTargetHud = await autoTargetHudP;
  ok(autoTargetHud?.enabled === true && autoTargetHud?.project_id === PROJECT_ID,
    "rolled-over Project conversation inherits Auto on", JSON.stringify(autoTargetHud));

  // Restore the shared Project preference for later independent scenarios.
  let resolveAutoOff;
  const autoOffP = new Promise((r) => { resolveAutoOff = r; });
  onMsg({ type: "h2w_set_project_automation", project_id: PROJECT_ID, convKey: PROJECT_TARGET, enabled: false }, { tab: { id: storage.herdrWakeBindings[targetKey]?.tabId } }, (r) => resolveAutoOff(r));
  await autoOffP;
}

// ---- Scenario 7b: a false insert failure is reconciled before declaring seed failure ----
console.log("\n[project handoff insert false-negative]");
{
  delete storage.herdrWakeBindings[`${PROJECT_KEY}::wH`];
  delete storage.herdrWakeBindings[`${PROJECT_TARGET}::wH`];
  targetSeeded = false;
  handoffSeedMode = "insert_failed_but_committed";
  handoffPrompt = "";
  installContentScript(402, PROJECT_SOURCE_URL, PROJECT_SOURCE);

  let resolveBind;
  const bindP = new Promise((r) => { resolveBind = r; });
  onMsg({ type: "h2w_bind", tabId: 402, workspace_id: "wH", workspace_label: "herdr-mcp (wH)" }, { tab: { id: 402 } }, (r) => resolveBind(r));
  await bindP;
  const sourceKey = `${PROJECT_KEY}::wH`;
  const continuityId = storage.herdrWakeBindings[sourceKey]?.continuity_id;

  let resolveStart;
  const startP = new Promise((r) => { resolveStart = r; });
  onMsg({ type: "h2w_handoff_start", tabId: 402 }, { tab: { id: 402 } }, (r) => resolveStart(r));
  await startP;
  const transferId = Object.values(storage.herdrConversationTransfers || {})
    .filter((transfer) => transfer?.source_conv_key === PROJECT_SOURCE)
    .sort((a, b) => Number(b.created_at || 0) - Number(a.created_at || 0))[0]?.id;
  const assistantText = [
    `<<<HERDR_HANDOFF_V1 id=${transferId}>>>`,
    "# Project handoff",
    "Current objective: recover an editor false-negative.",
    "Next: verify target evidence before cutover.",
    "<<<END_HERDR_HANDOFF_V1>>>",
  ].join("\n");
  let resolveEnded;
  const endedP = new Promise((r) => { resolveEnded = r; });
  onMsg({ type: "h2w_turn_ended", convKey: PROJECT_SOURCE, assistantText, userText: "roll over" }, { tab: { id: 402 } }, (r) => resolveEnded(r));
  await endedP;
  await waitForTest(() => storage.herdrConversationTransfers?.[transferId]?.status === "committed");
  const targetKey = sourceKey;
  ok(storage.herdrConversationTransfers[transferId]?.status === "committed",
    "insert-failed with confirmed target evidence commits instead of failing");
  ok(!!storage.herdrWakeBindings[targetKey]
      && storage.herdrWakeBindings[targetKey]?.active_conv_key === PROJECT_TARGET,
    "false-negative reconciliation switches the Project binding target atomically");
  ok(storage.herdrWakeBindings[targetKey]?.continuity_id === continuityId,
    "false-negative reconciliation preserves the source continuity id");
}

// ---- Scenario 7c: recover a legacy failed seed after the user provisionally rebound the target ----
console.log("\n[project handoff legacy failed-seed recovery]");
{
  delete storage.herdrWakeBindings[`${PROJECT_KEY}::wH`];
  delete storage.herdrWakeBindings[`${PROJECT_TARGET}::wH`];
  targetSeeded = false;
  handoffSeedMode = "uncertain";
  handoffPrompt = "";
  installContentScript(403, PROJECT_SOURCE_URL, PROJECT_SOURCE);

  let resolveBind;
  const bindP = new Promise((r) => { resolveBind = r; });
  onMsg({ type: "h2w_bind", tabId: 403, workspace_id: "wH", workspace_label: "herdr-mcp (wH)" }, { tab: { id: 403 } }, (r) => resolveBind(r));
  await bindP;
  const projectSourceKey = `${PROJECT_KEY}::wH`;
  const sourceKey = `${PROJECT_SOURCE}::wH`;
  const seededProjectBinding = storage.herdrWakeBindings[projectSourceKey];
  delete storage.herdrWakeBindings[projectSourceKey];
  storage.herdrWakeBindings[sourceKey] = {
    ...seededProjectBinding,
    convKey: PROJECT_SOURCE,
    binding_scope: "conversation",
    active_conv_key: null,
    tabId: 403,
    tabUrl: PROJECT_SOURCE_URL,
  };
  const continuityId = storage.herdrWakeBindings[sourceKey]?.continuity_id;

  let resolveStart;
  const startP = new Promise((r) => { resolveStart = r; });
  onMsg({ type: "h2w_handoff_start", tabId: 403 }, { tab: { id: 403 } }, (r) => resolveStart(r));
  await startP;
  const transferId = Object.values(storage.herdrConversationTransfers || {})
    .filter((transfer) => transfer?.source_conv_key === PROJECT_SOURCE)
    .sort((a, b) => Number(b.created_at || 0) - Number(a.created_at || 0))[0]?.id;
  // This scenario exercises recovery of a transfer created by an older build,
  // where manual Project handoff opened a separate target tab.
  storage.herdrConversationTransfers[transferId].trigger = "legacy_manual";
  const assistantText = [
    `<<<HERDR_HANDOFF_V1 id=${transferId}>>>`,
    "# Project handoff",
    "Current objective: recover an already-materialized target.",
    "Next: adopt the provisional target binding.",
    "<<<END_HERDR_HANDOFF_V1>>>",
  ].join("\n");
  let resolveEnded;
  const endedP = new Promise((r) => { resolveEnded = r; });
  onMsg({ type: "h2w_turn_ended", convKey: PROJECT_SOURCE, assistantText, userText: "roll over" }, { tab: { id: 403 } }, (r) => resolveEnded(r));
  await endedP;
  await waitForTest(() => storage.herdrConversationTransfers?.[transferId]?.status === "seed_uncertain");

  const transfer = storage.herdrConversationTransfers[transferId];
  transfer.status = "failed";
  transfer.error = "insert-failed";
  const targetTabId = transfer.target_tab_id;
  targetSeeded = true;
  tabs.get(targetTabId).url = PROJECT_TARGET_URL;

  let resolveTargetBind;
  const targetBindP = new Promise((r) => { resolveTargetBind = r; });
  onMsg({ type: "h2w_bind", tabId: targetTabId, workspace_id: "wH", workspace_label: "herdr-mcp (wH)" }, { tab: { id: targetTabId } }, (r) => resolveTargetBind(r));
  await targetBindP;
  const targetKey = `${PROJECT_KEY}::wH`;
  ok(storage.herdrWakeBindings[targetKey]?.continuity_id !== continuityId,
    "provisional manual target binding initially has a separate continuity id");

  let resolveResume;
  const resumeP = new Promise((r) => { resolveResume = r; });
  onMsg({ type: "h2w_handoff_start", tabId: 403 }, { tab: { id: 403 } }, (r) => resolveResume(r));
  const resumed = await resumeP;
  ok(resumed?.ok === true, "legacy failed seed can resume without opening a third conversation", JSON.stringify(resumed));
  ok(storage.herdrConversationTransfers[transferId]?.status === "committed",
    "legacy failed seed is finalized as committed after target probe confirmation");
  ok(!storage.herdrWakeBindings[sourceKey] && !!storage.herdrWakeBindings[targetKey],
    "legacy recovery removes the old conversation binding and keeps the Project binding slot");
  ok(storage.herdrWakeBindings[targetKey]?.continuity_id === continuityId,
    "legacy recovery rewrites the provisional target onto the original continuity chain");
  ok(storage.herdrWakeBindings[targetKey]?.handoff_from === PROJECT_SOURCE,
    "legacy recovery records the predecessor conversation on the target binding");
  ok(storage.herdrWakeBindings[targetKey]?.active_conv_key === PROJECT_TARGET,
    "legacy recovery upgrades the target to a Project binding with the confirmed active conversation");
}

console.log(`\n=== ${failures === 0 ? "BACKGROUND BIND ALL PASS" : failures + " FAILURES"} ===`);
process.exit(failures === 0 ? 0 : 1);
