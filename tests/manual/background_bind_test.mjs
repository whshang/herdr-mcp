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
const PROJECT_SOURCE = `https://chatgpt.com/g/${PROJECT_ID}/c/source123`;
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

// ---- chrome mock ----
const storage = { herdrWakeBindings: {}, herdrMcpUrl: "http://127.0.0.1:8772", token: "test-token", enabled: true, wakeTemplate: "a {status}" };
const listeners = { onMessage: [], onStartup: [], onInstalled: [] };
const sentMessages = []; // Messages from background to content.
const tabs = new Map();   // tabId -> { url, listener }.
let nextTabId = 500;
let handoffSeedMode = "uncertain";
let targetSeeded = false;
let zaiTargetSeeded = false;
let handoffPrompt = "";
let mockStateWorkspaces = [];

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
      sendResponse({
        ok: true,
        targetConvKey: targetSeeded ? PROJECT_TARGET : null,
        targetUrl: tab.url,
        seedConfirmed: targetSeeded,
      });
      return;
    }
    if (msg?.type === "h2w_handoff_seed") {
      if (handoffSeedMode === "confirmed") {
        targetSeeded = true;
        tab.url = PROJECT_TARGET_URL;
        sendResponse({
          ok: true,
          targetConvKey: PROJECT_TARGET,
          targetUrl: PROJECT_TARGET_URL,
          seedConfirmed: true,
        });
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
      async remove() {},
    },
  },
  tabs: {
    async query({ url }) {
      const glob = url.replace("*", "");
      return [...tabs.values()].filter((t) => t.url.startsWith(glob)).map((t) => ({ id: t.id, url: t.url }));
    },
    async sendMessage(tabId, msg) {
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
      return { id: t.id, url: t.url, status: t.status || "complete" };
    },
    async create({ url }) {
      const id = ++nextTabId;
      const tab = { id, url, status: "complete", listener: null };
      tab.listener = targetListener(tab);
      tabs.set(id, tab);
      return { id, url, status: "complete" };
    },
    reload: () => {},
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
  ok(manualHud?.conversation_automation_available === false && manualHud?.enabled === false,
    "global manual mode hides/disables z.ai conversation automation without deleting its preference", JSON.stringify(manualHud));
  ok(storage.herdrConversationAutomation?.[ZAI_CONV] === true,
    "global manual mode preserves the saved z.ai conversation preference");

  let resolveProjectMode;
  const projectModeP = new Promise((r) => { resolveProjectMode = r; });
  onMsg({ type: "h2w_set_config", config: { automationMode: "project_auto" } }, {}, (r) => resolveProjectMode(r));
  await projectModeP;
  let resolveRestoredHud;
  const restoredHudP = new Promise((r) => { resolveRestoredHud = r; });
  onMsg({ type: "h2w_page_hud", convKey: ZAI_CONV }, { tab: { id: 350, url: ZAI_CONV } }, (r) => resolveRestoredHud(r));
  const restoredHud = await restoredHudP;
  ok(restoredHud?.conversation_automation_available === true && restoredHud?.enabled === true,
    "returning to automation mode restores the saved z.ai conversation preference", JSON.stringify(restoredHud));

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

// ---- Scenario 6e: z.ai manual handoff uses a raw summary/seed and cuts over only after confirmation ----
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
    "z.ai handoff uses the z.ai-specific raw summary template");

  await new Promise((r) => setTimeout(r, 600));
  const targetKey = `${ZAI_TARGET}::wY`;
  ok(!storage.herdrWakeBindings[sourceKey] && !!storage.herdrWakeBindings[targetKey],
    "z.ai binding moves only after the fresh target confirms its seed");
  ok(storage.herdrWakeBindings[targetKey]?.continuity_id === continuityId,
    "z.ai manual handoff preserves continuity id across chat ids");
  ok(storage.herdrWakeBindings[targetKey]?.handoff_from === ZAI_SOURCE,
    "z.ai target binding records the predecessor chat");
}

// ---- Scenario 7: Project handoff keeps source authoritative until target seed is confirmed ----
console.log("\n[project handoff]");
{
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
  const sourceKey = `${PROJECT_SOURCE}::wH`;
  ok(!!storage.herdrWakeBindings[sourceKey], "source Project binding is authoritative before rollover");
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

  let resolveLockedHandoff;
  const lockedHandoffP = new Promise((r) => { resolveLockedHandoff = r; });
  onMsg({ type: "h2w_handoff_start", tabId: 401 }, { tab: { id: 401 } }, (r) => resolveLockedHandoff(r));
  const lockedHandoff = await lockedHandoffP;
  ok(lockedHandoff?.ok === false && lockedHandoff?.error === "automation_enabled",
    "manual handoff is rejected while Project automation is on", JSON.stringify(lockedHandoff));

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
    .find((transfer) => transfer?.source_conv_key === PROJECT_SOURCE)?.id;
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

  await new Promise((r) => setTimeout(r, 800));
  const uncertain = storage.herdrConversationTransfers[transferId];
  ok(uncertain?.status === "seed_uncertain", "unconfirmed target seed is recorded as delivery-uncertain", JSON.stringify(uncertain));
  ok(!!storage.herdrWakeBindings[sourceKey], "source binding stays authoritative while target delivery is uncertain");
  ok(!storage.herdrWakeBindings[`${PROJECT_TARGET}::wH`], "uncertain target never receives the workspace binding");

  handoffSeedMode = "confirmed";
  let resolveResume;
  const resumeP = new Promise((r) => { resolveResume = r; });
  onMsg({ type: "h2w_handoff_start", tabId: 401 }, { tab: { id: 401 } }, (r) => resolveResume(r));
  const resumed = await resumeP;
  ok(resumed?.ok === true, "explicit resume completes the target seed and cutover", JSON.stringify(resumed));
  const targetKey = `${PROJECT_TARGET}::wH`;
  ok(!storage.herdrWakeBindings[sourceKey], "committed rollover removes the old conversation binding");
  ok(!!storage.herdrWakeBindings[targetKey], "committed rollover moves binding to the new Project conversation");
  ok(storage.herdrWakeBindings[targetKey]?.continuity_id === continuityId, "continuity id survives the conversation cutover");
  ok(storage.herdrWakeBindings[targetKey]?.handoff_from === PROJECT_SOURCE, "target binding records its predecessor conversation");
  let resolveTargetHud;
  const targetHudP = new Promise((r) => { resolveTargetHud = r; });
  onMsg({ type: "h2w_page_hud", convKey: PROJECT_TARGET }, { tab: { id: storage.herdrWakeBindings[targetKey]?.tabId } }, (r) => resolveTargetHud(r));
  const targetHud = await targetHudP;
  ok(targetHud?.enabled === false && targetHud?.project_id === PROJECT_ID,
    "rolled-over conversation preserves the Project automation-off setting used for manual handoff", JSON.stringify(targetHud));
  ok(storage.herdrConversationTransfers[transferId]?.status === "committed", "transfer metadata records committed state");
  ok(storage.herdrConversationTransfers[transferId]?.handoff_text == null, "committed transfer clears the temporary handoff packet");
}

console.log(`\n=== ${failures === 0 ? "BACKGROUND BIND ALL PASS" : failures + " FAILURES"} ===`);
process.exit(failures === 0 ? 0 : 1);
