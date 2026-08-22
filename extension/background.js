// background.js — herdr-to-web wake-up extension backend (MV3 module service worker)
// Responsibilities:
//  1. Store configuration and bindings in chrome.storage.local.
//  2. Maintain one reconnecting SSE stream per workspace binding.
//  3. Turn workspace agent events into partial-progress or round-complete wake-ups.
//  4. Insert text in the MAIN world for contenteditable sites.
//  5. Handle popup and options messages for listing, binding, unbinding, and status.
// Version synchronization: extension reloads do not reinject content scripts into
// open tabs. Scan target tabs after a version change and reload stale scripts.
// Keep H2W_SCRIPT_VERSION here aligned with H2W_CONTENT_VERSION in wake.js.
import {
  decideWorkspaceWake, agentsInWorkspace, formatWorkspaceRoster, workspaceTitleWithId,
  pruneExpired, bindingRevision, buildWakeTemplate, shouldProgressTick, shouldSendProgress,
  isIdleNudgeText, looksLikeSubstantiveReply, isLlmJudgeConfigured, llmJudgeCompletionsUrl, buildLlmJudgeUserMessage, interpretLlmJudgeReply,
  assistantNudgeFingerprint,
  DEFAULT_LLM_JUDGE_PROMPT, DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
} from "./binding-core.js";

const H2W_SCRIPT_VERSION = "0.1.30";
const H2W_TAB_URLS = ["*://chat.z.ai/*", "*://chat.deepseek.com/*", "*://claude.ai/*", "*://chatgpt.com/*"];
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
function migrateBindingsMap(raw) {
  const out = {};
  let migrated = false;
  for (const [k, b] of Object.entries(raw || {})) {
    if (!b || typeof b !== "object") continue;
    const convKey = b.convKey || (k.includes("::") ? parseBindingStoreKey(k).convKey : k);
    const ws = b.workspace_id || normalizeWorkspaceId(b);
    if (!ws) {
      out[k] = { ...b, convKey };
      continue;
    }
    const sk = k.includes("::") ? k : bindingStoreKey(convKey, ws);
    if (sk !== k) migrated = true;
    out[sk] = { ...b, convKey, workspace_id: ws };
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
  // Prune expired bindings, abort their streams, and persist the cleanup.
  const { kept, prunedKeys } = pruneExpired(b);
  if (prunedKeys.length || migrated) {
    if (prunedKeys.length) {
      callLog(`pruned ${prunedKeys.length} expired bindings: ${prunedKeys.join(", ")}`);
      for (const k of prunedKeys) { const s = pushStreams.get(k); if (s) { try { s.ctrl.abort(); } catch {} } pushStreams.delete(k); clearProgressTimer(k); }
    }
    if (migrated) callLog("migrated legacy binding keys to convKey::workspace_id");
    try { await chrome.storage.local.set({ herdrWakeBindings: kept }); } catch (e) {}
  }
  return kept;
}
async function saveBindings(b) {
  try { await chrome.storage.local.set({ herdrWakeBindings: b }); } catch (e) {}
}

// ---- SSE push client (one stream per binding, workspace-scoped by default) ----
const pushStreams = new Map(); // storeKey -> { ctrl, retryTimer }
const pendingOutputByPane = new Map(); // `${storeKey}::${pane}` -> output

function pendingKey(storeKey, pane) {
  return `${storeKey}::${pane || "_"}`;
}

async function ensurePushStream(bindings, storeKey) {
  await configReady;
  if (pushStreams.has(storeKey)) return;
  const b = bindings[storeKey];
  const ws = normalizeWorkspaceId(b);
  if (!b || !ws) return;
  if (!b.workspace_id) b.workspace_id = ws;
  const ctrl = new AbortController();
  pushStreams.set(storeKey, { ctrl });
  void runPushStream(storeKey, ws, ctrl);
}

async function runPushStream(storeKey, workspace, ctrl) {
  const url = `${CFG.herdrMcpUrl.replace(/\/+$/, "")}/push/events?workspace=${encodeURIComponent(workspace)}`;
  let backoff = 2000;
  while (runtimeAlive() && !ctrl.signal.aborted) {
    try {
      const resp = await fetch(url, {
        signal: ctrl.signal,
        headers: CFG.token ? { Authorization: `Bearer ${CFG.token}` } : {},
      });
      if (!resp.ok) {
        callLog(`push ws=${workspace} HTTP ${resp.status}; retrying in ${backoff}ms`);
        await sleep(backoff);
        backoff = Math.min(backoff * 2, 15000);
        continue;
      }
      backoff = 2000;
      if (!resp.body) throw new Error("no-body");
      callLog(`push ws=${workspace} connected`);
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
          handlePushBlock(storeKey, block);
        }
      }
      callLog(`push ws=${workspace} stream ended; reconnecting in ${backoff}ms`);
    } catch (e) {
      if (ctrl.signal.aborted || !runtimeAlive()) break;
      callLog(`push ws=${workspace} disconnected (${e.message}); retrying in ${backoff}ms`);
    }
    if (ctrl.signal.aborted) break;
    await sleep(backoff);
    backoff = Math.min(backoff * 2, 15000);
  }
  pushStreams.delete(storeKey);
}

function handlePushBlock(storeKey, block) {
  let event = null, data = null;
  for (const line of block.split("\n")) {
    if (line.startsWith("event:")) event = line.slice(6).trim();
    else if (line.startsWith("data:")) { try { data = JSON.parse(line.slice(5).trim()); } catch {} }
  }
  if (!event || !data) return;
  if (event === "hello") void onPushHello(storeKey, data);
  else if (event === "agent_working") void onPushWorking(storeKey, data);
  else if (event === "agent_settled") void onPushSettled(storeKey, data);
  else if (event === "agent_output") {
    if (data.pane) pendingOutputByPane.set(pendingKey(storeKey, data.pane), data.output || "");
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
  if (d.status === "working") armProgressTimer(storeKey, b);
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
  armProgressTimer(storeKey, b);
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
  if (d.kind === "partial") {
    callLog(`partial settled: ${data.pane} @ ${ws}, still working=${d.working_count}`);
    await routeWake(b, fields, DEFAULT_PARTIAL_TEMPLATE);
    setActionBadge("…", "#d97706");
  } else {
    callLog(`round settled: ws=${ws} last=${data.pane} → ${data.status}`);
    await routeWake(b, fields, CFG.wakeTemplate || DEFAULT_TEMPLATE);
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
  if (!CFG.enabled || CFG.idleNudgeEnabled === false) return;
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
  if (!CFG.enabled || CFG.idleNudgeEnabled === false) {
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

async function routeWake(b, extra, template = CFG.wakeTemplate || DEFAULT_TEMPLATE) {
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
  let rendered = buildWakeTemplate(template, {
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
  if (roster && !String(template || "").includes("{roster}")) {
    rendered = `${rendered}\n\n${roster}`.trim();
  }
  if (idle_hint && !String(template || "").includes("{idle_hint}") && !rendered.includes(idle_hint)) {
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

  return deliverWakeToTab(b, payload);
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
          ensurePushStream(bindings, entry.storeKey);
        }
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
  if (msg?.type === "h2w_page_hud") {
    void (async () => {
      const convKey = String(msg.convKey || "").trim();
      const bindings = await loadBindings();
      const session = convKey ? bindingsForConv(bindings, convKey) : [];
      const labels = session.map((b) => b.workspace_label || b.workspace_id).filter(Boolean);
      let llmHost = "";
      try {
        llmHost = CFG.llmJudgeBaseUrl ? new URL(llmJudgeCompletionsUrl(CFG.llmJudgeBaseUrl)).host : "";
      } catch (_) { llmHost = ""; }
      const last = convKey ? (lastIdleNudgeResult.get(convKey) || null) : null;
      sendResponse({
        ok: true,
        version: H2W_SCRIPT_VERSION,
        enabled: CFG.enabled !== false,
        idleNudgeEnabled: CFG.idleNudgeEnabled !== false,
        progressTickSec: paceIntervalSec() || Number(CFG.progressTickSec) || 0,
        llmConfigured: isLlmJudgeConfigured(CFG),
        llmModel: String(CFG.llmJudgeModel || "").trim(),
        llmHost,
        bound: session.length > 0,
        workspace_label: labels.length ? labels.join(", ") : null,
        workspace_id: session[0]?.workspace_id || (session[0] ? normalizeWorkspaceId(session[0]) : null),
        binding_count: session.length,
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
        try { convInfo = await chrome.tabs.sendMessage(msg.tabId, { type: "h2w_get_convkey" }); } catch (e) { convInfo = null; }
      }
      const binding = convInfo ? primaryBindingForConv(bindings, convInfo.convKey) : null;
      const sessionBindings = convInfo
        ? bindingsForConv(bindings, convInfo.convKey).map((b) => bindingView(b))
        : [];
      const bindingViewOne = binding ? bindingView(binding) : null;
      const idleNudgeLast = convInfo?.convKey
        ? (lastIdleNudgeResult.get(convInfo.convKey) || null)
        : null;
      sendResponse({
        convInfo,
        binding: bindingViewOne,
        sessionBindings,
        idleNudgeLast,
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
          idleNudgeEnabled: CFG.idleNudgeEnabled !== false,
          llmJudgeConfigured: isLlmJudgeConfigured(CFG),
        },
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
      const workspace_id = msg.workspace_id
        || (typeof msg.pane === "string" && msg.pane.includes(":") ? msg.pane.split(":")[0] : null);
      if (!workspace_id) { sendResponse({ ok: false, error: "workspace_required" }); return; }
      const storeKey = bindingStoreKey(convInfo.convKey, workspace_id);
      if (bindings[storeKey]) { sendResponse({ ok: false, error: "already-bound", convKey: convInfo.convKey, workspace_id }); return; }
      const workspace_label = msg.workspace_label
        || workspaceTitleWithId({ id: workspace_id, label: msg.workspace_label_raw, roots: msg.roots });
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
        expires_at: Date.now() + 86400000,
        status: "unknown",
        lastSettle: null,
      };
      b.revision = bindingRevision(b);
      bindings[storeKey] = b;
      await saveBindings(bindings);
      ensurePushStream(bindings, storeKey);
      try { chrome.tabs.sendMessage(tabId, { type: "h2w_bound", pane: workspace_label, workspace_id, workspace_label }); } catch (e) {}
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
        const stream = pushStreams.get(storeKey);
        if (stream) { stream.ctrl.abort(); pushStreams.delete(storeKey); }
      }
      await saveBindings(bindings);
      if (!bindingsForConv(bindings, convKey).length) {
        clearIdleNudgeRetry(convKey);
        lastTurnEndedPayload.delete(convKey);
      }
      if (tabId) { try { chrome.tabs.sendMessage(tabId, { type: "h2w_unbound", workspace_id: wsId || null }); } catch (e) {} }
      if (!Object.keys(bindings).length) clearActionBadge();
      sendResponse({ ok: true });
    })();
    return true;
  }
  if (msg?.type === "h2w_turn_ended") {
    void (async () => {
      const r = await maybeIdleNudge(msg);
      sendResponse(r);
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

// Full rebuild: abort existing streams before recreating them from persisted bindings.
async function rebuildStreams() {
  await configReady;
  for (const [storeKey, stream] of pushStreams) {
    try { stream.ctrl.abort(); } catch (e) {}
    pushStreams.delete(storeKey);
  }
  const bindings = await loadBindings();
  callLog(
    `rebuild streams v${H2W_SCRIPT_VERSION}: ${Object.keys(bindings).length} binding(s),`,
    `token=${CFG.token ? "set" : "empty"}, enabled=${CFG.enabled}`,
  );
  for (const storeKey of Object.keys(bindings)) {
    ensurePushStream(bindings, storeKey);
  }
  reconcileProgressTimers(bindings); // Re-arm or stop working progress timers.
}

// After service-worker suspension, restore missing in-memory streams and timers
// from storage without aborting live streams or resetting existing clocks.
async function ensureAlive(preloaded) {
  await configReady;
  const bindings = preloaded || await loadBindings();
  for (const storeKey of Object.keys(bindings)) {
    ensurePushStream(bindings, storeKey);
  }
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
