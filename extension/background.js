// background.js — herdr→web 唤醒插件后台 (MV3 module service worker)
// 职责:
//  1. 配置 (herdr-mcp URL/token/唤醒模板/开关) 与绑定存储 (chrome.storage.local)
//  2. 每个绑定一条 SSE 推送流 (/push/events?pane=..., Bearer 鉴权), 断线自动重连
//  3. agent 状态迁移 → decideWake 决策 (工作→settled 才唤醒; 离线错过由 hello 补)
//  4. MAIN world 文本插入 (contenteditable 站点, 隔离世界不提交编辑模型)
//  5. popup/options 消息处理 (列 agent / 绑定 / 解绑 / 状态)
// 版本同步 (ctmc 教训): 内容脚本不随扩展重载自动重注入 — 版本变化时扫描目标站
// 标签页, 版本不匹配强制 reload。改 content 代码必须同步 bump
// H2W_SCRIPT_VERSION (background) 与 H2W_CONTENT_VERSION (wake.js)。
import { decideWake, pruneExpired, bindingRevision, buildWakeTemplate, shouldProgressTick, shouldSendProgress } from "./binding-core.js";

const H2W_SCRIPT_VERSION = "0.1.8";
const H2W_TAB_URLS = ["*://chat.z.ai/*", "*://chat.deepseek.com/*", "*://claude.ai/*", "*://chatgpt.com/*"];
const tabVersions = new Map();
const reloadedTabs = new Set();
const DEFAULT_TEMPLATE = "herdr agent {agent} ({pane}) 已完成 ({status})。\n\n{output}\n\n请基于以上结果继续。";
const DEFAULT_PROGRESS_TEMPLATE = "herdr agent {agent} ({pane}) 仍在执行 ({status})。\n\n{output}\n\n请用 herdr_since 续看；能 fs/exec 就不要再开贵模型。网页继续编排，勿把规划交给本机 Claude/OMP。";

function callLog(...args) { console.log("[h2w]", ...args); }
function runtimeAlive() { try { return !!chrome.runtime?.id; } catch { return false; } }
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---- 配置 (configReady: onStartup/重建流前必须等 storage 加载完, 否则用默认空 token 连) ----
let CFG = {
  herdrMcpUrl: "http://127.0.0.1:8772", token: "", enabled: true, autoAllow: true,
  wakeTemplate: DEFAULT_TEMPLATE, progressTickSec: 60, progressFallbackSec: 600,
  progressTemplate: DEFAULT_PROGRESS_TEMPLATE,
};
let resolveConfigReady;
const configReady = new Promise((r) => { resolveConfigReady = r; });
(async () => {
  try { CFG = { ...CFG, ...(await chrome.storage.local.get(Object.keys(CFG))) }; } catch (e) {}
  // 0.1.6+: 默认间隔改为 60s; 仍是旧默认 120 的安装一并迁过去
  if (Number(CFG.progressTickSec) === 120) {
    CFG.progressTickSec = 60;
    try { await chrome.storage.local.set({ progressTickSec: 60 }); } catch (e) {}
  }
  resolveConfigReady();
})();

// ---- 工具栏图标徽章 (替代页内绿点: 用户反馈页内状态点困惑) ----
// 语义: 有绑定 agent 工作中 → 琥珀 "…"; 唤醒成功 → 绿 "✓" 4s; 唤醒失败 → 红 "!" 8s;
// 其余时间无徽章 (绑定状态看 popup)。
let badgeClearTimer = null;
function setActionBadge(text, color, clearAfterMs = 0) {
  try {
    chrome.action.setBadgeText({ text });
    chrome.action.setBadgeBackgroundColor({ color });
  } catch (e) {}
  if (clearAfterMs > 0) {
    clearTimeout(badgeClearTimer);
    badgeClearTimer = setTimeout(() => { try { chrome.action.setBadgeText({ text: "" }); } catch (e) {} }, clearAfterMs);
  }
}
function clearActionBadge() {
  try { chrome.action.setBadgeText({ text: "" }); } catch (e) {}
}

// ---- 内容脚本版本同步 (ctmc: 重载扩展不会重注入已打开页面) ----
async function sweepStaleTabs() {
  try {
    const tabs = await chrome.tabs.query({ url: H2W_TAB_URLS });
    for (const t of tabs) {
      if (t.status !== "complete" || reloadedTabs.has(t.id)) continue;
      if (tabVersions.get(t.id) === H2W_SCRIPT_VERSION) continue;
      reloadedTabs.add(t.id);
      callLog(`tab ${t.id} ${t.url} 内容脚本 ${tabVersions.get(t.id) || "旧/未上报"} → 自动刷新注入新版`);
      chrome.tabs.reload(t.id);
    }
  } catch (e) { callLog("旧脚本扫描失败:", e.message); }
}
chrome.storage.local.get("h2wBgVersion", ({ h2wBgVersion }) => {
  if (h2wBgVersion !== H2W_SCRIPT_VERSION) {
    chrome.storage.local.set({ h2wBgVersion: H2W_SCRIPT_VERSION });
    setTimeout(sweepStaleTabs, 6000);
  }
});

// ---- 绑定存储 ----
// herdrWakeBindings: { [convKey]: {
//   pane, agent, workspace_id,          // herdr 侧 (pane 最稳)
//   convKey, site, tabId, tabUrl,       // 网页侧 (tabId 由 register 刷新, 非权威)
//   created_at, expires_at,             // 24h 过期 — loadBindings 剪枝
//   revision, status, lastSettle,       // status 持久化 (armed 判定), lastSettle 去重
// } }
async function loadBindings() {
  let b = {};
  try { b = (await chrome.storage.local.get("herdrWakeBindings")).herdrWakeBindings || {}; } catch (e) {}
  // 剪枝过期绑定, 中止对应推送流, 持久化清理结果 (README 声明的 24h 过期必须真实生效)
  const { kept, prunedKeys } = pruneExpired(b);
  if (prunedKeys.length) {
    callLog(`剪枝 ${prunedKeys.length} 个过期绑定: ${prunedKeys.join(", ")}`);
    for (const k of prunedKeys) { const s = pushStreams.get(k); if (s) { try { s.ctrl.abort(); } catch {} } pushStreams.delete(k); clearProgressTimer(k); }
    try { await chrome.storage.local.set({ herdrWakeBindings: kept }); } catch (e) {}
  }
  return kept;
}
async function saveBindings(b) {
  try { await chrome.storage.local.set({ herdrWakeBindings: b }); } catch (e) {}
}

// ---- SSE 推送客户端 (每绑定一条流) ----
const pushStreams = new Map(); // convKey -> { ctrl, retryTimer }
const pendingOutput = new Map(); // convKey -> output 片段 (settle 后短暂窗口)

async function ensurePushStream(bindings, convKey) {
  await configReady;
  if (pushStreams.has(convKey)) return;
  const b = bindings[convKey];
  if (!b || !b.pane) return;
  const ctrl = new AbortController();
  pushStreams.set(convKey, { ctrl });
  void runPushStream(convKey, b.pane, ctrl);
}

async function runPushStream(convKey, pane, ctrl) {
  const url = `${CFG.herdrMcpUrl.replace(/\/+$/, "")}/push/events?pane=${encodeURIComponent(pane)}`;
  let backoff = 2000;
  while (runtimeAlive() && !ctrl.signal.aborted) {
    try {
      const resp = await fetch(url, {
        signal: ctrl.signal,
        headers: CFG.token ? { Authorization: `Bearer ${CFG.token}` } : {},
      });
      if (!resp.ok) {
        callLog(`push ${pane} HTTP ${resp.status}, 重试 ${backoff}ms`);
        await sleep(backoff);
        backoff = Math.min(backoff * 2, 15000);
        continue;
      }
      backoff = 2000;
      if (!resp.body) throw new Error("no-body");
      callLog(`push ${pane} 已连接`);
      const reader = resp.body.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });
        let idx;
        while ((idx = buf.indexOf("\n\n")) >= 0) {
          const block = buf.slice(0, idx); buf = buf.slice(idx + 2);
          handlePushBlock(convKey, block);
        }
      }
      callLog(`push ${pane} 流结束, 重连 ${backoff}ms`);
    } catch (e) {
      if (ctrl.signal.aborted || !runtimeAlive()) break;
      callLog(`push ${pane} 断开 (${e.message}), 重试 ${backoff}ms`);
    }
    if (ctrl.signal.aborted) break;
    await sleep(backoff);
    backoff = Math.min(backoff * 2, 15000);
  }
  pushStreams.delete(convKey);
}

function handlePushBlock(convKey, block) {
  let event = null, data = null;
  for (const line of block.split("\n")) {
    if (line.startsWith("event:")) event = line.slice(6).trim();
    else if (line.startsWith("data:")) { try { data = JSON.parse(line.slice(5).trim()); } catch {} }
  }
  if (!event || !data) return; // keepalive 注释或空
  if (event === "hello") void onPushHello(convKey, data);
  else if (event === "agent_working") void onPushWorking(convKey, data);
  else if (event === "agent_settled") void onPushSettled(convKey, data);
  else if (event === "agent_output") { if (data.pane) pendingOutput.set(convKey, data.output || ""); }
  // agent_gone: 绑定 agent 消失 — 记录, 不唤醒
}

async function onPushHello(convKey, data) {
  const bindings = await loadBindings();
  const b = bindings[convKey];
  if (!b) return;
  const ag = (data.agents || []).find((a) => a.pane === b.pane) || null;
  const d = decideWake({ status: b.status, lastSettle: b.lastSettle }, "hello", { agent: ag });
  b.status = d.status;
  b.lastSettle = d.lastSettle;
  await saveBindings(bindings);
  if (d.status === "working") armProgressTimer(convKey, b); // SW 重启/重连后仍是 working → 续 tick
  if (d.wake) {
    callLog(`hello 补唤醒: ${b.pane} → ${ag?.status} (离线期间错过 settle)`);
    await routeWake(b, { status: ag?.status ?? "idle", output: "" });
  }
}

async function onPushWorking(convKey, data) {
  const bindings = await loadBindings();
  const b = bindings[convKey];
  if (!b || b.pane !== data.pane) return;
  b.status = "working";
  await saveBindings(bindings);
  setActionBadge("…", "#d97706");
  armProgressTimer(convKey, b); // 重复 working 重置下次到期, 不叠加 interval; 保留实发基线
}

async function onPushSettled(convKey, data) {
  const bindings = await loadBindings();
  const b = bindings[convKey];
  if (!b || b.pane !== data.pane) return;
  clearProgressTimer(convKey); // 先停 tick, 再走收工唤醒
  const d = decideWake({ status: b.status, lastSettle: b.lastSettle }, "settled", data);
  b.status = d.status;
  b.lastSettle = d.lastSettle;
  await saveBindings(bindings);
  if (!d.wake) return;
  // 等 agent_output (服务端 2s 内补发输出片段)
  let output = pendingOutput.get(convKey) || "";
  pendingOutput.delete(convKey);
  if (!output) {
    await new Promise((resolve) => {
      const t0 = Date.now();
      const iv = setInterval(() => {
        const o = pendingOutput.get(convKey);
        if (o) { pendingOutput.delete(convKey); clearInterval(iv); resolve(); }
        else if (Date.now() - t0 > 1200) { clearInterval(iv); resolve(); }
      }, 100);
    });
  }
  const finalOutput = pendingOutput.get(convKey) || output;
  pendingOutput.delete(convKey);
  callLog(`settled: ${b.pane} → ${data.status}, 唤醒 ${b.convKey}`);
  await routeWake(b, { status: data.status, output: finalOutput });
  setActionBadge("✓", "#16a34a", 4000);
}

// ---- working 进度定时检查 (主线 A) ----
// 每个 convKey 一个 setInterval; 重复 working 事件先 clear 再重设 (不叠加 interval)。
// lastTickAt: 检查节奏 (默认 60s)。lastSentAt: 上次实发时刻 (底线从这里起算)。
// hasProgressSent: 本轮 working 是否已发过; 发过则未满 progressFallbackSec 一律不发。
const progressTimers = new Map(); // convKey -> { id, lastTickAt, lastSentAt, lastOutputSent, hasProgressSent, inFlight }

function progressTickSecMs() {
  const sec = Number(CFG.progressTickSec);
  if (!Number.isFinite(sec) || sec <= 0) return 0;
  return Math.min(sec, 86400) * 1000;
}

function armProgressTimer(convKey, bindingSeed = null) {
  const ms = progressTickSecMs();
  if (ms <= 0) return;
  const prev = progressTimers.get(convKey);
  if (prev) clearInterval(prev.id);
  const now = Date.now();
  const seedSent = typeof prev?.lastSentAt === "number" ? prev.lastSentAt
    : typeof bindingSeed?.lastProgressSentAt === "number" ? bindingSeed.lastProgressSentAt : now;
  const seedOut = typeof prev?.lastOutputSent === "string" ? prev.lastOutputSent
    : typeof bindingSeed?.lastProgressOutput === "string" ? bindingSeed.lastProgressOutput : "";
  const seedHasSent = prev?.hasProgressSent === true
    || typeof bindingSeed?.lastProgressSentAt === "number";
  const ts = {
    id: 0,
    lastTickAt: now,
    lastSentAt: seedSent,
    lastOutputSent: seedOut,
    hasProgressSent: seedHasSent,
    inFlight: false,
  };
  ts.id = setInterval(() => tickProgress(convKey, ts), ms);
  progressTimers.set(convKey, ts);
  callLog(`progress tick 启动: ${convKey} 每 ${ms / 1000}s 检查 (实发后 ${CFG.progressFallbackSec || 0}s 内不重发)`);
}

function clearProgressTimer(convKey) {
  const t = progressTimers.get(convKey);
  if (t) { clearInterval(t.id); progressTimers.delete(convKey); }
}

async function tickProgress(convKey, ts) {
  if (ts.inFlight) return;
  const bindings = await loadBindings();
  const b = bindings[convKey];
  if (!b) { clearProgressTimer(convKey); return; }
  if (b.status !== "working" || !CFG.enabled) { clearProgressTimer(convKey); return; }
  if (!shouldProgressTick({ status: b.status, lastTickAt: ts.lastTickAt }, Date.now(), CFG)) return;
  ts.inFlight = true;
  try {
    ts.lastTickAt = Date.now();
    // output: 优先 SSE agent_output; 否则 /push/state 的摘要字段 (不用 terminal_title — 太抖, 会每分钟误判 new_output)
    let output = pendingOutput.get(convKey) || "";
    if (!output) {
      try {
        const st = await fetchState();
        const ag = (st.agents || []).find((a) => a.pane === b.pane);
        output = (ag && (ag.summary || ag.output || ag.status_text)) || "";
      } catch (e) { output = ""; }
    }
    const cur = progressTimers.get(convKey);
    if (cur !== ts || !CFG.enabled) return;
    const bindingsNow = await loadBindings();
    const curB = bindingsNow[convKey];
    if (!curB || curB.status !== "working") return;
    const decision = shouldSendProgress(
      {
        lastSentAt: ts.lastSentAt,
        lastOutputSent: ts.lastOutputSent,
        hasProgressSent: ts.hasProgressSent === true,
      },
      Date.now(),
      output,
      CFG,
    );
    if (!decision.send) {
      callLog(`progress skip: ${b.pane} (${decision.reason}, out=${String(output).slice(0, 40)})`);
      return;
    }
    callLog(`progress send: ${b.pane} → ${convKey} (${decision.reason})`);
    await routeWake(curB, { status: curB.status, output }, CFG.progressTemplate || DEFAULT_PROGRESS_TEMPLATE);
    ts.lastSentAt = Date.now();
    ts.lastOutputSent = String(output || "").trim();
    ts.hasProgressSent = true;
    pendingOutput.delete(convKey);
    curB.lastProgressSentAt = ts.lastSentAt;
    curB.lastProgressOutput = ts.lastOutputSent;
    bindingsNow[convKey] = curB;
    await saveBindings(bindingsNow);
  } finally {
    ts.inFlight = false;
  }
}

// 配置/流重建后重新对账: 去掉非 working; working 用 armProgressTimer (保留实发基线)。
function reconcileProgressTimers(bindings) {
  if (!CFG.enabled || progressTickSecMs() <= 0) {
    for (const convKey of [...progressTimers.keys()]) clearProgressTimer(convKey);
    return;
  }
  for (const convKey of [...progressTimers.keys()]) {
    const b = bindings[convKey];
    if (!b || b.status !== "working") clearProgressTimer(convKey);
  }
  for (const [convKey, b] of Object.entries(bindings)) {
    if (b.status === "working") armProgressTimer(convKey, b);
  }
}

async function routeWake(b, extra, template = CFG.wakeTemplate || DEFAULT_TEMPLATE) {
  if (!CFG.enabled) return;
  const rendered = buildWakeTemplate(template, {
    agent: b.agent, pane: b.pane, status: extra.status, output: extra.output,
  });
  const payload = { type: "h2w_wake", data: { agent: b.agent, pane: b.pane, status: extra.status, output: (extra.output || "").slice(0, 4000), template: rendered, autoAllow: CFG.autoAllow !== false } };

  // 1) 直接发到已知 tabId (register 刷新过)
  if (b.tabId) {
    try {
      await chrome.tabs.sendMessage(b.tabId, payload);
      callLog(`唤醒已发到 tab ${b.tabId}`);
      return;
    } catch (e) { /* tab 失效 → 走 URL 恢复 */ }
  }
  // 2) 按会话 URL 恢复 (页面刷新/浏览器重启后): convKey = origin+pathname → glob
  try {
    const url = new URL(b.convKey);
    const glob = `${url.origin}${url.pathname}*`;
    const tabs = await chrome.tabs.query({ url: glob });
    for (const t of tabs) {
      try {
        await chrome.tabs.sendMessage(t.id, payload);
        b.tabId = t.id;
        b.tabUrl = t.url;
        const bindings = await loadBindings();
        if (bindings[b.convKey]) bindings[b.convKey].tabId = t.id;
        await saveBindings(bindings);
        callLog(`唤醒已发到恢复的 tab ${t.id} (${t.url})`);
        return;
      } catch (e) { /* 该 tab 无内容脚本 → 尝试下一个 */ }
    }
    callLog(`没有可送达的 tab (convKey=${b.convKey}) — 保留绑定, 页面打开后由 register 恢复`);
  } catch (e) {
    callLog("路由恢复失败:", e.message);
  }
}

// ---- 消息处理 ----
chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (msg?.type === "h2w_hello") {
    if (sender.tab?.id) tabVersions.set(sender.tab.id, msg.version || "");
    return;
  }
  if (msg?.type === "h2w_register") {
    void (async () => {
      const bindings = await loadBindings();
      const b = bindings[msg.convKey];
      if (b) {
        b.tabId = sender.tab?.id;
        b.tabUrl = msg.url || sender.tab?.url;
        await saveBindings(bindings);
        ensurePushStream(bindings, msg.convKey);
        sendResponse({ bound: true, pane: b.pane, status: b.status || null });
      } else {
        sendResponse({ bound: false });
      }
    })();
    return true;
  }
  if (msg?.type === "h2w_insert_main") {
    // MAIN world 文本插入 (ctmc page_insert 移植): content script 隔离世界的
    // execCommand 只改 DOM 不提交编辑模型, MAIN world 实测能提交。
    // 选最后一个可见匹配: contenteditable 组合选择器可能命中多个, 输入框通常最后。
    if (!sender.tab?.id) { sendResponse({ ok: false, error: "no-tab" }); return; }
    chrome.scripting.executeScript({
      target: { tabId: sender.tab.id },
      world: "MAIN",
      func: (text, selector) => {
        try {
          const all = [...document.querySelectorAll(selector)];
          const el = all.reverse().find((e) => e.offsetParent !== null) || all[0] || null;
          if (!el) return { ok: false, error: "no-input" };
          el.focus();
          const sel = window.getSelection();
          const range = document.createRange();
          // 全选后 insertText = 替换, 避免重试时在末尾追加叠成三份
          range.selectNodeContents(el);
          sel.removeAllRanges();
          sel.addRange(range);
          const ok = document.execCommand("insertText", false, text);
          el.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: text }));
          const got = (el.innerText || el.textContent || "").replace(/\s+/g, " ").trim();
          const want = String(text || "").replace(/\s+/g, " ").trim();
          return { ok: !!ok, committed: got.includes(want), text: got.slice(0, 40) };
        } catch (e) { return { ok: false, error: String(e) }; }
      },
      args: [msg.text, msg.selector],
    }).then((res) => {
      const r = res && res[0] && res[0].result;
      sendResponse(r || { ok: false, error: "no-result" });
    }).catch((e) => sendResponse({ ok: false, error: e.message }));
    return true;
  }
  if (msg?.type === "h2w_get_config") {
    sendResponse({ ...CFG, scriptVersion: H2W_SCRIPT_VERSION });
    return;
  }
  if (msg?.type === "h2w_set_config") {
    CFG = { ...CFG, ...(msg.config || {}) };
    chrome.storage.local.set(CFG).then(() => {
      void rebuildStreams(); // 配置变化 → 全量重建 (URL/token 可能变了)
      sendResponse({ ok: true });
    });
    return true;
  }
  if (msg?.type === "h2w_state") {
    void (async () => {
      const bindings = await loadBindings();
      // popup 打开会唤醒 SW; 顺手补齐 SSE / tick (MV3 休眠后 in-memory 流会丢)
      void ensureAlive(bindings);
      let convInfo = null;
      if (msg.tabId) {
        try { convInfo = await chrome.tabs.sendMessage(msg.tabId, { type: "h2w_get_convkey" }); } catch (e) { convInfo = null; }
      }
      const binding = convInfo ? bindings[convInfo.convKey] || null : null;
      sendResponse({
        convInfo, binding,
        bindings: Object.values(bindings).map((b) => ({ convKey: b.convKey, pane: b.pane, agent: b.agent, status: b.status, site: b.site, created_at: b.created_at })),
        config: { herdrMcpUrl: CFG.herdrMcpUrl, enabled: CFG.enabled, tokenSet: !!CFG.token },
      });
    })();
    return true;
  }
  if (msg?.type === "h2w_agents") {
    void (async () => {
      sendResponse(await fetchState() || { error: "fetch-failed" });
    })();
    return true;
  }
  if (msg?.type === "h2w_bind") {
    void (async () => {
      const bindings = await loadBindings();
      const tabId = msg.tabId;
      let convInfo = null;
      try { convInfo = await chrome.tabs.sendMessage(tabId, { type: "h2w_get_convkey" }); } catch (e) {}
      if (!convInfo?.convKey) { sendResponse({ ok: false, error: "conversation-unavailable" }); return; }
      if (bindings[convInfo.convKey]) { sendResponse({ ok: false, error: "already-bound", convKey: convInfo.convKey }); return; }
      const b = {
        pane: msg.pane,
        agent: msg.agent || msg.pane,
        workspace_id: msg.workspace_id || null,
        convKey: convInfo.convKey,
        site: convInfo.site || "unknown",
        tabId, tabUrl: convInfo.url || null,
        created_at: Date.now(),
        expires_at: Date.now() + 86400000,
        status: "unknown",
        lastSettle: null,
      };
      b.revision = bindingRevision(b);
      bindings[convInfo.convKey] = b;
      await saveBindings(bindings);
      ensurePushStream(bindings, convInfo.convKey);
      try { chrome.tabs.sendMessage(tabId, { type: "h2w_bound", pane: msg.pane }); } catch (e) {}
      sendResponse({ ok: true, convKey: convInfo.convKey });
    })();
    return true;
  }
  if (msg?.type === "h2w_unbind") {
    void (async () => {
      const bindings = await loadBindings();
      const b = bindings[msg.convKey];
      if (!b) { sendResponse({ ok: false, error: "not-found" }); return; }
      delete bindings[msg.convKey];
      await saveBindings(bindings);
      clearProgressTimer(msg.convKey);
      const stream = pushStreams.get(msg.convKey);
      if (stream) { stream.ctrl.abort(); pushStreams.delete(msg.convKey); }
      if (b.tabId) { try { chrome.tabs.sendMessage(b.tabId, { type: "h2w_unbound" }); } catch (e) {} }
      if (!Object.keys(bindings).length) clearActionBadge();
      sendResponse({ ok: true });
    })();
    return true;
  }
  if (msg?.type === "h2w_wake_ack") {
    callLog(`wake ack ${msg.convKey}:`, JSON.stringify(msg.result), JSON.stringify(msg.confirm || {}));
    // 工具栏徽章: 唤醒结果即时反馈 (成功 ✓ / 失败 !)
    if (msg.result?.ok) setActionBadge("✓", "#16a34a", 4000);
    else setActionBadge("!", "#dc2626", 8000);
    return;
  }
  sendResponse({});
});

async function fetchState() {
  try {
    const resp = await fetch(`${CFG.herdrMcpUrl.replace(/\/+$/, "")}/push/state`, {
      headers: CFG.token ? { Authorization: `Bearer ${CFG.token}` } : {},
    });
    if (!resp.ok) return { ok: false, status: resp.status };
    return { ok: true, ...(await resp.json()) };
  } catch (e) {
    return { ok: false, error: e.message };
  }
}

// 全量重建: 先中止全部现有流 (配置 URL/token 可能已变, 旧流继续收错源),
// 再从持久化绑定重建。
async function rebuildStreams() {
  await configReady;
  for (const [convKey, stream] of pushStreams) {
    try { stream.ctrl.abort(); } catch (e) {}
    pushStreams.delete(convKey);
  }
  const bindings = await loadBindings();
  callLog(
    `rebuild streams v${H2W_SCRIPT_VERSION}: ${Object.keys(bindings).length} binding(s),`,
    `token=${CFG.token ? "set" : "empty"}, enabled=${CFG.enabled}`,
  );
  for (const convKey of Object.keys(bindings)) {
    ensurePushStream(bindings, convKey);
  }
  reconcileProgressTimers(bindings); // 配置变化 → 重挂 working 的 tick / 关停
}

// SW 从休眠醒来时 in-memory 的 pushStreams / progressTimers 已空,
// 但 storage 里的绑定还在。只补缺, 不 abort 已有流, 也不重置已有 tick 时钟。
async function ensureAlive(preloaded) {
  await configReady;
  const bindings = preloaded || await loadBindings();
  for (const convKey of Object.keys(bindings)) {
    ensurePushStream(bindings, convKey);
  }
  if (!CFG.enabled || progressTickSecMs() <= 0) return;
  for (const [convKey, b] of Object.entries(bindings)) {
    if (b.status === "working" && !progressTimers.has(convKey)) armProgressTimer(convKey);
  }
}

// ---- 安装/浏览器启动/每次 SW 启动: 配置加载完再重建推送流 ----
// MV3: SW 可在不触发 onInstalled/onStartup 的情况下被杀再起; 必须在模块顶层重建。
chrome.runtime.onStartup.addListener(() => { void rebuildStreams(); });
chrome.runtime.onInstalled.addListener(() => {
  void rebuildStreams();
  chrome.storage.local.get(["herdrMcpUrl"], (cfg) => {
    if (!cfg.herdrMcpUrl) chrome.storage.local.set({ herdrMcpUrl: "http://127.0.0.1:8772", token: "", enabled: true, autoAllow: true, wakeTemplate: DEFAULT_TEMPLATE, progressTickSec: 60, progressFallbackSec: 600, progressTemplate: DEFAULT_PROGRESS_TEMPLATE });
  });
});
void rebuildStreams();

// 每分钟闹钟把休眠的 SW 拉起来, 补 SSE / 丢失的 tick (不重置已有 interval)
try {
  chrome.alarms.create("h2w-keepalive", { periodInMinutes: 1 });
  chrome.alarms.onAlarm.addListener((a) => {
    if (a.name === "h2w-keepalive") void ensureAlive();
  });
} catch (e) {
  callLog("alarms 不可用:", e.message);
}
