import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

const root = process.cwd();
const manifest = JSON.parse(await readFile(join(root, "extension/manifest.json"), "utf8"));
const identity = JSON.parse(await readFile(join(root, "contracts/browser-extension-store.json"), "utf8"));
const listing = JSON.parse(await readFile(join(root, "contracts/browser-extension-store-listing.json"), "utf8"));
const store = listing.chrome_web_store;

const STORE_ID = identity.chrome_web_store.extension_id;

test("Store listing SSOT matches the production extension identity and manifest", () => {
  assert.equal(store.extension_id, STORE_ID);
  assert.equal(store.product_name, manifest.name);
  assert.equal(store.product_name, "Herdr MCP - Herdr Workspace Connector");
  assert.doesNotMatch(store.product_name, /—/);
  assert.equal(store.summary, manifest.description);
  assert.match(store.item_url, new RegExp(`${STORE_ID}$`));
  assert.match(store.homepage_url, /^https:\/\/whshang\.github\.io\/herdr-mcp\/?/);
  assert.equal(store.support_url, "https://github.com/whshang/herdr-mcp/issues");
  assert.match(store.privacy_policy_url, /\/privacy\.html$/);
});

test("Store copy describes the production safety and experimental boundaries", () => {
  assert.match(store.description, /Native Messaging/i);
  assert.match(store.description, /Wake the bound Web AI/i);
  assert.match(store.description, /inspect, integrate, and verify/i);
  assert.match(store.description, /429/);
  assert.match(store.description, /chat\.z\.ai/i);
  assert.match(store.description, /chat\.deepseek\.com/i);
  assert.match(store.description, /disabled by default/i);
  assert.match(store.review_notes, /OFF by default/i);
  assert.match(store.review_notes, /No remote code/i);
  assert.doesNotMatch(store.product_name, /Web wake/i);
});

test("Store package workflow prepares every main extension update without publishing it", async () => {
  const workflow = await readFile(join(root, ".github/workflows/extension-store.yml"), "utf8");
  assert.match(workflow, /branches: \[main\]/);
  assert.match(workflow, /'extension\/\*\*'/);
  for (const suite of [
    "browser-control-plane.test.mjs",
    "chatgpt-artifact-capture.test.mjs",
    "continuity-journal.test.mjs",
    "extension-native-host.test.mjs",
    "extension-recovery.test.mjs",
    "options-i18n.test.mjs",
    "queued-insert.test.mjs",
  ]) {
    assert.match(workflow, new RegExp(suite.replaceAll(".", "\\.")), suite);
  }
  assert.match(workflow, /tests\/manual\/extension_smoke\.mjs/);
  assert.match(workflow, /tests\/manual\/background_bind_test\.mjs/);
  assert.match(workflow, /scripts\/pack-extension\.mjs --out-dir store-artifact/);
  assert.match(workflow, /contracts\/browser-extension-store\.json/);
  assert.match(workflow, /actions\/upload-artifact@v4/);
  assert.doesNotMatch(workflow, /kpcengcaammanfnbclapecdgahdmhanp/);
  assert.doesNotMatch(workflow, /chromewebstore\.googleapis\.com|:publish|refresh[_-]?token|client[_-]?secret/i);
});

test("README languages link the frozen Store item instead of a generic search page", async () => {
  for (const name of ["README.md", "README.zh.md", "README.ja.md"]) {
    const text = await readFile(join(root, name), "utf8");
    assert.match(text, new RegExp(`chromewebstore\\.google\\.com/detail/${STORE_ID}`), name);
  }
});
