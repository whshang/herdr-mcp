import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (p) => readFileSync(path.join(ROOT, p), "utf8");

const README_PRIMARY = [
  ["README.md", "## Agent-first setup"],
  ["README.zh.md", "## Agent-first 安装"],
  ["README.ja.md", "## Agent-first セットアップ"],
];

const INSTALL_PRIMARY = [
  ["docs/i18n/en/install.md", "## Step 1: install the native herdr-mcp runtime"],
  ["docs/i18n/zh-CN/install.md", "## 第一步：安装原生 herdr-mcp runtime"],
];

const QUICK_START_POST_INSTALL = [
  ["docs/i18n/en/quick-start.md", /this page starts after herdr-mcp is installed and connected/i, /## 1\. Start with a read-only check/],
  ["docs/i18n/zh-CN/quick-start.md", /本页从“herdr-mcp 已安装并连接”开始/, /## 1\. 先做一次只读检查/],
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

test("quick-start starts after installation instead of duplicating the install runbook", () => {
  for (const [rel, role, firstTask] of QUICK_START_POST_INSTALL) {
    const doc = read(rel);
    assert.match(doc, role, `${rel} must explicitly start after installation`);
    assert.match(doc, firstTask, `${rel} must lead with a real read-only task`);
    assert.match(doc, /agent-install\.md/);
    assert.match(doc, /install\.md/);
    assert.doesNotMatch(doc, /## .*Install the local runtime|## .*安装 herdr-mcp/i);
    assert.doesNotMatch(doc, /herdr-mcp dev sync|wrangler deploy|GitHub Releases/i);
  }
});

test("release model keeps publication and ownership boundaries explicit", () => {
  const model = read("docs/release-model.md");
  assert.match(model, /workflow_dispatch/);
  assert.match(model, /tag push/i);
  assert.match(model, /STORE/);
  assert.match(model, /STANDALONE/);
  assert.match(model, /DEV/);
});

test("v0.4.3 source-development docs expose DEV/PROD dogfood without the old npm rebuild path", () => {
  for (const rel of ["README.md", "README.zh.md", "README.ja.md"]) {
    const doc = read(rel);
    assert.match(doc, /herdr-mcp dev status/, `${rel} must expose read-only DEV provenance`);
    assert.match(doc, /herdr-mcp dev sync/, `${rel} must expose the explicit DEV activation path`);
    assert.match(doc, /herdr-mcp dev rollback/, `${rel} must expose the pinned PROD rollback path`);
  }

  for (const rel of ["docs/i18n/en/cli-reference.md", "docs/i18n/zh-CN/cli-reference.md"]) {
    const doc = read(rel);
    assert.match(doc, /herdr-mcp dev status/);
    assert.match(doc, /herdr-mcp dev sync --dry-run/);
    assert.match(doc, /herdr-mcp dev rollback/);
    assert.doesNotMatch(doc, /npm run build && herdr-mcp restart/);
  }
});

test("workstation_offline docs preserve layered self-healing and delivery-state safety", () => {
  const cases = [
    ["docs/i18n/en/troubleshooting.md", /2 seconds/, /300 seconds/, /delivery_state=not_delivered/, /retry_after_ms=5000/, /browser extension does not make this decision/],
    ["docs/i18n/zh-CN/troubleshooting.md", /2 秒/, /300 秒/, /not_delivered/, /retry_after_ms=5000/, /浏览器扩展不参与这个错误判定/],
  ];
  for (const [rel, edgeGrace, localRecycle, notDelivered, retryAfter, extensionBoundary] of cases) {
    const doc = read(rel);
    assert.match(doc, edgeGrace, `${rel} must document bounded Edge reconnect grace`);
    assert.match(doc, localRecycle, `${rel} must document prolonged local Link recycle`);
    assert.match(doc, notDelivered, `${rel} must document confirmed non-delivery`);
    assert.match(doc, retryAfter, `${rel} must document machine-readable retry timing`);
    assert.match(doc, extensionBoundary, `${rel} must keep browser extension outside the offline decision path`);
  }
});

test("README continuity path stays no-ID and fail-closed across languages", () => {
  const cases = [
    ["README.md", /simply say \*\*“continue”\*\* or \*\*“resume”\*\*/, /without supplying an internal continuity ID/, /Recency or text similarity alone never selects a chain/],
    ["README.zh.md", /直接说 \*\*“继续” \/ “接着上次”\*\*/, /不需要提供内部 `continuity_id`/, /不会因为“最近一次”或“文字最像”就直接猜/],
    ["README.ja.md", /\*\*“continue” \/ “resume”\*\*/, /内部 `continuity_id` を入力せず/, /最新・文字類似だけで chain を選びません/],
  ];
  for (const [rel, intent, noId, failClosed] of cases) {
    const doc = read(rel);
    assert.match(doc, intent, `${rel} must expose the manual continue/resume path`);
    assert.match(doc, noId, `${rel} must say an internal continuity ID is not required`);
    assert.match(doc, failClosed, `${rel} must keep ambiguous selection fail-closed`);
  }
});
