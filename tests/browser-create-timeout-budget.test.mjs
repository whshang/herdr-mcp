import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

// Cross-language budget contract: the extension-side session-create worst case
// (scope handshake + content acceptance) must stay strictly inside the Rust
// broker's create actuation wait, so a healthy-but-slow create completes within
// the single wait instead of settling `uncertain` for same-key replay. The
// local CLI's create budget must in turn cover that wait plus the late-acceptance
// readback actuation and reconcile sweep without exceeding the MCP transport
// maximum. Every side is parsed from real source text; a missing constant fails
// the test.
const __dirname = path.dirname(fileURLToPath(import.meta.url));
const extensionDir = path.join(__dirname, "..", "extension");
const rustSrcDir = path.join(__dirname, "..", "crates", "herdr-mcp", "src");
const backgroundSource = readFileSync(path.join(extensionDir, "background.js"), "utf8");
const actuationSource = readFileSync(
  path.join(rustSrcDir, "mcp_http", "actuation.rs"),
  "utf8",
);
const localAgentCliSource = readFileSync(path.join(rustSrcDir, "local_agent_cli.rs"), "utf8");
const localMcpSource = readFileSync(path.join(rustSrcDir, "link", "local_mcp.rs"), "utf8");

function parseOrFail(source, pattern, label) {
  const match = source.match(pattern);
  assert.ok(match, `${label} must remain declared in the Rust source`);
  return Number(match[1].replaceAll("_", ""));
}

function extensionConstantMs(name) {
  return parseOrFail(
    backgroundSource,
    new RegExp(`const\\s+${name}\\s*=\\s*(\\d[\\d_]*)\\s*;`),
    name,
  );
}

function rustDurationSeconds(source, name) {
  return parseOrFail(
    source,
    new RegExp(`const\\s+${name}:\\s*Duration\\s*=\\s*Duration::from_secs\\((\\d+)\\);`),
    name,
  );
}

function rustMsConstant(source, name) {
  return parseOrFail(
    source,
    new RegExp(`const\\s+${name}:\\s*u64\\s*=\\s*(\\d[\\d_]*)\\s*;`),
    name,
  );
}

function localCliCreateBudgetMs() {
  return parseOrFail(
    localAgentCliSource,
    /method\s*==\s*"herdr_mcp\.browser_session\.create"\s*\{\s*(\d[\d_]*)/,
    "local CLI browser_session.create budget",
  );
}

test("user session create settles within one runtime wait | Given the extension create budgets and the Rust broker create timeout parsed from source | When the extension-side worst case is compared with the runtime wait | Then the handshake plus content budget stays strictly inside the create actuation timeout", () => {
  const handshakeMs = extensionConstantMs("BROWSER_CREATE_SCOPE_HANDSHAKE_TIMEOUT_MS");
  const contentMs = extensionConstantMs("BROWSER_CREATE_CONTENT_TIMEOUT_MS");
  const runtimeCreateWaitMs = rustDurationSeconds(
    actuationSource,
    "BROWSER_SESSION_CREATE_ACTUATION_TIMEOUT",
  ) * 1000;
  assert.ok(
    handshakeMs + contentMs < runtimeCreateWaitMs,
    `extension create worst case (${handshakeMs}ms handshake + ${contentMs}ms content = ${handshakeMs + contentMs}ms) ` +
      `must stay below the Rust create actuation timeout (${runtimeCreateWaitMs}ms)`,
  );
});

test("user local create budget covers readback and stays within transport max | Given the runtime create wait, browser actuation timeout, local CLI create budget, and MCP transport maximum parsed from source | When the budget chain is compared | Then the CLI budget covers the create wait plus late-acceptance actuation plus a 6s reconcile allowance and does not exceed the transport maximum", () => {
  const createWaitMs = rustDurationSeconds(
    actuationSource,
    "BROWSER_SESSION_CREATE_ACTUATION_TIMEOUT",
  ) * 1000;
  const actuationTimeoutMs = rustDurationSeconds(
    actuationSource,
    "BROWSER_ACTUATION_TIMEOUT",
  ) * 1000;
  const cliCreateBudgetMs = localCliCreateBudgetMs();
  const transportMaxMs = rustMsConstant(localMcpSource, "LOCAL_MCP_MAX_TIMEOUT_MS");
  const minimumCliBudgetMs = createWaitMs + actuationTimeoutMs + 6000;
  assert.ok(
    cliCreateBudgetMs >= minimumCliBudgetMs,
    `local CLI create budget (${cliCreateBudgetMs}ms) must cover the ${createWaitMs}ms create wait + ` +
      `${actuationTimeoutMs}ms late-acceptance actuation + 6000ms reconcile allowance = ${minimumCliBudgetMs}ms`,
  );
  assert.ok(
    cliCreateBudgetMs <= transportMaxMs,
    `local CLI create budget (${cliCreateBudgetMs}ms) must not exceed LOCAL_MCP_MAX_TIMEOUT_MS (${transportMaxMs}ms)`,
  );
});
