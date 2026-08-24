// background.js — herdr-to-web wake-up extension backend (MV3 module service worker)
// Responsibilities:
//  1. Store configuration and bindings in chrome.storage.local.
//  2. Maintain one reconnecting SSE stream per workspace binding.
//  3. Turn workspace agent events into partial-progress or round-complete wake-ups.
//  4. Insert text in the MAIN world for contenteditable sites.
//  5. Handle in-page HUD and options messages for listing, binding, unbinding, and status.
// Version synchronization: extension reloads do not reinject content scripts into
// open tabs. Scan target tabs after a version change and reload stale scripts.
// Keep H2W_SCRIPT_VERSION here aligned with H2W_CONTENT_VERSION in wake.js.
import {
  decideWorkspaceWake, agentsInWorkspace, formatWorkspaceRoster, workspaceTitleWithId,
  reconcileWorkspaceWakeKind,
  pruneExpired, bindingRevision, buildWakeTemplate, shouldProgressTick, shouldSendProgress,
  isIdleNudgeText, looksLikeSubstantiveReply, isLlmJudgeConfigured, llmJudgeCompletionsUrl, buildLlmJudgeUserMessage, interpretLlmJudgeReply,
  assistantNudgeFingerprint,
  DEFAULT_LLM_JUDGE_PROMPT, DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
  conversationInfoFromSupportedUrl,
} from "./binding-core.js";
import {
  buildHandoffRequest, buildHandoffSeed, chatGptConversationInfo,
  extractHandoffPacket, handoffSeedContainsTransfer, handoffStatusIsActive,
  newContinuityId, newTransferId,
} from "./continuity-core.js";

const H2W_SCRIPT_VERSION = "0.1.41";
const H2W_TAB_URLS = ["*://chat.z.ai/*", "*://chat.deepseek.com/*", "*://claude.ai/*", "*://chatgpt.com/*"];
const PUSH_CONNECT_MS = 5000;
const STATE_FETCH_MS = 4000;
const HANDOFF_STORAGE_KEY = "herdrConversationTransfers";
const HANDOFF_RETENTION_MS = 7 * 86400000;
const tabVersions = new Map();
const reloadedTabs = new Set();
const DEFAULT_TEMPLATE =
  "herdr workspace {workspace_label}: agents stopped (focus {agent} @ {pane} → {status}).\n\nFocus pane output:\n{output}\n\n{roster}\n\n{idle_hint}\n\nContinue orchestration from these results; prefer fs/exec over expensive models.";
const DEFAULT_PROGRESS_TEMPLATE =
  "herdr workspace {workspace_label} progress (focus {agent} @ {pane} · {status}; {working_count} still working in this space).\n\nFocus pane output:\n{output}\n\n{roster}\n\n{idle_hint}\n\nUse herdr_since / inspect to continue; keep orchestrating on the web.";
const DEFAULT_PARTIAL_TEMPLATE =
  "herdr workspace {workspace_label}: focus {agent} @ {pane} stopped ({status}); {working_count} still working in this space.\n\nFocus pane output:\n{output}\n\n{roster}\n\n{idle_hint}\n\nThis is a partial finish, not a full round settle. Keep watching or schedule the remaining workers.";

function callLog(...args) { console.log("[h2w]", ...args); }
function runtimeAlive() { try { return !!chrome.runtime?.id; } catch { return false; } }
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---- Configuration (wait for storage before startup or stream rebuild) ----
let CFG = {
  herdrMcpUrl: "http://127.0.0.1:8772", token: "", enabled: true, autoAllow: true,
  wakeTemplate: DEFAULT_TEMPLATE, progressTickSec: 60, progressFallbackSec: 1200,
  progressTemplate: DEFAULT_PROGRESS_TEMPLATE,
  idleNudgeEnabled: true,
  // Post-turn LLM judge (OpenAI-compatible). Defaults empty — fill in Options.
  llmJudgeBaseUrl: "",
  llmJudgeApiKey: "",
  llmJudgeModel: "",
  llmJudgePromptTemplate: DEFAULT_LLM_JUDGE_PROMPT,
  llmJudgeSkipKeywords: DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
};
let resolveConfigReady;
const configReady = new Promise((r) => { resolveConfigReady = r; });
(async () => {
  let stored = {};
  try {
    const keys = [...Object.keys(CFG), "idleNudgeCooldownSec"];
    stored = await chrome.storage.local.get(keys);
    CFG = { ...CFG, ...stored };
  } catch (e) {}
  const patch = {};
  // Legacy: idleNudgeCooldownSec merged into progressTickSec (one interval for progress + nudge).
  if (stored.idleNudgeCooldownSec != null) {
    const tick = Number(CFG.progressTickSec);
    const nudge = Number(stored.idleNudgeCooldownSec);
    if ((!Number.isFinite(tick) || tick < 0) && Number.isFinite(nudge) && nudge >= 0) {
      CFG.progressTickSec = nudge;
      patch.progressTickSec = nudge;
    }
    delete CFG.idleNudgeCooldownSec;
    patch.idleNudgeCooldownSec = null;
  }
  if (Number(CFG.progressFallbackSec) === 600) {
    CFG.progressFallbackSec = 1200;
    patch.progressFallbackSec = 1200;
  }
  // 0.1.40: wake delivery and the post-turn small-model nudge share one
  // operational switch. Keep the legacy field mirrored for stored-config and
  // older content-script compatibility, but `enabled` is authoritative.
  if (CFG.idleNudgeEnabled !== CFG.enabled) {
    CFG.idleNudgeEnabled = CFG.enabled;
    patch.idleNudgeEnabled = CFG.enabled;
  }
  if (Object.keys(patch).length) {
    try {
      await chrome.storage.local.set(patch);
      if (patch.idleNudgeCooldownSec === null) {
        await chrome.storage.local.remove("idleNudgeCooldownSec");
      }
    } catch (e) {}
  }
  resolveConfigReady();
})();

// ---- Toolbar badge (replaces the ambiguous in-page status dot) ----
// Semantics: bound agent working → amber "…"; wake succeeded → green "✓" for 4s;
// wake failed → red "!" for 8s; otherwise no badge.
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

async function conversationInfoForTab(tabId) {
  if (!tabId) return null;
  try {
    const live = await chrome.tabs.sendMessage(tabId, { type: "h2w_get_convkey" });
    if (live?.convKey) {
      const parsed = conversationInfoFromSupportedUrl(live.url || live.convKey);
      return parsed ? { ...live, ...parsed, convKey: parsed.convKey } : live;
    }
  } catch (_) {}

  let tab = null;
  try { tab = await chrome.tabs.get(tabId); } catch (_) {}
  const fallback = conversationInfoFromSupportedUrl(tab?.url);
  if (!fallback) return null;

  // A manual MV3 extension reload can leave an already-open ChatGPT tab without
  // a live listener even when the extension version did not change. Recover the
  // content script in-place, then prefer its canonical adapter identity.
  try {
    await chrome.scripting.executeScript({
      target: { tabId },
      files: ["content/base.js", "content/injector/chatgpt.js", "content/wake.js"],
    });
    await sleep(50);
    const live = await chrome.tabs.sendMessage(tabId, { type: "h2w_get_convkey" });
    if (live?.convKey) {
      const parsed = conversationInfoFromSupportedUrl(live.url || live.convKey);
      return parsed ? { ...live, ...parsed, convKey: parsed.convKey } : live;
    }
  } catch (e) {
    callLog(`content-script recovery failed for tab ${tabId}:`, e.message);
  }
  return fallback;
}

// ---- Content-script version synchronization ----
async function sweepStaleTabs() {
  try {
    const tabs = await chrome.tabs.query({ url: H2W_TAB_URLS });
    for (const t of tabs) {
      if (t.status !== "complete" || reloadedTabs.has(t.id)) continue;
      if (tabVersions.get(t.id) === H2W_SCRIPT_VERSION) continue;
      reloadedTabs.add(t.id);
      callLog(`tab ${t.id} ${t.url} content script ${tabVersions.get(t.id) || "old/unreported"}; reloading`);
      chrome.tabs.reload(t.id);
    }
  } catch (e) { callLog("stale-script scan failed:", e.message); }
}
chrome.storage.local.get("h2wBgVersion", ({ h2wBgVersion }) => {
  if (h2wBgVersion !== H2W_SCRIPT_VERSION) {
    chrome.storage.local.set({ h2wBgVersion: H2W_SCRIPT_VERSION });
    setTimeout(sweepStaleTabs, 6000);
  }
});

// ---- Binding storage ----
// herdrWakeBindings: { [`${convKey}::${workspace_id}`]: {
//   workspace_id,                       // Primary binding: entire herdr workspace
//   pane, agent,                        // Most recently active pane/agent for display and compatibility
//   workingPanes: { [paneId]: true },   // Panes still working in scope
//   convKey, site, tabId, tabUrl,
//   created_at, expires_at,
//   revision, status, lastSettle,
// } }
function normalizeWorkspaceId(b) {
  if (b?.workspace_id) return b.workspace_id;
  if (typeof b?.pane === "string" && b.pane.includes(":")) return b.pane.split(":")[0];
  return null;
}

function bindingStoreKey(convKey, workspaceId) {
  return `${convKey}::${workspaceId}`;
}

function parseBindingStoreKey(storeKey) {
  const i = storeKey.lastIndexOf("::");
  if (i <= 0) return { convKey: storeKey, workspaceId: null };
  return { convKey: storeKey.slice(0, i), workspaceId: storeKey.slice(i + 2) };
}

function bindingStoreKeyFromBinding(b) {
  const convKey = b?.convKey;
  const ws = normalizeWorkspaceId(b);
  if (!convKey || !ws) return convKey || null;
  return bindingStoreKey(convKey, ws);
}

/** Migrate legacy convKey-only keys to `${convKey}::${workspace_id}`. */
function migrateBindingsMap(raw, now = Date.now()) {
  const out = {};
  let migrated = false;
  for (const [k, b] of Object.entries(raw || {})) {
    if (!b || typeof b !== "object") continue;
    const rawConvKey = b.convKey || (k.includes("::") ? parseBindingStoreKey(k).convKey : k);
    const convKey = conversationInfoFromSupportedUrl(rawConvKey)?.convKey || rawConvKey;
    const ws = b.workspace_id || normalizeWorkspaceId(b);
    if (!ws) {
      out[k] = { ...b, convKey };
      continue;
    }
    const sk = bindingStoreKey(convKey, ws);
    if (sk !== k) migrated = true;
    const next = { ...b, convKey, workspace_id: ws };
    // Workspace bindings are explicit user choices. Older builds imposed a
    // 24-hour expiry, which is hostile to long-running Project conversations.
    // Preserve already-expired rows so pruneExpired can remove them, but promote
    // every still-live legacy row to explicit persistence.
    if (next.persistence !== "explicit"
      && !(typeof next.expires_at === "number" && next.expires_at <= now)) {
      next.persistence = "explicit";
      delete next.expires_at;
      migrated = true;
    }
    const prev = out[sk];
    if (!prev || Number(next.last_seen_at || next.created_at || 0) >= Number(prev.last_seen_at || prev.created_at || 0)) {
      out[sk] = next;
    }
  }
  return { map: out, migrated };
}

function bindingsForConv(bindings, convKey) {
  const out = [];
  for (const [storeKey, b] of Object.entries(bindings)) {
    if (b?.convKey === convKey) out.push({ storeKey, ...b });
  }
  return out;
}

function primaryBindingForConv(bindings, convKey) {
  return bindingsForConv(bindings, convKey)[0] || null;
}

function bindingView(b) {
  return {
    ...b,
    workspace_id: b.workspace_id || normalizeWorkspaceId(b),
    workspace_label: b.workspace_label || null,
    working_count: Object.keys(workingPaneMap(b)).length,
  };
}

function workingPaneMap(b) {
  if (!b.workingPanes || typeof b.workingPanes !== "object") b.workingPanes = {};
  return b.workingPanes;
}

function syntheticWorkingAgents(b) {
  const ws = normalizeWorkspaceId(b);
  return Object.keys(workingPaneMap(b)).map((pane) => ({
    pane, status: "working", workspace: ws, name: b.agent || null,
  }));
}
async function loadBindings() {
  let raw = {};
  try { raw = (await chrome.storage.local.get("herdrWakeBindings")).herdrWakeBindings || {}; } catch (e) {}
  const { map: b, migrated } = migrateBindingsMap(raw);
  // Prune expired bindings and persist the cleanup. Push transport is shared
  // across all bindings, so pruning one binding must not tear down transport
  // for the remaining bindings.
  const { kept, prunedKeys } = pruneExpired(b);
  if (prunedKeys.length || migrated) {
    if (prunedKeys.length) {
      callLog(`pruned ${prunedKeys.length} expired bindings: ${prunedKeys.join(", ")}`);
      for (const k of prunedKeys) clearProgressTimer(k);
    }
    if (migrated) callLog("migrated legacy binding keys to convKey::workspace_id");
    try { await chrome.storage.local.set({ herdrWakeBindings: kept }); } catch (e) {}
  }
  return kept;
}
async function saveBindings(b) {
  try { await chrome.storage.local.set({ herdrWakeBindings: b }); } catch (e) {}
}

// ---- Conversation continuity / handoff storage ----
async function loadHandoffTransfers() {
  let raw = {};
  try { raw = (await chrome.storage.local.get(HANDOFF_STORAGE_KEY))[HANDOFF_STORAGE_KEY] || {}; } catch (_) {}
  const now = Date.now();
  const kept = {};
  let changed = false;
  for (const [id, row] of Object.entries(raw || {})) {
    if (!row || typeof row !== "object") { changed = true; continue; }
    const at = Number(row.updated_at || row.created_at || 0);
    if (at > 0 && now - at > HANDOFF_RETENTION_MS && !handoffStatusIsActive(row.status)) {
      changed = true;
      continue;
    }
    kept[id] = row;
  }
  if (changed) {
    try { await chrome.storage.local.set({ [HANDOFF_STORAGE_KEY]: kept }); } catch (_) {}
  }
  return kept;
}

async function saveHandoffTransfers(transfers) {
  await chrome.storage.local.set({ [HANDOFF_STORAGE_KEY]: transfers || {} });
}

function latestTransferForConversation(transfers, convKey) {
  let best = null;
  for (const row of Object.values(transfers || {})) {
    if (!row || (row.source_conv_key !== convKey && row.target_conv_key !== convKey)) continue;
    if (!best || Number(row.updated_at || row.created_at || 0) > Number(best.updated_at || best.created_at || 0)) best = row;
  }
  return best;
}

function activeTransferFromSource(transfers, convKey) {
  let best = null;
  for (const row of Object.values(transfers || {})) {
    if (!row || row.source_conv_key !== convKey || !handoffStatusIsActive(row.status)) continue;
    if (!best || Number(row.updated_at || row.created_at || 0) > Number(best.updated_at || best.created_at || 0)) best = row;
  }
  return best;
}

function handoffView(row, convKey = null) {
  if (!row) return null;
  const ageMs = Math.max(0, Date.now() - Number(row.updated_at || row.created_at || Date.now()));
  const canResume = row.status === "seed_uncertain"
    || (ageMs >= 120000 && ["summary_requested", "summary_ready", "target_opening", "seed_submitting"].includes(row.status));
  return {
    id: row.id,
    continuity_id: row.continuity_id || null,
    status: row.status || "unknown",
    source_conv_key: row.source_conv_key || null,
    target_conv_key: row.target_conv_key || null,
    role: convKey && row.target_conv_key === convKey ? "target" : "source",
    error: row.error || null,
    can_resume: canResume,
    age_ms: ageMs,
    created_at: row.created_at || null,
    updated_at: row.updated_at || null,
  };
}

function sameWorkspaceSet(a, b) {
  const left = [...new Set((a || []).map((x) => String(x || "")).filter(Boolean))].sort();
  const right = [...new Set((b || []).map((x) => String(x || "")).filter(Boolean))].sort();
  return left.length === right.length && left.every((v, i) => v === right[i]);
}

function transferSourceSnapshot(sessionBindings) {
  return (sessionBindings || []).map((b) => ({
    workspace_id: b.workspace_id || normalizeWorkspaceId(b),
    revision: b.revision || bindingRevision(b),
  })).filter((b) => b.workspace_id);
}

async function markTransfer(transferId, patch) {
  const transfers = await loadHandoffTransfers();
  const row = transfers[transferId];
  if (!row) return null;
  transfers[transferId] = {
    ...row,
    ...(patch || {}),
    updated_at: Date.now(),
  };
  await saveHandoffTransfers(transfers);
  return transfers[transferId];
}

async function waitForTabComplete(tabId, timeoutMs = 15000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const tab = await chrome.tabs.get(tabId);
      if (tab?.status === "complete") return tab;
    } catch (_) { return null; }
    await sleep(150);
  }
  try { return await chrome.tabs.get(tabId); } catch (_) { return null; }
}

async function tabStillExists(tabId) {
  if (!tabId) return false;
  try { return Boolean(await chrome.tabs.get(tabId)); } catch (_) { return false; }
}

function missingReceiverError(error) {
  const text = String(error?.message || error || "");
  return /receiving end does not exist|could not establish connection/i.test(text);
}

async function sendChatGptTabMessage(tabId, message) {
  try {
    return await chrome.tabs.sendMessage(tabId, message);
  } catch (first) {
    if (!missingReceiverError(first)) throw first;
    await chrome.scripting.executeScript({
      target: { tabId },
      files: ["content/base.js", "content/injector/chatgpt.js", "content/wake.js"],
    });
    await sleep(80);
    return chrome.tabs.sendMessage(tabId, message);
  }
}

// ---- SSE push client (ONE shared stream for every binding) ----
// Long-lived HTTP/1.1 SSE requests consume Chromium's small per-origin
// connection pool. One stream per binding can therefore starve /push/state
// once enough historical bindings exist. The server already includes the
// workspace id in agent events and an all-workspace snapshot in hello, so a
// single unfiltered stream is sufficient; fan-out happens in this worker.
let pushStream = null; // { ctrl }
let pushDispatch = Promise.resolve();
const pendingOutputByPane = new Map(); // `${storeKey}::${pane}` -> output
let pushWorkspaceCatalog = [];
let pushWorkspaceCatalogAt = 0;
let stateFetchInFlight = null;

function cachePushWorkspaceCatalog(workspaces) {
  if (!Array.isArray(workspaces)) return;
  pushWorkspaceCatalog = workspaces
    .filter((w) => w && typeof w === "object" && typeof w.id === "string" && w.id)
    .map((w) => ({ ...w }));
  pushWorkspaceCatalogAt = Date.now();
}

function cachedPushWorkspaceCatalog() {
  return pushWorkspaceCatalog.map((w) => ({ ...w }));
}

function pendingKey(storeKey, pane) {
  return `${storeKey}::${pane || "_"}`;
}

function stopPushStream() {
  const stream = pushStream;
  pushStream = null;
  if (stream) { try { stream.ctrl.abort(); } catch {} }
}

async function ensurePushStream(bindings) {
  await configReady;
  // Keep exactly one extension-wide observation stream even when automatic
  // wake/nudge is paused. Bound workspace runtime state must stay live in the
  // HUD; CFG.enabled gates actions, not observability.
  if (pushStream) return;
  const ctrl = new AbortController();
  pushStream = { ctrl };
  void runPushStream(ctrl);
}

async function runPushStream(ctrl) {
  const url = `${CFG.herdrMcpUrl.replace(/\/+$/, "")}/push/events`;
  let backoff = 2000;
  while (runtimeAlive() && !ctrl.signal.aborted) {
    const attempt = new AbortController();
    const relayAbort = () => attempt.abort();
    ctrl.signal.addEventListener("abort", relayAbort, { once: true });
    const connectTimer = setTimeout(() => attempt.abort(), PUSH_CONNECT_MS);
    try {
      const resp = await fetch(url, {
        signal: attempt.signal,
        headers: CFG.token ? { Authorization: `Bearer ${CFG.token}` } : {},
      });
      clearTimeout(connectTimer);
      if (!resp.ok) {
        callLog(`push HTTP ${resp.status}; retrying in ${backoff}ms`);
        await sleep(backoff);
        backoff = Math.min(backoff * 2, 15000);
        continue;
      }
      backoff = 2000;
      if (!resp.body) throw new Error("no-body");
      callLog("push connected (shared stream)");
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
          pushDispatch = pushDispatch
            .then(() => handlePushBlock(block))
            .catch((e) => callLog("push dispatch failed:", e?.message || String(e)));
        }
      }
      callLog(`push stream ended; reconnecting in ${backoff}ms`);
    } catch (e) {
      if (ctrl.signal.aborted || !runtimeAlive()) break;
      callLog(`push disconnected (${e.message}); retrying in ${backoff}ms`);
    } finally {
      clearTimeout(connectTimer);
      ctrl.signal.removeEventListener("abort", relayAbort);
    }
    if (ctrl.signal.aborted) break;
    await sleep(backoff);
    backoff = Math.min(backoff * 2, 15000);
  }
  if (pushStream?.ctrl === ctrl) pushStream = null;
}

async function handlePushBlock(block) {
  let event = null, data = null;
  for (const line of block.split("\n")) {
    if (line.startsWith("event:")) event = line.slice(6).trim();
    else if (line.startsWith("data:")) { try { data = JSON.parse(line.slice(5).trim()); } catch {} }
  }
  if (!event || !data) return;
  const bindings = await loadBindings();
  const entries = Object.entries(bindings);
  const scoped = data.workspace
    ? entries.filter(([, b]) => normalizeWorkspaceId(b) === data.workspace)
    : entries;
  if (event === "hello") {
    cachePushWorkspaceCatalog(data.workspaces);
    for (const [storeKey] of entries) await onPushHello(storeKey, data);
  } else if (event === "agent_working") {
    for (const [storeKey] of scoped) await onPushWorking(storeKey, data);
  } else if (event === "agent_settled") {
    for (const [storeKey] of scoped) await onPushSettled(storeKey, data);
  } else if (event === "agent_output" && data.pane) {
    for (const [storeKey] of scoped) {
      pendingOutputByPane.set(pendingKey(storeKey, data.pane), data.output || "");
    }
  }
}

async function onPushHello(storeKey, data) {
  const bindings = await loadBindings();
  const b = bindings[storeKey];
  if (!b) return;
  const ws = normalizeWorkspaceId(b);
  b.workspace_id = ws;
  const scope = agentsInWorkspace(data.agents || [], ws);
  const wp = workingPaneMap(b);
  for (const k of Object.keys(wp)) delete wp[k];
  for (const a of scope) {
    if (a.status === "working" && a.pane) wp[a.pane] = true;
  }
  const d = decideWorkspaceWake(
    { status: b.status, lastSettle: b.lastSettle },
    "hello",
    {},
    scope,
  );
  b.status = d.status;
  b.lastSettle = d.lastSettle;
  await saveBindings(bindings);
  if (d.status === "working" && CFG.enabled) armProgressTimer(storeKey, b);
  if (d.wake) {
    callLog(`hello recovery wake: ws=${ws} → ${d.status} (settle missed while offline)`);
    await routeWake(b, { status: d.status, output: "", working_count: d.working_count }, CFG.wakeTemplate || DEFAULT_TEMPLATE);
  }
}

async function onPushWorking(storeKey, data) {
  const bindings = await loadBindings();
  const b = bindings[storeKey];
  const ws = normalizeWorkspaceId(b);
  if (!b || !ws) return;
  if (data.workspace && data.workspace !== ws) return;
      if (data.pane) workingPaneMap(b)[data.pane] = true;
  b.pane = data.pane || b.pane;
  b.focus_agent = data.agent || b.focus_agent;
  b.status = "working";
  await saveBindings(bindings);
  setActionBadge("…", "#d97706");
  if (CFG.enabled) armProgressTimer(storeKey, b);
  else clearProgressTimer(storeKey);
}

async function onPushSettled(storeKey, data) {
  const bindings = await loadBindings();
  const b = bindings[storeKey];
  const ws = normalizeWorkspaceId(b);
  if (!b || !ws) return;
  if (data.workspace && data.workspace !== ws) return;
  if (data.pane) delete workingPaneMap(b)[data.pane];
  b.pane = data.pane || b.pane;
  b.focus_agent = data.agent || b.focus_agent;
  const scope = syntheticWorkingAgents(b);
  const d = decideWorkspaceWake(
    { status: b.status, lastSettle: b.lastSettle },
    "settled",
    data,
    scope,
  );
  b.status = d.status;
  b.lastSettle = d.lastSettle;
  await saveBindings(bindings);

  if (d.kind === "round") clearProgressTimer(storeKey);
  if (!d.wake) return;

  let output = "";
  if (data.pane) {
    const pk = pendingKey(storeKey, data.pane);
    output = pendingOutputByPane.get(pk) || "";
    pendingOutputByPane.delete(pk);
    if (!output) {
      await new Promise((resolve) => {
        const t0 = Date.now();
        const iv = setInterval(() => {
          const o = pendingOutputByPane.get(pk);
          if (o) { pendingOutputByPane.delete(pk); clearInterval(iv); resolve(); }
          else if (Date.now() - t0 > 1200) { clearInterval(iv); resolve(); }
        }, 100);
      });
      output = pendingOutputByPane.get(pk) || "";
      pendingOutputByPane.delete(pk);
    }
  }

  const fields = {
    status: data.status,
    output,
    working_count: d.working_count,
    agent: data.agent || b.agent,
    pane: data.pane || b.pane,
  };
  const routed = await routeWake(
    b,
    fields,
    d.kind === "partial" ? DEFAULT_PARTIAL_TEMPLATE : (CFG.wakeTemplate || DEFAULT_TEMPLATE),
    d.kind,
  );
  const finalKind = routed?.h2w_wake_kind || d.kind;
  const finalWorkingCount = routed?.working_count ?? d.working_count;
  if (finalKind === "partial") {
    callLog(`partial settled: ${data.pane} @ ${ws}, still working=${finalWorkingCount}`);
    setActionBadge("…", "#d97706");
  } else {
    callLog(`round settled: ws=${ws} last=${data.pane} → ${data.status}`);
    setActionBadge("✓", "#16a34a", 4000);
  }
}

// ---- Periodic progress checks while working ----
// One setInterval per convKey; repeated working events replace rather than stack it.
// lastTickAt controls check cadence. lastSentAt anchors the send cooldown.
// hasProgressSent records whether this working round has sent progress.
const progressTimers = new Map(); // convKey -> { id, lastTickAt, lastSentAt, lastOutputSent, hasProgressSent, inFlight }
const lastIdleNudgeAt = new Map(); // convKey -> ms
const lastJudgedAssistantFp = new Map(); // convKey -> fingerprint of assistant text already judged
const lastIdleNudgeResult = new Map(); // convKey -> { at, reason, raw?, nudged }
const idleNudgeRetryTimers = new Map(); // convKey -> timeout id
const lastTurnEndedPayload = new Map(); // convKey -> last h2w_turn_ended payload (for cooldown retry)

const LLM_JUDGE_TIMEOUT_MS = 60000;
const LLM_JUDGE_MAX_ATTEMPTS = 2;

/**
 * One-shot OpenAI-compatible chat/completions call to judge if the turn is done.
 * Secrets stay in chrome.storage only — never logged.
 * @param {string} userText
 * @param {string} assistantText
 * @param {object|null} [cfgOverride] — Options test may pass form values before Save.
 */
async function fetchLlmJudgeOnce(userText, assistantText, cfgOverride = null) {
  const cfg = cfgOverride || CFG;
  if (!isLlmJudgeConfigured(cfg)) return { ok: false, reason: "not_configured" };
  const url = llmJudgeCompletionsUrl(cfg.llmJudgeBaseUrl);
  const prompt = buildLlmJudgeUserMessage(cfg.llmJudgePromptTemplate, { userText, assistantText });
  const body = {
    model: String(cfg.llmJudgeModel).trim(),
    messages: [{ role: "user", content: prompt }],
    temperature: 0,
    stream: false,
  };
  try {
    const resp = await fetch(url, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${String(cfg.llmJudgeApiKey).trim()}`,
      },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(LLM_JUDGE_TIMEOUT_MS),
    });
    if (!resp.ok) {
      const errText = await resp.text().catch(() => "");
      return { ok: false, reason: "http", status: resp.status, error: errText.slice(0, 200) };
    }
    const j = await resp.json();
    const content = j?.choices?.[0]?.message?.content;
    if (typeof content !== "string") return { ok: false, reason: "bad_response" };
    return { ok: true, content };
  } catch (e) {
    const name = e?.name || "";
    if (name === "TimeoutError" || name === "AbortError") {
      return { ok: false, reason: "timeout", error: `timed out after ${LLM_JUDGE_TIMEOUT_MS}ms` };
    }
    return { ok: false, reason: "network", error: e.message };
  }
}

async function fetchLlmJudge(userText, assistantText, cfgOverride = null) {
  let last = null;
  for (let attempt = 1; attempt <= LLM_JUDGE_MAX_ATTEMPTS; attempt += 1) {
    last = await fetchLlmJudgeOnce(userText, assistantText, cfgOverride);
    if (last.ok) return last;
    const retryable = last.reason === "timeout" || last.reason === "network";
    if (!retryable || attempt >= LLM_JUDGE_MAX_ATTEMPTS) break;
    callLog(`llm-judge retry ${attempt}/${LLM_JUDGE_MAX_ATTEMPTS - 1} after ${last.reason}`);
    await new Promise((r) => setTimeout(r, 800));
  }
  return last;
}

function rememberIdleNudge(convKey, result) {
  const row = { at: Date.now(), ...result };
  lastIdleNudgeResult.set(convKey, row);
  callLog(`llm-judge: ${convKey} → ${result.reason}${result.raw != null ? ` raw=${JSON.stringify(result.raw).slice(0, 80)}` : ""}`);
  return result;
}

function clearIdleNudgeRetry(convKey) {
  const id = idleNudgeRetryTimers.get(convKey);
  if (id) {
    clearTimeout(id);
    idleNudgeRetryTimers.delete(convKey);
  }
}

/** After cooldown, re-judge the last settled turn — turn_ended may have fired mid-cooldown. */
function scheduleIdleNudgeRetry(convKey, delayMs) {
  clearIdleNudgeRetry(convKey);
  const ms = Math.max(500, Math.min(delayMs, 600_000));
  const id = setTimeout(() => {
    idleNudgeRetryTimers.delete(convKey);
    void retryIdleNudge(convKey);
  }, ms);
  idleNudgeRetryTimers.set(convKey, id);
  callLog(`llm-judge retry scheduled: ${convKey} in ${Math.round(ms / 1000)}s`);
}

async function retryIdleNudge(convKey) {
  if (!CFG.enabled) return;
  const cooldownSec = paceIntervalSec();
  if (cooldownSec <= 0) return;
  const bindings = await loadBindings();
  const primary = primaryBindingForConv(bindings, convKey);
  if (!primary?.tabId) return;
  let payload = lastTurnEndedPayload.get(convKey);
  try {
    const snap = await chrome.tabs.sendMessage(primary.tabId, { type: "h2w_snapshot_turn" });
    if (snap?.assistantText) {
      payload = {
        convKey: snap.convKey || convKey,
        userText: snap.userText || "",
        assistantText: snap.assistantText,
        endedAt: snap.endedAt || Date.now(),
        generating: !!snap.generating,
        turnInProgress: !!snap.turnInProgress,
      };
    }
  } catch (e) {
    callLog(`llm-judge retry snapshot failed ${convKey}:`, e.message);
  }
  if (!payload?.assistantText) return;
  if (payload.generating || payload.turnInProgress) {
    scheduleIdleNudgeRetry(convKey, 5000);
    return;
  }
  if (!looksLikeSubstantiveReply(payload.assistantText)) {
    callLog(`llm-judge retry skip: not substantive ${convKey}`);
    return;
  }
  callLog(`llm-judge retry firing: ${convKey}`);
  await maybeIdleNudge(payload);
}

/** Post-turn nudge: LLM judge only (no zero-tools / mid-stop heuristics). */
async function maybeIdleNudge(msg) {
  if (!CFG.enabled) {
    return rememberIdleNudge(msg.convKey || "", { nudged: false, reason: "disabled" });
  }
  const cooldownSec = paceIntervalSec();
  if (cooldownSec <= 0) {
    return rememberIdleNudge(msg.convKey || "", { nudged: false, reason: "disabled" });
  }
  const convKey = msg.convKey;
  if (!convKey) return rememberIdleNudge("", { nudged: false, reason: "no_conv" });
  lastTurnEndedPayload.set(convKey, {
    convKey,
    userText: msg.userText || "",
    assistantText: msg.assistantText || "",
    endedAt: msg.endedAt || Date.now(),
  });
  if (!isLlmJudgeConfigured(CFG)) {
    return rememberIdleNudge(convKey, { nudged: false, reason: "llm_not_configured" });
  }
  const bindings = await loadBindings();
  const primary = primaryBindingForConv(bindings, convKey);
  if (!primary) return rememberIdleNudge(convKey, { nudged: false, reason: "unbound" });
  const b = primary;

  const userText = msg.userText || "";
  let assistantText = msg.assistantText || "";
  if (!String(assistantText).trim()) {
    return rememberIdleNudge(convKey, { nudged: false, reason: "empty_assistant" });
  }

  if (b.tabId) {
    try {
      const snap = await chrome.tabs.sendMessage(b.tabId, { type: "h2w_snapshot_turn" });
      if (snap?.generating || snap?.turnInProgress) {
        scheduleIdleNudgeRetry(convKey, 5000);
        return rememberIdleNudge(convKey, { nudged: false, reason: "still_generating" });
      }
      if (snap?.assistantText) assistantText = snap.assistantText;
      if (!looksLikeSubstantiveReply(assistantText)) {
        return rememberIdleNudge(convKey, { nudged: false, reason: "not_substantive" });
      }
    } catch (e) {
      callLog(`llm-judge live snapshot failed ${convKey}:`, e.message);
      if (!looksLikeSubstantiveReply(assistantText)) {
        return rememberIdleNudge(convKey, { nudged: false, reason: "not_substantive" });
      }
    }
  } else if (!looksLikeSubstantiveReply(assistantText)) {
    return rememberIdleNudge(convKey, { nudged: false, reason: "not_substantive" });
  }

  // Only skip when the *assistant* bubble is our injected continue text (should not happen).
  // Do NOT skip when userText is a prior nudge — that is the normal follow-up turn after we wake.
  if (isIdleNudgeText(assistantText)) {
    return rememberIdleNudge(convKey, { nudged: false, reason: "nudge_loop" });
  }
  const fp = assistantNudgeFingerprint(assistantText);
  if (lastJudgedAssistantFp.get(convKey) === fp) {
    return rememberIdleNudge(convKey, { nudged: false, reason: "same_assistant" });
  }
  const now = Date.now();
  const cooldownMs = cooldownSec * 1000;
  const last = lastIdleNudgeAt.get(convKey) ?? null;
  if (typeof last === "number" && now - last < cooldownMs) {
    scheduleIdleNudgeRetry(convKey, cooldownMs - (now - last) + 500);
    return rememberIdleNudge(convKey, { nudged: false, reason: "cooldown" });
  }

  clearIdleNudgeRetry(convKey);

  const judged = await fetchLlmJudge(userText, assistantText);
  if (!judged.ok) {
    return rememberIdleNudge(convKey, {
      nudged: false,
      reason: `llm_${judged.reason}`,
      error: judged.error || "",
      status: judged.status || null,
    });
  }
  const verdict = interpretLlmJudgeReply(judged.content, {
    skipKeywords: CFG.llmJudgeSkipKeywords || DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
  });
  lastJudgedAssistantFp.set(convKey, fp);
  if (verdict.done) {
    clearIdleNudgeRetry(convKey);
    return rememberIdleNudge(convKey, { nudged: false, reason: "llm_done", raw: verdict.raw });
  }
  if (!verdict.cont || !verdict.nudgeText) {
    return rememberIdleNudge(convKey, { nudged: false, reason: "llm_ambiguous", raw: verdict.raw });
  }

  lastIdleNudgeAt.set(convKey, Date.now());
  setActionBadge("!", "#dc2626", 8000);
  const wakeResult = await routeWake(b, {
    status: "llm_continue_nudge",
    output: `llm-judge continue; raw=${verdict.raw.slice(0, 80)}`,
    working_count: 0,
    pane: b.pane,
    agent: b.agent,
  }, verdict.nudgeText);
  if (!wakeResult?.ok) {
    scheduleIdleNudgeRetry(convKey, cooldownMs);
    return rememberIdleNudge(convKey, {
      nudged: false,
      reason: "wake_failed",
      raw: verdict.raw,
      send: verdict.nudgeText,
      error: wakeResult?.error || wakeResult?.blocked || wakeResult?.reason || "submit-failed",
    });
  }
  return rememberIdleNudge(convKey, {
    nudged: true,
    reason: "llm_continue",
    raw: verdict.raw,
    send: verdict.nudgeText,
  });
}

function paceIntervalSec() {
  const sec = Number(CFG.progressTickSec);
  if (!Number.isFinite(sec) || sec <= 0) return 0;
  return Math.min(Math.floor(sec), 86400);
}

function progressTickSecMs() {
  const ms = paceIntervalSec() * 1000;
  return ms > 0 ? ms : 0;
}

function armProgressTimer(storeKey, bindingSeed = null) {
  const ms = progressTickSecMs();
  if (ms <= 0) return;
  const prev = progressTimers.get(storeKey);
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
  ts.id = setInterval(() => tickProgress(storeKey, ts), ms);
  progressTimers.set(storeKey, ts);
  callLog(`progress tick armed: ${storeKey}, check every ${ms / 1000}s, cooldown ${CFG.progressFallbackSec || 0}s`);
}

function clearProgressTimer(storeKey) {
  const t = progressTimers.get(storeKey);
  if (t) { clearInterval(t.id); progressTimers.delete(storeKey); }
}

async function tickProgress(storeKey, ts) {
  if (ts.inFlight) return;
  const bindings = await loadBindings();
  const b = bindings[storeKey];
  if (!b) { clearProgressTimer(storeKey); return; }
  if (b.status !== "working" || !CFG.enabled) { clearProgressTimer(storeKey); return; }
  if (!shouldProgressTick({ status: b.status, lastTickAt: ts.lastTickAt }, Date.now(), CFG)) return;
  ts.inFlight = true;
  try {
    ts.lastTickAt = Date.now();
    // Prefer SSE agent_output from any pane in this workspace; fall back to /push/state.
    let output = "";
    const ws = normalizeWorkspaceId(b);
    for (const [k, v] of pendingOutputByPane) {
      if (k.startsWith(`${storeKey}::`) && v) { output = v; break; }
    }
    if (!output) {
      try {
        const st = await fetchState();
        const scope = agentsInWorkspace(st.agents || [], ws);
        const working = scope.filter((a) => a.status === "working");
        const pick = working[0] || scope[0];
        output = (pick && (pick.summary || pick.output || pick.status_text)) || "";
        if (pick?.pane) b.pane = pick.pane;
        if (pick?.name) b.agent = pick.name;
      } catch (e) { output = ""; }
    }
    const cur = progressTimers.get(storeKey);
    if (cur !== ts || !CFG.enabled) return;
    const bindingsNow = await loadBindings();
    const curB = bindingsNow[storeKey];
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
      callLog(`progress skip: ws=${ws} (${decision.reason}, out=${String(output).slice(0, 40)})`);
      return;
    }
    callLog(`progress send: ws=${ws} → ${storeKey} (${decision.reason})`);
    await routeWake(curB, {
      status: curB.status,
      output,
      working_count: Object.keys(workingPaneMap(curB)).length,
    }, CFG.progressTemplate || DEFAULT_PROGRESS_TEMPLATE);
    ts.lastSentAt = Date.now();
    ts.lastOutputSent = String(output || "").trim();
    ts.hasProgressSent = true;
    for (const k of [...pendingOutputByPane.keys()]) {
      if (k.startsWith(`${storeKey}::`)) pendingOutputByPane.delete(k);
    }
    curB.lastProgressSentAt = ts.lastSentAt;
    curB.lastProgressOutput = ts.lastOutputSent;
    bindingsNow[storeKey] = curB;
    await saveBindings(bindingsNow);
  } finally {
    ts.inFlight = false;
  }
}

// Reconcile after configuration or stream rebuilds, preserving send baselines.
function reconcileProgressTimers(bindings) {
  if (!CFG.enabled || progressTickSecMs() <= 0) {
    for (const storeKey of [...progressTimers.keys()]) clearProgressTimer(storeKey);
    return;
  }
  for (const storeKey of [...progressTimers.keys()]) {
    const b = bindings[storeKey];
    if (!b || b.status !== "working") clearProgressTimer(storeKey);
  }
  for (const [storeKey, b] of Object.entries(bindings)) {
    if (b.status === "working") armProgressTimer(storeKey, b);
  }
}

async function routeWake(b, extra, template = CFG.wakeTemplate || DEFAULT_TEMPLATE, wakeKind = null) {
  if (!CFG.enabled) return { ok: false, reason: "disabled" };
  const isLlmNudge = extra.status === "llm_continue_nudge";
  const rawText = String(template || "").trim();

  if (isLlmNudge) {
    const payload = {
      type: "h2w_wake",
      data: {
        template: rawText,
        llmNudge: true,
        autoAllow: CFG.autoAllow !== false,
      },
    };
    return deliverWakeToTab(b, payload);
  }

  const ws = normalizeWorkspaceId(b) || "";
  const focusPane = extra.pane ?? b.pane ?? null;
  let roster = extra.roster || "";
  let idle_hint = extra.idle_hint || "";
  let working_count = extra.working_count ?? Object.keys(workingPaneMap(b)).length;
  let workspace_label = b.workspace_label || extra.workspace_label || "";
  if (!roster || !workspace_label) {
    try {
      const st = await fetchState();
      const scope = agentsInWorkspace(st.agents || [], ws);
      const meta = (st.workspaces || []).find((w) => w.id === ws) || null;
      if (!workspace_label) {
        workspace_label = workspaceTitleWithId({
          id: ws,
          label: meta?.label || b.workspace_label,
          roots: meta?.roots,
          agents: scope,
        });
        b.workspace_label = workspace_label;
      }
      const pack = formatWorkspaceRoster(scope, focusPane, {
        id: ws, label: meta?.label || b.workspace_label, roots: meta?.roots,
      });
      roster = pack.roster;
      idle_hint = pack.idle_hint;
      working_count = pack.working_count;
      workspace_label = pack.workspace_label || workspace_label;
    } catch (e) {
      if (!roster) roster = `workspace ${workspace_label || ws} pane roster: (failed to fetch /push/state)`;
      if (!workspace_label) workspace_label = workspaceTitleWithId({ id: ws, label: b.workspace_label });
    }
  }
  const effectiveWakeKind = wakeKind ? reconcileWorkspaceWakeKind(wakeKind, working_count) : null;
  const effectiveTemplate = effectiveWakeKind === "partial"
    ? DEFAULT_PARTIAL_TEMPLATE
    : effectiveWakeKind === "round"
      ? (CFG.wakeTemplate || DEFAULT_TEMPLATE)
      : template;
  let rendered = buildWakeTemplate(effectiveTemplate, {
    agent: extra.agent ?? b.focus_agent ?? b.agent,
    pane: focusPane,
    status: extra.status,
    output: extra.output,
    workspace: ws,
    workspace_label,
    working_count,
    roster,
    idle_hint,
  });
  // Append roster and idle_hint when custom templates omit their placeholders.
  if (roster && !String(effectiveTemplate || "").includes("{roster}")) {
    rendered = `${rendered}\n\n${roster}`.trim();
  }
  if (idle_hint && !String(effectiveTemplate || "").includes("{idle_hint}") && !rendered.includes(idle_hint)) {
    rendered = `${rendered}\n\n${idle_hint}`.trim();
  }
  const payload = {
    type: "h2w_wake",
    data: {
      agent: extra.agent ?? b.focus_agent ?? b.agent,
      pane: focusPane,
      workspace: ws,
      workspace_label,
      status: extra.status,
      output: (extra.output || "").slice(0, 4000),
      roster: roster.slice(0, 6000),
      idle_hint,
      working_count,
      template: rendered,
      autoAllow: CFG.autoAllow !== false,
    },
  };

  const result = await deliverWakeToTab(b, payload);
  return {
    ...(result || {}),
    h2w_wake_kind: effectiveWakeKind,
    working_count,
  };
}

async function deliverWakeToTab(b, payload) {
  // 1) Send directly to the latest registered tabId.
  if (b.tabId) {
    try {
      const result = await chrome.tabs.sendMessage(b.tabId, payload);
      callLog(`wake sent to tab ${b.tabId}`, JSON.stringify(result || {}));
      return result || { ok: true };
    } catch (e) { /* Stale tab; recover by URL. */ }
  }
  // 2) Recover by conversation URL after page refresh or browser restart.
  try {
    const url = new URL(b.convKey);
    const glob = `${url.origin}${url.pathname}*`;
    const tabs = await chrome.tabs.query({ url: glob });
    for (const t of tabs) {
      try {
        const result = await chrome.tabs.sendMessage(t.id, payload);
        b.tabId = t.id;
        b.tabUrl = t.url;
        const bindings = await loadBindings();
        const sk = bindingStoreKeyFromBinding(b);
        if (sk && bindings[sk]) bindings[sk].tabId = t.id;
        await saveBindings(bindings);
        callLog(`wake sent to recovered tab ${t.id} (${t.url})`, JSON.stringify(result || {}));
        return result || { ok: true };
      } catch (e) { /* No content script in this tab; try the next match. */ }
    }
    callLog(`no reachable tab for convKey=${b.convKey}; retaining binding for registration recovery`);
    return { ok: false, reason: "no_tab" };
  } catch (e) {
    callLog("route recovery failed:", e.message);
    return { ok: false, reason: "route_failed", error: e.message };
  }
}

// ---- ChatGPT Project conversation handoff ----
async function commitHandoffTransfer(transferId, targetConvKey, targetTabId, targetUrl = null) {
  const transfers = await loadHandoffTransfers();
  const transfer = transfers[transferId];
  if (!transfer) return { ok: false, error: "handoff_not_found" };
  const targetInfo = chatGptConversationInfo(targetConvKey);
  if (!targetInfo?.project_id || targetInfo.project_id !== transfer.project_id) {
    await markTransfer(transferId, { status: "seed_uncertain", error: "target_project_mismatch" });
    return { ok: false, error: "target_project_mismatch" };
  }
  if (targetInfo.convKey === transfer.source_conv_key) {
    await markTransfer(transferId, { status: "seed_uncertain", error: "target_is_source" });
    return { ok: false, error: "target_is_source" };
  }

  const bindings = await loadBindings();
  const expected = transfer.source_bindings || [];
  const expectedIds = expected.map((b) => b.workspace_id);
  const source = bindingsForConv(bindings, transfer.source_conv_key);
  const target = bindingsForConv(bindings, targetInfo.convKey);

  // Idempotent recovery: binding storage may have committed before the transfer
  // record was updated. In that case, only finalize metadata.
  if (!source.length
    && sameWorkspaceSet(target.map((b) => b.workspace_id || normalizeWorkspaceId(b)), expectedIds)
    && target.every((b) => b.continuity_id === transfer.continuity_id)) {
    const done = await markTransfer(transferId, {
      status: "committed",
      target_conv_key: targetInfo.convKey,
      target_tab_id: targetTabId || transfer.target_tab_id || null,
      target_url: targetUrl || targetInfo.url || null,
      error: null,
      handoff_text: null,
    });
    return { ok: true, recovered: true, transfer: handoffView(done) };
  }

  if (!sameWorkspaceSet(source.map((b) => b.workspace_id || normalizeWorkspaceId(b)), expectedIds)) {
    await markTransfer(transferId, { status: "failed", error: "source_binding_set_changed" });
    return { ok: false, error: "source_binding_set_changed" };
  }
  for (const snapshot of expected) {
    const current = source.find((b) => (b.workspace_id || normalizeWorkspaceId(b)) === snapshot.workspace_id);
    if (!current || (current.revision || bindingRevision(current)) !== snapshot.revision) {
      await markTransfer(transferId, { status: "failed", error: "source_binding_revision_changed" });
      return { ok: false, error: "source_binding_revision_changed" };
    }
  }
  if (target.length) {
    await markTransfer(transferId, { status: "failed", error: "target_already_bound" });
    return { ok: false, error: "target_already_bound" };
  }

  const now = Date.now();
  for (const current of source) {
    const ws = current.workspace_id || normalizeWorkspaceId(current);
    const oldKey = current.storeKey || bindingStoreKey(transfer.source_conv_key, ws);
    const next = {
      ...current,
      convKey: targetInfo.convKey,
      tabId: targetTabId || null,
      tabUrl: targetUrl || targetInfo.url || null,
      continuity_id: transfer.continuity_id,
      persistence: "explicit",
      last_seen_at: now,
      handoff_from: transfer.source_conv_key,
      handoff_at: now,
    };
    delete next.storeKey;
    delete next.expires_at;
    next.revision = bindingRevision(next);
    bindings[bindingStoreKey(targetInfo.convKey, ws)] = next;
    delete bindings[oldKey];
    clearProgressTimer(oldKey);
    if (next.status === "working") armProgressTimer(bindingStoreKey(targetInfo.convKey, ws), next);
  }

  // Binding cutover is the authoritative commit point. Persist it before marking
  // the transfer committed so a crash can be recovered idempotently above.
  try {
    await chrome.storage.local.set({ herdrWakeBindings: bindings });
  } catch (e) {
    await markTransfer(transferId, { status: "seed_uncertain", error: `binding_commit_failed:${e.message}` });
    return { ok: false, error: "binding_commit_failed" };
  }
  ensurePushStream(bindings);
  const done = await markTransfer(transferId, {
    status: "committed",
    target_conv_key: targetInfo.convKey,
    target_tab_id: targetTabId || transfer.target_tab_id || null,
    target_url: targetUrl || targetInfo.url || null,
    error: null,
    packet_chars: String(transfer.handoff_text || "").length,
    handoff_text: null,
  });
  try {
    if (transfer.source_tab_id) chrome.tabs.sendMessage(transfer.source_tab_id, { type: "h2w_handoff_moved", transferId, targetConvKey: targetInfo.convKey });
  } catch (_) {}
  try {
    if (targetTabId) chrome.tabs.sendMessage(targetTabId, { type: "h2w_handoff_committed", transferId, sourceConvKey: transfer.source_conv_key });
  } catch (_) {}
  return { ok: true, transfer: handoffView(done) };
}

async function seedHandoffIntoTarget(transferId, targetTabId) {
  const transfers = await loadHandoffTransfers();
  const transfer = transfers[transferId];
  if (!transfer?.handoff_text) return { ok: false, error: "handoff_packet_missing" };
  const seed = buildHandoffSeed({ transferId, packet: transfer.handoff_text });
  await markTransfer(transferId, {
    status: "seed_submitting",
    target_tab_id: targetTabId,
    error: null,
  });
  let result;
  try {
    result = await sendChatGptTabMessage(targetTabId, {
      type: "h2w_handoff_seed",
      transferId,
      template: seed,
    });
  } catch (e) {
    await markTransfer(transferId, { status: "seed_uncertain", error: `seed_delivery_unknown:${e.message}` });
    return { ok: false, error: "seed_delivery_unknown" };
  }
  if (!result?.ok) {
    // Content-side failures are pre-submit evidence (busy composer, missing input,
    // etc.). The source binding therefore remains authoritative and a fresh
    // explicit retry is safe.
    await markTransfer(transferId, { status: "failed", error: result?.error || result?.blocked || "seed_not_submitted" });
    return { ok: false, error: result?.error || result?.blocked || "seed_not_submitted" };
  }
  if (!result?.targetConvKey || !result?.seedConfirmed) {
    await markTransfer(transferId, {
      status: "seed_uncertain",
      target_conv_key: result?.targetConvKey || null,
      error: "seed_submit_unconfirmed",
    });
    return { ok: false, error: "seed_submit_unconfirmed" };
  }
  return commitHandoffTransfer(transferId, result.targetConvKey, targetTabId, result.targetUrl || null);
}

async function launchHandoffTarget(transferId) {
  const transfers = await loadHandoffTransfers();
  const transfer = transfers[transferId];
  if (!transfer?.handoff_text || !transfer.project_launch_url) return { ok: false, error: "handoff_not_ready" };
  await markTransfer(transferId, { status: "target_opening", error: null });
  let tab;
  try {
    tab = await chrome.tabs.create({ url: transfer.project_launch_url, active: true });
  } catch (e) {
    await markTransfer(transferId, { status: "failed", error: `target_open_failed:${e.message}` });
    return { ok: false, error: "target_open_failed" };
  }
  if (!tab?.id) {
    await markTransfer(transferId, { status: "failed", error: "target_tab_missing" });
    return { ok: false, error: "target_tab_missing" };
  }
  await markTransfer(transferId, { status: "target_opening", target_tab_id: tab.id });
  const ready = await waitForTabComplete(tab.id, 20000);
  if (!ready) {
    await markTransfer(transferId, { status: "failed", error: "target_load_failed" });
    return { ok: false, error: "target_load_failed" };
  }
  await sleep(350);
  return seedHandoffIntoTarget(transferId, tab.id);
}

async function resumeUncertainHandoff(transfer) {
  if (!transfer?.target_tab_id) {
    await markTransfer(transfer.id, { status: "summary_ready", error: null });
    return launchHandoffTarget(transfer.id);
  }
  if (!(await tabStillExists(transfer.target_tab_id))) {
    await markTransfer(transfer.id, {
      status: "summary_ready",
      target_tab_id: null,
      target_conv_key: null,
      target_url: null,
      error: null,
    });
    return launchHandoffTarget(transfer.id);
  }
  let probe = null;
  try {
    probe = await sendChatGptTabMessage(transfer.target_tab_id, {
      type: "h2w_handoff_probe",
      transferId: transfer.id,
    });
  } catch (_) {}
  if (probe?.seedConfirmed && probe?.targetConvKey) {
    return commitHandoffTransfer(transfer.id, probe.targetConvKey, transfer.target_tab_id, probe.targetUrl || null);
  }
  // The user explicitly asked to resume and the target page does not show our
  // transfer marker, so retrying the seed is an intentional operation rather
  // than a blind automatic replay.
  await markTransfer(transfer.id, { status: "summary_ready", error: null });
  return seedHandoffIntoTarget(transfer.id, transfer.target_tab_id);
}

async function resumeSummaryRequested(transfer) {
  const tabId = transfer?.source_tab_id;
  if (!tabId || !(await tabStillExists(tabId))) {
    const failed = await markTransfer(transfer.id, { status: "failed", error: "source_tab_missing" });
    return { ok: false, error: "source_tab_missing", handoff: handoffView(failed) };
  }

  let snapshot = null;
  try { snapshot = await sendChatGptTabMessage(tabId, { type: "h2w_snapshot_turn" }); } catch (_) {}
  const recoveredPacket = extractHandoffPacket(snapshot?.assistantText, transfer.id);
  if (recoveredPacket) {
    const ready = await markTransfer(transfer.id, {
      status: "summary_ready",
      handoff_text: recoveredPacket,
      error: null,
    });
    setTimeout(() => { void launchHandoffTarget(transfer.id); }, 0);
    return { ok: true, pending: true, recovered: true, handoff: handoffView(ready) };
  }
  if (snapshot?.turnInProgress || snapshot?.generating) {
    return { ok: true, pending: true, handoff: handoffView(transfer) };
  }

  const bindings = await loadBindings();
  const source = bindingsForConv(bindings, transfer.source_conv_key);
  const expected = transfer.source_bindings || [];
  if (!sameWorkspaceSet(
    source.map((b) => b.workspace_id || normalizeWorkspaceId(b)),
    expected.map((b) => b.workspace_id),
  )) {
    const failed = await markTransfer(transfer.id, { status: "failed", error: "source_binding_set_changed" });
    return { ok: false, error: "source_binding_set_changed", handoff: handoffView(failed) };
  }

  const prompt = buildHandoffRequest({ transferId: transfer.id, bindings: source });
  const retried = await markTransfer(transfer.id, { status: "summary_requested", error: null });
  try {
    const result = await sendChatGptTabMessage(tabId, {
      type: "h2w_handoff_prompt",
      transferId: transfer.id,
      template: prompt,
    });
    if (!result?.ok) {
      const failed = await markTransfer(transfer.id, {
        status: "failed",
        error: result?.error || result?.blocked || "summary_prompt_not_submitted",
      });
      return { ok: false, error: failed.error, handoff: handoffView(failed) };
    }
  } catch (e) {
    const failed = await markTransfer(transfer.id, { status: "failed", error: `summary_prompt_failed:${e.message}` });
    return { ok: false, error: "summary_prompt_failed", handoff: handoffView(failed) };
  }
  return { ok: true, pending: true, handoff: handoffView(retried) };
}

async function startHandoffForTab(tabId) {
  const convInfo = await conversationInfoForTab(tabId);
  if (!convInfo?.project_id || !convInfo?.project_launch_url) {
    return { ok: false, error: "project_conversation_required" };
  }
  const bindings = await loadBindings();
  const session = bindingsForConv(bindings, convInfo.convKey);
  if (!session.length) return { ok: false, error: "binding_required" };

  // Roll over only at a quiescent boundary. Moving the wake destination while
  // a bound workspace is actively working risks racing an agent-settled wake
  // against the summary/seed messages. Prefer fresh localhost state, with the
  // persisted binding state as a conservative fallback.
  const boundWorkspaceIds = session.map((b) => b.workspace_id || normalizeWorkspaceId(b)).filter(Boolean);
  let working = session.some((b) => b.status === "working" || Object.keys(workingPaneMap(b)).length > 0);
  try {
    const state = await fetchState();
    if (state?.ok && Array.isArray(state.agents)) {
      working = state.agents.some((a) => boundWorkspaceIds.includes(a?.workspace) && a?.status === "working");
    }
  } catch (_) {}
  if (working) return { ok: false, error: "workspace_busy" };

  const transfers = await loadHandoffTransfers();
  const active = activeTransferFromSource(transfers, convInfo.convKey);
  if (active) {
    if (active.status === "seed_uncertain") return resumeUncertainHandoff(active);
    const ageMs = Date.now() - Number(active.updated_at || active.created_at || Date.now());
    if (ageMs >= 120000 && active.status === "summary_requested") return resumeSummaryRequested(active);
    if (ageMs >= 120000 && ["summary_ready", "target_opening"].includes(active.status)) {
      if (active.target_tab_id && await tabStillExists(active.target_tab_id)) {
        return seedHandoffIntoTarget(active.id, active.target_tab_id);
      }
      return launchHandoffTarget(active.id);
    }
    if (ageMs >= 120000 && active.status === "seed_submitting") {
      const uncertain = await markTransfer(active.id, { status: "seed_uncertain", error: "stale_seed_submission" });
      return resumeUncertainHandoff(uncertain);
    }
    return { ok: true, pending: true, handoff: handoffView(active) };
  }

  const chainIds = [...new Set(session.map((b) => b.continuity_id).filter(Boolean))];
  if (chainIds.length > 1) return { ok: false, error: "binding_continuity_conflict" };
  const now = Date.now();
  const transferId = newTransferId(now);
  const continuityId = chainIds[0] || newContinuityId(now);
  const row = {
    version: 1,
    id: transferId,
    continuity_id: continuityId,
    site: "chatgpt",
    status: "summary_requested",
    source_conv_key: convInfo.convKey,
    source_tab_id: tabId,
    project_id: convInfo.project_id,
    project_key: convInfo.project_key,
    project_launch_url: convInfo.project_launch_url,
    source_bindings: transferSourceSnapshot(session),
    handoff_text: null,
    target_tab_id: null,
    target_conv_key: null,
    error: null,
    created_at: now,
    updated_at: now,
  };
  transfers[transferId] = row;
  await saveHandoffTransfers(transfers);
  const prompt = buildHandoffRequest({ transferId, bindings: session });
  let result;
  try {
    result = await sendChatGptTabMessage(tabId, {
      type: "h2w_handoff_prompt",
      transferId,
      template: prompt,
    });
  } catch (e) {
    await markTransfer(transferId, { status: "failed", error: `summary_prompt_failed:${e.message}` });
    return { ok: false, error: "summary_prompt_failed" };
  }
  if (!result?.ok) {
    await markTransfer(transferId, { status: "failed", error: result?.error || result?.blocked || "summary_prompt_not_submitted" });
    return { ok: false, error: result?.error || result?.blocked || "summary_prompt_not_submitted" };
  }
  return { ok: true, pending: true, handoff: handoffView(row) };
}

async function handleHandoffTurnEnded(msg) {
  const convKey = String(msg?.convKey || "").trim();
  if (!convKey) return { handled: false };
  const transfers = await loadHandoffTransfers();
  const candidates = Object.values(transfers)
    .filter((t) => t?.source_conv_key === convKey && t.status === "summary_requested")
    .sort((a, b) => Number(b.created_at || 0) - Number(a.created_at || 0));
  const transfer = candidates[0];
  if (!transfer) return { handled: false };
  const packet = extractHandoffPacket(msg.assistantText, transfer.id);
  if (!packet) {
    const failed = await markTransfer(transfer.id, { status: "failed", error: "handoff_packet_invalid" });
    return { handled: true, ok: false, error: "handoff_packet_invalid", handoff: handoffView(failed) };
  }
  const ready = await markTransfer(transfer.id, {
    status: "summary_ready",
    handoff_text: packet,
    error: null,
  });
  setTimeout(() => { void launchHandoffTarget(transfer.id); }, 0);
  return { handled: true, ok: true, pending: true, handoff: handoffView(ready) };
}

// ---- Message handling ----
chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (msg?.type === "h2w_hello") {
    if (sender.tab?.id) tabVersions.set(sender.tab.id, msg.version || "");
    return;
  }
  if (msg?.type === "h2w_register") {
    void (async () => {
      const bindings = await loadBindings();
      const matched = bindingsForConv(bindings, msg.convKey);
      if (matched.length) {
        for (const entry of matched) {
          const b = bindings[entry.storeKey];
          b.tabId = sender.tab?.id;
          b.tabUrl = msg.url || sender.tab?.url;
          b.last_seen_at = Date.now();
        }
        ensurePushStream(bindings);
        await saveBindings(bindings);
        const first = matched[0];
        sendResponse({
          bound: true,
          workspace_id: first.workspace_id || normalizeWorkspaceId(first),
          workspace_label: first.workspace_label || null,
          pane: first.pane,
          status: first.status || null,
          bindings: matched.map((b) => bindingView(b)),
        });
      } else {
        sendResponse({ bound: false });
      }
    })();
    return true;
  }
  if (msg?.type === "h2w_insert_main") {
    // MAIN-world text insertion: isolated-world execCommand changes the DOM
    // without committing the editor model. Select the last visible match because
    // contenteditable selectors may match several elements and the composer is usually last.
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
          // Select all before insertText so retries replace instead of append.
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
  if (msg?.type === "h2w_open_options") {
    void chrome.runtime.openOptionsPage()
      .then(() => sendResponse({ ok: true }))
      .catch((e) => sendResponse({ ok: false, error: String(e) }));
    return true;
  }
  if (msg?.type === "h2w_page_hud") {
    void (async () => {
      const convKey = String(msg.convKey || "").trim();
      const bindings = await loadBindings();
      const session = convKey ? bindingsForConv(bindings, convKey) : [];
      const transfers = await loadHandoffTransfers();
      const transfer = convKey ? latestTransferForConversation(transfers, convKey) : null;
      const transferView = handoffView(transfer, convKey);
      const convInfo = convKey ? chatGptConversationInfo(convKey) : null;
      const labels = session.map((b) => b.workspace_label || b.workspace_id).filter(Boolean);
      const cachedWorkspaces = cachedPushWorkspaceCatalog();
      // /push/events hello already carries the authoritative workspace list.
      // Render that immediately; /push/state becomes an async freshness probe.
      const state = cachedWorkspaces.length
        ? { ok: true, workspaces: cachedWorkspaces, source: "push_hello_cache", cached_at: pushWorkspaceCatalogAt }
        : await fetchState();
      if (cachedWorkspaces.length) void fetchState();
      let llmHost = "";
      try {
        llmHost = CFG.llmJudgeBaseUrl ? new URL(llmJudgeCompletionsUrl(CFG.llmJudgeBaseUrl)).host : "";
      } catch (_) { llmHost = ""; }
      const last = convKey ? (lastIdleNudgeResult.get(convKey) || null) : null;
      sendResponse({
        ok: true,
        version: H2W_SCRIPT_VERSION,
        enabled: CFG.enabled !== false,
        idleNudgeEnabled: CFG.enabled !== false,
        progressTickSec: paceIntervalSec() || Number(CFG.progressTickSec) || 0,
        progressFallbackSec: Number(CFG.progressFallbackSec) || 0,
        llmConfigured: isLlmJudgeConfigured(CFG),
        llmModel: String(CFG.llmJudgeModel || "").trim(),
        llmHost,
        bound: session.length > 0,
        workspace_label: labels.length ? labels.join(", ") : null,
        workspace_id: session[0]?.workspace_id || (session[0] ? normalizeWorkspaceId(session[0]) : null),
        binding_count: session.length,
        bound_workspace_ids: session.map((b) => b.workspace_id || normalizeWorkspaceId(b)).filter(Boolean),
        bindings: session.map((b) => bindingView(b)),
        continuity_id: session.map((b) => b.continuity_id).find(Boolean) || transfer?.continuity_id || null,
        can_handoff: Boolean(
          convInfo?.project_id
          && session.length > 0
          && (!handoffStatusIsActive(transfer?.status) || transferView?.can_resume === true),
        ),
        handoff: transferView,
        workspaces: state?.ok && Array.isArray(state.workspaces) ? state.workspaces : [],
        workspace_source: state?.source || (state?.ok ? "push_state" : null),
        workspace_status: state?.ok ? 200 : (state?.status || 0),
        workspace_error: state?.ok ? null : (state?.error || (state?.status ? `HTTP ${state.status}` : "fetch-failed")),
        last: last ? {
          at: last.at || null,
          reason: last.reason || "",
          nudged: !!last.nudged,
          raw: last.raw != null ? String(last.raw).slice(0, 80) : "",
          send: last.send != null ? String(last.send).slice(0, 80) : "",
          error: last.error ? String(last.error).slice(0, 80) : "",
        } : null,
      });
    })();
    return true;
  }
  if (msg?.type === "h2w_set_config") {
    const incoming = { ...(msg.config || {}) };
    delete incoming.idleNudgeCooldownSec;
    if (Object.prototype.hasOwnProperty.call(incoming, "idleNudgeEnabled")
      && !Object.prototype.hasOwnProperty.call(incoming, "enabled")) {
      incoming.enabled = incoming.idleNudgeEnabled !== false;
    }
    if (Object.prototype.hasOwnProperty.call(incoming, "enabled")) {
      incoming.enabled = incoming.enabled !== false;
      incoming.idleNudgeEnabled = incoming.enabled;
    }
    CFG = { ...CFG, ...incoming };
    delete CFG.idleNudgeCooldownSec;
    chrome.storage.local.set(CFG).then(async () => {
      try { await chrome.storage.local.remove("idleNudgeCooldownSec"); } catch (e) {}
      void rebuildStreams();
      sendResponse({ ok: true });
    });
    return true;
  }
  if (msg?.type === "h2w_test_llm") {
    void (async () => {
      const form = msg.config || {};
      const judged = await fetchLlmJudge(
        form.userText || "继续验证",
        form.assistantText || "助手已跑完部分验证，下一步还要跑 Convex pytest。",
        form,
      );
      if (!judged.ok) {
        sendResponse({
          ok: false,
          reason: judged.reason,
          status: judged.status || null,
          error: judged.error || "",
        });
        return;
      }
      const verdict = interpretLlmJudgeReply(judged.content, {
        skipKeywords: form.llmJudgeSkipKeywords || CFG.llmJudgeSkipKeywords || DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
      });
      sendResponse({
        ok: true,
        content: judged.content,
        done: verdict.done,
        cont: verdict.cont,
        nudgeText: verdict.nudgeText,
      });
    })();
    return true;
  }
  if (msg?.type === "h2w_state") {
    void (async () => {
      const bindings = await loadBindings();
      // Opening the popup wakes the service worker; restore streams and timers lost to suspension.
      void ensureAlive(bindings);
      let convInfo = null;
      if (msg.tabId) {
        convInfo = await conversationInfoForTab(msg.tabId);
      }
      const binding = convInfo ? primaryBindingForConv(bindings, convInfo.convKey) : null;
      const sessionBindings = convInfo
        ? bindingsForConv(bindings, convInfo.convKey).map((b) => bindingView(b))
        : [];
      const bindingViewOne = binding ? bindingView(binding) : null;
      const idleNudgeLast = convInfo?.convKey
        ? (lastIdleNudgeResult.get(convInfo.convKey) || null)
        : null;
      const transfers = await loadHandoffTransfers();
      const transfer = convInfo?.convKey ? latestTransferForConversation(transfers, convInfo.convKey) : null;
      sendResponse({
        convInfo,
        binding: bindingViewOne,
        sessionBindings,
        idleNudgeLast,
        handoff: handoffView(transfer, convInfo?.convKey || null),
        bindings: Object.entries(bindings).map(([storeKey, b]) => ({
          storeKey,
          convKey: b.convKey,
          workspace_id: b.workspace_id || normalizeWorkspaceId(b),
          workspace_label: b.workspace_label || null,
          pane: b.pane,
          focus_agent: b.focus_agent || b.agent || null,
          status: b.status,
          site: b.site,
          created_at: b.created_at,
          working_count: Object.keys(workingPaneMap(b)).length,
        })),
        config: {
          herdrMcpUrl: CFG.herdrMcpUrl,
          enabled: CFG.enabled,
          tokenSet: !!CFG.token,
          progressTickSec: CFG.progressTickSec,
          progressFallbackSec: CFG.progressFallbackSec,
          idleNudgeEnabled: CFG.enabled !== false,
          llmJudgeConfigured: isLlmJudgeConfigured(CFG),
        },
      });
    })();
    return true;
  }
  if (msg?.type === "h2w_agents") {
    void (async () => {
      // Popup workspace list must prefer a fresh state read. The push hello cache
      // is an optimization for HUD rendering, not the authority for discovery.
      sendResponse(await fetchStateFresh() || { error: "fetch-failed" });
    })();
    return true;
  }
  if (msg?.type === "h2w_bind") {
    void (async () => {
      const bindings = await loadBindings();
      const tabId = msg.tabId || sender.tab?.id;
      if (!tabId) { sendResponse({ ok: false, error: "tab-unavailable" }); return; }
      const convInfo = await conversationInfoForTab(tabId);
      if (!convInfo?.convKey) { sendResponse({ ok: false, error: "conversation-unavailable" }); return; }
      const workspace_id = msg.workspace_id
        || (typeof msg.pane === "string" && msg.pane.includes(":") ? msg.pane.split(":")[0] : null);
      if (!workspace_id) { sendResponse({ ok: false, error: "workspace_required" }); return; }
      const storeKey = bindingStoreKey(convInfo.convKey, workspace_id);
      if (bindings[storeKey]) { sendResponse({ ok: false, error: "already-bound", convKey: convInfo.convKey, workspace_id }); return; }
      const workspace_label = msg.workspace_label
        || workspaceTitleWithId({ id: workspace_id, label: msg.workspace_label_raw, roots: msg.roots });
      const continuity_id = bindingsForConv(bindings, convInfo.convKey).map((x) => x.continuity_id).find(Boolean)
        || newContinuityId();
      const b = {
        workspace_id,
        workspace_label,
        pane: msg.pane || null,
        focus_agent: msg.agent || null,
        agent: null, // The binding targets a workspace, not an individual agent.
        workingPanes: {},
        convKey: convInfo.convKey,
        site: convInfo.site || "unknown",
        tabId, tabUrl: convInfo.url || null,
        created_at: Date.now(),
        last_seen_at: Date.now(),
        persistence: "explicit",
        continuity_id,
        status: "unknown",
        lastSettle: null,
      };
      b.revision = bindingRevision(b);
      bindings[storeKey] = b;
      await saveBindings(bindings);
      ensurePushStream(bindings);
      try { void chrome.tabs.sendMessage(tabId, { type: "h2w_bound", pane: workspace_label, workspace_id, workspace_label }).catch(() => {}); } catch (e) {}
      sendResponse({ ok: true, convKey: convInfo.convKey, workspace_id, workspace_label });
    })();
    return true;
  }
  if (msg?.type === "h2w_unbind") {
    void (async () => {
      const bindings = await loadBindings();
      const convKey = msg.convKey;
      const wsId = msg.workspace_id || null;
      const targets = wsId
        ? [{ storeKey: bindingStoreKey(convKey, wsId) }]
        : bindingsForConv(bindings, convKey).map((b) => ({ storeKey: b.storeKey }));
      if (!targets.length || targets.every((t) => !bindings[t.storeKey])) {
        sendResponse({ ok: false, error: "not-found" });
        return;
      }
      let tabId = null;
      for (const { storeKey } of targets) {
        const b = bindings[storeKey];
        if (!b) continue;
        tabId = b.tabId || tabId;
        delete bindings[storeKey];
        clearProgressTimer(storeKey);
      }
      await saveBindings(bindings);
      if (!bindingsForConv(bindings, convKey).length) {
        clearIdleNudgeRetry(convKey);
        lastTurnEndedPayload.delete(convKey);
      }
      if (tabId) { try { void chrome.tabs.sendMessage(tabId, { type: "h2w_unbound", workspace_id: wsId || null }).catch(() => {}); } catch (e) {} }
      if (!Object.keys(bindings).length) clearActionBadge();
      sendResponse({ ok: true });
    })();
    return true;
  }
  if (msg?.type === "h2w_turn_ended") {
    void (async () => {
      const handoff = await handleHandoffTurnEnded(msg);
      if (handoff.handled) {
        sendResponse(handoff);
        return;
      }
      const r = await maybeIdleNudge(msg);
      sendResponse(r);
    })();
    return true;
  }
  if (msg?.type === "h2w_handoff_start") {
    void (async () => {
      const tabId = msg.tabId || sender.tab?.id;
      if (!tabId) { sendResponse({ ok: false, error: "tab-unavailable" }); return; }
      sendResponse(await startHandoffForTab(tabId));
    })();
    return true;
  }
  if (msg?.type === "h2w_wake_ack") {
    callLog(`wake ack ${msg.convKey}:`, JSON.stringify(msg.result), JSON.stringify(msg.confirm || {}));
    // Toolbar badge gives immediate wake result feedback.
    if (msg.result?.ok) setActionBadge("✓", "#16a34a", 4000);
    else setActionBadge("!", "#dc2626", 8000);
    return;
  }
  sendResponse({});
});

async function fetchStateFresh() {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), STATE_FETCH_MS);
  try {
    const resp = await fetch(`${CFG.herdrMcpUrl.replace(/\/+$/, "")}/push/state`, {
      headers: CFG.token ? { Authorization: `Bearer ${CFG.token}` } : {},
      signal: ctrl.signal,
    });
    if (!resp.ok) return { ok: false, status: resp.status };
    const body = await resp.json();
    if (Array.isArray(body?.workspaces)) cachePushWorkspaceCatalog(body.workspaces);
    return { ok: true, source: "push_state", ...body };
  } catch (e) {
    let loopback_permission = null;
    try {
      loopback_permission = (await navigator.permissions.query({ name: "loopback-network" })).state;
    } catch (_) { /* Chrome <145 or browser without split LNA permissions */ }
    // Chrome can reject loopback access immediately with TypeError("Failed to fetch")
    // instead of waiting for our AbortController deadline. Permission state is
    // authoritative for every network exception, not only AbortError.
    if (loopback_permission && loopback_permission !== "granted") {
      return {
        ok: false,
        error: `loopback_permission_${loopback_permission}`,
        loopback_permission,
      };
    }
    if (e?.name === "AbortError") {
      return {
        ok: false,
        error: "fetch_timeout",
        loopback_permission,
      };
    }
    return { ok: false, error: e.message, loopback_permission };
  } finally {
    clearTimeout(timer);
  }
}

async function fetchState() {
  // HUD, popup, progress and wake reconciliation share one bounded localhost
  // request instead of multiplying sockets when they refresh concurrently.
  if (stateFetchInFlight) return stateFetchInFlight;
  stateFetchInFlight = fetchStateFresh().finally(() => { stateFetchInFlight = null; });
  return stateFetchInFlight;
}

// Full rebuild: exactly one shared stream regardless of binding count.
async function rebuildStreams() {
  await configReady;
  stopPushStream();
  // A configured endpoint may have changed; never show a workspace catalog
  // from the previous server while the new shared stream is reconnecting.
  pushWorkspaceCatalog = [];
  pushWorkspaceCatalogAt = 0;
  const bindings = await loadBindings();
  callLog(
    `rebuild streams v${H2W_SCRIPT_VERSION}: ${Object.keys(bindings).length} binding(s),`,
    `token=${CFG.token ? "set" : "empty"}, enabled=${CFG.enabled}`,
  );
  ensurePushStream(bindings);
  reconcileProgressTimers(bindings); // Re-arm or stop working progress timers.
}

// After service-worker suspension, restore missing in-memory streams and timers
// from storage without aborting live streams or resetting existing clocks.
async function ensureAlive(preloaded) {
  await configReady;
  const bindings = preloaded || await loadBindings();
  ensurePushStream(bindings);
  if (!CFG.enabled || progressTickSecMs() <= 0) return;
  for (const [storeKey, b] of Object.entries(bindings)) {
    if (b.status === "working" && !progressTimers.has(storeKey)) armProgressTimer(storeKey);
  }
}

// ---- Install, browser startup, and every service-worker startup ----
// MV3 can restart the worker without onInstalled/onStartup, so rebuild at module scope.
chrome.runtime.onStartup.addListener(() => { void rebuildStreams(); });
chrome.runtime.onInstalled.addListener(() => {
  void rebuildStreams();
  chrome.storage.local.get(["herdrMcpUrl"], (cfg) => {
    if (!cfg.herdrMcpUrl) chrome.storage.local.set({ herdrMcpUrl: "http://127.0.0.1:8772", token: "", enabled: true, autoAllow: true, wakeTemplate: DEFAULT_TEMPLATE, progressTickSec: 60, progressFallbackSec: 1200, progressTemplate: DEFAULT_PROGRESS_TEMPLATE, idleNudgeEnabled: true });
  });
});
try {
  chrome.action.onClicked.addListener(() => { void chrome.runtime.openOptionsPage(); });
} catch (e) {
  callLog("action click unavailable:", e.message);
}
void rebuildStreams();

// Wake the service worker each minute to restore missing SSE streams and timers.
try {
  chrome.alarms.create("h2w-keepalive", { periodInMinutes: 1 });
  chrome.alarms.onAlarm.addListener((a) => {
    if (a.name === "h2w-keepalive") void ensureAlive();
  });
} catch (e) {
  callLog("alarms unavailable:", e.message);
}
