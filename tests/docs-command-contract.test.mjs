import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (p) => readFileSync(path.join(ROOT, p), "utf8");

const README_PRIMARY = [
  ["README.md", "## Fastest path to a working setup"],
  ["README.zh.md", "## 最短安装路径"],
  ["README.ja.md", "## 最短セットアップ"],
];

const INSTALL_PRIMARY = [
  ["docs/i18n/en/install.md", "## Step 1: install the native runtime (primary)"],
  ["docs/i18n/zh-CN/install.md", "## 第一步：安装原生 runtime（主路径）"],
];

const QUICK_START_RUNTIME = [
  ["docs/i18n/en/quick-start.md", "## 1. Install the local runtime"],
  ["docs/i18n/zh-CN/quick-start.md", "## 2. 安装 herdr-mcp"],
];

function sectionAfterHeading(doc, heading) {
  const start = doc.indexOf(heading);
  assert.ok(start >= 0, `missing heading: ${heading}`);
  const after = doc.slice(start + heading.length);
  const next = after.search(/\n## /);
  return next >= 0 ? after.slice(0, next) : after;
}

function assertNotNodeRuntimePrimary(rel, section) {
  assert.doesNotMatch(
    section,
    /^\s*npm (ci|install)\b/m,
    `${rel} primary install must not require npm to run the MCP runtime`
  );
  assert.doesNotMatch(
    section,
    /^\s*node dist\/server\.js\b/m,
    `${rel} primary install must not run node dist/server.js`
  );
  assert.doesNotMatch(
    section,
    /^\s*herdr-mcp service install\b/m,
    `${rel} primary install must not use service install`
  );
}

test("README primary install path is native binary, not Node or service install", () => {
  for (const [rel, heading] of README_PRIMARY) {
    const section = sectionAfterHeading(read(rel), heading);
    assert.match(
      section,
      /GitHub Releases|github\.com\/whshang\/herdr-mcp\/releases/i,
      `${rel} primary path must point at GitHub Releases`
    );
    assert.match(
      section,
      /herdr-mcp doctor/,
      `${rel} primary path must document herdr-mcp doctor`
    );
    assert.match(
      section,
      /do \*\*not\*\* need Node|不需要.*Node|Node\.js \/ npm は\*\*不要\*\*/i,
      `${rel} must state Node is not required for the local MCP runtime`
    );
    assertNotNodeRuntimePrimary(rel, section);
  }
});

test("install.md primary step is native binary, not Node or service install", () => {
  for (const [rel, heading] of INSTALL_PRIMARY) {
    const section = sectionAfterHeading(read(rel), heading);
    assert.match(section, /GitHub Releases|github\.com\/whshang\/herdr-mcp\/releases/i);
    assert.match(section, /herdr-mcp doctor/);
    assert.match(section, /herdr-mcp status/);
    assertNotNodeRuntimePrimary(rel, section);
  }
});

test("quick-start runtime section rejects Node runtime and service install primary", () => {
  for (const [rel, heading] of QUICK_START_RUNTIME) {
    const section = sectionAfterHeading(read(rel), heading);
    assert.match(section, /herdr-mcp doctor/);
    assertNotNodeRuntimePrimary(rel, section);
  }
});

test("G20 archived notes keep the known CLI FAIL list for remaining mismatches", () => {
  const wip = read("docs/history/ga/g20-command-contract.md");
  assert.match(wip, /G1 debt/);
  assert.match(wip, /herdr-mcp start/);
  assert.match(wip, /herdr-mcp watchdog/);
  assert.match(wip, /herdr-mcp lang/);
  assert.match(wip, /herdr-mcp connector/);
  assert.match(wip, /cli-reference\.md/);
  assert.match(wip, /agent-install\.md/);
});
