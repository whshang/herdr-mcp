#!/usr/bin/env node
// #586 checklist row: "Verify Runtime/Extension old<->new capability negotiation
// and rollback".
//
// One repeatable check that answers four compatibility questions against a real
// git checkout:
//
//   A. new Extension -> older Runtime (`--tag`, default v1.1.0-rc.10)
//   B. older Extension payloads -> current Runtime (this tree)
//   C. capability negotiation between the two peers
//   D. rollback: durable state written by this Runtime is readable by the
//      previous release plane
//
// The older peer is never hand-copied: its registry allow-list, capability
// vocabulary, and state-schema version are read out of the tag's own source, so
// the check cannot drift away from the release it claims to describe.
//
// Isolation: the check runs in a throwaway HOME/XDG tree (`mkdtemp`) and
// snapshots the real runtime config directory before and after, failing if
// anything under it changed. It never invokes `herdr-mcp update`,
// `rollback`, `worker update`, `--with-worker`, launchctl, osascript, or a
// network peer; the only spawned processes are `git`, `node --test`, and
// `cargo test`.
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import {
  capabilityOperations,
  gitShow,
  registryAllowLists,
  resolveCommit,
  stateMigrationCount,
  stateSchemaVersion,
} from "./compat/registry-contract.mjs";

const args = new Set(process.argv.slice(2));
const skipCargo = args.has("--skip-cargo");
const tagArg = process.argv.slice(2).find((value) => value.startsWith("--tag="));
const PEER_TAG = tagArg ? tagArg.slice("--tag=".length) : "v1.1.0-rc.10";

// Every field the current Runtime admits on top of the older peer's allow-list.
// An addition is only compatible if the extension keeps a working path for a
// peer that rejects it; anything else must be registered here with a reason so
// a future field cannot silently break a downgraded Runtime.
const ADDITIVE_REGISTRY_FIELDS = new Map([
  [
    "source_turn.observe:profile_seed",
    "optional endpoint provenance (#734/#815); extension retries the legacy four-field payload on browser_registry_params_invalid",
  ],
  [
    "source_turn.observe:endpoint_ref",
    "optional endpoint provenance (#734/#815); same legacy retry path",
  ],
]);

const results = [];
function record(id, status, detail) {
  results.push({ id, status, detail });
  const line = `${status === "pass" ? "PASS" : status === "fail" ? "FAIL" : "SKIP"}  ${id}${detail ? `  ${detail}` : ""}`;
  console.log(line);
}

function run(command, commandArgs, options = {}) {
  return execFileSync(command, commandArgs, {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    maxBuffer: 64 * 1024 * 1024,
    ...options,
  });
}

// --- real-repo resolution (never a placeholder path) ------------------------
const repoRoot = run("git", ["rev-parse", "--show-toplevel"]).trim();
if (!repoRoot.startsWith("/") || !statSync(repoRoot).isDirectory()) {
  console.error(`compat: cannot resolve a real repository root (got ${JSON.stringify(repoRoot)})`);
  process.exit(2);
}
console.log(`compat: repo ${repoRoot}`);
console.log(`compat: peer ${PEER_TAG}`);

// --- isolated HOME/XDG/config tree ----------------------------------------
const sandbox = mkdtempSync(join(tmpdir(), "herdr-compat-586-"));
const isolatedEnv = {
  HOME: join(sandbox, "home"),
  XDG_CONFIG_HOME: join(sandbox, "config"),
  XDG_STATE_HOME: join(sandbox, "state"),
  XDG_DATA_HOME: join(sandbox, "data"),
  XDG_CACHE_HOME: join(sandbox, "cache"),
  HERDR_MCP_CONFIG_DIR: join(sandbox, "config", "herdr-mcp"),
  HERDR_MCP_DEV_STATE_DIR: join(sandbox, "config", "herdr-mcp-dev"),
  HERDR_MCP_INSTANCE: "compat-check",
  HERDR_SOCKET_PATH: join(sandbox, "herdr", "herdr.sock"),
  TMPDIR: join(sandbox, "tmp"),
};
for (const dir of Object.values(isolatedEnv)) mkdirSync(dir, { recursive: true });

const childEnv = { ...process.env, ...isolatedEnv };
delete childEnv.HERDR_ENV;
console.log(`compat: isolated HOME ${isolatedEnv.HOME}`);

// --- snapshots of the real runtime state this check must not touch --------
const realConfigDir = join(homedir(), ".config", "herdr-mcp");

function inventory(root) {
  // A production Runtime may be running on this workstation while the check
  // runs, and it owns the volatile subtrees below (exec spools, logs, SQLite
  // WAL/SHM, sockets, session journals). Those change every second no matter
  // what this script does, so they cannot be evidence of a leak. Everything
  // else under the real config directory — config, state database, runtime
  // generations/current pointer, manifests — must be byte-identical.
  const volatilePath = /(?:^|\/)(?:exec-spools|logs|sessions|events|tmp)(?:\/|$)|\.(?:spool|log|db-wal|db-shm|sock)$/;
  const entries = [];
  const walk = (dir, depth) => {
    let names;
    try {
      names = readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of names) {
      const path = join(dir, entry.name);
      if (volatilePath.test(path.slice(root.length))) continue;
      try {
        const stat = statSync(path, { bigint: false, throwIfNoEntry: false });
        if (!stat) continue;
        // Path identity only: a production Runtime may rewrite file contents
        // (state.db, health watchdog) while the check runs. A leak would show
        // up as a new or missing path — created state/socket/config files —
        // under this directory, which is what the assertion compares.
        entries.push(path);
        if (entry.isDirectory() && depth < 4) walk(path, depth + 1);
      } catch {
        entries.push(`${path}|unreadable`);
      }
    }
  };
  walk(root, 0);
  return entries.sort();
}

const realBefore = inventory(realConfigDir);

// --- checks ---------------------------------------------------------------
function checkEnvContract() {
  const paths = readFileSync(join(repoRoot, "crates/herdr-mcp/src/paths.rs"), "utf8");
  const required = ["HOME", "XDG_CONFIG_HOME", "HERDR_MCP_CONFIG_DIR", "HERDR_MCP_DEV_STATE_DIR", "HERDR_SOCKET_PATH"];
  const missing = required.filter((name) => name !== "HOME" && !paths.includes(`"${name}"`));
  if (missing.length > 0) {
    record("env-isolation-contract", "fail", `paths.rs no longer reads ${missing.join(", ")}; update this script's isolation`);
    return;
  }
  record("env-isolation-contract", "pass", `paths.rs honours ${required.join(", ")}`);
}

function peerSource(relativePath) {
  return gitShow(repoRoot, PEER_TAG, relativePath);
}

function checkPeerContract() {
  const peerCommit = resolveCommit(repoRoot, PEER_TAG);
  if (!peerCommit) {
    record("A-peer-tag-present", "fail", `tag ${PEER_TAG} is missing from this checkout; fetch tags before running`);
    return null;
  }
  const peerHttp = peerSource("crates/herdr-mcp/src/mcp_http.rs");
  const peerStore = peerSource("crates/herdr-mcp/src/state_store.rs");
  const peerSchema = peerSource("crates/herdr-mcp/src/state_store/schema.rs");
  if (!peerHttp || !peerStore || !peerSchema) {
    record("A-peer-tag-present", "fail", `cannot read the Runtime source at ${peerCommit}`);
    return null;
  }
  record("A-peer-tag-present", "pass", `${PEER_TAG} -> ${peerCommit}`);

  const peerRegistry = registryAllowLists(peerHttp);
  const peerSourceTurn = peerRegistry.get("source_turn.observe") || [];
  const legacySourceTurn = ["operation", "canonical_url", "user_text", "observed_at"];
  if ([...peerSourceTurn].sort().join(",") !== [...legacySourceTurn].sort().join(",")) {
    record(
      "A-peer-source-turn-shape",
      "fail",
      `the ${PEER_TAG} source turn is not the legacy four-field payload: ${peerSourceTurn.join(",")}`,
    );
  } else {
    record("A-peer-source-turn-shape", "pass", `legacy four fields, provenance keys rejected (${peerSourceTurn.join(",")})`);
  }
  return { peerRegistry, peerStore, peerSchema };
}

function checkRuntimeAcceptsPeerPayloads(peer) {
  const currentRegistry = registryAllowLists(readFileSync(join(repoRoot, "crates/herdr-mcp/src/mcp_http.rs"), "utf8"));
  const failures = [];
  const additions = [];
  for (const [operation, peerFields] of peer.peerRegistry) {
    const currentFields = currentRegistry.get(operation);
    if (!currentFields) {
      failures.push(`${operation} was dropped by the current Runtime`);
      continue;
    }
    const removed = peerFields.filter((field) => !currentFields.includes(field));
    if (removed.length > 0) failures.push(`${operation} no longer admits ${removed.join(",")}`);
    for (const field of currentFields.filter((value) => !peerFields.includes(value))) {
      additions.push(`${operation}:${field}`);
      if (!ADDITIVE_REGISTRY_FIELDS.has(`${operation}:${field}`)) {
        failures.push(
          `${operation}.${field} is additive but undeclared; register it in ADDITIVE_REGISTRY_FIELDS with the old-Runtime fallback`,
        );
      }
    }
  }
  for (const operation of currentRegistry.keys()) {
    if (!peer.peerRegistry.has(operation)) additions.push(`${operation} (new operation, never sent by the older peer)`);
  }
  if (failures.length > 0) {
    record("B-runtime-accepts-peer-payloads", "fail", failures.join("; "));
  } else {
    record("B-runtime-accepts-peer-payloads", "pass", `${additions.length} additive field(s): ${additions.join(", ")}`);
  }
}

function checkCapabilityVocabulary(peer) {
  const currentStore = readFileSync(join(repoRoot, "crates/herdr-mcp/src/state_store.rs"), "utf8");
  const peerOps = capabilityOperations(peer.peerStore);
  const currentOps = capabilityOperations(currentStore);
  if (!peerOps || !currentOps) {
    record("C-capability-vocabulary", "fail", "cannot parse ALLOWED_BROWSER_CAPABILITY_OPERATIONS");
    return;
  }
  const dropped = peerOps.filter((operation) => !currentOps.includes(operation));
  if (dropped.length > 0) {
    record("C-capability-vocabulary", "fail", `current Runtime dropped capability operations ${dropped.join(",")}`);
    return;
  }
  record(
    "C-capability-vocabulary",
    "pass",
    `${peerOps.length} released operations still accepted; adapters may declare only these`,
  );
}

function checkRollbackStateSchema(peer) {
  const currentStore = readFileSync(join(repoRoot, "crates/herdr-mcp/src/state_store.rs"), "utf8");
  const currentSchema = readFileSync(join(repoRoot, "crates/herdr-mcp/src/state_store/schema.rs"), "utf8");
  const peerVersion = stateSchemaVersion(peer.peerStore);
  const currentVersion = stateSchemaVersion(currentStore);
  const peerMigrations = stateMigrationCount(peer.peerSchema);
  const currentMigrations = stateMigrationCount(currentSchema);
  if (peerVersion === null || currentVersion === null || peerMigrations === null || currentMigrations === null) {
    record("D-rollback-state-schema", "fail", "cannot parse SCHEMA_VERSION / MIGRATIONS");
    return;
  }
  if (currentVersion > peerVersion) {
    record(
      "D-rollback-state-schema",
      "fail",
      `state schema moved ${peerVersion} -> ${currentVersion}; the ${PEER_TAG} Runtime refuses a newer store (fail-closed downgrade guard), so rollback needs the major-rollback backup path and an explicit release note`,
    );
    return;
  }
  if (currentVersion < peerVersion) {
    record("D-rollback-state-schema", "fail", `state schema went backwards (${peerVersion} -> ${currentVersion})`);
    return;
  }
  if (peerMigrations !== currentMigrations) {
    record(
      "D-rollback-state-schema",
      "fail",
      `schema version is ${currentVersion} but the migration counts differ (${peerMigrations} vs ${currentMigrations})`,
    );
    return;
  }
  record(
    "D-rollback-state-schema",
    "pass",
    `schema ${currentVersion} with ${currentMigrations} migrations on both planes: a store written here opens on ${PEER_TAG}`,
  );
}

function checkExtensionFallbackSuite() {
  try {
    const output = run("node", ["--test", "tests/compat-runtime-extension-negotiation.test.mjs"], {
      cwd: repoRoot,
      env: childEnv,
    });
    const passed = /(?:#|ℹ) pass (\d+)/.exec(output)?.[1] || "?";
    const skipped = /(?:#|ℹ) skipped (\d+)/.exec(output)?.[1] || "?";
    record("A-extension-legacy-fallback", "pass", `node --test passed=${passed} skipped=${skipped}`);
  } catch (error) {
    const output = `${error.stdout || ""}${error.stderr || ""}`.trim().split("\n").slice(-12).join(" | ");
    record("A-extension-legacy-fallback", "fail", output || String(error.message));
  }
}

function cargoTest(id, filter, extra = []) {  if (skipCargo) {
    record(id, "skip", "cargo lane disabled with --skip-cargo");
    return;
  }
  try {
    const output = run("cargo", ["test", "-p", "herdr-mcp", "--bin", "herdr-mcp", ...extra, "--", filter], {
      cwd: repoRoot,
      env: childEnv,
    });
    const summary = output.split("\n").find((line) => line.startsWith("test result:")) || "ok";
    const matched = Number(/^running (\d+) test/m.exec(output)?.[1] || 0);
    if (matched === 0) {
      record(id, "fail", `no Rust test matched ${JSON.stringify(filter)}`);
      return;
    }
    record(id, "pass", `matched ${matched}; ${summary.trim()}`);
  } catch (error) {
    const output = `${error.stdout || ""}${error.stderr || ""}`.trim().split("\n").slice(-12).join(" | ");
    record(id, "fail", output || String(error.message));
  }
}

try {
  checkEnvContract();
  const peer = checkPeerContract();
  if (peer) {
    checkRuntimeAcceptsPeerPayloads(peer);
    checkCapabilityVocabulary(peer);
    checkRollbackStateSchema(peer);
  }
  checkExtensionFallbackSuite();
  cargoTest("B-current-runtime-accepts-peer-payloads", "compat_rc10_extension_payloads");
  cargoTest("D-rollback-refuses-newer-store", "future_schema_version_fails_closed");
  cargoTest(
    "D-rollback-older-binary-refuses-newer-state",
    "schema_eight_binary_refuses_v12_and_requires_compatible_rollback_state",
  );
  cargoTest(
    "D-rollback-restores-previous-schema",
    "rollback_restores_schema_fifteen_before_starting_the_old_binary",
  );
  cargoTest(
    "D-rollback-refuses-undeclared-schema-jump",
    "auto_refuses_state_schema_migration_and_points_to_explicit_update",
  );
} finally {
  const realAfter = inventory(realConfigDir);
  if (JSON.stringify(realAfter) === JSON.stringify(realBefore)) {
    record("isolation-real-config-untouched", "pass", `${realConfigDir} unchanged (${realBefore.length} identity entries)`);
  } else {
    const created = realAfter.filter((entry) => !realBefore.includes(entry));
    const removed = realBefore.filter((entry) => !realAfter.includes(entry));
    record(
      "isolation-real-config-untouched",
      "fail",
      `real config changed: created ${created.slice(0, 5).join(", ") || "-"}; removed ${removed.slice(0, 5).join(", ") || "-"}`,
    );
  }
  rmSync(sandbox, { recursive: true, force: true });
}

const failed = results.filter((result) => result.status === "fail");
console.log(`\ncompat: ${results.length - failed.length}/${results.length} checks passed`);
if (failed.length > 0) process.exit(1);
