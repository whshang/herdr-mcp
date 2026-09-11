import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const source = fs.readFileSync(new URL("../crates/herdr-mcp/src/windows_service_manager.rs", import.meta.url), "utf8");

test("Windows Link reconciles durable runtime-control before restart", () => {
  const start = source.indexOf("pub fn reconcile_link()");
  const end = source.indexOf("\nfn reconcile_runtime_control", start);
  assert.ok(start >= 0 && end > start, "Windows reconcile_link body must remain discoverable");
  const body = source.slice(start, end);
  const reconcile = body.indexOf("reconcile_runtime_control(&paths, &runtime)?");
  const startLink = body.indexOf("start_link(&paths)");
  assert.ok(reconcile >= 0, "Windows Link must reconcile stale runtime-control");
  assert.ok(startLink > reconcile, "runtime-control reconciliation must happen before Link starts");
});

test("Windows managed children are pinned to the active generation", () => {
  const start = source.indexOf("fn start_managed_process(");
  assert.ok(start >= 0);
  const body = source.slice(start, source.indexOf("\nfn open_startup_log", start));
  assert.match(body, /current_generation\(paths\)/);
  assert.match(body, /\.env\("HERDR_RUNTIME_GENERATION", &generation\)/);
});
