#!/usr/bin/env node
/**
 * Extension static checks and pure-logic tests without Chrome.
 *
 * 1. Manifest references exist and the manifest is valid JSON.
 * 2. All extension JavaScript passes node --check.
 * 3. binding-core state transitions, pruning, and template rendering.
 * 4. speaks-json.js tool-call parsing in a VM with a stub window.
 *
 * Usage: node tests/manual/extension_smoke.mjs
 */
import { readFileSync, existsSync } from "node:fs";
import { spawnSync } from "node:child_process";
import vm from "node:vm";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import {
  decideWake, decideWorkspaceWake, reconcileWorkspaceWakeKind, agentsInWorkspace, formatWorkspaceRoster, workspaceTitleWithId, pruneExpired, bindingRevision, buildWakeTemplate, shouldProgressTick, shouldSendProgress,
  progressOutputFingerprint,
  isIdleNudgeText, looksLikeSubstantiveReply, isHerdrWakeComposerText,
  interpretLlmJudgeReply, isLlmJudgeConfigured, llmJudgeCompletionsUrl, buildLlmJudgeUserMessage,
  parseLlmSkipKeywords, llmReplyMatchesSkipKeyword, assistantNudgeFingerprint, assistantDeclaresPendingWork,
  conversationInfoFromSupportedUrl,
} from "../../extension/binding-core.js";
import {
  buildHandoffRequest, buildHandoffSeed, chatGptConversationInfo,
  classifyHandoffAssistantReply, extractHandoffPacket, handoffSeedContainsTransfer,
  newContinuityId, newTransferId,
} from "../../extension/continuity-core.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const EXT = path.join(__dirname, "..", "..", "extension");
let failures = 0;
function ok(cond, label, detail = "") {
  if (cond) console.log(`  ✅ ${label}`);
  else { failures++; console.error(`  ❌ ${label} ${detail}`); }
}

// ---- 1. Manifest reference integrity ----
const manifest = JSON.parse(readFileSync(path.join(EXT, "manifest.json"), "utf8"));
const referenced = [];
for (const cs of manifest.content_scripts || []) for (const js of cs.js || []) referenced.push(js);
referenced.push(manifest.background?.service_worker);
referenced.push(manifest.options_page);
referenced.push(manifest.action?.default_popup);
for (const [k, v] of Object.entries(manifest.icons || {})) referenced.push(v);
for (const [k, v] of Object.entries(manifest.action?.default_icon || {})) referenced.push(v);
for (const r of referenced) {
  if (!r) continue;
  ok(existsSync(path.join(EXT, r)), `manifest reference exists: ${r}`);
}
ok(manifest.background?.type === "module", "background is a module worker");
ok(manifest.content_scripts.length === 4, "manifest contains four site content scripts");
ok(manifest.permissions?.includes("nativeMessaging"), "manifest enables Chrome Native Messaging for automatic local authentication");
ok(!manifest.key, "unpacked extension keeps its existing Chromium path-derived identity");

const backgroundSource = readFileSync(path.join(EXT, "background.js"), "utf8");
const wakeSource = readFileSync(path.join(EXT, "content", "wake.js"), "utf8");
const localAuthSource = readFileSync(path.join(EXT, "local-auth.js"), "utf8");
const nativeHostSource = readFileSync(path.join(EXT, "..", "bin", "herdr-extension-host"), "utf8");
const jsonBridgeSource = readFileSync(path.join(EXT, "content", "webmcp", "json-bridge.js"), "utf8");
ok(manifest.version === "0.1.60", "manifest version stays 0.1.60 for the browser performance budget");
ok(backgroundSource.includes('const H2W_SCRIPT_VERSION = "0.1.60"'), "background version matches manifest");
ok(wakeSource.includes('const H2W_CONTENT_VERSION = "0.1.60"'), "content version matches manifest");
ok(backgroundSource.includes("sendResponse({ ok: true, ...automationScopeForConversation(convKey) });")
    && backgroundSource.includes("void notifyAutomationChanged();")
    && wakeSource.includes("finally {\n      setHudActionBusy(false);"),
  "automation toggles release the initiating HUD without waiting for tab broadcasts");
ok(wakeSource.includes('regenerate-thread-error-button')
    && wakeSource.includes("maybeRecoverExplicitThreadError")
    && wakeSource.includes("thread_error_server_ahead")
    && wakeSource.includes("thread_error_delivery_unknown"),
  "ChatGPT explicit send-timeout cards retry once or safely reload before generic recovery");
ok(wakeSource.includes("maybeRecoverDisconnectedReply")
    && wakeSource.includes("连接已中断")
    && wakeSource.includes("waiting for (?:the )?full response")
    && wakeSource.includes("chatgpt_disconnected")
    && wakeSource.includes("{ ...safety, streaming: false }")
    && wakeSource.includes("(assistantChanged || curLen > lastAsstLen)"),
  "ChatGPT disconnected-stream placeholders stop faking progress and allow one bounded reload without resubmitting the user task");
ok(backgroundSource.includes("idleNudgeInFlight")
    && backgroundSource.includes("assistantDeclaresPendingWork")
    && backgroundSource.includes("scheduleIdleNudgeRetry(convKey, 30000)")
    && backgroundSource.includes("assistant_pending_override"),
  "LLM auto-continue retries ambiguous/send-failed turns and honors strong pending-work declarations");
ok(wakeSource.includes('data-testid^="conversation-turn-"')
    && wakeSource.includes("observedConversationMessageFloor")
    && wakeSource.includes("mergeMessageCountFloor")
    && wakeSource.includes('"chatgpt_virtual_turn_index"'),
  "ChatGPT context pressure preserves a virtualized conversation-turn count floor across reloads");
ok(
  backgroundSource.includes('from "./local-auth.js"')
    && localAuthSource.includes("sendNativeMessage")
    && localAuthSource.includes("connectNative")
    && localAuthSource.includes('type: "request"')
    && localAuthSource.includes('type: "stream"')
    && !localAuthSource.includes("expires_at")
    && !backgroundSource.includes("CFG.token")
    && nativeHostSource.includes("extension.sock")
    && nativeHostSource.includes('transport: "ipc"')
    && nativeHostSource.includes("/extension/session")
    && nativeHostSource.includes("allowed_origins")
    && nativeHostSource.includes("chromiumIdForPath")
    && nativeHostSource.includes("EXTENSION_PATH")
    && nativeHostSource.includes('"Citro Labs", "ego lite", "NativeMessagingHosts"'),
  "extension uses tokenless Native Messaging IPC and retains old-session server compatibility",
);
ok(
  jsonBridgeSource.includes("const ROUND_YIELD_INTERVAL = 12")
    && jsonBridgeSource.includes("while (taskSeq === currentTaskSeq)")
    && !jsonBridgeSource.includes("MAX_ROUNDS")
    && jsonBridgeSource.includes("continuing after ${round} tool rounds"),
  "JSON bridge keeps running past round 12 until the assistant returns a non-tool answer",
);
ok(
  jsonBridgeSource.includes("root.parentElement.insertBefore(bar, root)")
    && jsonBridgeSource.includes('root.style.display = expanded ? originalDisplay : "none"')
    && !jsonBridgeSource.includes("root.insertBefore(bar, root.firstChild)"),
  "JSON folding toggles the whole site message from an external sibling bar instead of squeezing its flex children",
);
ok(
  jsonBridgeSource.includes("CORE.hasPendingToolReply(entries)")
    && jsonBridgeSource.includes("resuming pending tool JSON after page/script recovery")
    && jsonBridgeSource.includes("schedulePendingResume();")
    && jsonBridgeSource.includes("scheduleFold();"),
  "JSON bridge refolds history and resumes a last pending Herdr tool JSON after recovery",
);
ok(
  wakeSource.includes(".bar.automation-on")
    && wakeSource.includes(".bar.automation-on .handoff")
    && wakeSource.includes('enabled ? " automation-on" : ""')
    && wakeSource.includes("rgba(236,253,245,.97)")
    && wakeSource.includes("transition: none;"),
  "automation-on state gives the whole HUD a deterministic light-green treatment without refresh-restarted transitions",
);
ok(
  manifest.content_scripts.find((cs) => cs.matches?.includes("https://chatgpt.com/*"))?.js?.includes("context-pressure.js"),
  "ChatGPT loads the classic context-pressure policy before wake.js",
);
ok(
  backgroundSource.includes("bound_workspace_ids:")
    && backgroundSource.includes("bindingView(b)")
    && backgroundSource.includes("workspaces: liveWorkspaces"),
  "page HUD response carries live workspace and binding runtime state",
);
ok(
  backgroundSource.includes("msg.tabId || sender.tab?.id"),
  "in-page bind resolves the sender tab without popup-only tabId",
);
ok(
  wakeSource.includes("Compact in-page HUD + operational drawer")
    && wakeSource.includes("h2w_bind")
    && wakeSource.includes("h2w_unbind")
    && wakeSource.includes("ws-action"),
  "in-page HUD contains the conversation controls",
);
{
  const panelStart = wakeSource.indexOf('<div class="panel" part="panel" hidden>');
  const barStart = wakeSource.indexOf('<div class="bar" part="bar">');
  const panelHtml = wakeSource.slice(panelStart, barStart);
  const barHtml = wakeSource.slice(barStart, wakeSource.indexOf("`;", barStart));
  ok(
    panelStart >= 0 && barStart > panelStart
      && !panelHtml.includes("manual-continue")
      && !panelHtml.includes('class="quick"')
      && barHtml.includes("manual-continue")
      && barHtml.includes("manual-status")
      && barHtml.includes("manual-judge")
      && barHtml.includes("manual-handoff")
      && barHtml.includes('class="quick"'),
    "frequent manual actions, manual handoff, and automation switch live on the HUD bar, not in the drawer",
  );
}
ok(
  wakeSource.includes("hudBoundRuntimeState")
    && wakeSource.includes('document.addEventListener("pointerdown"')
    && wakeSource.includes("path.includes(host)"),
  "HUD stays runtime-state driven and closes its drawer on outside click",
);
ok(
  backgroundSource.includes("global/Project automation policy")
    && !backgroundSource.includes("if (pushStream || CFG.enabled === false) return;"),
  "push observation remains live while Project automation is unavailable or off",
);
ok(
  backgroundSource.includes('const PROJECT_AUTOMATION_STORAGE_KEY = "herdrProjectAutomation"')
    && backgroundSource.includes('const CONVERSATION_AUTOMATION_STORAGE_KEY = "herdrConversationAutomation"')
    && backgroundSource.includes("automationScopeForConversation")
    && backgroundSource.includes("validateJsonBridgeSender")
    && backgroundSource.includes("authorizeConversationAutomation")
    && backgroundSource.includes('msg?.type === "h2w_set_project_automation"')
    && backgroundSource.includes("stored.enabled === true ? AUTOMATION_MODE_PROJECT : AUTOMATION_MODE_MANUAL")
    && wakeSource.includes("hud?.project_automation_available !== true")
    && wakeSource.includes("hud?.conversation_automation_available !== true")
    && wakeSource.includes('site: ADAPTER.name')
    && wakeSource.includes('type: "h2w_set_project_automation"')
    && wakeSource.includes('setHudProjectAutomation(!(hudCache?.enabled === true))'),
  "manual JSON bridge is site/sender scoped while automation uses the effective Project/conversation state",
);
ok(
  wakeSource.includes('class="handoff manual-handoff"')
    && wakeSource.includes("manualHandoffAction")
    && wakeSource.includes("hudActionBusy || active || hud?.bound !== true")
    && !wakeSource.includes('if (hudCache?.enabled === true) return { ok: false, error: "automation_enabled" }')
    && wakeSource.includes("h2w_handoff_start")
    && wakeSource.includes('trigger: "manual"')
    && wakeSource.includes("h2w_handoff_seed")
    && wakeSource.includes("h2w_handoff_probe"),
  "manual HUD handoff stays available with Auto on and reuses recoverable internals",
);
ok(
  backgroundSource.includes('manual_handoff_available: Boolean(chatgpt.project_id && chatgpt.conversation_id)')
    && wakeSource.includes('button.hidden = !chatGptConversationActionsAvailable'),
  "ChatGPT root/Project-home pages keep binding controls but hide conversation-only manual actions",
);
ok(
  backgroundSource.includes("herdrConversationTransfers")
    && backgroundSource.includes("source_binding_set_changed")
    && backgroundSource.includes("seed_uncertain")
    && backgroundSource.includes("commitHandoffTransfer")
    && backgroundSource.includes("source_automation_enabled")
    && backgroundSource.includes("inheritedAutomationStorageForTransfer")
    && backgroundSource.includes('reason: "handoff_active"'),
  "background persists crash-safe handoff state, Auto inheritance, and suppresses source wakes during cutover",
);
ok(
  wakeSource.includes("registerCurrentConversation")
    && wakeSource.includes("startConversationRouteWatch")
    && wakeSource.includes("convKey !== registeredConvKey"),
  "content script re-registers when an SPA conversation route changes",
);
ok(
  backgroundSource.includes("ONE shared stream for every binding")
    && backgroundSource.includes("openLocalHerdrStream({")
    && backgroundSource.includes('path: "/push/events"')
    && !backgroundSource.includes("/push/events?workspace="),
  "extension uses one shared SSE stream instead of one stream per workspace",
);
ok(
  backgroundSource.includes("PUSH_CONNECT_MS = 5000")
    && backgroundSource.includes("STATE_FETCH_MS = 4000")
    && backgroundSource.includes("nativeTimeoutMs: STATE_FETCH_MS")
    && localAuthSource.includes("timeout_ms"),
  "native localhost transport is bounded without browser bearer state",
);
ok(
  backgroundSource.includes("reconcileWorkspaceWakeKind(wakeKind, working_count)"),
  "final wake template is reconciled against the fresh workspace working count",
);
ok(
  backgroundSource.includes("cachePushWorkspaceCatalog(data.workspaces)")
    && backgroundSource.includes('source: "push_hello_cache"')
    && backgroundSource.includes("cachedPushWorkspaceCatalog()"),
  "HUD can render workspaces from the shared SSE hello catalog",
);
ok(
  backgroundSource.includes("if (stateFetchInFlight) return stateFetchInFlight")
    && !backgroundSource.includes("pushStream || !Object.keys(bindings || {}).length"),
  "workspace discovery keeps one shared SSE and deduplicates state fetches",
);
ok(
  !backgroundSource.includes("if (!Object.keys(kept).length) stopPushStream()")
    && backgroundSource.includes("if (!Object.keys(bindings).length) clearActionBadge()")
    && backgroundSource.includes("pushWorkspaceCatalog = []")
    && backgroundSource.includes("pushWorkspaceCatalogAt = 0"),
  "workspace discovery stream survives zero bindings and endpoint rebuilds clear stale catalog data",
);
ok(
  !backgroundSource.includes('navigator.permissions.query({ name: "loopback-network" })')
    && localAuthSource.includes("native-messaging-unavailable")
    && backgroundSource.includes("native-transport-failed"),
  "local Herdr failures surface Native Messaging errors instead of browser loopback permission state",
);
const optionsSource = readFileSync(path.join(EXT, "options.js"), "utf8");
ok(
  optionsSource.includes('type: "h2w_agents"')
    && optionsSource.includes("native_host_help")
    && optionsSource.includes("native_ipc_timeout_help"),
  "Options connection test uses bounded background transport and Native Messaging guidance",
);

{
  const baseCode = readFileSync(path.join(EXT, "content/base.js"), "utf8");
  const chatgptCode = readFileSync(path.join(EXT, "content", "injector", "chatgpt.js"), "utf8");
  const projectConversation = "https://chatgpt.com/g/g-p-6a89c078669481918c8eb70fdfd3d978-herdr-mcp/c/6a89c95e-70bc-83ea-bf3d-fab6b83fc86e";
  const u = new URL(projectConversation);
  const window = {};
  const ctx = vm.createContext({
    window,
    location: { origin: u.origin, pathname: u.pathname },
    document: { querySelector: () => null, querySelectorAll: () => [], body: null, documentElement: null },
    console,
  });
  vm.runInContext(baseCode, ctx);
  vm.runInContext(chatgptCode, ctx);
  const key = vm.runInContext("window.__H2W_ADAPTER__.getConversationKey()", ctx);
  ok(key === "https://chatgpt.com/g/g-p-6a89c078669481918c8eb70fdfd3d978/c/6a89c95e-70bc-83ea-bf3d-fab6b83fc86e",
    "ChatGPT Project slug is normalized out of the binding key");

  const projectCtx = vm.createContext({
    window: {},
    location: { origin: u.origin, pathname: "/g/g-p-6a89c078669481918c8eb70fdfd3d978-herdr-mcp/project" },
    document: { querySelector: () => null, querySelectorAll: () => [], body: null, documentElement: null },
    console,
  });
  vm.runInContext(baseCode, projectCtx);
  vm.runInContext(chatgptCode, projectCtx);
  ok(vm.runInContext("window.__H2W_ADAPTER__.getConversationKey()", projectCtx)
      === "https://chatgpt.com/g/g-p-6a89c078669481918c8eb70fdfd3d978",
    "ChatGPT Project home exposes the stable Project binding key");

  const rootCtx = vm.createContext({
    window: {},
    location: { origin: u.origin, pathname: "/" },
    document: { querySelector: () => null, querySelectorAll: () => [], body: null, documentElement: null },
    console,
  });
  vm.runInContext(baseCode, rootCtx);
  vm.runInContext(chatgptCode, rootCtx);
  ok(vm.runInContext("window.__H2W_ADAPTER__.getConversationKey()", rootCtx) === "https://chatgpt.com",
    "ChatGPT root exposes a pending binding key before a conversation exists");
}
{
  const normal = "https://chatgpt.com/c/6a89c95e-70bc-83ea-bf3d-fab6b83fc86e";
  const project = "https://chatgpt.com/g/g-p-6a89c078669481918c8eb70fdfd3d978/c/6a8ae745-a3dc-83ea-91f0-218dd5be7807";
  const slugged = "https://chatgpt.com/g/g-p-6a89c078669481918c8eb70fdfd3d978-herdr-mcp/c/6a8ae745-a3dc-83ea-91f0-218dd5be7807";
  const projectHome = "https://chatgpt.com/g/g-p-6a89c078669481918c8eb70fdfd3d978-herdr-mcp/project";
  ok(conversationInfoFromSupportedUrl(normal)?.convKey === normal,
    "URL fallback recognizes a normal ChatGPT conversation");
  ok(conversationInfoFromSupportedUrl(project)?.convKey === project,
    "URL fallback recognizes the reported ChatGPT project conversation");
  ok(conversationInfoFromSupportedUrl(slugged)?.convKey === project,
    "URL fallback normalizes a slugged ChatGPT Project alias to the resource-id key");
  ok(chatGptConversationInfo(slugged)?.project_launch_url === "https://chatgpt.com/g/g-p-6a89c078669481918c8eb70fdfd3d978",
    "Project rollover launcher uses stable Project resource id");
  ok(conversationInfoFromSupportedUrl(projectHome)?.convKey
      === "https://chatgpt.com/g/g-p-6a89c078669481918c8eb70fdfd3d978"
      && conversationInfoFromSupportedUrl(projectHome)?.binding_scope === "project",
    "URL fallback recognizes a ChatGPT Project home as a stable binding scope");
  ok(conversationInfoFromSupportedUrl("https://chatgpt.com/")?.binding_scope === "pending",
    "URL fallback recognizes ChatGPT root as a pending binding scope");
  ok(conversationInfoFromSupportedUrl("https://example.com/c/abc") === null,
    "URL fallback rejects unsupported hosts");
}

console.log("\n[conversation continuity]");
{
  const transferId = newTransferId(1700000000000, 0.25);
  const continuityId = newContinuityId(1700000000000, 0.5);
  ok(transferId.startsWith("ht:") && continuityId.startsWith("hc:"),
    "continuity and transfer ids use separate namespaces");
  const request = buildHandoffRequest({
    transferId,
    bindings: [{ workspace_id: "w5W", workspace_label: "herdr-mcp (w5W)" }],
  });
  ok(request.includes(`<<<HERDR_HANDOFF_V1 id=${transferId}>>>`) && request.includes("herdr-mcp (w5W)"),
    "handoff request carries transfer marker and bound workspace context");
  const assistant = `<<<HERDR_HANDOFF_V1 id=${transferId}>>>\n# Project handoff\nCurrent objective: continue binding work.\nNext: verify live state.\n<<<END_HERDR_HANDOFF_V1>>>`;
  const packet = extractHandoffPacket(assistant, transferId);
  ok(!!packet && packet.includes("Current objective"), "handoff packet extracts the marked assistant payload");
  ok(extractHandoffPacket(assistant, "ht:wrong") === null, "handoff packet rejects a mismatched transfer id");
  ok(classifyHandoffAssistantReply({
    text: "the previous assistant reply",
    transferId,
    sourceAssistantFingerprint: "fp-old",
    currentAssistantFingerprint: "fp-old",
  }).kind === "stale_source", "handoff ignores the pre-summary assistant body instead of failing the transfer");
  ok(classifyHandoffAssistantReply({
    text: assistant,
    transferId,
    sourceAssistantFingerprint: "fp-old",
    currentAssistantFingerprint: "fp-new",
  }).kind === "packet", "handoff accepts the first new assistant body when it contains the transfer packet");
  ok(classifyHandoffAssistantReply({
    text: "a genuinely new reply without the requested transfer marker",
    transferId,
    sourceAssistantFingerprint: "fp-old",
    currentAssistantFingerprint: "fp-new",
  }).kind === "invalid", "handoff still rejects a genuinely new malformed summary reply");
  const seed = buildHandoffSeed({ transferId, packet });
  ok(handoffSeedContainsTransfer(seed, transferId), "new-conversation seed carries the transfer marker");
  ok(seed.includes("开始任何 mutation 前，重新检查相关的 Herdr/runtime/Git 状态"),
    "seed requires live-state verification before mutations");
}

ok(
  backgroundSource.includes("source_assistant_fp")
    && backgroundSource.includes("recoverableFailedTransferFromSource")
    && backgroundSource.includes("recoverExistingHandoffPacket")
    && backgroundSource.includes("classifyHandoffAssistantReply"),
  "handoff summary race is guarded and a late valid packet can recover a failed transfer",
);

ok(
  backgroundSource.includes("conversationInfoForTab(msg.tabId)")
    && backgroundSource.includes("files: CHATGPT_CONTENT_SCRIPT_FILES")
    && backgroundSource.includes('"context-pressure.js"')
    && backgroundSource.includes('"conversation-health.js"')
    && backgroundSource.includes('"recovery-controller.js"'),
  "popup/bind recovery reinjects the complete ChatGPT content-script stack when an open tab lost its MV3 listener",
);

const localeCodes = ["en", "zh", "ja"];
const localeHud = {};
const localeHandoff = {};
for (const code of localeCodes) {
  const locPath = path.join(EXT, "locales", `${code}.json`);
  ok(existsSync(locPath), `locale file exists: ${code}`);
  const loc = JSON.parse(readFileSync(locPath, "utf8"));
  localeHud[code] = Object.keys(loc).filter((k) => k.startsWith("hud_")).sort();
  localeHandoff[code] = Object.keys(loc).filter((k) => k.startsWith("handoff_")).sort();
}
ok(localeHud.en.length > 0, "en has hud keys");
ok(localeHud.zh.join(",") === localeHud.en.join(","), "zh hud keys match en");
ok(localeHud.ja.join(",") === localeHud.en.join(","), "ja hud keys match en");
ok(localeHandoff.en.length > 0, "en has handoff keys");
ok(localeHandoff.zh.join(",") === localeHandoff.en.join(","), "zh handoff keys match en");
ok(localeHandoff.ja.join(",") === localeHandoff.en.join(","), "ja handoff keys match en");
const zhLocale = JSON.parse(readFileSync(path.join(EXT, "locales", "zh.json"), "utf8"));
ok(zhLocale.hud_manual_continue === "手动继续", "zh HUD manual continue label is exact");
ok(zhLocale.hud_manual_status === "herdr监控", "zh HUD Herdr monitor label is exact");
ok(zhLocale.hud_manual_judge === "LLM 分析", "zh HUD LLM analysis label is exact");
ok(zhLocale.hud_manual_handoff === "手动接力", "zh HUD manual handoff label is exact");
ok(zhLocale.hud_manual_handoff_hint.includes("ChatGPT")
    && zhLocale.hud_manual_handoff_hint.includes("z.ai")
    && zhLocale.hud_manual_handoff_hint.includes("seed")
    && zhLocale.hud_manual_handoff_hint.includes("binding")
    && zhLocale.hud_manual_handoff_hint.includes("自动"),
  "zh HUD manual handoff tooltip explains cross-site safe rollover semantics");
ok(zhLocale.hud_automation_off === "自动 关", "zh HUD automation-off label is localized");
ok(
  zhLocale.hud_automation_on_hint.includes("进度")
    && (zhLocale.hud_automation_on_hint.includes("LLM") || zhLocale.hud_automation_on_hint.includes("小模型"))
    && zhLocale.hud_automation_on_hint.includes("恢复")
    && zhLocale.hud_automation_on_hint.includes("接力")
    && zhLocale.hud_automation_on_hint.includes("权限卡"),
  "zh Auto-on tooltip enumerates all automatic continuity behaviors",
);
ok(
  zhLocale.hud_automation_off_hint.includes("当前 ChatGPT Project")
    && zhLocale.hud_automation_off_hint.includes("手动继续")
    && zhLocale.hud_automation_off_hint.includes("手动接力")
    && zhLocale.hud_automation_off_hint.includes("同一 Project"),
  "zh Auto-off tooltip explains Project scope and manual availability",
);
ok(zhLocale.label_automation_mode === "启用 ChatGPT 项目自动化"
    && zhLocale.hint_automation_mode.includes("只控制 ChatGPT 项目共享自动化")
    && zhLocale.hint_automation_mode.includes("普通 ChatGPT")
    && zhLocale.hint_automation_mode.includes("即使这里关闭也能单独启用")
    && zhLocale.hint_automation_mode.includes("权限卡")
    && zhLocale.hint_automation_mode.includes("z.ai")
    && zhLocale.hint_automation_mode.includes("DeepSeek"),
  "zh automation checkbox explains the Project-only gate and independent conversation scopes");
for (const obsolete of ["hud_wake_on", "hud_wake_off", "hud_nudge_on", "hud_nudge_off", "hud_llm", "hud_llm_off"]) {
  ok(!(obsolete in zhLocale), `obsolete HUD locale key removed: ${obsolete}`);
}
ok(!wakeSource.includes("Wake on") && !wakeSource.includes("Wake off") && !wakeSource.includes("Automation off"),
  "HUD source has no legacy English wake/automation labels");
ok(!readFileSync(path.join(EXT, "options.html"), "utf8").includes("Enable wake + LLM nudge"),
  "Options source no longer exposes the legacy wake+nudge switch name");
const optionsHtml = readFileSync(path.join(EXT, "options.html"), "utf8");
const popupHtml = readFileSync(path.join(EXT, "popup.html"), "utf8");
ok(optionsHtml.includes('<input type="checkbox" id="automationMode">')
    && !optionsHtml.includes('id="enabled"')
    && !optionsHtml.includes('id="autoAllow"')
    && !optionsHtml.includes('<select id="automationMode">'),
  "Options exposes one Project-automation checkbox and no independent permission toggle");
ok(!readFileSync(path.join(EXT, "options.js"), "utf8").includes('$("autoAllow")')
    && !backgroundSource.includes("CFG.autoAllow"),
  "permission-card automation is folded into effective Project automation");
ok(!popupHtml.includes('id="idleNudgeEnabled"') && !popupHtml.includes('id="enabled"') && popupHtml.includes('id="automationModeStatus"'),
  "popup reports the global mode without exposing an automation toggle");

// ---- 2. JavaScript syntax for the fixed file list ----
const fixed = ["background.js", "binding-core.js", "continuity-core.js", "options.js", "popup.js", "context-pressure.js", "performance-core.js", "content/base.js",
  "content/injector/zai.js", "content/injector/deepseek.js", "content/injector/claude.js",
  "content/injector/chatgpt.js", "content/webmcp/speaks-json.js", "content/wake.js"];
for (const f of fixed) {
  const p = path.join(EXT, f);
  const r = spawnSync(process.execPath, ["--check", p], { encoding: "utf8" });
  ok(r.status === 0, `node --check ${f}`, r.stderr?.slice(0, 200));
}

console.log("\n[ui pressure meter]");
{
  const pcore = readFileSync(path.join(EXT, "performance-core.js"), "utf8");
  const ctx = vm.createContext({ console });
  vm.runInContext(pcore, ctx);
  const perf = vm.runInContext("H2W_BROWSER_PERFORMANCE", ctx);
  ok(typeof perf.createCoalescedScheduler === "function", "performance-core exports createCoalescedScheduler");
  ok(typeof perf.createUiPressureMeter === "function"
      && typeof perf.classifyUiPressure === "function",
    "performance-core exports the bounded ui pressure meter and classifier");
  const meter = perf.createUiPressureMeter();
  for (let i = 0; i < 10; i++) meter.recordMutation();
  const evaluated = meter.evaluate();
  ok(typeof evaluated.level === "string"
      && ["healthy", "warning", "high"].includes(evaluated.level),
    "meter evaluate returns a three-band level");
  ok(typeof evaluated.mutation_rate_per_min === "number"
      && evaluated.mutation_rate_per_min > 0,
    "meter aggregates the mutation callback rate");
  ok(evaluated.reasons.every((r) => typeof r === "string"), "classifier reasons are strings");
}

// ---- 3. binding-core state machine ----
console.log("\n[decideWake]");
const none = { status: null, lastSettle: null };
// Initial settled hello records a baseline without waking.
let d = decideWake(none, "hello", { agent: { status: "idle", seq: 10 } });
ok(!d.wake && d.status === "idle" && d.lastSettle === null, "initial settled hello records baseline without waking");
// working → armed
d = decideWake({ status: "idle", lastSettle: null }, "working", { status: "working" });
ok(!d.wake && d.status === "working", "working arms without waking");
// Settled after working wakes once.
d = decideWake({ status: "working", lastSettle: null }, "settled", { status: "done", seq: 11 });
ok(d.wake && d.status === "done" && d.lastSettle.seq === 11, "working to settled wakes once");
// Duplicate sequence does not wake.
d = decideWake({ status: "done", lastSettle: { seq: 11, at: 1 } }, "settled", { status: "done", seq: 11 });
ok(!d.wake, "duplicate settle sequence does not wake");
// A new settle sequence after more work wakes.
d = decideWake({ status: "working", lastSettle: { seq: 11, at: 1 } }, "settled", { status: "idle", seq: 12 });
ok(d.wake, "new settle sequence wakes");
// Settle without an observed working state does not wake.
d = decideWake({ status: "idle", lastSettle: null }, "settled", { status: "done", seq: 13 });
ok(!d.wake, "unarmed settle does not wake");
// Reconnecting hello recovers a missed working-to-settled transition.
d = decideWake({ status: "working", lastSettle: { seq: 11, at: 1 } }, "hello", { agent: { status: "done", seq: 14 } });
ok(d.wake, "reconnecting hello recovers a new settled sequence");
// Reconnecting hello with the same sequence does not wake.
d = decideWake({ status: "done", lastSettle: { seq: 14, at: 2 } }, "hello", { agent: { status: "done", seq: 14 } });
ok(!d.wake, "reconnecting hello deduplicates the same sequence");
// A still-working reconnect remains armed.
d = decideWake({ status: "working", lastSettle: null }, "hello", { agent: { status: "working", seq: 15 } });
ok(!d.wake && d.status === "working", "working hello remains armed");

console.log("\n[decideWorkspaceWake]");
{
  const scopeBusy = [
    { pane: "wH:p1", status: "working", workspace: "wH", seq: 1 },
    { pane: "wH:p2", status: "idle", workspace: "wH", seq: 2 },
  ];
  let w = decideWorkspaceWake(none, "hello", {}, scopeBusy);
  ok(!w.wake && w.status === "working" && w.working_count === 1, "workspace hello with working agent arms");
  w = decideWorkspaceWake({ status: "working", lastSettle: null }, "settled", { status: "done", seq: 3, pane: "wH:p1" }, [
    { pane: "wH:p2", status: "working", workspace: "wH" },
  ]);
  ok(w.wake && w.kind === "partial" && w.status === "working" && w.working_count === 1, "workspace partial settle emits partial wake");
  w = decideWorkspaceWake({ status: "working", lastSettle: null }, "settled", { status: "done", seq: 4, pane: "wH:p2" }, []);
  ok(w.wake && w.kind === "round" && w.working_count === 0, "fully settled workspace emits round wake");
  ok(reconcileWorkspaceWakeKind("partial", 0) === "round", "stale partial wake becomes round when fresh state has zero workers");
  ok(reconcileWorkspaceWakeKind("partial", 2) === "partial", "partial wake remains partial while peers are still working");
  ok(reconcileWorkspaceWakeKind("round", 1) === "partial", "stale round wake becomes partial when fresh state still has a worker");
  ok(agentsInWorkspace([{ workspace: "wH" }, { workspace: "wX" }], "wH").length === 1, "agentsInWorkspace filters by workspace");
  const pack = formatWorkspaceRoster([
    { pane: "wH:p1", name: "pi", status: "done", terminal_title: "fix tests", cwd: "/tmp/a", workspace: "wH" },
    { pane: "wH:p2", name: "cline", status: "working", terminal_title: "edit server", cwd: "/tmp/a", workspace: "wH" },
    { pane: "wH:p3", name: "anti", status: "idle", terminal_title: "", cwd: "/tmp/b", workspace: "wH" },
  ], "wH:p1", { id: "wH", label: "herdr-mcp", roots: ["/Users/x/Documents/herdr-mcp"] });
  ok(pack.working_count === 1 && pack.idle_count === 2, "roster counts working and idle agents");
  ok(pack.workspace_label.includes("herdr-mcp") && pack.workspace_label.includes("wH"), "roster uses label rather than bare id");
  ok(pack.roster.includes("← focus") && pack.roster.includes("fix tests") && pack.roster.includes("cline"), "roster includes focus marker and titles");
  ok(pack.idle_hint.includes("keep for the next task") && pack.idle_hint.includes("reclaim"), "idle_hint includes the available decisions");
  ok(
    buildWakeTemplate("ws:{workspace_label}\nout:{output}\n{roster}\n{idle_hint}", {
      workspace_label: pack.workspace_label, output: "DONE", roster: pack.roster, idle_hint: pack.idle_hint,
    }).includes("herdr-mcp"),
    "template renders workspace_label and roster",
  );
}

console.log("\n[pruneExpired / revision / template]");
const now = Date.now();
const { kept, prunedKeys } = pruneExpired({
  fresh: { expires_at: now + 1000 },
  stale: { expires_at: now - 1 },
  noexp: { pane: "x" },
}, now);
ok(Object.keys(kept).length === 2 && prunedKeys.length === 1 && prunedKeys[0] === "stale", "pruning retains unexpired and timeless bindings");
ok(/^h2w:[0-9a-f]+$/.test(bindingRevision({ pane: "wH:p1", convKey: "https://chat.z.ai/chat/s/1", created_at: 1 })), "bindingRevision format");
ok(buildWakeTemplate("a {agent} {pane} {status} {output}", { agent: "pi", pane: "wH:p1", status: "done", output: "hello\nworld" }).includes("hello\nworld"), "template rendering preserves output newlines");
ok(
  buildWakeTemplate("herdr {status}.\n\n{output}\n\nPlease continue.", { agent: "omp", pane: "w5A:p1", status: "working", output: "" }) === "herdr working.\n\nPlease continue.",
  "empty output removes excess blank lines",
);

console.log("\n[shouldProgressTick]");
// Non-positive or nonnumeric intervals disable ticks.
ok(!shouldProgressTick({ status: "working", lastTickAt: 0 }, 100000, { progressTickSec: 0 }), "progressTickSec=0 disables ticks");
ok(!shouldProgressTick({ status: "working", lastTickAt: 0 }, 100000, { progressTickSec: -5 }), "negative progressTickSec disables ticks");
ok(!shouldProgressTick({ status: "working", lastTickAt: 0 }, 100000, { progressTickSec: "abc" }), "nonnumeric progressTickSec disables ticks");
ok(!shouldProgressTick({ status: "working", lastTickAt: 0 }, 100000, {}), "missing progressTickSec disables ticks");
// Only working state can tick.
ok(!shouldProgressTick({ status: "idle", lastTickAt: 0 }, 100000, { progressTickSec: 120 }), "idle state does not tick");
ok(!shouldProgressTick({ status: null, lastTickAt: 0 }, 100000, { progressTickSec: 120 }), "null status does not tick");
ok(!shouldProgressTick(null, 100000, { progressTickSec: 120 }), "missing previous state does not tick");
// Baseline and interval boundaries.
ok(!shouldProgressTick({ status: "working", lastTickAt: null }, 100000, { progressTickSec: 120 }), "missing lastTickAt does not tick");
ok(!shouldProgressTick({ status: "working", lastTickAt: 0 }, 119999, { progressTickSec: 120 }), "tick waits for full interval");
ok(shouldProgressTick({ status: "working", lastTickAt: 0 }, 120000, { progressTickSec: 120 }), "tick fires exactly at interval");
ok(shouldProgressTick({ status: "working", lastTickAt: 0 }, 240001, { progressTickSec: 120 }), "tick fires after interval");
ok(!shouldProgressTick({ status: "working", lastTickAt: 0 }, 999, { progressTickSec: 1 }), "one-second tick waits through 999ms");
ok(shouldProgressTick({ status: "working", lastTickAt: 0 }, 1000, { progressTickSec: 1 }), "one-second tick fires at 1000ms");

console.log("\n[shouldSendProgress]");
const base = { lastSentAt: 0, lastOutputSent: "", hasProgressSent: false };
ok(shouldSendProgress(base, 1000, "hello", { progressFallbackSec: 600 }).reason === "new_output", "first nonempty summary is new_output");
ok(shouldSendProgress({ lastSentAt: 0, lastOutputSent: "hello", hasProgressSent: true }, 1000, "hello", { progressFallbackSec: 600 }).reason === "skip", "sent progress inside cooldown is skipped");
ok(shouldSendProgress({ lastSentAt: 0, lastOutputSent: "hello", hasProgressSent: true }, 1000, "hello world", { progressFallbackSec: 600 }).reason === "skip", "changed summary inside cooldown is skipped");
ok(shouldSendProgress({ lastSentAt: 0, lastOutputSent: "hello", hasProgressSent: true }, 600_000, "hello world", { progressFallbackSec: 600 }).reason === "new_output", "changed fingerprint after cooldown is new_output");
ok(shouldSendProgress({ lastSentAt: 0, lastOutputSent: "hello", hasProgressSent: true }, 600_000, "hello", { progressFallbackSec: 600 }).reason === "fallback", "unchanged fingerprint at boundary is fallback");
ok(shouldSendProgress({ lastSentAt: 0, lastOutputSent: "hello", hasProgressSent: true }, 599_999, "hello", { progressFallbackSec: 600 }).reason === "skip", "one millisecond before boundary is skipped");
ok(shouldSendProgress({ lastSentAt: 0, lastOutputSent: "", hasProgressSent: false }, 600_000, "", { progressFallbackSec: 0 }).reason === "skip", "fallback=0 disables fallback");
ok(shouldSendProgress({ lastSentAt: 0, lastOutputSent: "a", hasProgressSent: false }, 1000, "  a  ", { progressFallbackSec: 600 }).reason === "skip", "equivalent unsent fingerprint is skipped");
ok(shouldSendProgress({ lastSentAt: 0, lastOutputSent: "build ok", hasProgressSent: true }, 1000, "⠋ build ok 12:34", { progressFallbackSec: 600 }).reason === "skip", "spinner and clock noise inside cooldown is skipped");
ok(progressOutputFingerprint("⠋ x 1s") === progressOutputFingerprint("⠙ x 2s"), "fingerprint ignores spinner and short elapsed time");

// ---- 4. speaks-json.js parsing in a VM with a stub window ----
console.log("\n[speaks-json extractToolCalls]");
{
  const code = readFileSync(path.join(EXT, "content/webmcp/speaks-json.js"), "utf8");
  const window = {};
  window.__H2W_ADAPTER__ = { name: "z.ai" };
  const ctx = vm.createContext({ window, document: { querySelectorAll: () => [] }, console });
  vm.runInContext(code, ctx);
  const sj = window.__H2W_SPEAKS_JSON__;
  ok(!!sj && sj.enabled === true, "VM loads speaks-json for z.ai");
  // Nested braces and escaped strings.
  const calls = sj.extractToolCalls(
    `prefix text {"tool":"apply_patch","args":{"patch":"diff --git a/x b/x\\n@@ -1 +1 @@\\n-{\\"a\\":1}"}} suffix text {"tool":"exec_command","args":{"cmd":"echo hi"}}`,
  );
  ok(calls.length === 2, "extracts two tool calls", JSON.stringify(calls));
  ok(calls[0].tool === "apply_patch" && calls[0].args.patch.includes("{\"a\":1}"), "restores nested braces and escapes");
  ok(calls[1].tool === "exec_command", "extracts the second call");
  // Skip non-tool objects and stop at incomplete JSON.
  const mixed = sj.extractToolCalls(`{"tool":"read_file","args":{"path":"a"}} {"not_a_tool":1} {"tool":"list_dir","args":`);
  ok(mixed.length === 1 && mixed[0].tool === "read_file", "skips non-tool objects and stops at incomplete JSON");
  ok(sj.extractToolCalls(null).length === 0 && sj.extractToolCalls("").length === 0, "empty input is safe");
}

console.log("\n[permission auto-allow decisions]");
{
  // Load base.js as a classic script in the VM.
  const code = readFileSync(path.join(EXT, "content/base.js"), "utf8");
  const window = {};
  const ctx = vm.createContext({ window, document: { querySelectorAll: () => [] }, console });
  vm.runInContext(code, ctx);
  const fn = (name) => vm.runInContext(name, ctx);
  ok(fn("isPermissionDialogText('ChatGPT 请求权限以使用工具')") === true, "recognizes Chinese permission text");
  ok(fn("isPermissionDialogText('ChatGPT needs your permission to use tools')") === true, "recognizes English permission text");  ok(fn("isPermissionDialogText('这是一个普通对话框')") === false, "rejects text without permission terms");
  ok(fn("isAllowButtonText('允许')") === true, "accepts Chinese Allow");
  ok(fn("isAllowButtonText('Allow')") === true, "accepts English Allow");
  ok(fn("isAllowButtonText('同意并继续')") === true, "accepts Chinese Agree and continue");
  ok(fn("isAllowButtonText('拒绝')") === false, "rejects Chinese Deny");
  ok(fn("isAllowButtonText('取消')") === false, "rejects Chinese Cancel");
  ok(fn("isAllowButtonText('Deny')") === false, "rejects English Deny");
  ok(fn("isAllowButtonText('不要允许')") === false, "rejects negated Allow");
}

console.log("\n[tool-action permission-card auto-allow]");
{
  // Lightweight dependency-free DOM fixture implementing only the APIs used by the helper.
  class MockEl {
    constructor(tag, attrs = {}) {
      this.tagName = tag.toUpperCase();
      this.nodeType = 1;
      this.parentElement = null;
      this.childNodes = [];
      this.attrs = { ...attrs };
      this.clickCount = 0;
      this.isConnected = true;
      this.disabled = false;
      this.hidden = false;
    }
    get className() { return this.attrs.class || ""; }
    getAttribute(n) { return this.attrs[n] ?? null; }
    hasAttribute(n) { return n in this.attrs; }
    matches(sel) {
      const btnSel = "button, [role=button], [class*=btn]";
      if (sel !== btnSel) return false;
      return this.tagName === "BUTTON" || this.getAttribute("role") === "button"
        || (this.className || "").toLowerCase().includes("btn");
    }
    click() { this.clickCount++; }
    get innerText() {
      // Approximate browser innerText by concatenating the text subtree.
      let out = "";
      for (const c of this.childNodes) {
        if (c.nodeType === 3) out += c.data;
        else if (c.nodeType === 1) out += c.innerText;
      }
      return out;
    }
    get textContent() { return this.innerText; }
    querySelectorAll(sel) {
      const out = [];
      (function walk(n) {
        for (const c of n.childNodes || []) {
          if (c.nodeType === 1) {
            if (typeof c.matches === "function" && c.matches(sel)) out.push(c);
            walk(c);
          }
        }
      })(this);
      return out;
    }
  }
  function textNode(data) { return { nodeType: 3, data, innerText: data, textContent: data }; }
  function el(tag, attrs = {}, ...kids) {
    const e = new MockEl(tag, attrs);
    for (const k of kids) {
      if (typeof k === "string") e.childNodes.push(textNode(k));
      else { k.parentElement = e; e.childNodes.push(k); }
    }
    return e;
  }
  function btn(label, attrs = {}) {
    const b = el("button", attrs, label);
    b.disabled = !!attrs.disabled;
    b.hidden = !!attrs.hidden;
    return b;
  }
  // Use body as the upward traversal boundary, matching the browser DOM.
  function buildDoc(card) {
    const docEl = el("html", {});
    const body = el("body", {});
    body.childNodes.push(card); card.parentElement = body;
    docEl.childNodes.push(body); body.parentElement = docEl;
    const doc = { body, documentElement: docEl };
    (function setOwner(n) {
      for (const c of n.childNodes || []) {
        if (c.nodeType === 1) { c.ownerDocument = doc; setOwner(c); }
      }
    })(body);
    return { document: body, body, documentElement: docEl };
  }

  // Load base.js and obtain the __H2W_PERMISSION__ test hook.
  const code = readFileSync(path.join(EXT, "content/base.js"), "utf8");
  const window = {};
  const ctx = vm.createContext({ window, document: { querySelectorAll: () => [] }, console });
  vm.runInContext(code, ctx);
  const P = vm.runInContext("window.__H2W_PERMISSION__", ctx);

  // 1) A new tool-action card clicks the primary Allow exactly once.
  {
    const allow = btn("允许");
    const deny = btn("拒绝");
    const drop = btn("", { "aria-haspopup": "menu", "aria-label": "更多操作" });
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"),
      el("p", {}, "此工具需要权限访问你的文件"),
      el("div", { class: "btn-area", "data-testid": "tool-action-buttons" }, deny, allow, drop));
    const { document } = buildDoc(card);
    const clicker = P.createPermissionClicker();
    const r1 = clicker.tryClick(document);
    ok(r1.handled === true && r1.button === allow, "tool-action card finds the clickable Allow");
    const r2 = clicker.tryClick(document);
    ok(r2.duplicate === true && r2.handled === false, "repeated mutation does not click twice");
    ok(allow.clickCount === 1, "Allow is clicked exactly once");
    ok(deny.clickCount === 0, "Deny is not clicked");
    ok(drop.clickCount === 0, "aria-haspopup menu is not clicked");
  }
  // 2) A separate dropdown arrow remains untouched.
  {
    const allow = btn("允许");
    const deny = btn("拒绝");
    const drop = btn("", { "aria-haspopup": "menu", "aria-label": "更多操作" });
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"),
      el("p", {}, "此工具需要权限"),
      el("div", { class: "btn" }, deny, allow, drop));
    const { document } = buildDoc(card);
    const clicker = P.createPermissionClicker();
    const r = clicker.tryClick(document);
    ok(r.handled === true && r.button === allow, "clicks Allow rather than the dropdown");
    ok(drop.clickCount === 0, "dropdown click count remains zero");
  }
  // 3) Missing deny action fails closed.
  {
    const allow = btn("允许");
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"),
      el("p", {}, "此工具需要权限"),
      el("div", { class: "btn" }, allow));
    const { document } = buildDoc(card);
    const clicker = P.createPermissionClicker();
    const r = clicker.tryClick(document);
    ok(r.handled === false && allow.clickCount === 0, "missing deny action prevents clicking");
  }
  // 4) A non-permission title prevents clicking.
  {
    const allow = btn("允许");
    const deny = btn("拒绝");
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "保存确认"),
      el("div", { class: "btn" }, deny, allow));
    const { document } = buildDoc(card);
    const clicker = P.createPermissionClicker();
    const r = clicker.tryClick(document);
    ok(r.handled === false && allow.clickCount === 0, "non-permission title prevents clicking");
  }
  // 5) Disabled or hidden Allow actions are not clicked.
  {
    const allowDis = btn("允许", { disabled: true });
    const allowHid = btn("允许", { hidden: true });
    const deny = btn("拒绝");
    const card1 = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"), el("p", {}, "需要权限"),
      el("div", { class: "btn" }, deny, allowDis));
    const card2 = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"), el("p", {}, "需要权限"),
      el("div", { class: "btn" }, deny, allowHid));
    for (const [card, a, tag] of [[card1, allowDis, "disabled"], [card2, allowHid, "hidden"]]) {
      const { document } = buildDoc(card);
      const clicker = P.createPermissionClicker();
      const r = clicker.tryClick(document);
      ok(r.handled === false && a.clickCount === 0, `${tag} Allow is not clicked`);
    }
  }
  // 6) Legacy role=dialog remains supported.
  {
    const allow = btn("Allow");
    const deny = btn("Deny");
    const card = el("div", { role: "dialog", "aria-modal": "true" },
      el("h3", {}, "Grant permission"),
      el("p", {}, "Allow this tool to access your data"),
      el("div", { class: "btn" }, deny, allow));
    const { document } = buildDoc(card);
    const clicker = P.createPermissionClicker();
    const r = clicker.tryClick(document);
    ok(r.handled === true && r.button === allow && allow.clickCount === 1, "legacy role=dialog clicks Allow once");
    ok(deny.clickCount === 0, "dialog Deny is not clicked");
  }
  // 7) Unlabeled and more-actions icon buttons are not clicked.
  {
    const iconBtn = btn("", { "aria-label": "更多操作" });
    const allow = btn("允许");
    const deny = btn("拒绝");
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"), el("p", {}, "需要权限"),
      el("div", { class: "btn" }, iconBtn, deny, allow));
    const { document } = buildDoc(card);
    const clicker = P.createPermissionClicker();
    const r = clicker.tryClick(document);
    ok(r.handled === true && r.button === allow && iconBtn.clickCount === 0, "unlabeled more-actions icon is not clicked");
  }
  // 8) aria-disabled and aria-hidden actions fail closed.
  {
    const allowArDis = btn("允许", { "aria-disabled": "true" });
    const allowArHid = btn("允许", { "aria-hidden": "true" });
    const deny = btn("拒绝");
    const mkCard = (allow) => el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"), el("p", {}, "需要权限"),
      el("div", { class: "btn" }, deny, allow));
    for (const [allow, tag] of [[allowArDis, "aria-disabled=true"], [allowArHid, "aria-hidden=true"]]) {
      const { document } = buildDoc(mkCard(allow));
      const clicker = P.createPermissionClicker();
      const r = clicker.tryClick(document);
      ok(r.handled === false && allow.clickCount === 0, `${tag} Allow is not clicked`);
    }
  }
  // 9) An external Allow does not override the card's primary action.
  {
    const mainAllow = btn("允许");
    const deny = btn("拒绝");
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"), el("p", {}, "此工具需要权限"),
      el("div", { class: "btn-area" }, deny, mainAllow));
    // An isolated external Allow has no deny action in its action area.
    const externalAllow = btn("允许");
    // Place it first in DOM order to verify it is skipped before the primary action.
    const docEl = el("html", {});
    const body = el("body", {}, externalAllow, card);
    externalAllow.parentElement = body; card.parentElement = body;
    docEl.childNodes.push(body); body.parentElement = docEl;
    const doc = { body, documentElement: docEl };
    (function setOwner(n) {
      for (const c of n.childNodes || []) {
        if (c.nodeType === 1) { c.ownerDocument = doc; setOwner(c); }
      }
    })(body);
    const clicker = P.createPermissionClicker();
    const r = clicker.tryClick(body);
    ok(r.handled === true && r.button === mainAllow, "selects the primary action despite an external Allow");
    ok(mainAllow.clickCount === 1 && externalAllow.clickCount === 0, "external Allow is not clicked");
  }
  // 10) An initially disabled Allow can be clicked after being enabled, then deduplicates.
  {
    const allow = btn("允许", { disabled: true });
    const deny = btn("拒绝");
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"), el("p", {}, "需要权限"),
      el("div", { class: "btn" }, deny, allow));
    const { document } = buildDoc(card);
    const clicker = P.createPermissionClicker();
    const r1 = clicker.tryClick(document);
    ok(r1.handled === false && allow.clickCount === 0, "initially disabled Allow is not clicked");
    // Simulate a site enabling the action before the observer retries.
    delete allow.attrs.disabled;
    allow.disabled = false;
    const r2 = clicker.tryClick(document); // Equivalent to another observer callback.
    ok(r2.handled === true && r2.button === allow && allow.clickCount === 1, "enabled Allow is clicked once");
    const r3 = clicker.tryClick(document);
    ok(r3.duplicate === true && r3.handled === false && allow.clickCount === 1, "retry after click is deduplicated");
  }
  // 11) Nested external deny and Allow controls do not expand the exact action area.
  {
    const mainAllow = btn("允许");
    const innerDeny = btn("拒绝");
    const actionArea = el("div", { class: "btn-area", "data-testid": "tool-action-buttons" }, innerDeny, mainAllow);
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"), el("p", {}, "此工具需要权限"), actionArea);
    // A larger outer deny container must not expand the exact testid area.
    const outerDeny = btn("拒绝");
    const outerWrap = el("div", { class: "outer" }, outerDeny, card);
    // Isolated Allow unrelated to the card.
    const externalAllow = btn("允许");
    const docEl = el("html", {});
    const body = el("body", {}, externalAllow, outerWrap);
    externalAllow.parentElement = body; outerWrap.parentElement = body;
    docEl.childNodes.push(body); body.parentElement = docEl;
    const doc = { body, documentElement: docEl };
    (function setOwner(n) {
      for (const c of n.childNodes || []) {
        if (c.nodeType === 1) { c.ownerDocument = doc; setOwner(c); }
      }
    })(body);
    const clicker = P.createPermissionClicker();
    const r = clicker.tryClick(body);
    ok(r.handled === true && r.button === mainAllow, "nested outer deny still selects exact primary action");
    ok(mainAllow.clickCount === 1 && externalAllow.clickCount === 0 && outerDeny.clickCount === 0, "only the primary Allow is clicked");
  }
  // 12) Semantic fallback uses the nearest deny ancestor without a data-testid.
  {
    const allow = btn("允许");
    const deny = btn("拒绝");
    const card = el("div", { class: "tool-action-card" },
      el("h3", {}, "ChatGPT 请求使用工具"), el("p", {}, "此工具需要权限"),
      el("div", { class: "btn" }, deny, allow)); // No data-testid.
    const { document } = buildDoc(card);
    const clicker = P.createPermissionClicker();
    const r = clicker.tryClick(document);
    ok(r.handled === true && r.button === allow && allow.clickCount === 1, "semantic fallback clicks primary Allow once");
  }
}


console.log("\n[progress / nudge config]");
ok(shouldProgressTick({ status: "working", lastTickAt: 0 }, 120000, { progressTickSec: 120 }), "progress tick at 120s interval");

console.log("\n[llmJudge]");
ok(isLlmJudgeConfigured({ llmJudgeBaseUrl: "https://x/v1", llmJudgeApiKey: "k", llmJudgeModel: "m" }), "configured when three set");
ok(!isLlmJudgeConfigured({ llmJudgeBaseUrl: "", llmJudgeApiKey: "k", llmJudgeModel: "m" }), "empty url = off");
ok(llmJudgeCompletionsUrl("https://x/v1") === "https://x/v1/chat/completions", "url append completions");
ok(llmJudgeCompletionsUrl("https://x/v1/chat/completions") === "https://x/v1/chat/completions", "url already full");
ok(buildLlmJudgeUserMessage("看：{content}", { assistantText: "hello" }).includes("hello"), "prompt fills content");
ok(interpretLlmJudgeReply("好的").done === true, "好的 → done");
ok(interpretLlmJudgeReply("继续").cont === true, "继续 → continue");
ok(interpretLlmJudgeReply("继续").nudgeText === "继续", "bare 继续 sends model text");
ok(interpretLlmJudgeReply("继续，按你的建议推进").cont === true, "full continue phrase");
ok(interpretLlmJudgeReply("继续，按你的建议推进").nudgeText.includes("建议推进"), "full phrase kept as model text");
ok(interpretLlmJudgeReply("好的，没有完成。继续。").cont === true, "messy 好的+继续 → cont");
ok(interpretLlmJudgeReply("好的，没有完成。继续。").nudgeText.includes("继续"), "messy keeps model text");
ok(isIdleNudgeText("继续，按你的建议推进"), "continue text is nudge fingerprint");
ok(isIdleNudgeText("继续"), "bare 继续 is nudge fingerprint");
ok(!isIdleNudgeText("请继续验证 Convex"), "normal user not fingerprint");
ok(isHerdrWakeComposerText("herdr workspace demo: agents stopped"), "herdr wake template detected");
ok(!isHerdrWakeComposerText("please check herdr workspace layout"), "user mention not wake template");
ok(!looksLikeSubstantiveReply("Running herdr_inspect"), "tool status stub not substantive");
ok(!looksLikeSubstantiveReply("Called herdr_exec"), "short tool line not substantive");
ok(looksLikeSubstantiveReply(
  "I ran herdr_inspect and herdr_exec. The MCP server is healthy and all panes are idle. Next I will run the pytest suite for convex."
), "long prose reply is substantive");
ok(assistantDeclaresPendingWork("当前已经定位。\n\n下一步\n\n我会继续做两件事：读取 request id，然后修最小范围。"), "explicit Chinese self-declared next work is pending");
ok(assistantDeclaresPendingWork("Validation is not yet complete.\nNext: I will run the production smoke test."), "explicit English self-declared next work is pending");
ok(!assistantDeclaresPendingWork("处理已经完成。下一步建议用户可以考虑补充更多监控。"), "optional next-step advice is not forced pending");
ok(assistantNudgeFingerprint("abc") === assistantNudgeFingerprint("abc"), "fp stable");
ok(assistantNudgeFingerprint("abc") !== assistantNudgeFingerprint("abd"), "fp differs");
ok(parseLlmSkipKeywords("").includes("好的"), "empty skip → built-in");
ok(parseLlmSkipKeywords("完成\nPASS").join(",") === "完成,PASS", "custom skip parse");
ok(llmReplyMatchesSkipKeyword("完成。", "完成\nPASS"), "custom skip match");
ok(interpretLlmJudgeReply("完成", { skipKeywords: "完成\nPASS" }).done === true, "custom skip → done");
ok(interpretLlmJudgeReply("好的", { skipKeywords: "完成" }).done === false, "好的 not in custom skip → not done");

console.log(`\n=== ${failures === 0 ? "EXTENSION SMOKE ALL PASS" : failures + " FAILURES"} ===`);
process.exit(failures === 0 ? 0 : 1);
