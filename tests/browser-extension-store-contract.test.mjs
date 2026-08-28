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


test("production native-host identity is Store-first and ignores legacy local extension locations", async () => {
  const source = await readFile(path.join(RUST_SRC, "native_host.rs"), "utf8");
  assert.match(source, /HERDR_EXTENSION_PATH/);
  assert.doesNotMatch(source, /config_dir\.join\("extension"\)/);
  assert.doesNotMatch(source, /HERDR_MCP_ROOT[^\n]*extension/);
  assert.doesNotMatch(source, /development_extension_path/);
  assert.doesNotMatch(source, /CARGO_MANIFEST_DIR[^\n]*extension/);
});

test("runtime GitHub Release does not distribute a browser-extension zip", async () => {
  const release = await readFile(path.join(ROOT, ".github", "workflows", "rust-release.yml"), "utf8");
  const recovery = await readFile(path.join(ROOT, ".github", "workflows", "rust-release-recover.yml"), "utf8");
  for (const source of [release, recovery]) {
    assert.doesNotMatch(source, /pack-extension\.mjs/);
    assert.doesNotMatch(source, /herdr-mcp-extension-/);
  }
});
