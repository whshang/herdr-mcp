import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const zh = JSON.parse(readFileSync(new URL("../extension/locales/zh.json", import.meta.url), "utf8"));
const optionsHtml = readFileSync(new URL("../extension/options.html", import.meta.url), "utf8");
const optionsJs = readFileSync(new URL("../extension/options.js", import.meta.url), "utf8");

test("Simplified Chinese Options copy avoids legacy mixed-language prose", () => {
  assert.equal(zh.options_title, "Herdr 浏览器设置");

  const optionKeys = [
    "hint_locale",
    "hint_tick",
    "hint_fallback",
    "label_progress_template",
    "hint_progress_template",
    "hint_idle_nudge",
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

test("Options keeps local Runtime secrets and transport details out of extension settings", () => {
  assert.doesNotMatch(optionsHtml, /id="token"|HERDR_MCP_TOKEN|Bearer Token/);
  assert.doesNotMatch(optionsJs, /\$\("token"\)|cfg\.token|config\.token/);
  assert.doesNotMatch(optionsHtml, /id="url"|id="pageAssistOrigins"/);
  assert.doesNotMatch(optionsJs, /herdrMcpUrl|pageAssistOrigins|hostPermissionPatternForUrl/);
  assert.match(optionsHtml, /~\/\.config\/herdr-mcp\/config\.json/);
  assert.match(optionsHtml, /"semantic"/);
  assert.match(optionsHtml, /"routes"/);
  assert.match(optionsHtml, /id="runtime_config_guide"/);
  assert.match(zh.options_runtime_config_hint, /不保存在扩展/);
  assert.match(zh.options_runtime_config_hint, /0600/);
});

test("Simplified Chinese editable automation prompts use Chinese prose", () => {
  const promptKeys = [
    "manual_status_continue_intro",
    "recovery_probe_template",
    "stale_view_activation_template",
    "default_wake_template",
    "default_progress_template",
    "default_partial_template",
    "handoff_request_template",
    "handoff_seed_template",
  ];
  const prompts = promptKeys.map((key) => zh[key]).join("\n");
  for (const legacyEnglish of ["Agent", "mutation", "runtime", "Project", "handoff packet", "worker"]) {
    assert.equal(prompts.includes(legacyEnglish), false, `legacy English prose remains in prompt: ${legacyEnglish}`);
  }
});

test("user configures semantic providers outside the extension | Given Runtime owns LLM and Jev routing | When Options is inspected | Then no provider credential controls remain", () => {
  for (const retired of [
    "llmJudgeBaseUrl",
    "llmJudgeApiKey",
    "llmJudgeModel",
    "jevJudgeBaseUrl",
    "jevJudgeApiKey",
    "jevJudgeModel",
    "testLlm",
    "testJev",
    "llmJudgePromptTemplate",
    "llmJudgeSkipKeywords",
    "jevJudgeMode",
    "jevJudgeThreshold",
  ]) {
    assert.doesNotMatch(optionsHtml, new RegExp(`id="${retired}"`));
  }
  for (const retired of [
    "llmJudgeBaseUrl",
    "llmJudgeApiKey",
    "llmJudgeModel",
    "jevJudgeBaseUrl",
    "jevJudgeApiKey",
    "jevJudgeModel",
    "h2w_test_llm",
    "h2w_test_jev",
  ]) {
    assert.doesNotMatch(optionsJs, new RegExp(retired));
  }
});

test("Options waits for locale before showing fallback English copy", () => {
  assert.match(optionsHtml, /<html lang="en" class="i18n-pending">/);
  assert.match(optionsHtml, /\.i18n-pending body \{ visibility: hidden; \}/);
  assert.match(optionsJs, /classList\.remove\("i18n-pending"\)/);
});

test("Options requests optional host access only from explicit user settings", () => {
  assert.match(optionsJs, /chrome\.permissions\?\.request/);
  assert.match(optionsJs, /https:\/\/chat\.z\.ai\/\*/);
  assert.match(optionsJs, /https:\/\/chat\.deepseek\.com\/\*/);
  assert.match(optionsJs, /https:\/\/gemini\.google\.com\/\*/);
  assert.match(optionsJs, /https:\/\/grok\.com\/\*/);
  assert.match(optionsHtml, /id="grokSiteAccess"/);
  assert.doesNotMatch(optionsHtml, /id="experimentalGrokEnabled"/);
  assert.doesNotMatch(optionsHtml, /id="pageAssistOrigins"/);
  assert.match(optionsJs, /removeHostPermissions/);
  assert.doesNotMatch(optionsJs, /llmJudge|jevJudge/);
  assert.equal(typeof zh.host_permission_denied, "string");
});
