// options.js — settings + locale
import { detectOrLoadLocale, setLocale, getLocale, t, onLocaleReady } from "./i18n.js";
import {
  DEFAULT_LLM_JUDGE_PROMPT, DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
} from "./binding-core.js";

const $ = (id) => document.getElementById(id);
const KEYS = [
  "herdrMcpUrl", "token", "wakeTemplate", "progressTickSec", "progressFallbackSec",
  "progressTemplate", "autoAllow", "enabled",
  "idleNudgeEnabled", "llmJudgeBaseUrl", "llmJudgeApiKey", "llmJudgeModel",
  "llmJudgePromptTemplate", "llmJudgeSkipKeywords",
];
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
  $("lab_locale").textContent = t("label_locale");
  $("hint_locale").textContent = t("hint_locale");
  $("lab_url").textContent = t("label_url");
  $("hint_url").textContent = t("hint_url");
  $("lab_token").textContent = t("label_token");
  $("hint_token").textContent = t("hint_token");
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
  $("lab_auto").textContent = t("label_auto_allow");
  $("hint_auto").textContent = t("hint_auto_allow");
  $("lab_enabled").textContent = t("label_enabled");
  $("hint_enabled").textContent = t("hint_enabled");
  $("llmJudgeApiKey").placeholder = t("placeholder_llm_key");
  $("llmJudgeModel").placeholder = t("placeholder_llm_model");
  $("save").textContent = t("save");
  $("test").textContent = t("test");
  $("testLlm").textContent = t("test_llm");
  $("uiLocale").value = getLocale();
}

function setStatus(text, cls) {
  $("status").textContent = text;
  $("status").className = cls || "";
}

async function loadForm() {
  const cfg = await chrome.storage.local.get(KEYS);
  $("url").value = cfg.herdrMcpUrl || "http://127.0.0.1:8772";
  $("token").value = cfg.token || "";
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
  $("autoAllow").checked = cfg.autoAllow !== false;
  $("enabled").checked = cfg.enabled !== false;
}

$("uiLocale").addEventListener("change", async () => {
  await setLocale($("uiLocale").value);
  applyI18n();
  await new Promise((resolve) => {
    chrome.runtime.sendMessage({ type: "h2w_set_config", config: { uiLocale: getLocale() } }, () => resolve());
  });
});

$("save").addEventListener("click", () => {
  const config = {
    herdrMcpUrl: $("url").value.trim(),
    token: $("token").value.trim(),
    wakeTemplate: $("template").value,
    progressTickSec: parseTickSec($("progressTickSec").value, 60),
    progressFallbackSec: parseTickSec($("progressFallbackSec").value, 1200),
    progressTemplate: $("progressTemplate").value,
    // One operational switch controls both workspace wake delivery and the
    // optional post-turn small-model nudge. Keep the legacy field mirrored.
    idleNudgeEnabled: $("enabled").checked,
    llmJudgeBaseUrl: $("llmJudgeBaseUrl").value.trim(),
    llmJudgeApiKey: $("llmJudgeApiKey").value.trim(),
    llmJudgeModel: $("llmJudgeModel").value.trim(),
    llmJudgePromptTemplate: $("llmJudgePromptTemplate").value.trim() || t("default_llm_judge_prompt") || DEFAULT_LLM_JUDGE_PROMPT,
    llmJudgeSkipKeywords: $("llmJudgeSkipKeywords").value.trim() || DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
    autoAllow: $("autoAllow").checked,
    enabled: $("enabled").checked,
    uiLocale: getLocale(),
  };
  chrome.runtime.sendMessage({ type: "h2w_set_config", config }, (resp) => {
    if (chrome.runtime.lastError || !resp?.ok) {
      setStatus(`${t("save_failed")}: ${chrome.runtime.lastError?.message || ""}`, "err");
    } else {
      setStatus(`✓ ${t("saved")}`, "ok");
      $("llmJudgePromptTemplate").value = config.llmJudgePromptTemplate;
      $("llmJudgeSkipKeywords").value = config.llmJudgeSkipKeywords;
    }
  });
});

$("test").addEventListener("click", () => {
  const url = $("url").value.trim().replace(/\/+$/, "");
  if (!url) { setStatus(t("need_url"), "err"); return; }
  setStatus(t("testing"), "");
  // Exercise the exact same bounded background transport used by popup/HUD.
  // Direct Options-page fetch can otherwise hang indefinitely on Chrome's
  // loopback-network permission gate and hide the actual remediation.
  chrome.runtime.sendMessage({ type: "h2w_agents" }, (resp) => {
    if (chrome.runtime.lastError) {
      setStatus(`✖ ${t("unreachable_detail", { msg: chrome.runtime.lastError.message })}`, "err");
      return;
    }
    if (resp?.ok) {
      setStatus(`✓ ${t("connect_ok", { n: resp.agents?.length || 0 })}`, "ok");
      return;
    }
    if (resp?.status === 401) {
      setStatus(`✖ ${t("http_401")}`, "err");
      return;
    }
    if (String(resp?.error || "").startsWith("loopback_permission_")) {
      setStatus(`✖ ${t("loopback_permission_help")}`, "err");
      return;
    }
    const detail = resp?.error === "fetch_timeout"
      ? t("loopback_timeout_help")
      : (resp?.error || `HTTP ${resp?.status || "?"}`);
    setStatus(`✖ ${t("unreachable_detail", { msg: detail })}`, "err");
  });
});

$("testLlm").addEventListener("click", () => {
  const base = $("llmJudgeBaseUrl").value.trim();
  const key = $("llmJudgeApiKey").value.trim();
  const model = $("llmJudgeModel").value.trim();
  if (!base || !key || !model) {
    setStatus(t("llm_need_config"), "err");
    return;
  }
  const btn = $("testLlm");
  btn.disabled = true;
  setStatus(t("testing"), "");
  // Route via service worker (host_permissions + shared timeout path); Options-page fetch can hang with no feedback.
  chrome.runtime.sendMessage({
    type: "h2w_test_llm",
    config: {
      llmJudgeBaseUrl: base,
      llmJudgeApiKey: key,
      llmJudgeModel: model,
      llmJudgePromptTemplate: $("llmJudgePromptTemplate").value.trim() || t("default_llm_judge_prompt") || DEFAULT_LLM_JUDGE_PROMPT,
      llmJudgeSkipKeywords: $("llmJudgeSkipKeywords").value.trim() || DEFAULT_LLM_SKIP_KEYWORDS_TEXT,
    },
  }, (resp) => {
    btn.disabled = false;
    if (chrome.runtime.lastError) {
      setStatus(`✖ ${chrome.runtime.lastError.message}`, "err");
      return;
    }
    if (!resp?.ok) {
      if (resp?.reason === "timeout") setStatus(`✖ ${t("llm_timeout")}`, "err");
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
      done: String(resp.done),
      cont: String(resp.cont),
      send,
    })}`, "ok");
  });
});

onLocaleReady(async () => {
  applyI18n();
  await loadForm();
});
void detectOrLoadLocale();
