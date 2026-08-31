import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (p) => readFileSync(path.join(ROOT, p), "utf8");

const USER_COMMANDS = [
  "herdr-mcp install",
  "herdr-mcp status",
  "herdr-mcp doctor",
  "herdr-mcp update check",
  "herdr-mcp update apply",
  "herdr-mcp update auto",
  "herdr-mcp update status",
  "herdr-mcp rollback",
  "herdr-mcp reinstall",
  "herdr-mcp uninstall",
];

function sectionAfterHeading(doc, heading) {
  const start = doc.indexOf(heading);
  assert.ok(start >= 0, `missing heading: ${heading}`);
  const after = doc.slice(start + heading.length);
  const next = after.search(/\n## /);
  return next >= 0 ? after.slice(0, next) : after;
}

test("README primary path documents the frozen top-level user CLI", () => {
  const cases = [
    ["README.md", "## Local runtime CLI"],
    ["README.zh.md", "## 本机 runtime CLI"],
    ["README.ja.md", "## Local runtime CLI"],
  ];

  for (const [rel, heading] of cases) {
    const doc = read(rel);
    const section = sectionAfterHeading(doc, heading);
    for (const command of USER_COMMANDS) {
      assert.match(section, new RegExp(command.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
    }
    assert.doesNotMatch(
      section,
      /^\s*herdr-mcp service install\b/m,
      `${rel} CLI section must not use service install as the primary install instruction`
    );
    assert.match(section, /advanced|内部|Advanced/i);
  }
});

test("cargo-built herdr-mcp --help lists the user path ahead of service", () => {
  // Only the worktree build artifact reflects this PR. Do not query runtime/current.
  const candidates = [
    path.join(ROOT, "target", "debug", "herdr-mcp"),
    path.join(ROOT, "target", "release", "herdr-mcp"),
  ];
  const binary = candidates.find((candidate) => existsSync(candidate));
  if (!binary) {
    // Source-tree CI may not have a built binary; README + Rust unit tests still cover the contract.
    return;
  }

  const result = spawnSync(binary, ["--help"], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr || result.stdout);
  const text = `${result.stdout}${result.stderr}`;
  for (const command of [
    "herdr-mcp install",
    "herdr-mcp status",
    "herdr-mcp doctor",
    "herdr-mcp update",
    "herdr-mcp rollback",
    "herdr-mcp reinstall",
    "herdr-mcp uninstall",
  ]) {
    assert.match(text, new RegExp(command.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
  }
  assert.match(text, /User path:/);
  assert.match(text, /Advanced \/ internal:/);
  const install = text.indexOf("herdr-mcp install");
  const service = text.indexOf("herdr-mcp service");
  assert.ok(install >= 0 && service > install, "user-path install must precede service");
});

const ENROLLMENT_COMMANDS = [
  "herdr-mcp worker pair",
  "herdr-mcp device pair",
  "herdr-mcp worker connect <pairing-address>",
];

test("v0.4.3 pairing docs use only the implemented CLI surface", () => {
  const controlPlane = read("docs/_wip/multi-device-worker-control-plane.md");
  const boundary = sectionAfterHeading(controlPlane, "## 13. CLI boundary");
  const pairing = sectionAfterHeading(controlPlane, "## 11. Pairing");
  const plan = read("docs/_wip/v0.4.3-plan.md");
  const p0c = sectionAfterHeading(plan, "### P0-C — Secure pairing");

  for (const section of [boundary, pairing, p0c]) {
    for (const command of ENROLLMENT_COMMANDS) {
      assert.match(section, new RegExp(command.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
    }
  }

  // The unimplemented onboarding tree must not be presented as the current path.
  assert.doesNotMatch(
    pairing,
    /herdr-mcp worker setup\b/,
    "pairing section must not present unimplemented `worker setup` as current onboarding",
  );
  assert.match(boundary, /Later command tree:[\s\S]*herdr-mcp worker setup/);

  // Secrets never ride argv; joining devices never need deploy credentials.
  assert.match(pairing, /never accepted as a command-line argument/);
  assert.match(pairing, /Cloudflare deployment credentials are not required/);
  assert.match(p0c, /never accepted on argv/);

  // Owner-only pairing creation and pending two-device UAT are stated.
  assert.match(pairing, /owner\/default workstation creates pairings/);
  assert.match(pairing, /cannot recursively create further pairings/);
  assert.match(controlPlane, /Real two-device UAT is still pending/);
  assert.match(plan, /pending production Edge[\s\S]*second Mac/);
});
