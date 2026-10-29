// ChatGPT (2026-10) no longer renders [data-message-author-role="user"]; user
// turns are div[data-user-message-bubble="true"] with the text in an inner
// .whitespace-pre-wrap node (#729). These tests exercise the shipped wake.js
// user-turn helpers and the ChatGPT adapter against both DOM generations.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const wakeSource = readFileSync(new URL("../extension/content/wake.js", import.meta.url), "utf8");
const adapterSource = readFileSync(new URL("../extension/content/injector/chatgpt.js", import.meta.url), "utf8");

function el({ tag = "div", attrs = {}, classes = [], text = "", children = [] } = {}) {
  const node = {
    tagName: tag.toUpperCase(),
    attrs: { ...attrs },
    classes: new Set(classes),
    ownText: text,
    children: [],
    parent: null,
    getAttribute(name) { return Object.hasOwn(this.attrs, name) ? this.attrs[name] : null; },
    get textContent() { return this.ownText + this.children.map((child) => child.textContent).join(""); },
    get innerText() { return this.textContent; },
    matches(selector) { return selector.split(",").some((part) => matchesSimple(this, part.trim())); },
    closest(selector) {
      for (let current = this; current; current = current.parent) if (current.matches(selector)) return current;
      return null;
    },
    querySelectorAll(selector) {
      const out = [];
      const walk = (parent) => {
        for (const child of parent.children) {
          if (child.matches(selector)) out.push(child);
          walk(child);
        }
      };
      walk(this);
      return out;
    },
    querySelector(selector) { return this.querySelectorAll(selector)[0] || null; },
  };
  for (const child of children) { child.parent = node; node.children.push(child); }
  return node;
}

function matchesSimple(node, selector) {
  let rest = selector;
  if (!rest) return false;
  const tag = rest.match(/^[A-Za-z][A-Za-z0-9-]*/);
  if (tag) {
    if (node.tagName !== tag[0].toUpperCase()) return false;
    rest = rest.slice(tag[0].length);
  }
  while (rest) {
    const cls = rest.match(/^\.([A-Za-z0-9_-]+)/);
    if (cls) {
      if (!node.classes.has(cls[1])) return false;
      rest = rest.slice(cls[0].length);
      continue;
    }
    const attr = rest.match(/^\[([A-Za-z0-9_-]+)(?:(\^?=)"([^"]*)")?\]/);
    if (attr) {
      const value = node.getAttribute(attr[1]);
      if (value === null) return false;
      if (attr[3] !== undefined && (attr[2] === "^=" ? !value.startsWith(attr[3]) : value !== attr[3])) return false;
      rest = rest.slice(attr[0].length);
      continue;
    }
    return false;
  }
  return true;
}

function documentOf(root) {
  return { querySelectorAll: (selector) => root.querySelectorAll(selector), querySelector: (s) => root.querySelector(s) };
}

const HELPERS_START = "  // ---- ChatGPT user-turn selectors ----";
const HELPERS_END = "\n  function observedConversationTurns()";

function loadWakeHelpers(document) {
  const start = wakeSource.indexOf(HELPERS_START);
  const end = wakeSource.indexOf(HELPERS_END, start);
  assert.ok(start >= 0 && end > start, "the user-turn helpers must stay extractable from wake.js");
  const body = `${wakeSource.slice(start, end)}
  ({ conversationTurnRole, userTurnTextRoot, conversationTurnNodesForRole });`;
  return vm.runInNewContext(body, { document });
}

function bubble(text, attrs = {}) {
  return el({
    attrs: { "data-user-message-bubble": "true", ...attrs },
    children: [el({ classes: ["text-size-chat", "whitespace-pre-wrap"], text })],
  });
}

test("user turn is found in the 2026-10 ChatGPT DOM | Given only data-user-message-bubble user turns | When wake.js looks up user turns | Then the bubbles are user turns and text comes from the inner pre-wrap node", () => {
  const first = bubble("first question");
  const latest = bubble("latest question");
  const assistant = el({ attrs: { "data-message-author-role": "assistant" }, text: "answer" });
  const root = el({
    children: [
      el({ attrs: { "data-content-search-unit-key": "a:user" }, children: [first] }),
      assistant,
      el({ attrs: { "data-content-search-unit-key": "b:user" }, children: [latest] }),
    ],
  });
  const helpers = loadWakeHelpers(documentOf(root));
  const users = helpers.conversationTurnNodesForRole("user");
  assert.equal(users.length, 2);
  assert.equal(users[1], latest);
  assert.equal(helpers.conversationTurnRole(latest), "user");
  assert.equal(helpers.conversationTurnRole(assistant), "assistant");
  assert.equal(helpers.userTurnTextRoot(latest).textContent, "latest question");
  assert.equal(helpers.userTurnTextRoot(assistant), assistant, "non-bubble roots are unchanged");
  assert.equal(helpers.conversationTurnNodesForRole("assistant").length, 1);
});

test("user turn keeps the legacy selector first | Given a legacy author-role user turn that also wraps a bubble | When wake.js counts user turns | Then the turn is counted once via the legacy node", () => {
  const nested = bubble("legacy text");
  const legacy = el({ attrs: { "data-message-author-role": "user" }, children: [nested] });
  const helpers = loadWakeHelpers(documentOf(el({ children: [legacy] })));
  const users = helpers.conversationTurnNodesForRole("user");
  assert.equal(users.length, 1);
  assert.equal(users[0], legacy);
  assert.equal(helpers.conversationTurnRole(nested), null, "a covered bubble is not a second user turn");
});

function loadAdapter(document) {
  let adapter;
  class BaseAdapter { elementVisible() { return true; } }
  vm.runInNewContext(adapterSource, {
    BaseAdapter,
    registerH2WAdapter: (registered) => { adapter = registered; },
    document,
    location: { origin: "https://chatgpt.com", pathname: "/c/current" },
    AbortController,
    setTimeout,
    clearTimeout,
  }, { filename: "chatgpt.js" });
  return adapter;
}

test("user App keywords are read from the bubble DOM | Given only data-user-message-bubble user turns | When the adapter reads the latest user App keywords | Then the latest bubble's App pill is reported", () => {
  const pill = el({ attrs: { "data-inline-selection-pill": "", "data-symbol": "ecosystemMention", "data-keyword": "Herdr" } });
  const older = bubble("older");
  const latest = el({ attrs: { "data-user-message-bubble": "true" }, children: [pill] });
  const adapter = loadAdapter(documentOf(el({ children: [older, latest] })));
  assert.deepEqual([...adapter.getLatestUserAppKeywords()], ["herdr"]);
});

test("user accepted App identity resolves through a bubble carrier | Given a data-message-id carrier holding a bubble with an App mention | When the accepted identity is read | Then the provider mention is reported", () => {
  const mention = el({ tag: "span", attrs: { "data-appearance": "inline-mention", "app-mention-name": "herdr", "app-mention-path": "connectors/herdr" } });
  const carrier = el({ attrs: { "data-message-id": "u1" }, children: [el({ attrs: { "data-user-message-bubble": "true" }, children: [mention] })] });
  const adapter = loadAdapter(documentOf(el({ children: [carrier] })));
  const identity = [...adapter.getAcceptedUserAppIdentity("u1")].map((app) => ({ name: app.name, path: app.path }));
  assert.deepEqual(identity, [{ name: "herdr", path: "connectors/herdr" }]);
});

function loadBrowserPerformance() {
  const context = vm.createContext({ console, Date, Math });
  const source = readFileSync(new URL("../extension/performance-core.js", import.meta.url), "utf8");
  new vm.Script(source, { filename: "performance-core.js" }).runInContext(context);
  return context.H2W_BROWSER_PERFORMANCE;
}

// Minimal Node-like tree for the sampler, which walks nodeType/childNodes.
function textNode(value) { return { nodeType: 3, nodeValue: value, parentElement: null }; }
function elementNode(children = [], attrs = {}) {
  const node = {
    nodeType: 1,
    childNodes: children,
    parentElement: null,
    getAttribute(name) { return Object.hasOwn(attrs, name) ? attrs[name] : null; },
    closest() { return null; },
    querySelector(selector) {
      const wantsInlineMention = selector.includes('[data-appearance="inline-mention"]');
      const wantsLegacyPill = selector.includes("[data-inline-selection-pill]");
      const stack = [...children];
      while (stack.length > 0) {
        const child = stack.shift();
        if (child?.nodeType !== 1) continue;
        const legacy = child.getAttribute("data-inline-selection-pill") !== null
          && child.getAttribute("data-keyword") !== null;
        const mention = child.getAttribute("data-appearance") === "inline-mention"
          && child.getAttribute("app-mention-name") !== null
          && child.getAttribute("app-mention-path") !== null;
        if ((wantsLegacyPill && legacy) || (wantsInlineMention && mention)) return child;
        stack.push(...child.childNodes);
      }
      return null;
    },
  };
  for (const child of children) child.parentElement = node;
  return node;
}

test("user App mention in a bubble is observed as the model sees it | Given a user bubble whose text holds an inline-mention App span | When the model-visible sampler reads the bubble | Then the mention is emitted as @name with single spacing", () => {
  const perf = loadBrowserPerformance();
  const mention = elementNode([textNode("herdr-mcp")], {
    "data-appearance": "inline-mention",
    "app-mention-name": "herdr-mcp",
    "app-mention-path": "connectors/herdr-mcp",
  });
  const textRoot = elementNode([mention, textNode("用 agent.task.dispatch 派发一个任务给 report-review：回复 OK")]);
  assert.equal(
    perf.sampleChatGptModelMessageText(textRoot).text,
    "@herdr-mcp 用 agent.task.dispatch 派发一个任务给 report-review：回复 OK",
  );

  // A span without the provider path is plain text, never an App identity.
  const lookalike = elementNode([textNode("herdr-mcp")], {
    "data-appearance": "inline-mention",
    "app-mention-name": "herdr-mcp",
  });
  const plainRoot = elementNode([lookalike, textNode(" 用 dispatch")]);
  assert.equal(perf.sampleChatGptModelMessageText(plainRoot).text, "herdr-mcp 用 dispatch");
});

// #754: the composer chip and the accepted bubble chip can render one App
// mention differently. Before the fix the capture check compared the two
// raw samples with a 160-char containment test, so a mismatch silently
// skipped source_turn.observe and dispatch later reported source_turn_not_found.
const legacyCaptureCheck = (expected, bubble) => {
  const norm = (value) => String(value || "").replace(/\s+/g, " ").trim();
  const e = norm(expected);
  const b = norm(bubble);
  return Boolean(e && b) && (b.includes(e.slice(0, 160)) || e.includes(b.slice(0, 160)));
};
const MESSAGE = "用 agent.task.dispatch 派发一个任务给 report-review：回复 OK 即可，不要改任何文件";

test("user manual @herdr-mcp send is captured when composer and bubble chips differ | Given a composer App chip and an accepted bubble chip rendered with different names or labels | When the submit-to-bubble capture check runs | Then the turn is confirmed while different text is still refused", () => {
  const perf = loadBrowserPerformance();
  const composerChip = elementNode([textNode("\uFEFFherdr-mcp\uFEFF")], {
    "data-appearance": "inline-mention",
    "app-mention-name": "herdr",
    "app-mention-path": "connectors/herdr",
  });
  const composer = perf.sampleChatGptModelMessageText(elementNode([composerChip, textNode(`\u00A0${MESSAGE}`)]));
  assert.equal(composer.text, `@herdr ${MESSAGE}`);
  assert.deepEqual(JSON.parse(JSON.stringify(composer.app_mentions)), [{ name: "herdr", label: "herdr-mcp" }]);

  const bubbles = {
    // Bubble chip without a provider path: sampled as its visible label.
    pathless: elementNode([elementNode([textNode("herdr-mcp")], {
      "data-appearance": "inline-mention",
      "app-mention-name": "herdr-mcp",
    }), textNode(`  ${MESSAGE}`)]),
    // Bubble chip carrying a different provider name than the composer chip.
    renamed: elementNode([elementNode([textNode("herdr-mcp")], {
      "data-appearance": "inline-mention",
      "app-mention-name": "herdr-mcp",
      "app-mention-path": "connectors/herdr-mcp",
    }), textNode(` ${MESSAGE}`)]),
  };
  for (const [kind, root] of Object.entries(bubbles)) {
    const bubble = perf.sampleChatGptModelMessageText(root);
    const mentions = [...composer.app_mentions, ...(bubble.app_mentions || [])];
    assert.equal(legacyCaptureCheck(composer.text, bubble.text), false, `${kind}: the pre-#754 check dropped this turn`);
    assert.equal(perf.sourceTurnTextConfirmsPending(composer.text, bubble.text, mentions), true, `${kind}: confirmed`);
  }

  assert.equal(perf.sourceTurnTextConfirmsPending(composer.text, `@herdr-mcp 用 dispatch 派发别的任务`, composer.app_mentions), false);
  assert.equal(perf.sourceTurnTextConfirmsPending("@herdr_mcp", "@herdr-mcp", composer.app_mentions), false, "mention-only never confirms through the loose form");
  assert.equal(perf.sourceTurnTextConfirmsPending("你好，请只回复 OK", "你好，请只回复 OK", []), true, "plain text keeps the exact check");
});

test("user source turn text matches the Runtime digest of current_user_message | Given an accepted bubble with a Herdr App chip under any name and another App chip | When the source-turn text is sampled with Herdr mentions omitted | Then only the Herdr mention is dropped", () => {
  const perf = loadBrowserPerformance();
  const herdrChip = elementNode([textNode("herdr-mcp")], {
    "data-appearance": "inline-mention",
    "app-mention-name": "connector_68abc",
    "app-mention-path": "connectors/connector_68abc",
  });
  const otherChip = elementNode([textNode("GitHub")], {
    "data-appearance": "inline-mention",
    "app-mention-name": "github",
    "app-mention-path": "connectors/github",
  });
  const root = elementNode([herdrChip, textNode(` ${MESSAGE} `), otherChip, textNode(" 看看")]);
  const isHerdr = ({ name, label }) => [name, label].some((value) => ["herdr", "herdr-mcp", "herdr_mcp"].includes(String(value).toLowerCase()));
  const sample = perf.sampleChatGptModelMessageText(root, { omitAppMention: isHerdr });
  assert.equal(sample.text.replace(/\s+/g, " "), `${MESSAGE} @github 看看`);
  // The Runtime drops "@herdr-mcp" from current_user_message before hashing,
  // so the model's copy and this observed text canonicalize to one digest.
  const runtimeCanonical = (text) => text.split(/\s+/).filter((token) => token && !/^@(herdr|herdr-mcp|herdr_mcp)$/i.test(token)).join(" ");
  assert.equal(runtimeCanonical(sample.text), runtimeCanonical(`@herdr-mcp ${MESSAGE} @github 看看`));

  const wakeSource = readFileSync(new URL("../extension/content/wake.js", import.meta.url), "utf8");
  assert.match(wakeSource, /BROWSER_PERFORMANCE\.sourceTurnTextConfirmsPending\(expectedUser, bubbleText, mentions\)/);
  assert.match(wakeSource, /pendingUserTextMatches\(expectedUser, comparableCurrentUser, currentUser\.appMentions\)/);
  assert.match(wakeSource, /userText: currentUser\.sourceTurnText \|\| currentUserText/);
});
