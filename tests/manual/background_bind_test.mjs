#!/usr/bin/env node
/**
 * background_bind_test.mjs — 用 chrome API mock 驱动 extension/background.js,
 * 验证 popup 触发 h2w_bind 的完整链路 (绑定创建/推送流/消息路由)。
 * 不依赖真实 Chrome; 答案在"点击绑定"是否在逻辑层可用。
 *
 * Usage: node tests/manual/background_bind_test.mjs
 */
import { pathToFileURL } from "node:url";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
let failures = 0;
function ok(cond, label, detail = "") {
  if (cond) console.log(`  ✅ ${label}`);
  else { failures++; console.error(`  ❌ ${label} ${detail}`); }
}

// ---- chrome mock ----
const storage = { herdrWakeBindings: {}, herdrMcpUrl: "http://127.0.0.1:8772", token: "test-token", enabled: true, wakeTemplate: "a {status}" };
const listeners = { onMessage: [], onStartup: [], onInstalled: [] };
const sentMessages = []; // background -> content 的消息
const tabs = new Map();   // tabId -> { url, listener } (listener = content script 的 onMessage)

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
        // 内容脚本监听器若是同步 sendResponse, 直接 resolve; 否则超时
        setTimeout(resolve, 200);
      });
      return resp;
    },
    reload: () => {},
  },
  scripting: { executeScript: async () => [{ result: { ok: true } }] },
};

// 内容脚本 stub (模拟 wake.js 的 h2w_get_convkey 应答)
function installContentScript(tabId, url, convKey) {
  tabs.set(tabId, {
    url, listener: (msg, _sender, sendResponse) => {
      if (msg?.type === "h2w_get_convkey") { sendResponse({ convKey, url, site: "chatgpt" }); return; }
      if (msg?.type === "h2w_wake") { sendResponse({}); return; }
      sendResponse({});
    },
  });
}

// ---- 加载 background.js ----
await import(pathToFileURL(path.join(__dirname, "..", "..", "extension", "background.js")).href);
const onMsg = listeners.onMessage[0];
ok(!!onMsg, "background onMessage 监听器已注册");

// ---- 场景 1: 绑定成功 (active tab 是支持站点) ----
console.log("\n[绑定链路]");
{
  installContentScript(101, "https://chatgpt.com/c/abc123", "https://chatgpt.com/c/abc123");
  let resolveP;
  const p = new Promise((r) => { resolveP = r; });
  onMsg({ type: "h2w_bind", tabId: 101, pane: "wH:p1", agent: "omp", workspace_id: "wH" }, { tab: { id: 101 } }, (r) => resolveP(r));
  const r = await p;
  ok(r?.ok === true && r.convKey === "https://chatgpt.com/c/abc123", "h2w_bind 成功创建绑定", JSON.stringify(r));
  ok(!!storage.herdrWakeBindings["https://chatgpt.com/c/abc123"], "绑定已持久化到 storage");
  const b = storage.herdrWakeBindings["https://chatgpt.com/c/abc123"];
  ok(b.pane === "wH:p1" && typeof b.expires_at === "number" && !!b.revision, "绑定字段完整 (pane/expires_at/revision)");
  const bound = sentMessages.find((m) => m.msg?.type === "h2w_bound");
  ok(!!bound, "content script 收到 h2w_bound");
}

// ---- 场景 2: active tab 无内容脚本 → conversation-unavailable ----
{
  const before = Object.keys(storage.herdrWakeBindings).length;
  let resolveP;
  const p = new Promise((r) => { resolveP = r; });
  onMsg({ type: "h2w_bind", tabId: 999, pane: "wH:p1", agent: "omp" }, { tab: { id: 999 } }, (r) => resolveP(r));
  const r = await p;
  ok(r?.ok === false && r.error === "conversation-unavailable", "非支持 tab → conversation-unavailable", JSON.stringify(r));
  ok(Object.keys(storage.herdrWakeBindings).length === before, "失败不产生绑定");
}

// ---- 场景 3: 重复绑定 → already-bound ----
{
  let resolveP;
  const p = new Promise((r) => { resolveP = r; });
  onMsg({ type: "h2w_bind", tabId: 101, pane: "w2Y:p1", agent: "omp" }, { tab: { id: 101 } }, (r) => resolveP(r));
  const r = await p;
  ok(r?.ok === false && r.error === "already-bound", "重复绑定被拒绝", JSON.stringify(r));
}

// ---- 场景 4: register (页面刷新恢复 tabId) ----
{
  let resolveP;
  const p = new Promise((r) => { resolveP = r; });
  onMsg({ type: "h2w_register", convKey: "https://chatgpt.com/c/abc123", url: "https://chatgpt.com/c/abc123", site: "chatgpt" }, { tab: { id: 202 } }, (r) => resolveP(r));
  const r = await p;
  ok(r?.bound === true && r.pane === "wH:p1", "register 恢复绑定 (刷新页面)", JSON.stringify(r));
  ok(storage.herdrWakeBindings["https://chatgpt.com/c/abc123"].tabId === 202, "tabId 已刷新");
}

// ---- 场景 5: unbind ----
{
  let resolveP;
  const p = new Promise((r) => { resolveP = r; });
  onMsg({ type: "h2w_unbind", convKey: "https://chatgpt.com/c/abc123" }, {}, (r) => resolveP(r));
  const r = await p;
  ok(r?.ok === true && !storage.herdrWakeBindings["https://chatgpt.com/c/abc123"], "unbind 删除绑定", JSON.stringify(r));
}

// ---- 场景 6: state (popup 渲染用) ----
{
  installContentScript(303, "https://chatgpt.com/c/xyz", "https://chatgpt.com/c/xyz");
  let resolveP;
  const p = new Promise((r) => { resolveP = r; });
  onMsg({ type: "h2w_state", tabId: 303 }, { tab: { id: 303 } }, (r) => resolveP(r));
  const r = await p;
  ok(!!r.convInfo && r.convInfo.convKey === "https://chatgpt.com/c/xyz" && Array.isArray(r.bindings), "h2w_state 返回 convInfo+bindings", JSON.stringify(r).slice(0, 120));
}

console.log(`\n=== ${failures === 0 ? "BACKGROUND BIND ALL PASS" : failures + " FAILURES"} ===`);
process.exit(failures === 0 ? 0 : 1);
