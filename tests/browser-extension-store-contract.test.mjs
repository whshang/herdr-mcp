import test from "node:test";
import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const CONTRACT = path.join(ROOT, "contracts", "browser-extension-store.json");
const RUST_SRC = path.join(ROOT, "crates", "herdr-mcp", "src");

async function rustFiles(dir) {
  const out = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...await rustFiles(full));
    else if (entry.isFile() && entry.name.endsWith(".rs")) out.push(full);
  }
  return out;
}

test("Chrome Web Store extension id has one machine-readable SSOT outside Rust source", async () => {
  const contract = JSON.parse(await readFile(CONTRACT, "utf8"));
  assert.equal(contract.schema_version, 1);
  const id = String(contract?.chrome_web_store?.extension_id || "");
  assert.match(id, /^[a-p]{32}$/);

  for (const file of await rustFiles(RUST_SRC)) {
    const source = await readFile(file, "utf8");
    assert.equal(
      source.includes(id),
      false,
      `${path.relative(ROOT, file)} must consume the Store contract instead of hard-coding the extension id`,
    );
  }
});
