import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { join } from "node:path";

const root = process.cwd();
const manifest = JSON.parse(await readFile(join(root, "extension/manifest.json"), "utf8"));
const listing = JSON.parse(await readFile(join(root, "contracts/browser-extension-store-listing.json"), "utf8"));
const locales = ["en", "zh-CN", "ja"];

test("user reads every declared extension permission in the privacy policy | Given the permissions the shipped manifest declares | When each locale privacy policy is checked | Then every declared permission is named in every locale", async () => {
  for (const locale of locales) {
    const policy = await readFile(join(root, "docs/i18n", locale, "privacy.md"), "utf8");
    for (const permission of manifest.permissions) {
      assert.match(policy, new RegExp(`\`${permission}\``), `${locale}/privacy.md must disclose ${permission}`);
    }
  }
});

test("user reads the debugger disclosure and the bounds it states | Given the Store listing review notes | When the debugger permission is disclosed | Then the notes bound the DevTools attachment to owned pages and a fixed command set", () => {
  const notes = listing.chrome_web_store.review_notes;
  assert.match(notes, /`debugger`/);
  assert.match(notes, /Herdr-owned pages/);
  assert.match(notes, /fixed set of commands/);
  assert.match(notes, /navigation history/);
});
