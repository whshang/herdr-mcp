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
  assistantNudgeFingerprint, assistantDeclaresPendingWork,
  DEFAULT_LLM_JUDGE_PROMPT, LEGACY_DEFAULT_LLM_JUDGE_PROMPT, DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
  conversationInfoFromSupportedUrl,
} from "./binding-core.js";
import {
  buildHandoffRequest, buildHandoffSeed, chatGptConversationInfo,
  extractHandoffPacket, handoffSeedContainsTransfer, handoffStatusIsActive,
  newContinuityId, newTransferId,
} from "./continuity-core.js";
import { detectOrLoadLocale, getLocale, setLocale, t as i18nText } from "./i18n.js";
import { callMcpJsonRpc } from "./mcp-json-rpc.js";
import { localHerdrFetch, openLocalHerdrStream, resetLocalAuth } from "./local-auth.js";

const H2W_SCRIPT_VERSION = "0.1.54";
const H2W_TAB_URLS = ["*://chat.z.ai/*", "*://chat.deepseek.com/*", "*://claude.ai/*", "*://chatgpt.com/*"];
const CHATGPT_CONTENT_SCRIPT_FILES = [
  "content/base.js",
  "content/injector/chatgpt.js",
  "context-pressure.js",
  "conversation-health.js",
  "recovery-controller.js",
  "content/hud/state-view.js",
  "content/hud/tooltip.js",
  "content/hud/renderer.js",
  "content/hud/hud.js",
  "content/wake.js",
];
const PUSH_CONNECT_MS = 5000;
const STATE_FETCH_MS = 4000;
const TAB_RECOVERY_COOLDOWN_MS = 30000;
const HANDOFF_STORAGE_KEY = "herdrConversationTransfers";
const PROJECT_AUTOMATION_STORAGE_KEY = "herdrProjectAutomation";
const CONVERSATION_AUTOMATION_STORAGE_KEY = "herdrConversationAutomation";
const AUTOMATION_MODE_MANUAL = "manual";
const AUTOMATION_MODE_PROJECT = "project_auto";
const HANDOFF_RETENTION_MS = 7 * 86400000;
const tabVersions = new Map();
const reloadedTabs = new Set();
const tabRecoveryAttemptAt = new Map();
const FALLBACK_TEMPLATE =
  "herdr workspace {workspace_label}: agents stopped (focus {agent} @ {pane} → {status}).\n\nFocus pane output:\n{output}\n\n{roster}\n\n{idle_hint}\n\nContinue orchestration from these results; prefer fs/exec over expensive models.";
const FALLBACK_PROGRESS_TEMPLATE =
  "herdr workspace {workspace_label} progress (focus {agent} @ {pane} · {status}; {working_count} still working in this space).\n\nFocus pane output:\n{output}\n\n{roster}\n\n{idle_hint}\n\nUse herdr_since / inspect to continue; keep orchestrating on the web.";
const FALLBACK_PARTIAL_TEMPLATE =
  "herdr workspace {workspace_label}: focus {agent} @ {pane} stopped ({status}); {working_count} still working in this space.\n\nFocus pane output:\n{output}\n\n{roster}\n\n{idle_hint}\n\nThis is a partial finish, not a full round settle. Keep watching or schedule the remaining workers.";

// ---- Browser JSON bridge (z.ai / DeepSeek without MCP Connector) ----
// The page only receives tool schemas and results. The bearer token stays inside
// the extension service worker.
async function jsonBridgeRpc(method, params = {}) {
  return callMcpJsonRpc({
    baseUrl: CFG.herdrMcpUrl,
    method,
    params,
    fetchFn: (url, init) => localHerdrFetch(url, { ...init, nativeTimeoutMs: 90_000 }),
  });
}

function localizedText(key, vars = null, fallback = "") {
  const value = i18nText(key, vars || undefined);
  return value === key ? fallback : value;
}

function defaultWakeTemplate() {
  return localizedText("default_wake_template", null, FALLBACK_TEMPLATE);
}

function defaultProgressTemplate() {
  return localizedText("default_progress_template", null, FALLBACK_PROGRESS_TEMPLATE);
}

function defaultPartialTemplate() {
  return localizedText("default_partial_template", null, FALLBACK_PARTIAL_TEMPLATE);
}

function hudLabels() {
  const keys = [
    "automation", "automation_on", "automation_off", "manual_continue", "manual_status", "manual_judge", "manual_handoff",
    "manual_continue_hint", "manual_status_hint", "manual_judge_hint", "manual_handoff_hint", "controls", "advanced_options", "event_settings",
    "handoff_started", "handoff_failed", "handoff_binding_required", "handoff_workspace_busy", "handoff_automation_enabled",
    "automation_on_hint", "automation_off_hint", "conversation_automation_on_hint", "conversation_automation_off_hint", "on", "off", "interval", "fallback", "bindings", "bind", "unbind", "available",
    "no_workspaces", "workspaces_unavailable", "active", "bound_count", "aria_toggle_automation",
    "aria_open_controls", "automation_enabled", "automation_disabled", "automation_update_failed",
    "timing_saved", "timing_save_failed", "bound_to", "unbound_from", "binding_failed", "judge_no_continue",
    "continue_sent", "continue_failed", "tip_workspace", "tip_agent", "tip_conversation", "tip_state",
    "tip_recovery", "tip_last_event", "none",
    "reason_disabled", "reason_no_conv", "reason_llm_not_configured", "reason_unbound",
    "reason_still_generating", "reason_not_substantive", "reason_empty_assistant", "reason_nudge_loop",
    "reason_same_assistant", "reason_cooldown", "reason_llm_done", "reason_llm_ambiguous",
    "reason_llm_continue", "reason_llm_timeout", "reason_llm_http", "reason_llm_network",
    "reason_wake_failed", "reason_llm_bad_response",
  ];
  const out = {};
  for (const suffix of keys) out[suffix] = localizedText(`hud_${suffix}`);
  for (const key of ["handoff_resume", "handoff_compressing", "handoff_moving", "handoff_project_only"]) {
    out[key] = localizedText(key);
  }
  out.states = {};
  for (const state of [
    "unknown", "ready", "bound", "unbound", "working", "idle", "done", "blocked", "reply_waiting",
    "reply_suspect", "recovery_message_sent", "reload_pending", "recovering", "rollover_recommended",
    "rollover_required", "context_warning", "handoff_prepare", "high_risk",
  ]) out.states[state] = localizedText(`hud_state_${state}`);
  out.recovery_probe_template = localizedText("recovery_probe_template");
  out.stale_view_activation_template = localizedText("stale_view_activation_template");
  return out;
}

function callLog(...args) { console.log("[h2w]", ...args); }
function runtimeAlive() { try { return !!chrome.runtime?.id; } catch { return false; } }
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// ---- Configuration (wait for storage before startup or stream rebuild) ----
let CFG = {
  herdrMcpUrl: "http://127.0.0.1:8772", automationMode: AUTOMATION_MODE_MANUAL,
  // Legacy fields stay fail-closed for older content scripts. New code derives
  // automation from global mode + the current ChatGPT Project id.
  enabled: false,
  wakeTemplate: "", progressTickSec: 60, progressFallbackSec: 1200,
  progressTemplate: "",
  idleNudgeEnabled: true,
  // Post-turn LLM judge (OpenAI-compatible). Defaults empty — fill in Options.
  llmJudgeBaseUrl: "",
  llmJudgeApiKey: "",
  llmJudgeModel: "",
  llmJudgePromptTemplate: "",
  llmJudgeSkipKeywords: DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
};
let PROJECT_AUTOMATION = {};
let CONVERSATION_AUTOMATION = {};

function normalizeAutomationMode(value) {
  return value === AUTOMATION_MODE_PROJECT ? AUTOMATION_MODE_PROJECT : AUTOMATION_MODE_MANUAL;
}

function sanitizeProjectAutomation(raw) {
  const out = {};
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return out;
  for (const [projectId, enabled] of Object.entries(raw)) {
    if (enabled === true && /^g-p-[0-9a-f]{32}$/i.test(projectId)) out[projectId] = true;
  }
  return out;
}

function isJsonBridgeConversation(convKey) {
  try {
    const url = new URL(String(convKey || ""));
    return url.origin === "https://chat.z.ai" || url.origin === "https://chat.deepseek.com";
  } catch (_) {
    return false;
  }
}

function conversationAutomationSiteForConversation(convKey) {
  const chatgpt = chatGptConversationInfo(convKey);
  if (chatgpt?.site === "chatgpt" && !chatgpt.project_id && chatgpt.conversation_id) return "chatgpt";
  return jsonBridgeSiteForConversation(convKey);
}

function isConversationAutomationConversation(convKey) {
  return Boolean(conversationAutomationSiteForConversation(convKey));
}

function jsonBridgeSiteForConversation(convKey) {
  try {
    const origin = new URL(String(convKey || "")).origin;
    if (origin === "https://chat.z.ai") return "z.ai";
    if (origin === "https://chat.deepseek.com") return "deepseek";
  } catch (_) {}
  return null;
}

function zAiConversationInfo(rawUrl) {
  try {
    const url = new URL(String(rawUrl || ""));
    if (url.origin !== "https://chat.z.ai") return null;
    const pathname = url.pathname.replace(/\/+$/, "") || "";
    const match = pathname.match(/^\/c\/([^/]+)$/);
    return {
      site: "z.ai",
      url: url.href,
      convKey: `${url.origin}${pathname}`,
      conversation_id: match?.[1] || null,
      project_id: null,
      project_key: null,
      project_launch_url: null,
      handoff_launch_url: `${url.origin}/`,
      manual_handoff_available: Boolean(match?.[1]),
      is_new_chat_root: pathname === "",
    };
  } catch (_) {
    return null;
  }
}

function handoffConversationInfo(rawUrl, siteHint = null) {
  const chatgpt = chatGptConversationInfo(rawUrl);
  if (chatgpt) {
    return {
      ...chatgpt,
      handoff_launch_url: chatgpt.project_launch_url,
      manual_handoff_available: Boolean(chatgpt.project_id),
    };
  }
  if (!siteHint || siteHint === "z.ai") return zAiConversationInfo(rawUrl);
  return null;
}

function validateJsonBridgeSender(msg, sender) {
  const convKey = String(msg?.convKey || "").trim();
  const site = String(msg?.site || "").trim();
  const expectedSite = jsonBridgeSiteForConversation(convKey);
  if (!expectedSite || site !== expectedSite) return { ok: false, error: "json-bridge-site-mismatch" };
  try {
    const senderOrigin = new URL(String(sender?.tab?.url || sender?.url || "")).origin;
    const expectedOrigin = new URL(convKey).origin;
    if (senderOrigin !== expectedOrigin) return { ok: false, error: "json-bridge-sender-mismatch" };
  } catch (_) {
    return { ok: false, error: "json-bridge-sender-mismatch" };
  }
  return { ok: true, convKey, site };
}

async function authorizeConversationAutomation(msg, sender) {
  const convKey = String(msg?.convKey || "").trim();
  const expectedSite = conversationAutomationSiteForConversation(convKey);
  const site = String(msg?.site || "").trim();
  if (!expectedSite || site !== expectedSite) {
    return { ok: false, error: "conversation-automation-site-mismatch" };
  }
  try {
    const senderUrl = String(sender?.tab?.url || sender?.url || "");
    if (expectedSite === "chatgpt") {
      const senderInfo = chatGptConversationInfo(senderUrl);
      if (!senderInfo || senderInfo.project_id || senderInfo.convKey !== convKey) {
        return { ok: false, error: "conversation-automation-sender-mismatch" };
      }
    } else if (new URL(senderUrl).origin !== new URL(convKey).origin) {
      return { ok: false, error: "conversation-automation-sender-mismatch" };
    }
  } catch (_) {
    return { ok: false, error: "conversation-automation-sender-mismatch" };
  }
  return { ok: true, convKey, site };
}

function sanitizeConversationAutomation(raw) {
  const out = {};
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return out;
  for (const [convKey, enabled] of Object.entries(raw)) {
    if (enabled === true && isConversationAutomationConversation(convKey)) out[convKey] = true;
  }
  return out;
}

function automationScopeForConversation(convKey) {
  const info = chatGptConversationInfo(convKey);
  const projectId = info?.project_id || null;
  const globalMode = normalizeAutomationMode(CFG.automationMode);
  const projectMode = globalMode === AUTOMATION_MODE_PROJECT;
  const projectAutomationAvailable = projectMode && Boolean(projectId);
  const projectEnabled = projectAutomationAvailable && PROJECT_AUTOMATION[projectId] === true;
  // Conversation-scoped automation is independent from the global ChatGPT
  // Project gate. Plain ChatGPT /c/<id>, z.ai and DeepSeek can always opt in
  // from their own HUD. The global mode only gates Project-shared automation.
  const conversationAutomationAvailable = !projectId && isConversationAutomationConversation(convKey);
  const conversationEnabled = conversationAutomationAvailable && CONVERSATION_AUTOMATION[convKey] === true;
  const enabled = projectId ? projectEnabled : conversationEnabled;
  return {
    global_mode: globalMode,
    project_id: projectId,
    project_automation_available: projectAutomationAvailable,
    project_automation_enabled: projectEnabled,
    conversation_automation_available: conversationAutomationAvailable,
    conversation_automation_enabled: conversationEnabled,
    enabled,
  };
}

function automationEnabledForBinding(binding) {
  return automationScopeForConversation(binding?.convKey || binding?.tabUrl || "").enabled;
}

async function notifyAutomationChanged() {
  try {
    const groups = await Promise.all(H2W_TAB_URLS.map((url) => chrome.tabs.query({ url })));
    const tabs = [...new Map(groups.flat().filter((tab) => tab?.id).map((tab) => [tab.id, tab])).values()];
    await Promise.allSettled(tabs.map((tab) => (
      chrome.tabs.sendMessage(tab.id, { type: "h2w_automation_changed" })
    )));
  } catch (_) {}
}
let resolveConfigReady;
const configReady = new Promise((r) => { resolveConfigReady = r; });
(async () => {
  await detectOrLoadLocale();
  let stored = {};
  try {
    const keys = [...Object.keys(CFG), "idleNudgeCooldownSec", PROJECT_AUTOMATION_STORAGE_KEY, CONVERSATION_AUTOMATION_STORAGE_KEY];
    stored = await chrome.storage.local.get(keys);
    CFG = { ...CFG, ...stored };
    delete CFG[PROJECT_AUTOMATION_STORAGE_KEY];
    delete CFG[CONVERSATION_AUTOMATION_STORAGE_KEY];
    PROJECT_AUTOMATION = sanitizeProjectAutomation(stored[PROJECT_AUTOMATION_STORAGE_KEY]);
    CONVERSATION_AUTOMATION = sanitizeConversationAutomation(stored[CONVERSATION_AUTOMATION_STORAGE_KEY]);
  } catch (e) {}
  if (!String(CFG.wakeTemplate || "").trim()) CFG.wakeTemplate = defaultWakeTemplate();
  if (!String(CFG.progressTemplate || "").trim()) CFG.progressTemplate = defaultProgressTemplate();
  if (!String(CFG.llmJudgePromptTemplate || "").trim()) {
    CFG.llmJudgePromptTemplate = localizedText("default_llm_judge_prompt", null, DEFAULT_LLM_JUDGE_PROMPT);
  }
  const patch = {};
  if (String(CFG.llmJudgePromptTemplate || "").trim() === LEGACY_DEFAULT_LLM_JUDGE_PROMPT) {
    CFG.llmJudgePromptTemplate = localizedText("default_llm_judge_prompt", null, DEFAULT_LLM_JUDGE_PROMPT);
    patch.llmJudgePromptTemplate = CFG.llmJudgePromptTemplate;
  }
  if (![AUTOMATION_MODE_MANUAL, AUTOMATION_MODE_PROJECT].includes(stored.automationMode)) {
    // Upgrade compatibility: preserve the old global user's intent only as a
    // permission to use per-Project automation. No Project is auto-enabled by
    // migration; the user must explicitly turn it on from that Project HUD.
    CFG.automationMode = stored.enabled === true ? AUTOMATION_MODE_PROJECT : AUTOMATION_MODE_MANUAL;
    patch.automationMode = CFG.automationMode;
  } else {
    CFG.automationMode = normalizeAutomationMode(CFG.automationMode);
  }
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
  // Legacy global automation flags must remain off. Otherwise a stale content
  // script could bypass the new Project-scoped policy after an extension update.
  if (CFG.enabled !== false) { CFG.enabled = false; patch.enabled = false; }
  if (CFG.idleNudgeEnabled !== false) { CFG.idleNudgeEnabled = false; patch.idleNudgeEnabled = false; }
  if (Object.keys(patch).length) {
    try {
      await chrome.storage.local.set(patch);
      if (patch.idleNudgeCooldownSec === null) {
        await chrome.storage.local.remove("idleNudgeCooldownSec");
      }
    } catch (e) {}
  }
  // 0.1.49+: Herdr authentication is owned entirely by Native Messaging + the
  // mode-0600 local IPC socket. Remove historical browser-stored Herdr tokens
  // during upgrade; old extension binaries remain server-compatible separately.
  try { await chrome.storage.local.remove(["autoAllow", "token"]); } catch (e) {}
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
      files: CHATGPT_CONTENT_SCRIPT_FILES,
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

function workspaceMetaForBinding(b, workspaces = []) {
  const ws = b?.workspace_id || normalizeWorkspaceId(b);
  if (!ws) return null;
  return (Array.isArray(workspaces) ? workspaces : []).find((w) => String(w?.id || "") === ws) || null;
}

function canonicalWorkspaceLabel(b, workspaces = []) {
  const ws = b?.workspace_id || normalizeWorkspaceId(b);
  if (!ws) return b?.workspace_label || null;
  const meta = workspaceMetaForBinding(b, workspaces);
  if (!meta) return b?.workspace_label || workspaceTitleWithId({ id: ws });
  return workspaceTitleWithId({ id: ws, label: meta.label, roots: meta.roots });
}

async function reconcileBindingWorkspaceLabels(bindings, session, workspaces = []) {
  let changed = false;
  for (const b of session || []) {
    const canonical = canonicalWorkspaceLabel(b, workspaces);
    if (!canonical || canonical === b.workspace_label) continue;
    b.workspace_label = canonical;
    const storeKey = b.storeKey || bindingStoreKey(b.convKey, b.workspace_id || normalizeWorkspaceId(b));
    if (storeKey && bindings?.[storeKey]) {
      bindings[storeKey].workspace_label = canonical;
      changed = true;
    }
  }
  if (changed) await saveBindings(bindings);
  return session;
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

async function migrateZaiRootConversationState(bindings, targetConvKey, targetUrl, tabId) {
  const target = zAiConversationInfo(targetUrl || targetConvKey);
  if (!target?.conversation_id || !tabId) return { migrated: false, bindings };
  if (bindingsForConv(bindings, target.convKey).length) return { migrated: false, bindings };

  const rootKey = "https://chat.z.ai";
  const rootBindings = bindingsForConv(bindings, rootKey).filter((entry) => (
    Number(entry.tabId || 0) === Number(tabId)
    || zAiConversationInfo(entry.tabUrl)?.is_new_chat_root === true
  ));
  if (!rootBindings.length) return { migrated: false, bindings };

  const now = Date.now();
  for (const entry of rootBindings) {
    const ws = entry.workspace_id || normalizeWorkspaceId(entry);
    if (!ws) continue;
    const oldKey = entry.storeKey || bindingStoreKey(rootKey, ws);
    const next = {
      ...entry,
      convKey: target.convKey,
      site: "z.ai",
      tabId,
      tabUrl: targetUrl || target.url,
      last_seen_at: now,
    };
    delete next.storeKey;
    next.revision = bindingRevision(next);
    const nextKey = bindingStoreKey(target.convKey, ws);
    bindings[nextKey] = next;
    delete bindings[oldKey];
    clearProgressTimer(oldKey);
    if (next.status === "working") armProgressTimer(nextKey, next);
  }

  if (CONVERSATION_AUTOMATION[rootKey] === true
    && CONVERSATION_AUTOMATION[target.convKey] !== true) {
    CONVERSATION_AUTOMATION[target.convKey] = true;
  }
  delete CONVERSATION_AUTOMATION[rootKey];
  await chrome.storage.local.set({
    herdrWakeBindings: bindings,
    [CONVERSATION_AUTOMATION_STORAGE_KEY]: CONVERSATION_AUTOMATION,
  });
  callLog(`migrated z.ai root binding to ${target.convKey}`);
  return { migrated: true, bindings };
}

function tabExecutionView(tab) {
  if (!tab?.id) {
    return {
      state: "closed",
      reachable: false,
      active: false,
      frozen: false,
      discarded: false,
      protected: false,
      auto_discardable: null,
      status: null,
      tab_id: null,
    };
  }
  const discarded = tab.discarded === true;
  const frozen = tab.frozen === true;
  const active = tab.active === true;
  const state = discarded ? "discarded" : frozen ? "frozen" : active ? "foreground" : "background";
  return {
    state,
    reachable: !discarded && !frozen,
    active,
    frozen,
    discarded,
    protected: tab.autoDiscardable === false,
    auto_discardable: typeof tab.autoDiscardable === "boolean" ? tab.autoDiscardable : null,
    status: tab.status || null,
    tab_id: tab.id,
  };
}

async function getTabExecutionView(tabId) {
  if (!tabId) return tabExecutionView(null);
  try { return tabExecutionView(await chrome.tabs.get(tabId)); }
  catch (_) { return tabExecutionView(null); }
}

async function protectBoundTab(tabId) {
  if (!tabId) return tabExecutionView(null);
  try {
    const tab = await chrome.tabs.update(tabId, { autoDiscardable: false });
    return tabExecutionView(tab || await chrome.tabs.get(tabId));
  } catch (_) {
    return getTabExecutionView(tabId);
  }
}

async function restoreTabDiscardabilityIfUnbound(tabId, bindings) {
  if (!tabId) return;
  const stillBound = Object.values(bindings || {}).some((b) => b?.tabId === tabId);
  if (stillBound) return;
  try { await chrome.tabs.update(tabId, { autoDiscardable: true }); } catch (_) {}
}

async function protectAllBoundTabs(bindings) {
  const tabIds = [...new Set(Object.values(bindings || {}).map((b) => b?.tabId).filter(Boolean))];
  await Promise.allSettled(tabIds.map((tabId) => protectBoundTab(tabId)));
}

async function reconcileTabExecutionState(binding) {
  if (!binding?.tabId) return tabExecutionView(null);
  // HUD/state reads stay observational. Actual recovery only happens during
  // delivery, so automation off never reloads or activates a tab.
  return getTabExecutionView(binding.tabId);
}

async function waitForTabReachable(tabId, timeoutMs = 15000) {
  const deadline = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < deadline) {
    try {
      last = await chrome.tabs.get(tabId);
      if (last?.status === "complete" && !last.discarded && !last.frozen) return last;
    } catch (_) { return null; }
    await sleep(150);
  }
  return last;
}

async function withReachableBoundTab(tabId, operation) {
  if (!tabId) throw new Error("tab_closed");
  let previousActiveId = null;
  try {
    let tab = await chrome.tabs.get(tabId);
    await protectBoundTab(tabId);
    if (tab.discarded === true) {
      const last = tabRecoveryAttemptAt.get(tabId) || 0;
      if (Date.now() - last >= TAB_RECOVERY_COOLDOWN_MS) {
        tabRecoveryAttemptAt.set(tabId, Date.now());
        await chrome.tabs.reload(tabId);
      }
    }
    tab = await chrome.tabs.get(tabId);
    if (tab.frozen === true || tab.discarded === true) {
      const active = await chrome.tabs.query({ windowId: tab.windowId, active: true });
      previousActiveId = active[0]?.id && active[0].id !== tabId ? active[0].id : null;
      await chrome.tabs.update(tabId, { active: true, autoDiscardable: false });
    }
    const ready = await waitForTabReachable(tabId);
    if (!ready) throw new Error("tab_unreachable");
    return await operation(ready);
  } finally {
    if (previousActiveId) {
      try { await chrome.tabs.update(previousActiveId, { active: true }); } catch (_) {}
    }
  }
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
    trigger: row.trigger || "manual",
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
      files: CHATGPT_CONTENT_SCRIPT_FILES,
    });
    await sleep(80);
    return chrome.tabs.sendMessage(tabId, message);
  }
}

async function sendHandoffTabMessage(tabId, site, message) {
  if (site === "chatgpt") return sendChatGptTabMessage(tabId, message);
  let lastError = null;
  for (let attempt = 0; attempt < 20; attempt += 1) {
    try {
      return await chrome.tabs.sendMessage(tabId, message);
    } catch (error) {
      lastError = error;
      if (!missingReceiverError(error)) throw error;
      await sleep(100);
    }
  }
  throw lastError || new Error("handoff-content-script-unavailable");
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
  // HUD and popup stay observable regardless of the global/Project automation policy.
  if (pushStream) return;
  const ctrl = new AbortController();
  pushStream = { ctrl };
  void runPushStream(ctrl);
}

async function runPushStream(ctrl) {
  let backoff = 2000;
  while (runtimeAlive() && !ctrl.signal.aborted) {
    let stream = null;
    let connectTimer = null;
    let relayAbort = null;
    try {
      const decoder = new TextDecoder();
      let buf = "";
      const drainBlocks = () => {
        let idx;
        while ((idx = buf.indexOf("\n\n")) >= 0) {
          const block = buf.slice(0, idx);
          buf = buf.slice(idx + 2);
          pushDispatch = pushDispatch
            .then(() => handlePushBlock(block))
            .catch((e) => callLog("push dispatch failed:", e?.message || String(e)));
        }
      };
      stream = openLocalHerdrStream({
        baseUrl: CFG.herdrMcpUrl,
        path: "/push/events",
        timeoutMs: PUSH_CONNECT_MS,
        onChunk: (bytes) => {
          buf += decoder.decode(bytes, { stream: true });
          drainBlocks();
        },
      });
      relayAbort = () => stream?.close();
      ctrl.signal.addEventListener("abort", relayAbort, { once: true });
      const opened = await Promise.race([
        stream.opened,
        new Promise((_, reject) => {
          connectTimer = setTimeout(() => reject(new Error("native-stream-connect-timeout")), PUSH_CONNECT_MS + 500);
        }),
      ]);
      if (connectTimer) clearTimeout(connectTimer);
      connectTimer = null;
      if (opened.status < 200 || opened.status >= 300) {
        callLog(`push native HTTP ${opened.status}; retrying in ${backoff}ms`);
        stream.close();
        await sleep(backoff);
        backoff = Math.min(backoff * 2, 15000);
        continue;
      }
      backoff = 2000;
      callLog(`push connected (shared native stream via ${opened.transport})`);
      await stream.done;
      buf += decoder.decode();
      drainBlocks();
      callLog(`push stream ended; reconnecting in ${backoff}ms`);
    } catch (e) {
      if (ctrl.signal.aborted || !runtimeAlive()) break;
      callLog(`push disconnected (${e.message}); retrying in ${backoff}ms`);
    } finally {
      if (connectTimer) clearTimeout(connectTimer);
      if (relayAbort) ctrl.signal.removeEventListener("abort", relayAbort);
      try { stream?.close(); } catch (_) {}
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
  if (d.status === "working" && automationEnabledForBinding(b)) armProgressTimer(storeKey, b);
  if (d.wake) {
    callLog(`hello recovery wake: ws=${ws} → ${d.status} (settle missed while offline)`);
    await routeWake(b, { status: d.status, output: "", working_count: d.working_count }, CFG.wakeTemplate || defaultWakeTemplate());
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
  if (automationEnabledForBinding(b)) armProgressTimer(storeKey, b);
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
  if (!automationEnabledForBinding(b)) return;

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
    d.kind === "partial" ? defaultPartialTemplate() : (CFG.wakeTemplate || defaultWakeTemplate()),
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
const lastJudgedAssistantFp = new Map(); // convKey -> terminal assistant fingerprint (done or wake delivered)
const idleNudgeInFlight = new Set(); // convKey -> one judge/send attempt at a time
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
  if (!automationScopeForConversation(convKey).enabled) return;
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

/** Post-turn nudge: LLM judge plus strong assistant self-declared pending work. */
async function maybeIdleNudge(msg) {
  const convKey = msg?.convKey || "";
  if (convKey && idleNudgeInFlight.has(convKey)) {
    return rememberIdleNudge(convKey, { nudged: false, reason: "judge_in_flight" });
  }
  if (convKey) idleNudgeInFlight.add(convKey);
  try {
    return await maybeIdleNudgeInner(msg);
  } finally {
    if (convKey) idleNudgeInFlight.delete(convKey);
  }
}

async function maybeIdleNudgeInner(msg) {
  const convKey = msg.convKey;
  if (!convKey) return rememberIdleNudge("", { nudged: false, reason: "no_conv" });
  if (!automationScopeForConversation(convKey).enabled) {
    return rememberIdleNudge(convKey, { nudged: false, reason: "disabled" });
  }
  const cooldownSec = paceIntervalSec();
  if (cooldownSec <= 0) {
    return rememberIdleNudge(msg.convKey || "", { nudged: false, reason: "disabled" });
  }
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
  let verdict = interpretLlmJudgeReply(judged.content, {
    skipKeywords: CFG.llmJudgeSkipKeywords || DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
  });
  if (assistantDeclaresPendingWork(assistantText) && !verdict.cont) {
    verdict = {
      done: false,
      cont: true,
      nudgeText: localizedText(
        "default_auto_continue_nudge",
        null,
        "Continue with the unfinished work you identified.",
      ),
      raw: `${verdict.raw || ""} [assistant_pending_override]`.trim(),
    };
  }
  if (verdict.done) {
    lastJudgedAssistantFp.set(convKey, fp);
    clearIdleNudgeRetry(convKey);
    return rememberIdleNudge(convKey, { nudged: false, reason: "llm_done", raw: verdict.raw });
  }
  if (!verdict.cont || !verdict.nudgeText) {
    scheduleIdleNudgeRetry(convKey, 30000);
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
  lastJudgedAssistantFp.set(convKey, fp);
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
  if (b.status !== "working" || !automationEnabledForBinding(b)) { clearProgressTimer(storeKey); return; }
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
    if (cur !== ts || !automationEnabledForBinding(b)) return;
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
    }, CFG.progressTemplate || defaultProgressTemplate());
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
  if (progressTickSecMs() <= 0) {
    for (const storeKey of [...progressTimers.keys()]) clearProgressTimer(storeKey);
    return;
  }
  for (const storeKey of [...progressTimers.keys()]) {
    const b = bindings[storeKey];
    if (!b || b.status !== "working" || !automationEnabledForBinding(b)) clearProgressTimer(storeKey);
  }
  for (const [storeKey, b] of Object.entries(bindings)) {
    if (b.status === "working" && automationEnabledForBinding(b)) armProgressTimer(storeKey, b);
  }
}

async function routeWake(b, extra, template = CFG.wakeTemplate || defaultWakeTemplate(), wakeKind = null) {
  if (!automationEnabledForBinding(b)) return { ok: false, reason: "disabled" };
  const isLlmNudge = extra.status === "llm_continue_nudge";
  const rawText = String(template || "").trim();

  if (isLlmNudge) {
    const payload = {
      type: "h2w_wake",
      data: {
        template: rawText,
        llmNudge: true,
        autoAllow: true,
      },
    };
    return deliverWakeToTab(b, payload);
  }

  const ws = normalizeWorkspaceId(b) || "";
  const focusPane = extra.pane ?? b.pane ?? null;
  let roster = extra.roster || "";
  let idle_hint = extra.idle_hint || "";
  let working_count = extra.working_count ?? Object.keys(workingPaneMap(b)).length;
  const cachedCatalog = cachedPushWorkspaceCatalog();
  const cachedMeta = workspaceMetaForBinding(b, cachedCatalog);
  let workspace_label = cachedMeta
    ? canonicalWorkspaceLabel(b, cachedCatalog)
    : (extra.workspace_label || b.workspace_label || "");
  if (cachedMeta && workspace_label && workspace_label !== b.workspace_label) {
    b.workspace_label = workspace_label;
  }
  if (!roster || !cachedMeta) {
    try {
      const st = await fetchState();
      const scope = agentsInWorkspace(st.agents || [], ws);
      const meta = (st.workspaces || []).find((w) => w.id === ws) || null;
      if (meta) {
        workspace_label = canonicalWorkspaceLabel(b, st.workspaces || []) || workspace_label;
        b.workspace_label = workspace_label;
      } else if (!workspace_label) {
        workspace_label = workspaceTitleWithId({ id: ws, label: b.workspace_label, agents: scope });
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
    ? defaultPartialTemplate()
    : effectiveWakeKind === "round"
      ? (CFG.wakeTemplate || defaultWakeTemplate())
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
      autoAllow: true,
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

async function manualDirectContinue(tabId, convKey) {
  return deliverWakeToTab({ tabId, convKey, site: "chatgpt" }, {
    type: "h2w_wake",
    data: {
      template: localizedText("manual_continue_message", null, "Continue"),
      manual: true,
      autoAllow: false,
    },
  });
}

async function manualHerdrStatusContinue(tabId, convKey) {
  const bindings = await loadBindings();
  const session = bindingsForConv(bindings, convKey);
  if (!session.length) return { ok: false, error: "binding_required" };

  let state = await fetchStateFresh();
  if (!state?.ok) state = await fetchState();
  if (!state?.ok) return { ok: false, error: "herdr_state_unavailable" };

  const blocks = [];
  for (const b of session) {
    const ws = b.workspace_id || normalizeWorkspaceId(b);
    if (!ws) continue;
    const scope = agentsInWorkspace(state.agents || [], ws);
    const meta = (state.workspaces || []).find((w) => w.id === ws) || null;
    const pack = formatWorkspaceRoster(scope, b.pane || null, {
      id: ws,
      label: meta?.label || b.workspace_label,
      roots: meta?.roots,
    });
    blocks.push(pack.roster || `workspace ${pack.workspace_label || b.workspace_label || ws}: no active panes`);
  }
  if (!blocks.length) return { ok: false, error: "herdr_state_empty" };

  const template = [
    localizedText("manual_status_continue_intro", null, "Continue from the current Herdr state below."),
    ...blocks,
  ].join("\n\n");
  return deliverWakeToTab({ ...session[0], tabId: tabId || session[0].tabId, convKey }, {
    type: "h2w_wake",
    data: {
      template,
      manual: true,
      autoAllow: false,
    },
  });
}

async function manualLlmJudgeContinue(tabId, convKey, userText, assistantText) {
  if (!isLlmJudgeConfigured(CFG)) {
    return rememberIdleNudge(convKey, { ok: false, nudged: false, reason: "llm_not_configured", error: "llm_not_configured" });
  }
  const assistant = String(assistantText || "").trim();
  if (!assistant) {
    return rememberIdleNudge(convKey, { ok: false, nudged: false, reason: "empty_assistant", error: "empty_assistant" });
  }

  const judged = await fetchLlmJudge(String(userText || ""), assistant);
  if (!judged.ok) {
    return rememberIdleNudge(convKey, {
      ok: false,
      nudged: false,
      reason: `llm_${judged.reason}`,
      error: judged.error || judged.reason || "llm_failed",
      status: judged.status || null,
    });
  }
  const verdict = interpretLlmJudgeReply(judged.content, {
    skipKeywords: CFG.llmJudgeSkipKeywords || DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
  });
  if (verdict.done) {
    return rememberIdleNudge(convKey, { ok: true, continued: false, nudged: false, reason: "llm_done", raw: verdict.raw });
  }
  if (!verdict.cont || !verdict.nudgeText) {
    return rememberIdleNudge(convKey, { ok: true, continued: false, nudged: false, reason: "llm_ambiguous", raw: verdict.raw });
  }

  const result = await deliverWakeToTab({ tabId, convKey, site: "chatgpt" }, {
    type: "h2w_wake",
    data: {
      template: verdict.nudgeText,
      llmNudge: true,
      manual: true,
      autoAllow: false,
    },
  });
  if (!result?.ok) {
    return rememberIdleNudge(convKey, {
      ok: false,
      continued: false,
      nudged: false,
      reason: "wake_failed",
      raw: verdict.raw,
      error: result?.error || result?.blocked || result?.reason || "submit_failed",
    });
  }
  return rememberIdleNudge(convKey, {
    ok: true,
    continued: true,
    nudged: true,
    reason: "llm_continue",
    raw: verdict.raw,
    send: verdict.nudgeText,
  });
}

// ---- Conversation handoff (ChatGPT Project + z.ai persisted chat) ----
function handoffTargetInfoForTransfer(transfer, targetConvKey, targetUrl = null) {
  const site = transfer?.site || "chatgpt";
  if (site === "chatgpt") {
    const info = chatGptConversationInfo(targetConvKey || targetUrl);
    if (!info?.project_id || info.project_id !== transfer.project_id) {
      return { ok: false, error: "target_project_mismatch" };
    }
    return { ok: true, info };
  }
  if (site === "z.ai") {
    const info = zAiConversationInfo(targetUrl || targetConvKey) || zAiConversationInfo(targetConvKey);
    if (!info?.conversation_id) return { ok: false, error: "target_conversation_invalid" };
    return { ok: true, info };
  }
  return { ok: false, error: "handoff_site_unsupported" };
}

async function commitHandoffTransfer(transferId, targetConvKey, targetTabId, targetUrl = null) {
  const transfers = await loadHandoffTransfers();
  const transfer = transfers[transferId];
  if (!transfer) return { ok: false, error: "handoff_not_found" };
  const targetCheck = handoffTargetInfoForTransfer(transfer, targetConvKey, targetUrl);
  if (!targetCheck.ok) {
    await markTransfer(transferId, { status: "seed_uncertain", error: targetCheck.error });
    return { ok: false, error: targetCheck.error };
  }
  const targetInfo = targetCheck.info;
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
  const seed = buildHandoffSeed({
    transferId,
    packet: transfer.handoff_text,
    template: localizedText("handoff_seed_template"),
  });
  await markTransfer(transferId, {
    status: "seed_submitting",
    target_tab_id: targetTabId,
    error: null,
  });
  let result;
  try {
    result = await sendHandoffTabMessage(targetTabId, transfer.site || "chatgpt", {
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
  const launchUrl = transfer?.handoff_launch_url || transfer?.project_launch_url || null;
  if (!transfer?.handoff_text || !launchUrl) return { ok: false, error: "handoff_not_ready" };
  await markTransfer(transferId, { status: "target_opening", error: null });
  let tab;
  try {
    tab = await chrome.tabs.create({ url: launchUrl, active: true });
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
    probe = await sendHandoffTabMessage(transfer.target_tab_id, transfer.site || "chatgpt", {
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

function handoffRequestTemplateForSite(site) {
  if (site === "z.ai") {
    return localizedText("handoff_request_template_zai", null, localizedText("handoff_request_template"));
  }
  return localizedText("handoff_request_template");
}

async function acceptImmediateHandoffSummary(transfer, result) {
  const assistantText = String(result?.assistantText || "").trim();
  if (!assistantText) return { ok: true, pending: true, handoff: handoffView(transfer) };
  const packet = extractHandoffPacket(assistantText, transfer.id);
  if (!packet) {
    const failed = await markTransfer(transfer.id, { status: "failed", error: "handoff_packet_invalid" });
    return { ok: false, error: "handoff_packet_invalid", handoff: handoffView(failed) };
  }
  const ready = await markTransfer(transfer.id, {
    status: "summary_ready",
    handoff_text: packet,
    error: null,
  });
  setTimeout(() => { void launchHandoffTarget(transfer.id); }, 0);
  return { ok: true, pending: true, handoff: handoffView(ready) };
}

async function resumeSummaryRequested(transfer) {
  const tabId = transfer?.source_tab_id;
  if (!tabId || !(await tabStillExists(tabId))) {
    const failed = await markTransfer(transfer.id, { status: "failed", error: "source_tab_missing" });
    return { ok: false, error: "source_tab_missing", handoff: handoffView(failed) };
  }

  let snapshot = null;
  try { snapshot = await sendHandoffTabMessage(tabId, transfer.site || "chatgpt", { type: "h2w_snapshot_turn" }); } catch (_) {}
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

  const prompt = buildHandoffRequest({
    transferId: transfer.id,
    bindings: source,
    template: handoffRequestTemplateForSite(transfer.site || "chatgpt"),
  });
  const retried = await markTransfer(transfer.id, { status: "summary_requested", error: null });
  try {
    const result = await sendHandoffTabMessage(tabId, transfer.site || "chatgpt", {
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
    return acceptImmediateHandoffSummary(retried, result);
  } catch (e) {
    const failed = await markTransfer(transfer.id, { status: "failed", error: `summary_prompt_failed:${e.message}` });
    return { ok: false, error: "summary_prompt_failed", handoff: handoffView(failed) };
  }
  return { ok: true, pending: true, handoff: handoffView(retried) };
}

async function startHandoffForTab(tabId, trigger = "manual") {
  const liveInfo = await conversationInfoForTab(tabId);
  const convInfo = handoffConversationInfo(liveInfo?.url || liveInfo?.convKey, liveInfo?.site || null);
  if (!convInfo?.manual_handoff_available || !convInfo?.handoff_launch_url) {
    return { ok: false, error: "handoff_conversation_required" };
  }
  if (trigger === "manual" && automationScopeForConversation(convInfo.convKey).enabled) {
    return { ok: false, error: "automation_enabled" };
  }
  if (trigger !== "manual" && !automationScopeForConversation(convInfo.convKey).enabled) {
    return { ok: false, error: "automation_disabled" };
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
    site: convInfo.site,
    status: "summary_requested",
    source_conv_key: convInfo.convKey,
    source_tab_id: tabId,
    project_id: convInfo.project_id,
    project_key: convInfo.project_key,
    project_launch_url: convInfo.project_launch_url,
    handoff_launch_url: convInfo.handoff_launch_url,
    source_bindings: transferSourceSnapshot(session),
    handoff_text: null,
    target_tab_id: null,
    target_conv_key: null,
    error: null,
    trigger,
    created_at: now,
    updated_at: now,
  };
  transfers[transferId] = row;
  await saveHandoffTransfers(transfers);
  const prompt = buildHandoffRequest({
    transferId,
    bindings: session,
    template: handoffRequestTemplateForSite(convInfo.site),
  });
  let result;
  try {
    result = await sendHandoffTabMessage(tabId, convInfo.site, {
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
  return acceptImmediateHandoffSummary(row, result);
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
  if (msg?.type === "h2w_json_bridge_catalog") {
    void (async () => {
      const access = validateJsonBridgeSender(msg, sender);
      if (!access.ok) {
        sendResponse(access);
        return;
      }
      const result = await jsonBridgeRpc("tools/list", {});
      sendResponse(result.ok
        ? { ok: true, tools: result.result?.tools || [] }
        : result);
    })();
    return true;
  }
  if (msg?.type === "h2w_json_bridge_call") {
    void (async () => {
      const access = validateJsonBridgeSender(msg, sender);
      if (!access.ok) {
        sendResponse(access);
        return;
      }
      const result = await jsonBridgeRpc("tools/call", {
        name: String(msg.tool || ""),
        arguments: msg.args || {},
      });
      sendResponse(result.ok
        ? { ok: true, result: result.result }
        : result);
    })();
    return true;
  }
  if (msg?.type === "h2w_hello") {
    if (sender.tab?.id) tabVersions.set(sender.tab.id, msg.version || "");
    return;
  }
  if (msg?.type === "h2w_register") {
    void (async () => {
      const bindings = await loadBindings();
      let matched = bindingsForConv(bindings, msg.convKey);
      if (!matched.length && sender.tab?.id) {
        const migration = await migrateZaiRootConversationState(
          bindings,
          String(msg.convKey || ""),
          msg.url || sender.tab?.url || null,
          sender.tab.id,
        );
        if (migration.migrated) matched = bindingsForConv(bindings, msg.convKey);
      }
      if (matched.length) {
        for (const entry of matched) {
          const b = bindings[entry.storeKey];
          b.tabId = sender.tab?.id;
          b.tabUrl = msg.url || sender.tab?.url;
          b.execution_state = await protectBoundTab(b.tabId);
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
  if (msg?.type === "h2w_automation_state") {
    const scope = automationScopeForConversation(String(msg.convKey || "").trim());
    sendResponse({
      ok: true,
      ...scope,
      autoAllow: true,
      labels: hudLabels(),
    });
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
      const convInfo = convKey ? handoffConversationInfo(convKey, jsonBridgeSiteForConversation(convKey)) : null;
      const automation = automationScopeForConversation(convKey);
      const cachedWorkspaces = cachedPushWorkspaceCatalog();
      // /push/events hello already carries the authoritative workspace list.
      // Render that immediately; /push/state becomes an async freshness probe.
      const state = cachedWorkspaces.length
        ? { ok: true, workspaces: cachedWorkspaces, source: "push_hello_cache", cached_at: pushWorkspaceCatalogAt }
        : await fetchState();
      if (cachedWorkspaces.length) void fetchState();
      const liveWorkspaces = state?.ok && Array.isArray(state.workspaces) ? state.workspaces : [];
      await reconcileBindingWorkspaceLabels(bindings, session, liveWorkspaces);
      const labels = session.map((b) => canonicalWorkspaceLabel(b, liveWorkspaces) || b.workspace_id).filter(Boolean);
      let llmHost = "";
      try {
        llmHost = CFG.llmJudgeBaseUrl ? new URL(llmJudgeCompletionsUrl(CFG.llmJudgeBaseUrl)).host : "";
      } catch (_) { llmHost = ""; }
      const last = convKey ? (lastIdleNudgeResult.get(convKey) || null) : null;
      sendResponse({
        ok: true,
        version: H2W_SCRIPT_VERSION,
        locale: getLocale(),
        labels: hudLabels(),
        enabled: automation.enabled,
        automation_mode: automation.global_mode,
        project_id: automation.project_id,
        project_automation_available: automation.project_automation_available,
        project_automation_enabled: automation.project_automation_enabled,
        conversation_automation_available: automation.conversation_automation_available,
        conversation_automation_enabled: automation.conversation_automation_enabled,
        site: convInfo?.site || jsonBridgeSiteForConversation(convKey) || null,
        manual_handoff_available: Boolean(convInfo?.manual_handoff_available),
        autoAllow: true,
        idleNudgeEnabled: automation.enabled,
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
        bindings: await Promise.all(session.map(async (b) => ({
          ...bindingView(b),
          execution_state: await reconcileTabExecutionState(b),
        }))),
        continuity_id: session.map((b) => b.continuity_id).find(Boolean) || transfer?.continuity_id || null,
        can_handoff: Boolean(
          convInfo?.manual_handoff_available
          && session.length > 0
          && (!handoffStatusIsActive(transfer?.status) || transferView?.can_resume === true),
        ),
        handoff: transferView,
        workspaces: liveWorkspaces,
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
    void (async () => {
      const incoming = { ...(msg.config || {}) };
      // Current extension builds never persist or consume HERDR_MCP_TOKEN.
      delete incoming.token;
      delete incoming.idleNudgeCooldownSec;
      // Permission-card auto-allow is part of effective Project automation.
      // Ignore the 0.1.43-and-earlier independent preference.
      delete incoming.autoAllow;
      if (Object.prototype.hasOwnProperty.call(incoming, "uiLocale")) {
        await setLocale(String(incoming.uiLocale || "en"));
      }
      // Compatibility with 0.1.42 UI: the old global boolean now selects only
      // the global policy mode. It never enables a Project by itself.
      if (!Object.prototype.hasOwnProperty.call(incoming, "automationMode")
        && Object.prototype.hasOwnProperty.call(incoming, "enabled")) {
        incoming.automationMode = incoming.enabled === false ? AUTOMATION_MODE_MANUAL : AUTOMATION_MODE_PROJECT;
      }
      if (Object.prototype.hasOwnProperty.call(incoming, "automationMode")) {
        incoming.automationMode = normalizeAutomationMode(incoming.automationMode);
      }
      delete incoming.enabled;
      delete incoming.idleNudgeEnabled;
      CFG = { ...CFG, ...incoming };
      CFG.enabled = false;
      CFG.idleNudgeEnabled = false;
      delete CFG.idleNudgeCooldownSec;
      await chrome.storage.local.set({ ...CFG, enabled: false, idleNudgeEnabled: false });
      try { await chrome.storage.local.remove(["idleNudgeCooldownSec", "autoAllow", "token"]); } catch (e) {}
      void rebuildStreams();
      sendResponse({ ok: true });
      // The initiating Options/content-script request must not wait for every
      // matching tab to acknowledge the broadcast. A stale or suspended tab
      // can otherwise leave the caller disabled indefinitely even though the
      // preference was already persisted successfully.
      void notifyAutomationChanged();
    })();
    return true;
  }
  if (msg?.type === "h2w_set_project_automation") {
    void (async () => {
      const projectId = String(msg.project_id || "").trim();
      const convKey = String(msg.convKey || "").trim();
      if (!projectId && convKey) {
        const access = await authorizeConversationAutomation(msg, sender);
        if (!access.ok) {
          sendResponse(access);
          return;
        }
        if (msg.enabled === true) CONVERSATION_AUTOMATION[convKey] = true;
        else delete CONVERSATION_AUTOMATION[convKey];
        await chrome.storage.local.set({ [CONVERSATION_AUTOMATION_STORAGE_KEY]: CONVERSATION_AUTOMATION });
        sendResponse({ ok: true, ...automationScopeForConversation(convKey) });
        void notifyAutomationChanged();
        return;
      }
      if (!/^g-p-[0-9a-f]{32}$/i.test(projectId)) {
        sendResponse({ ok: false, error: "project_required" });
        return;
      }
      if (normalizeAutomationMode(CFG.automationMode) !== AUTOMATION_MODE_PROJECT) {
        sendResponse({ ok: false, error: "global_manual_mode" });
        return;
      }
      if (msg.enabled === true) PROJECT_AUTOMATION[projectId] = true;
      else delete PROJECT_AUTOMATION[projectId];
      await chrome.storage.local.set({ [PROJECT_AUTOMATION_STORAGE_KEY]: PROJECT_AUTOMATION });
      const bindings = await loadBindings();
      reconcileProgressTimers(bindings);
      for (const convKey of [...idleNudgeRetryTimers.keys()]) {
        if (!automationScopeForConversation(convKey).enabled) clearIdleNudgeRetry(convKey);
      }
      sendResponse({ ok: true, ...automationScopeForConversation(msg.convKey || msg.url || "") });
      void notifyAutomationChanged();
    })();
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
      const automation = automationScopeForConversation(convInfo?.convKey || "");
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
          automationMode: normalizeAutomationMode(CFG.automationMode),
          enabled: automation.enabled,
          projectAutomationAvailable: automation.project_automation_available,
          projectAutomationEnabled: automation.project_automation_enabled,
          progressTickSec: CFG.progressTickSec,
          progressFallbackSec: CFG.progressFallbackSec,
          idleNudgeEnabled: automation.enabled,
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
      const requestedConvKey = String(msg.convKey || "").trim();
      if (!convInfo?.convKey && !requestedConvKey) { sendResponse({ ok: false, error: "conversation-unavailable" }); return; }
      const effectiveConvKey = convInfo?.convKey || requestedConvKey;
      const workspace_id = msg.workspace_id
        || (typeof msg.pane === "string" && msg.pane.includes(":") ? msg.pane.split(":")[0] : null);
      if (!workspace_id) { sendResponse({ ok: false, error: "workspace_required" }); return; }
      const storeKey = bindingStoreKey(effectiveConvKey, workspace_id);
      if (bindings[storeKey]) { sendResponse({ ok: false, error: "already-bound", convKey: convInfo.convKey, workspace_id }); return; }
      const workspace_label = msg.workspace_label
        || workspaceTitleWithId({ id: workspace_id, label: msg.workspace_label_raw, roots: msg.roots });
      const continuity_id = bindingsForConv(bindings, effectiveConvKey).map((x) => x.continuity_id).find(Boolean)
        || newContinuityId();
      const b = {
        workspace_id,
        workspace_label,
        pane: msg.pane || null,
        focus_agent: msg.agent || null,
        agent: null, // The binding targets a workspace, not an individual agent.
        workingPanes: {},
        convKey: effectiveConvKey,
        site: convInfo?.site || "unknown",
        tabId, tabUrl: convInfo.url || null,
        execution_state: await protectBoundTab(tabId),
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
      await restoreTabDiscardabilityIfUnbound(tabId, bindings);
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
  if (msg?.type === "h2w_manual_continue") {
    void (async () => {
      const tabId = msg.tabId || sender.tab?.id;
      if (!tabId) { sendResponse({ ok: false, error: "tab-unavailable" }); return; }
      const convInfo = await conversationInfoForTab(tabId);
      const convKey = String(msg.convKey || convInfo?.convKey || "").trim();
      if (!convKey) { sendResponse({ ok: false, error: "conversation-unavailable" }); return; }
      if (automationScopeForConversation(convKey).enabled) {
        sendResponse({ ok: false, error: "automation_enabled" });
        return;
      }
      if (msg.action === "direct") {
        sendResponse(await manualDirectContinue(tabId, convKey));
        return;
      }
      if (msg.action === "status") {
        sendResponse(await manualHerdrStatusContinue(tabId, convKey));
        return;
      }
      if (msg.action === "judge") {
        sendResponse(await manualLlmJudgeContinue(tabId, convKey, msg.userText || "", msg.assistantText || ""));
        return;
      }
      sendResponse({ ok: false, error: "manual_action_unknown" });
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
      sendResponse(await startHandoffForTab(tabId, msg.trigger || "manual"));
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
  try {
    const url = `${CFG.herdrMcpUrl.replace(/\/+$/, "")}/push/state`;
    const resp = await localHerdrFetch(url, { nativeTimeoutMs: STATE_FETCH_MS });
    if (!resp.ok) return { ok: false, status: resp.status };
    const body = await resp.json();
    if (Array.isArray(body?.workspaces)) cachePushWorkspaceCatalog(body.workspaces);
    return { ok: true, source: "push_state", ...body };
  } catch (e) {
    return { ok: false, error: String(e?.message || e || "native-transport-failed") };
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
  resetLocalAuth();
  // A configured endpoint may have changed; never show a workspace catalog
  // from the previous server while the new shared stream is reconnecting.
  pushWorkspaceCatalog = [];
  pushWorkspaceCatalogAt = 0;
  const bindings = await loadBindings();
  callLog(
    `rebuild streams v${H2W_SCRIPT_VERSION}: ${Object.keys(bindings).length} binding(s),`,
    `transport=native-ipc, automationMode=${normalizeAutomationMode(CFG.automationMode)}`,
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
  if (progressTickSecMs() <= 0) return;
  for (const [storeKey, b] of Object.entries(bindings)) {
    if (b.status === "working" && automationEnabledForBinding(b) && !progressTimers.has(storeKey)) armProgressTimer(storeKey);
  }
}

// ---- Install, browser startup, and every service-worker startup ----
// MV3 can restart the worker without onInstalled/onStartup, so rebuild at module scope.
chrome.runtime.onStartup.addListener(() => { void rebuildStreams(); });
chrome.runtime.onInstalled.addListener(() => {
  void rebuildStreams();
  chrome.storage.local.get(["herdrMcpUrl"], (cfg) => {
    if (!cfg.herdrMcpUrl) chrome.storage.local.set({
      herdrMcpUrl: "http://127.0.0.1:8772",
      automationMode: AUTOMATION_MODE_MANUAL,
      enabled: false,
      wakeTemplate: defaultWakeTemplate(),
      progressTickSec: 60,
      progressFallbackSec: 1200,
      progressTemplate: defaultProgressTemplate(),
      idleNudgeEnabled: false,
    });
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
