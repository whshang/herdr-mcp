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

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CONV = "https://chatgpt.com/c/abc123";
const SK_WH = `${CONV}::wH`;

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

globalThis.chrome = {
  runtime: {
    id: "test-ext",
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
    reload: () => {},
  },
  scripting: { executeScript: async () => [{ result: { ok: true } }] },
  alarms: { create: () => {}, onAlarm: { addListener: () => {} } },
};

// Content-script stub for wake.js h2w_get_convkey responses.
function installContentScript(tabId, url, convKey) {
  tabs.set(tabId, {
    url, listener: (msg, _sender, sendResponse) => {
      if (msg?.type === "h2w_get_convkey") { sendResponse({ convKey, url, site: "chatgpt" }); return; }
      if (msg?.type === "h2w_wake") { sendResponse({}); return; }
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
  ok(b.workspace_id === "wH" && b.workspace_label.includes("herdr-mcp") && b.agent == null && typeof b.expires_at === "number" && !!b.revision, "binding fields include workspace scope and metadata", JSON.stringify(b));
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

console.log(`\n=== ${failures === 0 ? "BACKGROUND BIND ALL PASS" : failures + " FAILURES"} ===`);
process.exit(failures === 0 ? 0 : 1);
