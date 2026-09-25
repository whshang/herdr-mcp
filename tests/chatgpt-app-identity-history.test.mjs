import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { gunzipSync } from "node:zlib";

const SOURCE_FILES = {
  background: "extension/background.js",
  wake: "extension/content/wake.js",
  adapter: "extension/content/injector/chatgpt.js",
};

const FIXTURE_ROOT = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "fixtures",
  "chatgpt-app-identity-history",
);

const HISTORICAL_SOURCE = JSON.parse(gunzipSync(
  fs.readFileSync(path.join(FIXTURE_ROOT, "history.json.gz")),
).toString("utf8"));

function historicalSource(version) {
  const source = HISTORICAL_SOURCE[version];
  assert.ok(source, `missing historical fixture for ${version}`);
  return {
    background: source.background,
    wake: source.wake,
    adapter: source.adapter,
  };
}

function currentSource() {
  return Object.fromEntries(Object.entries(SOURCE_FILES).map(([key, filePath]) => [
    key,
    fs.readFileSync(filePath, "utf8"),
  ]));
}

function sourceSlice(source, startMarker, endMarker) {
  const start = source.indexOf(startMarker);
  const end = source.indexOf(endMarker, start + 1);
  return start >= 0 && end > start ? source.slice(start, end) : "";
}

function manifestVersion(version) {
  return HISTORICAL_SOURCE[version]?.version;
}

const probes = {
  initial_learning(source) {
    return source.adapter.includes("getLatestUserAppKeywords")
      && source.wake.includes("browserAppKeywords")
      && source.background.includes("browserAppKeywords.length === 1")
      && source.background.includes("b.herdr_app_keyword = browserAppKeywords[0]");
  },

  late_binding_learning(source) {
    return source.wake.includes("registeredConversationBound")
      && source.wake.includes("shouldLearnAppIdentity")
      && source.wake.includes('registerCurrentConversation(shouldLearnAppIdentity ? "app-identity" : "poll")')
      && source.wake.includes('registerCurrentConversation("binding-changed")');
  },

  queue_keyword_no_unknown_guess(source) {
    const helper = sourceSlice(
      source.background,
      "function bindingRequiredApps(binding)",
      "function primaryBindingForConv",
    );
    const queue = sourceSlice(
      source.wake,
      'if (msg?.type === "h2w_queue_deliver")',
      'if (msg?.type === "h2w_wake")',
    );
    return helper.includes("return keyword ? [keyword] : []")
      && !helper.includes('|| "herdr"')
      && queue.includes("requiredApps:")
      && source.adapter.includes("getComposerAppCandidates(keyword)")
      && source.adapter.includes("[data-keyword], [data-value]");
  },

  existing_turn_no_fixed_name(source) {
    const helper = sourceSlice(
      source.wake,
      "function currentHerdrRequiredApps()",
      "async function currentAdapterProjectIdentity",
    );
    return helper.includes("if (!registeredHerdrAppKeyword) return []")
      && !helper.includes('|| "herdr"')
      && !source.wake.includes('registeredHerdrAppKeyword || "herdr"');
  },

  queue_resume_handoff_inherit(source) {
    const wake = sourceSlice(source.wake, "async function performWake(data)", "// ---- Delivery confirmation");
    const resume = sourceSlice(wake, "if (resumeOnly) {", "if (clearBeforeInsert) await clearComposer();");
    const enqueue = sourceSlice(
      source.wake,
      "async function queueCurrentComposerMessage()",
      "function ensureQueuedInsertButton",
    );
    return enqueue.includes("selectedAppsBeforeQueue")
      && enqueue.includes("ensureRequiredComposerApps(selectedAppsBeforeQueue)")
      && resume.includes("ensureRequiredComposerApps(requiredApps)")
      && resume.indexOf("ensureRequiredComposerApps(requiredApps)") < resume.indexOf("await submit()")
      && source.background.includes("function learnedBindingRequiredApps(bindings)")
      && source.background.includes("async function handoffMessageWithRequiredApps")
      && source.background.includes("createParams = { ...params, required_apps: inheritedRequiredApps }")
      && source.background.includes("targetRow.herdr_app_keyword = inheritedAppKeyword");
  },

  fresh_no_guess_queue_text_and_shared_wake(source) {
    const enqueue = sourceSlice(
      source.wake,
      "async function queueCurrentComposerMessage()",
      "function ensureQueuedInsertButton",
    );
    const queue = sourceSlice(
      source.wake,
      'if (msg?.type === "h2w_queue_deliver")',
      'if (msg?.type === "h2w_wake")',
    );
    const wake = sourceSlice(source.wake, "async function performWake(data)", "// ---- Delivery confirmation");
    return !source.wake.includes("defaultChatGptApps")
      && source.adapter.includes("getComposerTextWithoutAppPills()")
      && enqueue.includes("getComposerTextWithoutAppPills")
      && queue.includes("queuedSelectedApps")
      && queue.includes("composerHasOnlyAppPills(queuedSelectedApps)")
      && wake.includes("observedComposerApps")
      && wake.includes("learnedHerdrApps");
  },

  picker_scope_and_search(source) {
    return source.adapter.includes(
      "const menuRoots = [...document.querySelectorAll('.popover, [role=\"menu\"], [role=\"listbox\"]')]",
    )
      && source.adapter.includes("for (const root of menuRoots)")
      && source.wake.includes("!composerModelVisibleText()")
      && source.wake.includes("const search = selector ? await insertMainWorld(app, selector) : null")
      && source.wake.includes("if (searchInserted) await clearComposer()");
  },

  manual_semantic_binding_identity(source) {
    const manual = sourceSlice(
      source.background,
      "async function manualLlmJudgeContinue(",
      "// ---- Conversation handoff",
    );
    return manual.includes("const bindings = await loadBindings()")
      && manual.includes("const binding = primaryBindingForConv(bindings, convKey)")
      && manual.includes("requiredApps: bindingRequiredApps(binding)");
  },
};

// Scenario table keeps the historical SHAs for documentation; fixtures are keyed by version.
// See tests/fixtures/chatgpt-app-identity-history/provenance.json.
const cases = [
  ["initial_learning", "991569b3", "0.1.121", "fd5d89ef", "0.1.122",
    "user keeps the real App identity | Given a bound ChatGPT turn with one provider-owned pill | When identity is first learned | Then the exact keyword is persisted instead of a configured display name"],
  ["late_binding_learning", "fd5d89ef", "0.1.122", "e3d9ffa0", "0.1.123",
    "user keeps the App after late binding | Given binding completes before the first App mention | When the next accepted user turn exposes one pill | Then the watcher learns it without another route change"],
  ["queue_keyword_no_unknown_guess", "e3d9ffa0", "0.1.123", "b1be2d13", "0.1.124",
    "user gets no synthetic Queue App | Given a binding without a learned keyword | When Queue and App re-selection run | Then unknown identity is empty and provider keyword evidence is used"],
  ["existing_turn_no_fixed_name", "b1be2d13", "0.1.124", "d5b0a78e", "0.1.125",
    "user never gets a guessed App on an existing chat | Given no learned keyword | When recovery or another generated turn runs | Then no fixed herdr fallback is synthesized"],
  ["queue_resume_handoff_inherit", "d5b0a78e", "0.1.125", "79a39bbf", "0.1.126",
    "user keeps the App across Queue resume and handoff | Given one learned source identity | When composer clearing retry or session migration occurs | Then that identity is restored or inherited before submit"],
  ["fresh_no_guess_queue_text_and_shared_wake", "79a39bbf", "0.1.126", "73c55dfb", "0.1.127",
    "user keeps App state separate from draft text | Given Queue or fresh session creation | When generated delivery runs | Then pills are attachment evidence and no fresh-session name is guessed"],
  ["picker_scope_and_search", "73c55dfb", "0.1.127", "c42b420a", "0.1.128",
    "user gets one bounded App restore | Given the recent App menu does not list the learned keyword | When re-selection runs | Then only the visible composer menu is searched and ambiguity fails closed"],
  ["manual_semantic_binding_identity", "c42b420a", "0.1.128", "2804d821", "0.1.129",
    "user keeps the App on manual semantic Continue | Given the content script has just reloaded | When background sends a JEV or LLM Continue turn | Then the authoritative binding identity is carried explicitly"],
].map(([name, brokenRef, brokenVersion, fixedRef, fixedVersion, title]) => ({
  name, brokenRef, brokenVersion, fixedRef, fixedVersion, title,
}));

const current = currentSource();

for (const scenario of cases) {
  test(scenario.title, () => {
    assert.equal(manifestVersion(scenario.brokenVersion), scenario.brokenVersion);
    assert.equal(manifestVersion(scenario.fixedVersion), scenario.fixedVersion);

    const probe = probes[scenario.name];
    assert.equal(
      probe(historicalSource(scenario.brokenVersion)),
      false,
      `${scenario.brokenVersion} must reproduce the historical boundary before its fix`,
    );
    assert.equal(
      probe(historicalSource(scenario.fixedVersion)),
      true,
      `${scenario.fixedVersion} must contain the boundary fix recorded for that release`,
    );
    assert.equal(
      probe(current),
      true,
      "the current authority tree must keep the historical boundary closed",
    );
  });
}
