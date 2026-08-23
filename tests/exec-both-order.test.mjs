/**
 * stream=both must preserve chunk arrival order (seq), not stdout-then-stderr.
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { startExecSession, readExecSession, killExecSession } from "../dist/exec-sessions.js";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

test("exec_read stream=both preserves interleaved arrival (not stdout-then-stderr)", async () => {
  // Sleeps force stderr chunks between stdout so arrival order is A,B,C,D.
  // The assertion stays exact; the deadline only tolerates CI scheduling/close jitter.
  const cmd =
    "printf 'A\\n'; sleep 0.08; printf 'B\\n' >&2; sleep 0.08; printf 'C\\n'; sleep 0.08; printf 'D\\n' >&2";
  const s = startExecSession({ command: cmd, cwd: process.cwd() });
  const deadline = Date.now() + 15_000;
  let last = null;

  try {
    while (Date.now() < deadline) {
      const both = readExecSession(s.id, { stream: "both", offset: 0, limit: 4096 });
      assert.equal(both.ok, true);
      last = {
        running: both.running,
        exit_code: both.exit_code ?? null,
        signal: both.signal ?? null,
        bytes_total: both.bytes_total,
        text: both.text,
      };

      // Output completeness and process close are separate events. Once all four
      // chunks are present, validate the ordering contract immediately instead of
      // requiring `running=false` to happen in the same polling window.
      if (both.bytes_total >= 8) {
        assert.equal(both.text, "A\nB\nC\nD\n", `got ${JSON.stringify(both.text)}`);
        const out = readExecSession(s.id, { stream: "stdout", offset: 0, limit: 4096 });
        const err = readExecSession(s.id, { stream: "stderr", offset: 0, limit: 4096 });
        assert.equal(out.ok && err.ok, true);
        assert.equal(out.text + err.text, "A\nC\nB\nD\n", "per-stream concat would scramble");
        return;
      }
      await sleep(50);
    }

    assert.fail(`session did not produce complete interleaved output before deadline: ${JSON.stringify(last)}`);
  } finally {
    let final = readExecSession(s.id, { stream: "both", offset: 0, limit: 4096 });
    if (final.ok && final.running) {
      killExecSession(s.id);
      const cleanupDeadline = Date.now() + 2_000;
      while (Date.now() < cleanupDeadline) {
        final = readExecSession(s.id, { stream: "both", offset: 0, limit: 4096 });
        if (!final.ok || !final.running) break;
        await sleep(50);
      }
    }
  }
});
