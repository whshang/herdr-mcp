import assert from "node:assert/strict";
import { mkdtemp, readFile, stat, writeFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

function run(args, env) {
  const result = spawnSync(process.execPath, ["bin/herdr-runtime-generation", ...args], {
    cwd: process.cwd(),
    env: { ...process.env, ...env },
    encoding: "utf8",
  });
  const line = result.stdout.trim().split(/\r?\n/).filter(Boolean).at(-1) || "{}";
  return { code: result.status, json: JSON.parse(line), stderr: result.stderr };
}

async function paths() {
  const dir = await mkdtemp(join(tmpdir(), "herdr-runtime-cli-"));
  return {
    dir,
    control: join(dir, "runtime-control.json"),
    status: join(dir, "runtime-status.json"),
    env: {
      HERDR_RUNTIME_CONTROL_PATH: join(dir, "runtime-control.json"),
      HERDR_RUNTIME_STATUS_PATH: join(dir, "runtime-status.json"),
    },
  };
}

test("runtime generation CLI registers and activates a loopback candidate using mode-600 control state", async () => {
  const p = await paths();
  const registered = run(["register", "--generation", "candidate-026", "--endpoint", "http://127.0.0.1:8773/mcp", "--runtime-version", "0.3.26"], p.env);
  assert.equal(registered.code, 0);
  assert.equal(registered.json.code, "generation_registered");
  let control = JSON.parse(await readFile(p.control, "utf8"));
  assert.equal(control.desired_active, "local-mcp-active");
  assert.equal(control.generations.find((g) => g.generation === "candidate-026").expected_runtime_version, "0.3.26");
  assert.equal((await stat(p.control)).mode & 0o777, 0o600);

  const activated = run(["activate", "--generation", "candidate-026", "--checks", "4", "--interval-ms", "250"], p.env);
  assert.equal(activated.code, 0);
  assert.equal(activated.json.code, "activation_requested");
  control = JSON.parse(await readFile(p.control, "utf8"));
  assert.equal(control.desired_active, "candidate-026");
  assert.deepEqual(control.observation, { checks: 4, interval_ms: 250 });
});

test("runtime generation CLI rejects non-loopback candidate endpoints", async () => {
  const p = await paths();
  const result = run(["register", "--generation", "bad", "--endpoint", "http://10.0.0.5:8773/mcp"], p.env);
  assert.equal(result.code, 2);
  assert.equal(result.json.code, "endpoint_must_be_loopback");
});

test("runtime generation CLI rollback uses link status previous_generation and never needs a credential", async () => {
  const p = await paths();
  run(["register", "--generation", "candidate", "--endpoint", "http://127.0.0.1:8773/mcp"], p.env);
  run(["activate", "--generation", "candidate"], p.env);
  await writeFile(p.status, JSON.stringify({
    schema_version: 1,
    processed_revision: 3,
    desired_active: "candidate",
    outcome: "activated",
    manager: {
      active_generation: "candidate",
      previous_generation: "local-mcp-active",
      last_good_generation: "candidate",
    },
  }), { mode: 0o600 });
  const result = run(["rollback"], p.env);
  assert.equal(result.code, 0);
  assert.equal(result.json.code, "rollback_requested");
  assert.equal(result.json.generation, "local-mcp-active");
  const control = JSON.parse(await readFile(p.control, "utf8"));
  assert.equal(control.desired_active, "local-mcp-active");
  assert.equal(JSON.stringify(control).includes("token"), false);
});
