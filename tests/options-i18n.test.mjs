import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const zh = JSON.parse(readFileSync(new URL("../extension/locales/zh.json", import.meta.url), "utf8"));
const optionsHtml = readFileSync(new URL("../extension/options.html", import.meta.url), "utf8");
const optionsJs = readFileSync(new URL("../extension/options.js", import.meta.url), "utf8");

test("Simplified Chinese Options copy avoids legacy mixed-language prose", () => {
  assert.equal(zh.options_title, "Herdr · 设置");
  assert.equal(zh.label_llm_url, "判定服务地址");
  assert.equal(zh.label_llm_key, "判定接口密钥");
  assert.equal(zh.placeholder_llm_model, "填写模型名称");

  const optionKeys = [
    "hint_url",
    "hint_locale",
    "hint_tick",
    "hint_fallback",
    "label_progress_template",
    "hint_progress_template",
    "hint_idle_nudge",
    "hint_llm_section",
    "llm_need_config",
    "llm_timeout",
    "hint_automation_mode",
    "hint_experimental_section",
    "hint_experimental_zai",
    "hint_experimental_deepseek",
    "connect_ok",
    "http_401",
  ];
  const visibleCopy = optionKeys.map((key) => zh[key]).join("\n");
  for (const legacyEnglish of [
    "Options",
    "background",
    "Bearer Token",
    "working",
    "Base URL",
    "Key + Model",
    "Project",
  ]) {
    assert.equal(visibleCopy.includes(legacyEnglish), false, `legacy mixed-language copy remains: ${legacyEnglish}`);
  }
});

test("Options no longer exposes or persists a Herdr bearer token", () => {
  assert.doesNotMatch(optionsHtml, /id="token"|HERDR_MCP_TOKEN|Bearer Token/);
  assert.doesNotMatch(optionsJs, /\$\("token"\)|cfg\.token|config\.token/);
  assert.match(zh.hint_url, /不保存 Herdr Token/);
});

test("Simplified Chinese editable automation prompts use Chinese prose", () => {
  const promptKeys = [
    "manual_status_continue_intro",
    "recovery_probe_template",
    "stale_view_activation_template",
    "default_wake_template",
    "default_progress_template",
    "default_partial_template",
    "default_llm_judge_prompt",
    "handoff_request_template",
    "handoff_seed_template",
  ];
  const prompts = promptKeys.map((key) => zh[key]).join("\n");
  for (const legacyEnglish of ["Agent", "mutation", "runtime", "Project", "handoff packet", "worker"]) {
    assert.equal(prompts.includes(legacyEnglish), false, `legacy English prose remains in prompt: ${legacyEnglish}`);
  }
});

test("Options waits for locale before showing fallback English copy", () => {
  assert.match(optionsHtml, /<html lang="en" class="i18n-pending">/);
  assert.match(optionsHtml, /\.i18n-pending body \{ visibility: hidden; \}/);
  assert.match(optionsJs, /classList\.remove\("i18n-pending"\)/);
});

test("LLM test booleans are localized instead of rendering true/false", () => {
  assert.equal(zh.boolean_yes, "是");
  assert.equal(zh.boolean_no, "否");
  assert.match(optionsJs, /t\(resp\.done \? "boolean_yes" : "boolean_no"\)/);
  assert.match(optionsJs, /t\(resp\.cont \? "boolean_yes" : "boolean_no"\)/);
});

test("Options requests optional host access only from explicit user settings", () => {
  assert.match(optionsJs, /chrome\.permissions\?\.request/);
  assert.match(optionsJs, /https:\/\/chat\.z\.ai\/\*/);
  assert.match(optionsJs, /https:\/\/chat\.deepseek\.com\/\*/);
  assert.match(optionsJs, /removeHostPermissions/);
  assert.equal(typeof zh.host_permission_denied, "string");
  assert.equal(typeof zh.host_permission_invalid_url, "string");
  assert.equal(typeof zh.hud_reason_llm_permission, "string");
});
