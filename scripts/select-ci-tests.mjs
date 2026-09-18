#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

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
  "tests/options-i18n.test.mjs",
  "tests/page-assist-content.test.mjs",
  "tests/page-assist-core.test.mjs",
  "tests/queued-insert.test.mjs",
]);

const EDGE_TESTS = new Set([
  "tests/public-contract-identity.integration.mjs",
  "tests/relay-edge-compat.integration.mjs",
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

export function classifyChangedFiles(files) {
  const changed = [...new Set(files.filter(Boolean))];
  if (changed.length === 0) return "full";
  if (changed.every(isDocPath)) return "docs";
  if (changed.every(isExtensionPath)) return "extension";
  if (changed.every(isEdgePath)) return "edge";
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
