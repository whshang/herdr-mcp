#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL("..", import.meta.url));

const RUST_WORKFLOWS = new Set([
  ".github/workflows/rust-release.yml",
  ".github/workflows/rust-release-recover.yml",
  ".github/workflows/windows-candidate.yml",
]);

const ROOT_BUILT_DIST_IMPORT = /["']\.\.\/dist\//;

const DOC_TESTS = new Set([
  "tests/agent-install-doc.test.mjs",
  "tests/docs-command-contract.test.mjs",
  "tests/platform-support-matrix.test.mjs",
  "tests/release-notes.test.mjs",
  "tests/site-build.test.mjs",
]);

const EXTENSION_TESTS = new Set([
  "tests/browser-actuation-recovery.test.mjs",
  "tests/browser-control-plane.test.mjs",
  "tests/browser-extension-store-contract.test.mjs",
  "tests/chatgpt-artifact-capture.test.mjs",
  "tests/extension-i18n.test.mjs",
  "tests/extension-version-guard.test.mjs",
  "tests/options-i18n.test.mjs",
  "tests/page-assist-content.test.mjs",
  "tests/page-assist-core.test.mjs",
  "tests/queued-insert.test.mjs",
  "tests/registration-single-flight.test.mjs",
]);

const EDGE_TESTS = new Set([
  "tests/public-contract-identity.integration.mjs",
]);

function isDocPath(path) {
  return (
    path.startsWith("docs/") ||
    path.startsWith("site/") ||
    /^README(?:\.[^.]+)?\.md$/.test(path) ||
    path === "CHANGELOG.md" ||
    DOC_TESTS.has(path)
  );
}

function isExtensionPath(path) {
  return path.startsWith("extension/") || EXTENSION_TESTS.has(path);
}

function isEdgePath(path) {
  return path.startsWith("edge/cloudflare/") || EDGE_TESTS.has(path);
}

function isRustPath(path) {
  return (
    path.startsWith("crates/") ||
    path.startsWith(".cargo/") ||
    path === "Cargo.toml" ||
    path === "Cargo.lock" ||
    path === "rust-toolchain.toml" ||
    RUST_WORKFLOWS.has(path)
  );
}

// Root tests that import the legacy root `dist/*.js` only exercise `src/*.ts`;
// every other root test (including cross-language assertions on Rust sources)
// runs without the root `tsc` build.
export function listRootTestsWithoutBuiltDist(root = ROOT) {
  return readdirSync(join(root, "tests"))
    .filter((name) => name.endsWith(".test.mjs"))
    .sort()
    .map((name) => "tests/" + name)
    .filter((rel) => !ROOT_BUILT_DIST_IMPORT.test(readFileSync(join(root, rel), "utf8")));
}

export function classifyChangedFiles(files) {
  const changed = [...new Set(files.filter(Boolean))];
  if (changed.length === 0) return "full";
  if (changed.every(isDocPath)) return "docs";
  if (changed.every(isExtensionPath)) return "extension";
  if (changed.every(isEdgePath)) return "edge";
  if (changed.every(isRustPath)) return "rust";
  return "full";
}

function git(args) {
  return execFileSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
}

function resolveBase(requestedBase) {
  if (requestedBase) {
    try {
      git(["cat-file", "-e", requestedBase + "^{commit}"]);
      return requestedBase;
    } catch {
      // Fall through to the checked-out PR merge parent.
    }
  }
  try {
    return git(["rev-parse", "HEAD^"]);
  } catch {
    return null;
  }
}

function main() {
  if (process.argv[2] === "--list-rust-node-tests") {
    process.stdout.write(listRootTestsWithoutBuiltDist().join("\n") + "\n");
    return;
  }

  const eventName = process.env.CI_EVENT_NAME || "";
  if (eventName !== "pull_request") {
    process.stdout.write("mode=full\n");
    return;
  }

  const base = resolveBase(process.argv[2]?.trim());
  if (!base) {
    process.stdout.write("mode=full\n");
    return;
  }

  const output = git(["diff", "--name-only", base, "HEAD", "--"]);
  const files = output ? output.split("\n") : [];
  process.stdout.write("mode=" + classifyChangedFiles(files) + "\n");
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  main();
}
