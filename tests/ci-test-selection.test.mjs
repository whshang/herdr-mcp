import test from "node:test";
import assert from "node:assert/strict";
import { classifyChangedFiles } from "../scripts/select-ci-tests.mjs";

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
  assert.equal(classifyChangedFiles(["Cargo.lock"]), "full");
  assert.equal(classifyChangedFiles(["scripts/release-gate.sh"]), "full");
  assert.equal(classifyChangedFiles(["extension/background.js", "src/server.ts"]), "full");
});
