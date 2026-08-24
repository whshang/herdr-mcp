/**
 * Unit tests for local-exec (herdr_exec TaskGroup fallback backend).
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { runLocalShell } from "../dist/local-exec.js";

test("runLocalShell captures exit 0 and stdout", async () => {
  const r = await runLocalShell({
    command: "printf 'hello-local\\n'",
    cwd: process.cwd(),
    timeoutMs: 5000,
  });
  assert.equal(r.timed_out, false);
  assert.equal(r.exit_code, 0);
  assert.match(r.output, /hello-local/);
});

test("runLocalShell captures non-zero exit", async () => {
  const r = await runLocalShell({
    command: "exit 42",
    cwd: process.cwd(),
    timeoutMs: 5000,
  });
  assert.equal(r.exit_code, 42);
});

test("runLocalShell times out long sleep", async () => {
  const startedAt = Date.now();
  const r = await runLocalShell({
    command: "sleep 30",
    cwd: process.cwd(),
    timeoutMs: 400,
  });
  assert.equal(r.timed_out, true);
  assert.ok(Date.now() - startedAt < 3000, "timeout must terminate the command tree promptly");
});

test("runLocalShell runs from requested cwd", async () => {
  const r = await runLocalShell({
    command: "pwd",
    cwd: "/tmp",
    timeoutMs: 5000,
  });
  assert.equal(r.exit_code, 0);
  assert.match(r.output.trim(), /\/tmp/);
});
