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

test("README keeps the exhaustive runtime CLI out of the primary user path", () => {
  for (const rel of ["README.md", "README.zh.md", "README.ja.md"]) {
    const doc = read(rel);
    assert.doesNotMatch(doc, /## (?:Local runtime CLI|本机 runtime CLI)/);
    assert.match(doc, /herdr-mcp status/);
    assert.match(doc, /herdr-mcp doctor/);
    assert.doesNotMatch(doc, /^\s*herdr-mcp service install\b/m);
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

test("worker connect owns local service and enrolled production Link activation", () => {
  const worker = read("crates/herdr-mcp/src/worker.rs");
  const refresh = read("crates/herdr-mcp/src/link/generation_refresh.rs");

  assert.match(worker, /fn activate_connected_runtime\(/);
  assert.match(worker, /service_lifecycle::run\(ServiceCommand::Install \{ adopt_node: false \}\)/);
  assert.match(worker, /"service_ready": true/);
  assert.match(worker, /"link_ready": true/);
  assert.match(refresh, /ensure_enrolled_rust_prod_link/);
  assert.match(refresh, /install_fresh_rust_prod_link/);
  assert.match(refresh, /configured_edge_device_identity\(home\)/);
  assert.match(refresh, /Never rewrite or bootstrap an existing Node\/foreign production Link/);
});
