/**
 * Unit tests for exec session closed/signal semantics (no live herdr).
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// exec-sessions persists an orphan-recovery journal. Root tests run in
// parallel, so sharing ~/.config/herdr-mcp/exec-sessions.json lets another
// test process mistake this file's short-lived child for its own recovery
// fixture. Give this test module a private journal before importing the code.
const stateDir = mkdtempSync(join(tmpdir(), "herdr-exec-sessions-test-"));
process.env.HERDR_MCP_STATE_DIR = stateDir;
process.on("exit", () => { try { rmSync(stateDir, { recursive: true, force: true }); } catch {} });

const {
  startExecSession,
  readExecSession,
  killExecSession,
  listExecSessions,
  resolveExecShell,
} = await import("../dist/exec-sessions.js");

test("exec shell is available on the current host", () => {
  const shell = resolveExecShell({});
  assert.match(shell, /^\//);
});

test("exec session marks closed after exit (including zero)", async () => {
  const s = startExecSession({ command: "echo hi", cwd: process.cwd() });
  // wait for close
  for (let i = 0; i < 50; i++) {
    const r = readExecSession(s.id);
    assert.equal(r.ok, true);
    if (!r.running) {
      assert.equal(r.exit_code, 0);
      return;
    }
    await new Promise((r) => setTimeout(r, 50));
  }
  assert.fail("session did not close");
});

test("kill sets closed via signal path", async () => {
  const s = startExecSession({ command: "sleep 30", cwd: process.cwd() });
  const k = killExecSession(s.id);
  assert.equal(k.ok, true);
  for (let i = 0; i < 80; i++) {
    const r = readExecSession(s.id);
    assert.equal(r.ok, true);
    if (!r.running) {
      // signal exit may leave exit_code null — running must still be false
      assert.equal(r.running, false);
      const listed = listExecSessions().find((x) => x.session_id === s.id);
      if (listed) assert.equal(listed.running, false);
      return;
    }
    await new Promise((r) => setTimeout(r, 50));
  }
  assert.fail("killed session still running");
});
