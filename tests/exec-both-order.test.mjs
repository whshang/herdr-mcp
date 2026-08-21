/**
 * stream=both must preserve chunk arrival order (seq), not stdout-then-stderr.
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { startExecSession, readExecSession } from "../dist/exec-sessions.js";

test("exec_read stream=both preserves interleaved arrival (not stdout-then-stderr)", async () => {
  // Sleeps force stderr chunks between stdout so arrival order is A,B,C,D.
  // Old implementation concatenated all stdout then all stderr -> A C B D.
  const cmd =
    "printf 'A\\n'; sleep 0.08; printf 'B\\n' >&2; sleep 0.08; printf 'C\\n'; sleep 0.08; printf 'D\\n' >&2";
  const s = startExecSession({ command: cmd, cwd: process.cwd() });
  for (let i = 0; i < 100; i++) {
    const r = readExecSession(s.id, { stream: "both", offset: 0, limit: 4096 });
    assert.equal(r.ok, true);
    if (!r.running && r.bytes_total >= 8) {
      assert.equal(r.text, "A\nB\nC\nD\n", `got ${JSON.stringify(r.text)}`);
      const out = readExecSession(s.id, { stream: "stdout", offset: 0, limit: 4096 });
      const err = readExecSession(s.id, { stream: "stderr", offset: 0, limit: 4096 });
      assert.equal(out.ok && err.ok, true);
      assert.equal(out.text + err.text, "A\nC\nB\nD\n", "per-stream concat would scramble");
      return;
    }
    await new Promise((r) => setTimeout(r, 50));
  }
  assert.fail("session did not finish with expected both-order text");
});
