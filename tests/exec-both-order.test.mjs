/**
 * stream=both preserves the chunk order actually captured by the runtime.
 *
 * stdout and stderr are independent OS pipes, so their relative delivery
 * order is not portable across kernels/runners. Test the cross-stream ordering
 * contract with a synthetic captured sequence, then separately smoke-test the
 * real child path for complete per-stream capture.
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { startExecSession, readExecSession, killExecSession } from "../dist/exec-sessions.js";

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

test("exec_read stream=both preserves recorded cross-stream chunk order", () => {
  const s = startExecSession({ command: ":", cwd: process.cwd() });
  s.chunks.splice(
    0,
    s.chunks.length,
    { seq: 0, stream: "stdout", data: Buffer.from("A\n") },
    { seq: 1, stream: "stderr", data: Buffer.from("B\n") },
    { seq: 2, stream: "stdout", data: Buffer.from("C\n") },
    { seq: 3, stream: "stderr", data: Buffer.from("D\n") },
  );
  s.nextSeq = 4;
  s.stdoutBytes = 4;
  s.stderrBytes = 4;

  const both = readExecSession(s.id, { stream: "both", offset: 0, limit: 4096 });
  const out = readExecSession(s.id, { stream: "stdout", offset: 0, limit: 4096 });
  const err = readExecSession(s.id, { stream: "stderr", offset: 0, limit: 4096 });

  assert.equal(both.ok && out.ok && err.ok, true);
  assert.equal(both.text, "A\nB\nC\nD\n");
  assert.equal(out.text, "A\nC\n");
  assert.equal(err.text, "B\nD\n");
  assert.notEqual(both.text, out.text + err.text, "stream=both must not group stdout before stderr");
});

test("exec_read stream=both captures both real child pipes without cross-FD ordering assumptions", async () => {
  const s = startExecSession({
    command: "printf 'A\\nC\\n'; printf 'B\\nD\\n' >&2",
    cwd: process.cwd(),
  });
  const deadline = Date.now() + 10_000;
  let last = null;

  try {
    while (Date.now() < deadline) {
      const both = readExecSession(s.id, { stream: "both", offset: 0, limit: 4096 });
      const out = readExecSession(s.id, { stream: "stdout", offset: 0, limit: 4096 });
      const err = readExecSession(s.id, { stream: "stderr", offset: 0, limit: 4096 });
      assert.equal(both.ok && out.ok && err.ok, true);
      last = { both, out, err };

      if (out.text === "A\nC\n" && err.text === "B\nD\n") {
        assert.deepEqual(both.text.trim().split(/\s+/).sort(), ["A", "B", "C", "D"]);
        return;
      }
      await sleep(25);
    }

    assert.fail(`child output not captured before deadline: ${JSON.stringify(last)}`);
  } finally {
    const current = readExecSession(s.id, { stream: "both", offset: 0, limit: 4096 });
    if (current.ok && current.running) killExecSession(s.id);
  }
});
