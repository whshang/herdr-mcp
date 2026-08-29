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
  assert.equal(store.summary, manifest.description);
  assert.match(store.item_url, new RegExp(`${STORE_ID}$`));
  assert.match(store.homepage_url, /^https:\/\/whshang\.github\.io\/herdr-mcp\/?/);
  assert.equal(store.support_url, "https://github.com/whshang/herdr-mcp/issues");
  assert.match(store.privacy_policy_url, /\/privacy\.html$/);
});

test("Store copy describes the production safety and experimental boundaries", () => {
  assert.match(store.description, /Native Messaging/i);
  assert.match(store.description, /429/);
  assert.match(store.description, /chat\.z\.ai/i);
  assert.match(store.description, /chat\.deepseek\.com/i);
  assert.match(store.description, /disabled by default/i);
  assert.match(store.review_notes, /OFF by default/i);
  assert.match(store.review_notes, /No remote code/i);
  assert.doesNotMatch(store.product_name, /Web wake/i);
});

test("README languages link the frozen Store item instead of a generic search page", async () => {
  for (const name of ["README.md", "README.zh.md", "README.ja.md"]) {
    const text = await readFile(join(root, name), "utf8");
    assert.match(text, new RegExp(`chromewebstore\\.google\\.com/detail/${STORE_ID}`), name);
  }
});
