// options.js — settings + locale
import { detectOrLoadLocale, setLocale, getLocale, t, onLocaleReady } from "./i18n.js";
import {
  DEFAULT_LLM_JUDGE_PROMPT, DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
} from "./binding-core.js";

const $ = (id) => document.getElementById(id);
const KEYS = [
  "herdrMcpUrl", "wakeTemplate", "progressTickSec", "progressFallbackSec",
  "progressTemplate", "automationMode", "enabled",
  "idleNudgeEnabled", "llmJudgeBaseUrl", "llmJudgeApiKey", "llmJudgeModel",
  "llmJudgePromptTemplate", "llmJudgeSkipKeywords",
  "experimentalZAiEnabled", "experimentalDeepSeekEnabled",
];
let loadedHostPermissionOrigins = [];

function hostPermissionPatternForUrl(rawUrl) {
  const value = String(rawUrl || "").trim();
  if (!value) return "";
  let url;
  try {
    url = new URL(value);
  } catch (_) {
    throw new Error("invalid_url");
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") throw new Error("invalid_url");
  return `${url.protocol}//${url.host}/*`;
}

function configuredHostPermissionOrigins(config) {
  const origins = [];
  if (config.experimentalZAiEnabled === true) origins.push("https://chat.z.ai/*");
  if (config.experimentalDeepSeekEnabled === true) origins.push("https://chat.deepseek.com/*");
  const llmOrigin = hostPermissionPatternForUrl(config.llmJudgeBaseUrl);
  if (llmOrigin) origins.push(llmOrigin);
  return [...new Set(origins)];
}

async function requestHostPermissions(origins) {
  if (!origins.length) return true;
  if (!chrome.permissions?.request) return false;
  return chrome.permissions.request({ origins });
}

async function removeHostPermissions(origins) {
  if (!origins.length || !chrome.permissions?.remove) return;
  try { await chrome.permissions.remove({ origins }); } catch (_) {}
}

function runtimeMessage(message) {
  return new Promise((resolve) => {
    chrome.runtime.sendMessage(message, (resp) => {
      resolve({ resp, error: chrome.runtime.lastError?.message || "" });
    });
  });
}

/** Seconds: empty/invalid → fallback; <=0 → 0 (off); cap 86400. */
function parseTickSec(v, fallback = 120) {
  const n = Number(v);
  if (v === "" || v === undefined || v === null) return fallback;
  if (!Number.isFinite(n)) return fallback;
  if (n <= 0) return 0;
  return Math.min(Math.floor(n), 86400);
}

function applyI18n() {
  document.documentElement.lang = getLocale() === "zh" ? "zh-CN" : getLocale();
  document.title = t("options_title");
  $("title").textContent = t("options_title");
  $("subtitle").textContent = t("options_subtitle");
  $("title_general").textContent = t("options_general_section");
  $("title_continuity").textContent = t("options_continuity_section");
  $("title_diagnostics").textContent = t("options_diagnostics_section");
  $("hint_diagnostics").textContent = t("options_diagnostics_hint");
  $("lab_locale").textContent = t("label_locale");
  $("hint_locale").textContent = t("hint_locale");
  $("lab_url").textContent = t("label_url");
  $("hint_url").textContent = t("hint_url");
  $("lab_wake").textContent = t("label_wake_template");
  $("hint_wake").textContent = t("hint_wake_template");
  $("lab_tick").textContent = t("label_tick");
  $("hint_tick").textContent = t("hint_tick");
  $("lab_fallback").textContent = t("label_fallback");
  $("hint_fallback").textContent = t("hint_fallback");
  $("lab_progress").textContent = t("label_progress_template");
  $("hint_progress").textContent = t("hint_progress_template");
  $("title_llm").textContent = t("label_llm_section");
  $("hint_llm_sec").textContent = t("hint_llm_section");
  $("lab_llm_url").textContent = t("label_llm_url");
  $("hint_llm_url").textContent = t("hint_llm_url");
  $("lab_llm_key").textContent = t("label_llm_key");
  $("hint_llm_key").textContent = t("hint_llm_key");
  $("lab_llm_model").textContent = t("label_llm_model");
  $("lab_llm_prompt").textContent = t("label_llm_prompt");
  $("hint_llm_prompt").textContent = t("hint_llm_prompt");
  $("lab_llm_skip").textContent = t("label_llm_skip");
  $("hint_llm_skip").textContent = t("hint_llm_skip");
  $("lab_automation_mode").textContent = t("label_automation_mode");
  $("hint_automation_mode").textContent = t("hint_automation_mode");
  $("title_experimental").textContent = t("label_experimental_section");
  $("experimental_badge").textContent = t("experimental_badge");
  $("hint_experimental").textContent = t("hint_experimental_section");
  $("lab_experimental_zai").textContent = t("label_experimental_zai");
  $("hint_experimental_zai").textContent = t("hint_experimental_zai");
  $("lab_experimental_deepseek").textContent = t("label_experimental_deepseek");
  $("hint_experimental_deepseek").textContent = t("hint_experimental_deepseek");
  $("llmJudgeApiKey").placeholder = t("placeholder_llm_key");
  $("llmJudgeModel").placeholder = t("placeholder_llm_model");
  $("save").textContent = t("save");
  $("test").textContent = t("test");
  $("testLlm").textContent = t("test_llm");
  $("uiLocale").value = getLocale();
  document.documentElement.classList.remove("i18n-pending");
}

function setStatus(text, cls) {
  $("status").textContent = text;
  $("status").className = cls || "";
}

async function loadForm() {
  const cfg = await chrome.storage.local.get(KEYS);
  $("url").value = cfg.herdrMcpUrl || "http://127.0.0.1:8772";
  $("template").value = cfg.wakeTemplate || t("default_wake_template");
  $("progressTickSec").value = cfg.progressTickSec ?? 60;
  $("progressFallbackSec").value = cfg.progressFallbackSec ?? 1200;
  $("progressTemplate").value = cfg.progressTemplate || t("default_progress_template");
  $("llmJudgeBaseUrl").value = cfg.llmJudgeBaseUrl || "";
  $("llmJudgeApiKey").value = cfg.llmJudgeApiKey || "";
  $("llmJudgeModel").value = cfg.llmJudgeModel || "";
  $("llmJudgePromptTemplate").value = (cfg.llmJudgePromptTemplate && String(cfg.llmJudgePromptTemplate).trim())
    ? cfg.llmJudgePromptTemplate
    : t("default_llm_judge_prompt") || DEFAULT_LLM_JUDGE_PROMPT;
  $("llmJudgeSkipKeywords").value = (cfg.llmJudgeSkipKeywords && String(cfg.llmJudgeSkipKeywords).trim())
    ? cfg.llmJudgeSkipKeywords
    : DEFAULT_LLM_SKIP_KEYWORDS_TEXT;
  $("automationMode").checked = cfg.automationMode === "project_auto"
    || (cfg.automationMode == null && cfg.enabled === true);
  $("experimentalZAiEnabled").checked = cfg.experimentalZAiEnabled === true;
  $("experimentalDeepSeekEnabled").checked = cfg.experimentalDeepSeekEnabled === true;
  try { loadedHostPermissionOrigins = configuredHostPermissionOrigins(cfg); } catch (_) { loadedHostPermissionOrigins = []; }
}

function setupGuideUrl() {
  if (getLocale() === "zh") {
    return "https://github.com/whshang/herdr-mcp/blob/main/docs/i18n/zh-CN/quick-agent-install.md";
  }
  if (getLocale() === "ja") return "https://github.com/whshang/herdr-mcp/blob/main/README.ja.md";
  return "https://github.com/whshang/herdr-mcp/blob/main/docs/i18n/en/quick-agent-install.md";
}

function setConnectionFailure(text) {
  const status = $("status");
  status.className = "err";
  status.replaceChildren();
  const message = document.createElement("span");
  message.textContent = text;
  status.append(message, document.createElement("br"));
  const link = document.createElement("a");
  link.href = setupGuideUrl();
  link.target = "_blank";
  link.rel = "noreferrer";
  link.textContent = t("open_github_setup_guide");
  status.append(link);
}

$("uiLocale").addEventListener("change", async () => {
  await setLocale($("uiLocale").value);
  applyI18n();
  await new Promise((resolve) => {
    chrome.runtime.sendMessage({ type: "h2w_set_config", config: { uiLocale: getLocale() } }, () => resolve());
  });
});

$("save").addEventListener("click", async () => {
  const config = {
    herdrMcpUrl: $("url").value.trim(),
    wakeTemplate: $("template").value,
    progressTickSec: parseTickSec($("progressTickSec").value, 60),
    progressFallbackSec: parseTickSec($("progressFallbackSec").value, 1200),
    progressTemplate: $("progressTemplate").value,
    automationMode: $("automationMode").checked ? "project_auto" : "manual",
    llmJudgeBaseUrl: $("llmJudgeBaseUrl").value.trim(),
    llmJudgeApiKey: $("llmJudgeApiKey").value.trim(),
    llmJudgeModel: $("llmJudgeModel").value.trim(),
    llmJudgePromptTemplate: $("llmJudgePromptTemplate").value.trim() || t("default_llm_judge_prompt") || DEFAULT_LLM_JUDGE_PROMPT,
    llmJudgeSkipKeywords: $("llmJudgeSkipKeywords").value.trim() || DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
    experimentalZAiEnabled: $("experimentalZAiEnabled").checked,
    experimentalDeepSeekEnabled: $("experimentalDeepSeekEnabled").checked,
    uiLocale: getLocale(),
  };
  let nextPermissionOrigins;
  try {
    nextPermissionOrigins = configuredHostPermissionOrigins(config);
  } catch (_) {
    setStatus(`${t("save_failed")}: ${t("host_permission_invalid_url")}`, "err");
    return;
  }
  let granted = false;
  try { granted = await requestHostPermissions(nextPermissionOrigins); } catch (_) { granted = false; }
  if (!granted) {
    setStatus(`${t("save_failed")}: ${t("host_permission_denied")}`, "err");
    return;
  }
  const { resp, error } = await runtimeMessage({ type: "h2w_set_config", config });
  if (error || !resp?.ok) {
    setStatus(`${t("save_failed")}: ${error}`, "err");
    return;
  }
  const staleOrigins = loadedHostPermissionOrigins.filter((origin) => !nextPermissionOrigins.includes(origin));
  await removeHostPermissions(staleOrigins);
  loadedHostPermissionOrigins = nextPermissionOrigins;
  setStatus(`✓ ${t("saved")}`, "ok");
  $("llmJudgePromptTemplate").value = config.llmJudgePromptTemplate;
  $("llmJudgeSkipKeywords").value = config.llmJudgeSkipKeywords;
});

$("test").addEventListener("click", () => {
  const url = $("url").value.trim().replace(/\/+$/, "");
  if (!url) { setStatus(t("need_url"), "err"); return; }
  setStatus(t("testing"), "");
  // Exercise the exact same bounded background transport used by the HUD / Control Center.
  // Direct Options-page fetch can otherwise hang indefinitely on Chrome's
  // loopback-network permission gate and hide the actual remediation.
  chrome.runtime.sendMessage({ type: "h2w_agents" }, (resp) => {
    if (chrome.runtime.lastError) {
      setConnectionFailure(`✖ ${t("unreachable_detail", { msg: chrome.runtime.lastError.message })}`);
      return;
    }
    if (resp?.ok) {
      setStatus(`✓ ${t("connect_ok", { n: resp.agents?.length || 0 })}`, "ok");
      return;
    }
    if (resp?.status === 401) {
      setConnectionFailure(`✖ ${t("http_401")}`);
      return;
    }
    const localError = String(resp?.error || "");
    if (/native[- ]messaging|native host|native-host|specified native/i.test(localError)) {
      setConnectionFailure(`✖ ${t("native_host_help")}`);
      return;
    }
    const detail = /native.*timeout/i.test(localError)
      ? t("native_ipc_timeout_help")
      : (localError || `HTTP ${resp?.status || "?"}`);
    setConnectionFailure(`✖ ${t("unreachable_detail", { msg: detail })}`);
  });
});

$("testLlm").addEventListener("click", async () => {
  const base = $("llmJudgeBaseUrl").value.trim();
  const key = $("llmJudgeApiKey").value.trim();
  const model = $("llmJudgeModel").value.trim();
  if (!base || !key || !model) {
    setStatus(t("llm_need_config"), "err");
    return;
  }
  let origin;
  try {
    origin = hostPermissionPatternForUrl(base);
  } catch (_) {
    setStatus(`✖ ${t("host_permission_invalid_url")}`, "err");
    return;
  }
  let granted = false;
  try { granted = await requestHostPermissions([origin]); } catch (_) { granted = false; }
  if (!granted) {
    setStatus(`✖ ${t("host_permission_denied")}`, "err");
    return;
  }
  const ephemeralOrigins = loadedHostPermissionOrigins.includes(origin) ? [] : [origin];
  const btn = $("testLlm");
  btn.disabled = true;
  setStatus(t("testing"), "");
  const { resp, error } = await runtimeMessage({
    type: "h2w_test_llm",
    config: {
      llmJudgeBaseUrl: base,
      llmJudgeApiKey: key,
      llmJudgeModel: model,
      llmJudgePromptTemplate: $("llmJudgePromptTemplate").value.trim() || t("default_llm_judge_prompt") || DEFAULT_LLM_JUDGE_PROMPT,
      llmJudgeSkipKeywords: $("llmJudgeSkipKeywords").value.trim() || DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
    },
  });
  btn.disabled = false;
  if (ephemeralOrigins.length) await removeHostPermissions(ephemeralOrigins);
  if (error) {
    setStatus(`✖ ${error}`, "err");
    return;
  }
  if (!resp?.ok) {
    if (resp?.reason === "timeout") setStatus(`✖ ${t("llm_timeout")}`, "err");
    else if (resp?.reason === "permission") setStatus(`✖ ${t("host_permission_denied")}`, "err");
    else if (resp?.reason === "http") {
      setStatus(`✖ ${t("llm_test_http_error", {
        status: resp.status || "?",
        detail: resp.error ? `: ${resp.error}` : "",
      })}`, "err");
    } else {
      setStatus(`✖ ${t("llm_test_failed", { error: resp?.error || resp?.reason || "?" })}`, "err");
    }
    return;
  }
  const send = resp.cont ? t("llm_test_send", { send: JSON.stringify(resp.nudgeText) }) : "";
  setStatus(`✓ ${t("llm_test_result", {
    raw: JSON.stringify(resp.content),
    done: t(resp.done ? "boolean_yes" : "boolean_no"),
    cont: t(resp.cont ? "boolean_yes" : "boolean_no"),
    send,
  })}`, "ok");
});

onLocaleReady(async () => {
  applyI18n();
  await loadForm();
});
void detectOrLoadLocale();
