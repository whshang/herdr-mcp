import test from "node:test";
import assert from "node:assert/strict";
import {
  existsSync,
  mkdtempSync,
  rmSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const {
  buildUtilityExecScript,
  cleanupStaleUtilityScripts,
  shellQuote,
  utilityPaneReadiness,
} = await import("../dist/utility-exec.js");

test("utility exec scripts force non-interactive pagers and self-clean", () => {
  const script = buildUtilityExecScript({
    execShell: "/bin/zsh",
    cwd: "/tmp/a b/it's",
    command: "git log -3 --oneline",
  });
  for (const key of ["PAGER", "GIT_PAGER", "GH_PAGER", "SYSTEMD_PAGER", "MANPAGER", "DELTA_PAGER"]) {
    assert.match(script, new RegExp(`export ${key}=cat`));
  }
  assert.match(script, /trap 'rm -f -- \"\$0\"' EXIT/);
  assert.match(script, /git log -3 --oneline/);
  assert.match(script, /cd -- '\/tmp\/a b\/it'\\''s'/);
  assert.equal(shellQuote("a'b"), "'a'\\''b'");
});

test("utility pane readiness is fail-closed while an interactive child owns the tty", () => {
  const ready = utilityPaneReadiness({
    process_info: {
      shell_pid: 100,
      foreground_process_group_id: 100,
      foreground_processes: [{ pid: 100, name: "zsh", cmdline: "-zsh" }],
    },
  });
  assert.equal(ready.ready, true);

  const pager = utilityPaneReadiness({
    process_info: {
      shell_pid: 100,
      foreground_process_group_id: 222,
      foreground_processes: [{ pid: 222, name: "/usr/bin/less", cmdline: "less" }],
    },
  });
  assert.equal(pager.ready, false);
  assert.equal(pager.foreground[0].name, "less");

  assert.equal(utilityPaneReadiness({}).ready, false);
});

test("stale utility script cleanup removes only old herdr wrappers", async () => {
  const dir = mkdtempSync(join(tmpdir(), "herdr-utility-exec-test-"));
  try {
    const oldScript = join(dir, "herdr-mcp-exec-old12345.sh");
    const freshScript = join(dir, "herdr-mcp-exec-new12345.sh");
    const unrelated = join(dir, "other.sh");
    writeFileSync(oldScript, "#!/bin/sh\n");
    writeFileSync(freshScript, "#!/bin/sh\n");
    writeFileSync(unrelated, "#!/bin/sh\n");
    const old = new Date(Date.now() - 48 * 60 * 60 * 1000);
    utimesSync(oldScript, old, old);

    const removed = await cleanupStaleUtilityScripts(dir);
    assert.equal(removed, 1);
    assert.equal(existsSync(oldScript), false);
    assert.equal(existsSync(freshScript), true);
    assert.equal(existsSync(unrelated), true);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
