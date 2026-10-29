// #740: an account-level ChatGPT conversation must not be attributed to a
// Project because its sidebar label happens to contain the Project's name.
// The real currentChatGptProjectFromCatalog is sliced out of
// extension/content/wake.js and run against a fake location and anchor.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const wakeSource = readFileSync(new URL("../extension/content/wake.js", import.meta.url), "utf8");
const start = wakeSource.indexOf("  function currentChatGptProjectFromCatalog(");
const end = wakeSource.indexOf("\n  function currentHerdrRequiredApps(", start);
assert.ok(start >= 0 && end > start, "currentChatGptProjectFromCatalog must remain extractable");
const source = wakeSource.slice(start, end);

const PROJECT_ID = "g-p-0123456789abcdef0123456789abcdef";
const OTHER_ID = "g-p-fedcba9876543210fedcba9876543210";
const CATALOG = [
  { id: PROJECT_ID, name: "herdr-mcp" },
  { id: OTHER_ID, name: "Other" },
];

function infer({ href, anchor }) {
  return new Function("location", "anchor", `
    const chatGptCurrentConversationAnchor = () => anchor;
    ${source}
    return currentChatGptProjectFromCatalog;
  `)({ href }, anchor)(CATALOG);
}

function anchor(href, ariaLabel) {
  return {
    href,
    getAttribute: (name) => (name === "href" ? href : name === "aria-label" ? ariaLabel : null),
  };
}

test("user account chat is not claimed by a Project whose name is in its label | Given an account-level /c/ chat whose sidebar label contains a Project name | When the current Project is inferred | Then no Project is attributed", () => {
  const project = infer({
    href: "https://chatgpt.com/c/6ae3235f-0000-0000-0000-000000000000",
    anchor: anchor("/c/6ae3235f-0000-0000-0000-000000000000", "herdr-mcp 派发测试"),
  });
  assert.equal(project, null);
});

test("user project chat keeps its Project from an exact id | Given a conversation whose route or sidebar href carries a Project id | When the current Project is inferred | Then only the exactly matching catalog Project is returned", () => {
  assert.deepEqual(
    infer({ href: `https://chatgpt.com/g/${PROJECT_ID}-herdr-mcp/c/abc`, anchor: null }),
    CATALOG[0],
  );
  assert.deepEqual(
    infer({
      href: "https://chatgpt.com/c/abc",
      anchor: anchor(`/g/${OTHER_ID}-other/c/abc`, "herdr-mcp chat"),
    }),
    CATALOG[1],
  );
  assert.equal(
    infer({ href: "https://chatgpt.com/g/g-p-99999999999999999999999999999999/c/abc", anchor: null }),
    null,
  );
});
