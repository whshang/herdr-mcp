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
const DEFAULT_TEMPLATE =
  "herdr workspace {workspace_label}: agents stopped (focus {agent} @ {pane} → {status}).\n\nFocus output:\n{output}\n\n{roster}\n\n{idle_hint}\n\nContinue orchestration; prefer fs/exec over expensive models.";
const DEFAULT_PROGRESS_TEMPLATE =
  "herdr workspace {workspace_label} progress (focus {agent} @ {pane} · {status}; {working_count} still working).\n\nFocus output:\n{output}\n\n{roster}\n\n{idle_hint}\n\nUse herdr_since / inspect; keep orchestrating on the web.";

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
  $("lab_idle").textContent = t("label_idle_nudge");
  $("hint_idle").textContent = t("hint_idle_nudge");
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
  $("template").value = cfg.wakeTemplate || DEFAULT_TEMPLATE;
  $("progressTickSec").value = cfg.progressTickSec ?? 60;
  $("progressFallbackSec").value = cfg.progressFallbackSec ?? 1200;
  $("progressTemplate").value = cfg.progressTemplate || DEFAULT_PROGRESS_TEMPLATE;
  $("idleNudgeEnabled").checked = cfg.idleNudgeEnabled !== false;
  $("llmJudgeBaseUrl").value = cfg.llmJudgeBaseUrl || "";
  $("llmJudgeApiKey").value = cfg.llmJudgeApiKey || "";
  $("llmJudgeModel").value = cfg.llmJudgeModel || "";
  $("llmJudgePromptTemplate").value = (cfg.llmJudgePromptTemplate && String(cfg.llmJudgePromptTemplate).trim())
    ? cfg.llmJudgePromptTemplate
    : DEFAULT_LLM_JUDGE_PROMPT;
  $("llmJudgeSkipKeywords").value = (cfg.llmJudgeSkipKeywords && String(cfg.llmJudgeSkipKeywords).trim())
    ? cfg.llmJudgeSkipKeywords
    : DEFAULT_LLM_SKIP_KEYWORDS_TEXT;
  $("autoAllow").checked = cfg.autoAllow !== false;
  $("enabled").checked = cfg.enabled !== false;
}

$("uiLocale").addEventListener("change", async () => {
  await setLocale($("uiLocale").value);
  applyI18n();
});

$("save").addEventListener("click", () => {
  const config = {
    herdrMcpUrl: $("url").value.trim(),
    token: $("token").value.trim(),
    wakeTemplate: $("template").value,
    progressTickSec: parseTickSec($("progressTickSec").value, 60),
    progressFallbackSec: parseTickSec($("progressFallbackSec").value, 1200),
    progressTemplate: $("progressTemplate").value,
    idleNudgeEnabled: $("idleNudgeEnabled").checked,
    llmJudgeBaseUrl: $("llmJudgeBaseUrl").value.trim(),
    llmJudgeApiKey: $("llmJudgeApiKey").value.trim(),
    llmJudgeModel: $("llmJudgeModel").value.trim(),
    llmJudgePromptTemplate: $("llmJudgePromptTemplate").value.trim() || DEFAULT_LLM_JUDGE_PROMPT,
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
  const token = $("token").value.trim();
  if (!url) { setStatus(t("need_url"), "err"); return; }
  setStatus(t("testing"), "");
  fetch(`${url}/push/state`, { headers: token ? { Authorization: `Bearer ${token}` } : {} })
    .then((r) => {
      if (r.ok) {
        return r.json().then((j) => setStatus(`✓ ${t("connect_ok", { n: j.agents?.length || 0 })}`, "ok"));
      }
      if (r.status === 401) setStatus(`✖ ${t("http_401")}`, "err");
      else setStatus(`✖ HTTP ${r.status}`, "err");
    })
    .catch((e) => setStatus(`✖ ${t("unreachable_detail", { msg: e.message })}`, "err"));
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
      llmJudgePromptTemplate: $("llmJudgePromptTemplate").value.trim() || DEFAULT_LLM_JUDGE_PROMPT,
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
      else if (resp?.reason === "http") setStatus(`✖ LLM HTTP ${resp.status}${resp.error ? `: ${resp.error}` : ""}`, "err");
      else setStatus(`✖ ${resp?.error || resp?.reason || "llm test failed"}`, "err");
      return;
    }
    setStatus(
      `✓ LLM raw=${JSON.stringify(resp.content)} → done=${resp.done} cont=${resp.cont}`
        + (resp.cont ? ` send=${JSON.stringify(resp.nudgeText)}` : ""),
      "ok",
    );
  });
});

onLocaleReady(async () => {
  applyI18n();
  await loadForm();
});
void detectOrLoadLocale();
