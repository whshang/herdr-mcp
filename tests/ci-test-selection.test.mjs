import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { classifyChangedFiles, listRootTestsWithoutBuiltDist } from "../scripts/select-ci-tests.mjs";

const ROOT = fileURLToPath(new URL("..", import.meta.url));

test("user changes only documentation | Given documentation and its behavior tests | When CI classifies the PR | Then only the documentation gate is selected", () => {
  assert.equal(
    classifyChangedFiles([
      "docs/i18n/en/install.md",
      "site/index.html",
      "tests/site-build.test.mjs",
    ]),
    "docs",
  );
});

test("user changes only browser extension behavior | Given extension code and extension acceptance tests | When CI classifies the PR | Then only the extension gate is selected", () => {
  assert.equal(
    classifyChangedFiles([
      "extension/background.js",
      "extension/content/wake.js",
      "tests/browser-actuation-recovery.test.mjs",
      "tests/registration-single-flight.test.mjs",
    ]),
    "extension",
  );
});

test("user changes only Edge behavior | Given Edge code and cross-language Edge contracts | When CI classifies the PR | Then only the Edge gate is selected", () => {
  assert.equal(
    classifyChangedFiles([
      "edge/cloudflare/src/index.ts",
      "tests/public-contract-identity.integration.mjs",
    ]),
    "edge",
  );
});

test("user changes a shared dependency | Given a change with broad dependency reach | When CI classifies the PR | Then the complete gate is selected", () => {
  assert.equal(classifyChangedFiles(["package-lock.json"]), "full");
  assert.equal(classifyChangedFiles(["scripts/release-gate.sh"]), "full");
  assert.equal(classifyChangedFiles(["extension/background.js", "package.json"]), "full");
});

test("user changes only Rust runtime scope | Given crates, Cargo, toolchain, and Rust workflow edits | When CI classifies the PR | Then the rust gate is selected instead of the full root Node suite", () => {
  assert.equal(classifyChangedFiles(["Cargo.lock"]), "rust");
  assert.equal(
    classifyChangedFiles([
      "crates/herdr-mcp/src/main.rs",
      "crates/herdr-mcp/tests/service.rs",
      "Cargo.toml",
      "Cargo.lock",
      "rust-toolchain.toml",
      ".cargo/config.toml",
      ".github/workflows/rust-release.yml",
      ".github/workflows/rust-release-recover.yml",
      ".github/workflows/windows-candidate.yml",
    ]),
    "rust",
  );
});

test("user changes Rust together with a cross-language or Node surface | Given a Rust edit mixed with contracts, Edge, extension, src, bin, scripts, or the shared CI workflow | When CI classifies the PR | Then the complete gate is selected", () => {
  for (const other of [
    "contracts/epoch2.json",
    "edge/cloudflare/src/index.ts",
    "extension/background.js",
    "src/version.ts",
    "bin/herdr-self-update",
    "scripts/release-gate.sh",
    ".github/workflows/ci.yml",
    "tests/release-surface.test.mjs",
    "docs/i18n/en/install.md",
    "package.json",
  ]) {
    assert.equal(classifyChangedFiles(["crates/herdr-mcp/src/main.rs", other]), "full", other);
  }
});

test("user keeps Rust-reading Node contracts in the rust gate | Given the root test directory | When the rust gate lists its Node tests | Then only tests importing the legacy root dist are dropped", () => {
  const listed = listRootTestsWithoutBuiltDist();
  const all = readdirSync(`${ROOT}/tests`).filter((name) => name.endsWith(".test.mjs")).map((name) => `tests/${name}`);
  const dropped = all.filter((rel) => !listed.includes(rel));
  assert.ok(dropped.length > 0);
  for (const rel of dropped) {
    assert.match(readFileSync(`${ROOT}/${rel}`, "utf8"), /["']\.\.\/dist\//, rel);
  }
  for (const rel of listed) {
    assert.doesNotMatch(readFileSync(`${ROOT}/${rel}`, "utf8"), /["']\.\.\/dist\//, rel);
  }
  for (const rustContract of [
    "tests/release-surface.test.mjs",
    "tests/rust-release-manifest.test.mjs",
    "tests/compat-runtime-extension-negotiation.test.mjs",
  ]) {
    assert.ok(listed.includes(rustContract), rustContract);
  }

  const cli = execFileSync(process.execPath, ["scripts/select-ci-tests.mjs", "--list-rust-node-tests"], {
    cwd: ROOT,
    encoding: "utf8",
  });
  assert.deepEqual(cli.trim().split("\n"), listed);
});
