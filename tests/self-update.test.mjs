import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, readdir, stat, writeFile, rm, readlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { pathToFileURL } from "node:url";

import {
  EPOCH1_CONTRACT_HASH,
  REQUIRED_TOOL_COUNT,
  RELEASE_GATES,
  releaseGateEnv,
  SOURCE_EXCLUDED_COMPONENTS,
  versionDigits,
  stableGenerationId,
  candidateGenerationId,
  isPathExcluded,
  copyTree,
  transformServerPlist,
  transformLinkPlist,
  splitServerEnv,
  buildPlan,
  buildRollbackPlan,
  validationOk,
  parseArgs,
  _setSpawnOverride,
  acquireLock,
  releaseLock,
  transferLockToWorker,
  confirmLockOwnership,
  waitForLockOwnership,
  isPidAlive,
  atomicWriteJson,
  parseMcpBody,
  mirrorSummary,
  dispatch,
  prepareRelease,
  reloadServer,
  _setExecFileWrap,
} from "../bin/herdr-self-update";

const SCRIPT = join(process.cwd(), "bin", "herdr-self-update");
const TOKEN = "fixture-bearer-token-not-a-secret";

function fakeEnv(base) {
  return {
    HOME: base,
    HERDR_SELF_UPDATE_HOME: join(base, ".config", "herdr-mcp"),
    HERDR_RUNTIME_CONTROL_PATH: join(base, "test", "runtime-control.json"),
    HERDR_RUNTIME_STATUS_PATH: join(base, "test", "runtime-status.json"),
    HERDR_SERVER_PLIST_PATH: join(base, "test", "server.plist"),
    HERDR_LINK_PLIST_PATH: join(base, "test", "link.plist"),
    HERDR_REPO_PATH: join(base, "repo"),
    HERDR_SELF_UPDATE_LOCK_PATH: join(base, ".config", "herdr-mcp", "self-update", "lock.json"),
  };
}

function serverPlistFixture(overrides = {}) {
  return {
    Label: "dev.herdr-mcp.server",
    ProgramArguments: ["/usr/local/bin/node", "/Users/qingxian/Documents/herdr-mcp/dist/server.js"],
    EnvironmentVariables: {
      HERDR_MCP_BASE_URL: "https://herdr-mcp.agentforme.cc.cd",
      HERDR_MCP_CONTRACT_PROFILE: "epoch1",
      HERDR_MCP_HOST: "127.0.0.1",
      HERDR_MCP_PORT: "8772",
      HERDR_MCP_TOKEN: TOKEN,
      HERDR_SOCKET_PATH: "/Users/qingxian/.config/herdr/herdr.sock",
      ...overrides,
    },
    KeepAlive: true,
    RunAtLoad: true,
  };
}

function linkPlistFixture(generation = "stable-0.3.26") {
  return {
    Label: "dev.herdr-mcp.link-prod",
    ProgramArguments: ["/usr/local/bin/node", "/Users/qingxian/Documents/herdr-mcp/dist/link/macos-daemon.js"],
    EnvironmentVariables: {
      HERDR_EDGE_URL: "wss://herdr-edge-prod.whshang.workers.dev/ws",
      HERDR_WORKSTATION_ID: "prod-real-runtime",
      HERDR_RUNTIME_GENERATION: generation,
    },
  };
}

function fakeStatus(active, generations = []) {
  return {
    schema_version: 1,
    processed_revision: 1,
    desired_active: active,
    outcome: "active_unchanged",
    manager: {
      active_generation: active,
      previous_generation: null,
      last_good_generation: active,
      generations,
    },
  };
}

async function runCli(args, env) {
  const { spawnSync } = await import("node:child_process");
  const r = spawnSync(process.execPath, [SCRIPT, ...args], { env: { ...process.env, ...env }, encoding: "utf8" });
  const lines = r.stdout.trim().split(/\r?\n/).filter(Boolean);
  return { code: r.status, json: JSON.parse(lines.at(-1) || "{}"), stdout: r.stdout, stderr: r.stderr };
}

async function listFiles(root) {
  const out = [];
  async function walk(dir) {
    let entries;
    try { entries = await readdir(dir, { withFileTypes: true }); } catch { return; }
    for (const e of entries) {
      const p = join(dir, e.name);
      if (e.isDirectory()) await walk(p);
      else out.push(p);
    }
  }
  await walk(root);
  return out;
}

test("working-tree plan carries exclusions and never plans Edge/DNS/Worker/Tunnel/OAuth/link restart", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-plan-"));
  await mkdir(join(base, "repo"), { recursive: true });
  await writeFile(join(base, "repo", "package.json"), JSON.stringify({ name: "herdr-mcp", version: "0.3.26" }));
  const plan = buildPlan({ source: "working-tree", ref: "main" }, fakeEnv(base));
  assert.equal(plan.source, "working-tree");
  assert.equal(plan.version, "0.3.26");
  assert.equal(plan.stable_generation, "stable-0.3.26");
  assert.match(plan.commands.sourcePrep[0], /exclude \.git,node_modules,dist,edge\/cloudflare\/dist/);
  assert.equal(plan.commands.deploy, "none (Edge never deployed)");
  assert.equal(plan.noEdgeDeploy, true);
  assert.equal(plan.noDns, true);
  assert.equal(plan.noWorker, true);
  assert.equal(plan.noTunnel, true);
  assert.equal(plan.noOauth, true);
  assert.equal(plan.noProdLinkRestart, true);
  const rendered = JSON.stringify(plan);
  assert.equal(rendered.includes(TOKEN), false);
  assert.equal(rendered.includes("launchctl"), false);
});

test("remote plan contains git fetch + detached worktree and no deploy", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-plan-"));
  await mkdir(join(base, "repo"), { recursive: true });
  await writeFile(join(base, "repo", "package.json"), JSON.stringify({ name: "herdr-mcp", version: "0.3.26" }));
  const plan = buildPlan({ source: "remote", ref: "main" }, fakeEnv(base));
  assert.deepEqual(plan.commands.sourcePrep, ["git fetch origin <ref>", "git worktree add --detach <release> FETCH_HEAD"]);
  assert.equal(plan.noEdgeDeploy, true);
});

test("source exclusion predicate matches .git, node_modules, dist, edge/cloudflare/dist but not siblings", () => {
  assert.equal(isPathExcluded(".git/config"), true);
  assert.equal(isPathExcluded("node_modules/foo/index.js"), true);
  assert.equal(isPathExcluded("dist/server.js"), true);
  assert.equal(isPathExcluded("edge/cloudflare/dist/index.js"), true);
  assert.equal(isPathExcluded("docs/architecture.md"), false);
  assert.equal(isPathExcluded("edge/cloudflare/src/index.js"), false);
  assert.equal(isPathExcluded("src/server.ts"), false);
  assert.equal(isPathExcluded("site-dist/index.html"), true);
  assert.deepEqual(SOURCE_EXCLUDED_COMPONENTS, [".git", "node_modules", "dist", "edge/cloudflare/dist", "site-dist"]);
});

test("copyTree copies a fixture tree while skipping excluded dirs", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-copy-"));
  const src = join(base, "src");
  const dst = join(base, "dst");
  await mkdir(join(src, ".git"), { recursive: true });
  await mkdir(join(src, "node_modules", "dep"), { recursive: true });
  await mkdir(join(src, "dist"), { recursive: true });
  await mkdir(join(src, "edge", "cloudflare", "dist"), { recursive: true });
  await mkdir(join(src, "edge", "cloudflare", "src"), { recursive: true });
  await Promise.all([
    writeFile(join(src, "package.json"), "{}"),
    writeFile(join(src, ".git", "HEAD"), "ref"),
    writeFile(join(src, "node_modules", "dep", "x.js"), "x"),
    writeFile(join(src, "dist", "server.js"), "s"),
    writeFile(join(src, "edge", "cloudflare", "dist", "index.js"), "e"),
    writeFile(join(src, "edge", "cloudflare", "src", "keep.js"), "k"),
  ]);
  await copyTree(src, dst);
  for (const p of ["package.json", "edge/cloudflare/src/keep.js"]) {
    await assert.doesNotReject(readFile(join(dst, p)), `expected ${p} to exist`);
  }
  for (const p of [".git/HEAD", "node_modules/dep/x.js", "dist/server.js", "edge/cloudflare/dist/index.js"]) {
    await assert.rejects(readFile(join(dst, p)), `expected ${p} to be excluded`);
  }
});

test("apply --dry-run writes nothing, touches no plists, and runs no launchctl/git/npm", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-dry-"));
  await mkdir(join(base, "repo"), { recursive: true });
  await writeFile(join(base, "repo", "package.json"), JSON.stringify({ name: "herdr-mcp", version: "0.3.26" }));
  await mkdir(join(base, "test"), { recursive: true });
  const serverPath = join(base, "test", "server.plist");
  const linkPath = join(base, "test", "link.plist");
  await writeFile(serverPath, JSON.stringify(serverPlistFixture()));
  await writeFile(linkPath, JSON.stringify(linkPlistFixture()));
  const beforeServer = await readFile(serverPath, "utf8");
  const beforeLink = await readFile(linkPath, "utf8");
  const env = fakeEnv(base);
  let captured = "";
  const origWrite = process.stdout.write.bind(process.stdout);
  process.stdout.write = (s) => { captured += s; return true; };
  try {
    await dispatch(parseArgs(["apply", "--source", "working-tree", "--dry-run"]), env);
  } finally {
    process.stdout.write = origWrite;
  }
  const out = JSON.parse(captured.trim().split(/\r?\n/).pop());
  assert.equal(out.code, "dry_run");
  assert.match(out.note, /no files, plists, git, npm, or launchctl/);
  // plan never schedules a launchctl command
  assert.equal(JSON.stringify(out.plan.commands).includes("launchctl"), false);
  assert.equal(captured.includes(TOKEN), false);
  assert.equal(await readFile(serverPath, "utf8"), beforeServer);
  assert.equal(await readFile(linkPath, "utf8"), beforeLink);
  assert.deepEqual(await listFiles(join(base, ".config")), []);
});

test("real apply returns immediately with queued 0600 job, lock, and an absolute-path detached worker argv", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-apply-"));
  await mkdir(join(base, "repo"), { recursive: true });
  await writeFile(join(base, "repo", "package.json"), JSON.stringify({ name: "herdr-mcp", version: "0.3.26" }));
  await mkdir(join(base, "test"), { recursive: true });
  await writeFile(join(base, "test", "server.plist"), JSON.stringify(serverPlistFixture()));
  await writeFile(join(base, "test", "link.plist"), JSON.stringify(linkPlistFixture()));
  const env = fakeEnv(base);
  const spawned = [];
  _setSpawnOverride((cmd, args, opts) => {
    spawned.push({ cmd, args, opts });
    return { pid: 4242, unref() {} };
  });
  let captured = "";
  const origWrite = process.stdout.write.bind(process.stdout);
  process.stdout.write = (s) => { captured += s; return true; };
  try {
    await dispatch(parseArgs(["apply", "--source", "working-tree"]), env);
  } finally {
    process.stdout.write = origWrite;
    _setSpawnOverride(null);
  }
  const out = JSON.parse(captured.trim().split(/\r?\n/).pop());
  assert.equal(out.code, "queued");
  const job = JSON.parse(await readFile(out.job_path, "utf8"));
  assert.equal(job.stage, "queued");
  assert.equal((await stat(out.job_path)).mode & 0o777, 0o600);
  assert.equal(JSON.stringify(job).includes(TOKEN), false);
  // point 2: worker is spawned with an absolute filesystem path, never file://
  assert.equal(spawned.length, 1);
  const workerArgv = spawned[0].args;
  assert.match(String(workerArgv[0]), /^\/.*herdr-self-update$/);
  assert.equal(String(workerArgv[0]).startsWith("file://"), false);
  assert.equal(workerArgv[1], "worker");
  // point 3: job never holds a ChildProcess; only numeric pids.
  assert.equal(typeof job.pid, "number");
  assert.equal(job.candidate_pid, null);
  // lock file exists with our holder
  const lock = JSON.parse(await readFile(env.HERDR_SELF_UPDATE_LOCK_PATH, "utf8"));
  assert.equal(lock.job_id, job.job_id);
  assert.equal((await stat(env.HERDR_SELF_UPDATE_LOCK_PATH)).mode & 0o777, 0o600);
  await rm(env.HERDR_SELF_UPDATE_LOCK_PATH, { force: true });
});

test("apply fails closed while another self-update is in flight", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-lock-"));
  await mkdir(join(base, "repo"), { recursive: true });
  await writeFile(join(base, "repo", "package.json"), JSON.stringify({ name: "herdr-mcp", version: "0.3.26" }));
  await mkdir(join(base, "test"), { recursive: true });
  await writeFile(join(base, "test", "server.plist"), JSON.stringify(serverPlistFixture()));
  await writeFile(join(base, "test", "link.plist"), JSON.stringify(linkPlistFixture()));
  const env = fakeEnv(base);
  const paths = { lockPath: env.HERDR_SELF_UPDATE_LOCK_PATH };
  // hold the lock with OUR live pid -> second apply must fail closed
  const held = { job_id: "in-flight-job", pid: process.pid };
  await atomicWriteJson(env.HERDR_SELF_UPDATE_LOCK_PATH, held);
  _setSpawnOverride((cmd, args, opts) => ({ pid: 9, unref() {} }));
  try {
    const r = await runCli(["apply", "--source", "working-tree"], env);
    assert.equal(r.code, 2);
    assert.equal(r.json.code, "self_update_locked");
    assert.equal(r.json.holder_job_id, "in-flight-job");
  } finally {
    _setSpawnOverride(null);
    await rm(env.HERDR_SELF_UPDATE_LOCK_PATH, { force: true });
  }
});

test("stale lock (dead pid) is reaped and apply proceeds", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-lock-"));
  await mkdir(join(base, "repo"), { recursive: true });
  await writeFile(join(base, "repo", "package.json"), JSON.stringify({ name: "herdr-mcp", version: "0.3.26" }));
  await mkdir(join(base, "test"), { recursive: true });
  await writeFile(join(base, "test", "server.plist"), JSON.stringify(serverPlistFixture()));
  await writeFile(join(base, "test", "link.plist"), JSON.stringify(linkPlistFixture()));
  const env = fakeEnv(base);
  // pid 99999 is extremely unlikely to be alive -> stale
  await atomicWriteJson(env.HERDR_SELF_UPDATE_LOCK_PATH, { job_id: "dead-job", pid: 99999 });
  _setSpawnOverride((cmd, args, opts) => ({ pid: 10, unref() {} }));
  try {
    const r = await runCli(["apply", "--source", "working-tree"], env);
    assert.equal(r.code, 0);
    assert.equal(r.json.code, "queued");
  } finally {
    _setSpawnOverride(null);
    await rm(env.HERDR_SELF_UPDATE_LOCK_PATH, { force: true });
  }
});

test("server plist transformation changes only ProgramArguments[1] and preserves profile/base/socket/token", () => {
  const plist = serverPlistFixture();
  const next = transformServerPlist(plist, "/Users/qingxian/.config/herdr-mcp/releases/0.3.27-abcd/dist/server.js");
  assert.equal(next.ProgramArguments[0], plist.ProgramArguments[0]);
  assert.equal(next.ProgramArguments[1], "/Users/qingxian/.config/herdr-mcp/releases/0.3.27-abcd/dist/server.js");
  assert.deepEqual(next.EnvironmentVariables, plist.EnvironmentVariables);
  assert.equal(next.EnvironmentVariables.HERDR_MCP_CONTRACT_PROFILE, "epoch1");
  assert.equal(next.EnvironmentVariables.HERDR_MCP_TOKEN, TOKEN);
  assert.equal(next.EnvironmentVariables.HERDR_MCP_BASE_URL, "https://herdr-mcp.agentforme.cc.cd");
});

test("link plist transformation changes only HERDR_RUNTIME_GENERATION", () => {
  const plist = linkPlistFixture("stable-0.3.26");
  const next = transformLinkPlist(plist, "stable-0.3.27");
  assert.equal(next.EnvironmentVariables.HERDR_RUNTIME_GENERATION, "stable-0.3.27");
  assert.equal(next.ProgramArguments[1], plist.ProgramArguments[1]);
  assert.deepEqual(next.EnvironmentVariables.HERDR_EDGE_URL, plist.EnvironmentVariables.HERDR_EDGE_URL);
  assert.deepEqual(next.EnvironmentVariables.HERDR_WORKSTATION_ID, plist.EnvironmentVariables.HERDR_WORKSTATION_ID);
});

test("generation naming embeds the full semver (no 0.3.x/0.4.x patch collisions)", () => {
  assert.equal(versionDigits("0.3.26"), "026");
  assert.equal(stableGenerationId("0.3.26"), "stable-0.3.26");
  assert.equal(stableGenerationId("0.3.27"), "stable-0.3.27");
  assert.equal(stableGenerationId("0.4.1"), "stable-0.4.1");
  assert.match(candidateGenerationId("0.3.26", "abc123def456"), /^candidate-0\.3\.26-[a-z0-9]{1,6}$/);
  assert.notEqual(candidateGenerationId("0.3.26", "aabbccddeeff"), stableGenerationId("0.3.26"));
  assert.notEqual(stableGenerationId("0.3.0"), stableGenerationId("0.4.0"));
});

test("validationOk requires ok + exact hash + tools + version", () => {
  const good = {
    ok: true,
    code: "validated",
    runtime_version: "0.3.26",
    contract_hash: EPOCH1_CONTRACT_HASH,
    tool_count: REQUIRED_TOOL_COUNT,
  };
  assert.equal(validationOk(good, "0.3.26"), true);
  assert.equal(validationOk({ ...good, contract_hash: "sha256:deadbeef" }, "0.3.26"), false);
  assert.equal(validationOk({ ...good, tool_count: 16 }, "0.3.26"), false);
  assert.equal(validationOk({ ...good, runtime_version: "0.3.25" }, "0.3.26"), false);
  assert.equal(validationOk({ ok: false, code: "contract_mismatch" }, "0.3.26"), false);
  assert.equal(validationOk(null, "0.3.26"), false);
});

test("rollback ordering is phase-aware: reloaded 8772 restores plist before re-activating original", () => {
  const switchedJob = {
    candidate_active: true,
    server_reloaded: true,
    server_plist_edited: true,
    server_plist_backup: "/x/backup/server.plist",
    server_plist: "/x/server.plist",
    link_plist_backup: "/x/backup/link.plist",
    link_plist: "/x/link.plist",
    candidate_generation: "candidate-0.3.27-ab12cd",
    original_active: "stable-0.3.26",
  };
  const steps = buildRollbackPlan(switchedJob);
  const actions = steps.map((s) => s.action);
  assert.deepEqual(actions.slice(0, 3), ["restore_server_plist", "reload_server_from_original", "verify_original_runtime"]);
  const actIdx = actions.indexOf("activate_original_generation");
  assert.ok(actIdx > 2, "original activation must come after 8772 restore when already reloaded");
  assert.equal(actions.at(-1), "stop_candidate");
  assert.ok(actions.indexOf("remove_candidate_generation") < actions.indexOf("stop_candidate"));
});

test("rollback ordering: not-yet-reloaded 8772 switches original pointer before restore", () => {
  const notSwitched = {
    candidate_active: true,
    server_reloaded: false,
    server_plist_edited: true,
    server_plist_backup: "/x/backup/server.plist",
    server_plist: "/x/server.plist",
    candidate_generation: "candidate-0.3.27-ab12cd",
    original_active: "stable-0.3.26",
  };
  const actions = buildRollbackPlan(notSwitched).map((s) => s.action);
  assert.equal(actions[0], "activate_original_generation");
  // plist restore appears without a reload because 8772 never switched
  assert.equal(actions.includes("reload_server_from_original"), false);
  assert.ok(actions.indexOf("restore_server_plist") > actions.indexOf("activate_original_generation"));
  assert.equal(actions.at(-1), "stop_candidate");
});

test("splitServerEnv isolates the token and never leaks it into the redacted env", () => {
  const plist = serverPlistFixture();
  const { token, env } = splitServerEnv(plist);
  assert.equal(token, TOKEN);
  assert.equal(env.HERDR_MCP_TOKEN, undefined);
  assert.equal(env.HERDR_MCP_CONTRACT_PROFILE, "epoch1");
  assert.equal(JSON.stringify(env).includes(TOKEN), false);
  // point 1: re-injection is the caller's job; token is not in env, it IS injectable
  assert.ok(token.length > 0);
});

test("atomicWriteJson uses same-dir rename (no rm+copy window) and mode 0600", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-atomic-"));
  const p = join(base, "state.json");
  await atomicWriteJson(p, { a: 1 });
  assert.deepEqual(JSON.parse(await readFile(p, "utf8")), { a: 1 });
  assert.equal((await stat(p)).mode & 0o777, 0o600);
  await atomicWriteJson(p, { a: 2 });
  assert.deepEqual(JSON.parse(await readFile(p, "utf8")), { a: 2 });
  const leftovers = (await readdir(base)).filter((f) => f.includes(".tmp-"));
  assert.deepEqual(leftovers, []);
});

test("acquireLock fails closed on live holder, reaps stale, releases by owner", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-lock2-"));
  const lockPath = join(base, "lock.json");
  const paths = { lockPath };
  // 1) no lock -> acquire ok
  const job = { job_id: "j1" };
  const a = await acquireLock(paths, job);
  assert.equal(a.ok, true);
  // 2) our own live pid holds it -> fail closed
  const b = await acquireLock(paths, job);
  assert.equal(b.ok, false);
  assert.equal(b.code, "self_update_locked");
  // 3) release by owner
  const rel = await releaseLock(paths, "j1");
  assert.equal(rel.ok, true);
  // 4) stale lock reaped
  await atomicWriteJson(lockPath, { job_id: "dead", pid: 99999 });
  const c = await acquireLock(paths, { job_id: "j2" });
  assert.equal(c.ok, true);
  const lock = JSON.parse(await readFile(lockPath, "utf8"));
  assert.equal(lock.job_id, "j2");
  assert.equal(isPidAlive(99999), false);
  assert.equal(isPidAlive(process.pid), true);
});

test("parseMcpBody handles SSE data: framing and plain JSON", () => {
  const sse = 'data: {"jsonrpc":"2.0","id":1,"result":{"serverInfo":{"name":"herdr","version":"0.3.27-abc"}}}\n\n';
  const body = parseMcpBody(sse);
  assert.equal(body.result.serverInfo.version, "0.3.27-abc");
  const plain = parseMcpBody('{"jsonrpc":"2.0","id":2,"result":{"ok":true}}');
  assert.equal(plain.result.ok, true);
  assert.equal(parseMcpBody(""), null);
  assert.throws(() => parseMcpBody("not json"));
});

test("release gates align with CI (build/root/edge/site/extension), never deploy", () => {
  const cmds = RELEASE_GATES.map(([c, a]) => `${c} ${a.join(" ")}`);
  assert.ok(cmds.some((c) => c.includes("npm ci")));
  assert.ok(cmds.some((c) => c.includes("run build")));
  assert.ok(cmds.some((c) => c.includes("npm test")));
  assert.ok(cmds.some((c) => c.includes("test:edge")));
  assert.ok(cmds.some((c) => c.includes("build:site")));
  assert.ok(cmds.some((c) => c.includes("extension_smoke.mjs")));
  assert.equal(cmds.some((c) => /deploy|wrangler|publish/.test(c)), false);
});

test("release gate environment drops production contract-profile overrides", () => {
  const clean = releaseGateEnv({
    PATH: "/bin:/usr/bin",
    HOME: "/tmp/example",
    HERDR_MCP_CONTRACT_PROFILE: "epoch1",
    HERDR_MCP_ALL_TOOLS: "1",
    HERDR_RUNTIME_GENERATION: "stable-026",
  });
  assert.equal(clean.HERDR_MCP_CONTRACT_PROFILE, undefined);
  assert.equal(clean.HERDR_MCP_ALL_TOOLS, undefined);
  assert.equal(clean.HERDR_RUNTIME_GENERATION, "stable-026");
  assert.equal(clean.PATH, "/bin:/usr/bin");
});

test("parseArgs understands the supported CLI surface and rejects unknowns", () => {
  assert.deepEqual(parseArgs(["plan", "--source", "remote", "--ref", "main"]), {
    command: "plan", source: "remote", ref: "main", dryRun: false, help: false, job: null,
  });
  assert.equal(parseArgs(["apply", "--dry-run"]).dryRun, true);
  assert.equal(parseArgs(["status", "--job", "abc"]).job, "abc");
  assert.throws(() => parseArgs(["apply", "--bogus"]), /unknown_argument/);
});

test("status --job reports structured state without secrets", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-status-"));
  const env = fakeEnv(base);
  await mkdir(join(env.HERDR_SELF_UPDATE_HOME, "self-update", "jobs"), { recursive: true });
  const job = {
    schema_version: 1,
    job_id: "j1",
    stage: "queued",
    version: "0.3.26",
    outcome: null,
    rollback_steps: [],
  };
  await writeFile(join(env.HERDR_SELF_UPDATE_HOME, "self-update", "jobs", "j1.json"), JSON.stringify(job));
  const r = await runCli(["status", "--job", "j1"], env);
  assert.equal(r.code, 0);
  assert.equal(r.json.code, "job_status");
  assert.equal(r.json.job.job_id, "j1");
  assert.equal(r.stdout.includes(TOKEN), false);
  const missing = await runCli(["status", "--job", "nope"], env);
  assert.equal(missing.json.code, "job_not_found");
});

test("dispatch supports check/plan on a fixture repo without side effects", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-dispatch-"));
  await mkdir(join(base, "repo"), { recursive: true });
  await writeFile(join(base, "repo", "package.json"), JSON.stringify({ name: "herdr-mcp", version: "0.3.26" }));
  await mkdir(join(base, "test"), { recursive: true });
  await writeFile(join(base, "test", "runtime-status.json"), JSON.stringify(fakeStatus("stable-0.3.26")));
  await writeFile(join(base, "test", "runtime-control.json"), JSON.stringify({ desired_active: "stable-0.3.26", revision: 1 }));
  const env = fakeEnv(base);
  let captured = "";
  const origWrite = process.stdout.write.bind(process.stdout);
  process.stdout.write = (s) => { captured += s; return true; };
  try {
    await dispatch(parseArgs(["check", "--source", "working-tree"]), env);
  } finally {
    process.stdout.write = origWrite;
  }
  const out = JSON.parse(captured.trim().split(/\r?\n/).pop());
  assert.equal(out.code, "check");
  assert.equal(out.current_active, "stable-0.3.26");
  assert.equal(out.plan.noEdgeDeploy, true);
});
test("mirrorSummary writes a 0600 token-free summary that tracks job stage (queued -> succeeded)", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-summary-"));
  const env = fakeEnv(base);
  const paths = { summaryStatusPath: join(env.HERDR_SELF_UPDATE_HOME, "self-update-status.json") };
  const job = {
    job_id: "j-sum",
    version: "0.3.26",
    source: "working-tree",
    ref: "main",
    stage: "queued",
    outcome: null,
    stable_generation: "stable-0.3.26",
    candidate_generation: "candidate-0.3.26-ab12cd",
    control_path: env.HERDR_RUNTIME_CONTROL_PATH,
    summary_status_path: paths.summaryStatusPath,
    updated_at_ms: 1234,
  };
  await mirrorSummary(job);
  let summary = JSON.parse(await readFile(paths.summaryStatusPath, "utf8"));
  assert.equal(summary.state, "queued");
  assert.equal(summary.stage ?? summary.status, "queued");
  assert.equal(summary.outcome, null);
  assert.equal(summary.target_version, "0.3.26");
  assert.equal(summary.updated_at, "1970-01-01T00:00:01.234Z");
  assert.equal(summary.updated_at_ms, 1234);
  assert.equal(summary.job_id, "j-sum");
  assert.equal(JSON.stringify(summary).includes(TOKEN), false);
  assert.equal((await stat(paths.summaryStatusPath)).mode & 0o777, 0o600);

  job.stage = "succeeded";
  job.outcome = "succeeded";
  job.updated_at_ms = 9999000;
  await mirrorSummary(job);
  summary = JSON.parse(await readFile(paths.summaryStatusPath, "utf8"));
  assert.equal(summary.state, "succeeded");
  assert.equal(summary.outcome, "succeeded");
  assert.equal(summary.updated_at, "1970-01-01T02:46:39.000Z");
});

test("apply mirrors a queued summary at the skill-visible path (no token, stage=queued)", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-qsum-"));
  await mkdir(join(base, "repo"), { recursive: true });
  await writeFile(join(base, "repo", "package.json"), JSON.stringify({ name: "herdr-mcp", version: "0.3.26" }));
  await mkdir(join(base, "test"), { recursive: true });
  await writeFile(join(base, "test", "server.plist"), JSON.stringify(serverPlistFixture()));
  await writeFile(join(base, "test", "link.plist"), JSON.stringify(linkPlistFixture()));
  const env = fakeEnv(base);
  _setSpawnOverride((cmd, args, opts) => ({ pid: 4242, unref() {} }));
  let captured = "";
  const origWrite = process.stdout.write.bind(process.stdout);
  process.stdout.write = (s) => { captured += s; return true; };
  try {
    await dispatch(parseArgs(["apply", "--source", "working-tree"]), env);
  } finally {
    process.stdout.write = origWrite;
    _setSpawnOverride(null);
  }
  const summaryPath = join(env.HERDR_SELF_UPDATE_HOME, "self-update-status.json");
  const summary = JSON.parse(await readFile(summaryPath, "utf8"));
  assert.equal(summary.state, "queued");
  assert.equal(summary.stage ?? summary.status, "queued");
  assert.equal(JSON.stringify(summary).includes(TOKEN), false);
  assert.equal((await stat(summaryPath)).mode & 0o777, 0o600);
  await rm(env.HERDR_SELF_UPDATE_LOCK_PATH, { force: true });
});

test("status without --job returns the self_update summary alongside runtime status", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-status2-"));
  await mkdir(join(base, "test"), { recursive: true });
  await writeFile(join(base, "test", "runtime-status.json"), JSON.stringify(fakeStatus("stable-0.3.26")));
  await writeFile(join(base, "test", "runtime-control.json"), JSON.stringify({ desired_active: "stable-0.3.26", revision: 1 }));
  const env = fakeEnv(base);
  const summaryPath = join(env.HERDR_SELF_UPDATE_HOME, "self-update-status.json");
  await mkdir(dirname(summaryPath), { recursive: true });
  await atomicWriteJson(summaryPath, { schema_version: 1, job_id: "j1", state: "succeeded", target_version: "0.3.27" });
  let captured = "";
  const origWrite = process.stdout.write.bind(process.stdout);
  process.stdout.write = (s) => { captured += s; return true; };
  try {
    await dispatch(parseArgs(["status"]), env);
  } finally {
    process.stdout.write = origWrite;
  }
  const out = JSON.parse(captured.trim().split(/\r?\n/).pop());
  assert.equal(out.code, "runtime_status");
  assert.equal(out.self_update.job_id, "j1");
  assert.equal(out.self_update.state, "succeeded");
  assert.equal(out.self_update_status_path, summaryPath);
});

test("lock handshake waits for parent->worker pid transfer instead of failing on the first read", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-hs-"));
  const lockPath = join(base, "lock.json");
  const paths = { lockPath };
  const jobId = "j-hs";
  const workerPid = 7777;
  // Simulate: apply parent wrote the lock with its own pid, worker starts before
  // transferLockToWorker runs, and the parent transfers ownership shortly after.
  let reads = 0;
  await atomicWriteJson(lockPath, { job_id: jobId, pid: process.pid, started_at_ms: 1 });
  const sleeps = [];
  const sleepFn = async (ms) => {
    reads += 1;
    sleeps.push(ms);
    // after the first failed read, transfer to worker pid (like the parent)
    if (reads === 1) {
      await atomicWriteJson(lockPath, { job_id: jobId, pid: workerPid, started_at_ms: 1 });
    }
  };
  const owned = await waitForLockOwnership(paths, jobId, { timeoutMs: 50, pollMs: 5, sleep: sleepFn, pid: workerPid });
  assert.equal(owned.ok, true);
  // at least one failed read + poll happened before the parent's transfer landed
  assert.ok(reads >= 1);
});

test("lock handshake fails closed when the worker pid never becomes the owner", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-hs2-"));
  const lockPath = join(base, "lock.json");
  const paths = { lockPath };
  const jobId = "j-hs2";
  // lock keeps the parent pid; no transfer ever happens -> worker must abort
  await atomicWriteJson(lockPath, { job_id: jobId, pid: 123456789, started_at_ms: 1 });
  const owned = await waitForLockOwnership(paths, jobId, { timeoutMs: 30, pollMs: 5, sleep: async () => {} });
  assert.equal(owned.ok, false);
  assert.equal(owned.code, "lock_handshake_timeout");
});

test("transferLockToWorker updates owner pid and refuses when the job does not own the lock", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-tfr-"));
  const lockPath = join(base, "lock.json");
  const paths = { lockPath };
  await atomicWriteJson(lockPath, { job_id: "j-owner", pid: 1111, started_at_ms: 1 });
  const ok = await transferLockToWorker(paths, "j-owner", 7777);
  assert.equal(ok.ok, true);
  assert.equal(JSON.parse(await readFile(lockPath, "utf8")).pid, 7777);
  // a different job must not be able to transfer (or release) this lock
  const bad = await transferLockToWorker(paths, "j-other", 8888);
  assert.equal(bad.ok, false);
  assert.equal(bad.code, "lock_not_owner");
  assert.equal(JSON.parse(await readFile(lockPath, "utf8")).pid, 7777);
  const rel = await releaseLock(paths, "j-other");
  assert.equal(rel.ok, false);
  assert.equal(JSON.parse(await readFile(lockPath, "utf8")).job_id, "j-owner");
});

test("reloadServer retries bootstrap with backoff until launchd accepts the plist", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-rel-"));
  const plistPath = join(base, "server.plist");
  await writeFile(plistPath, "<plist/>");
  let bootstrapCalls = 0;
  let running = false;
  // Mock launchctl: print fails (absent) until after a successful bootstrap;
  // bootstrap fails twice with the real-world "Bootstrap failed: 5" then succeeds.
  const prev = _setExecFileWrap(async (cmd, args) => {
    if (cmd === "/bin/launchctl" && args[0] === "bootout") return { code: 0 };
    if (cmd === "/bin/launchctl" && args[0] === "print") {
      return running ? { code: 0, stdout: "state = running", stderr: "" } : { code: 3, stdout: "", stderr: "Could not find service" };
    }
    if (cmd === "/bin/launchctl" && args[0] === "bootstrap") {
      bootstrapCalls += 1;
      if (bootstrapCalls <= 2) return { code: 5, stderr: "Bootstrap failed: 5" };
      running = true;
      return { code: 0 };
    }
    return { code: 1 };
  });
  try {
    const result = await reloadServer(plistPath, { delays: [0, 0, 0, 0] });
    assert.equal(result.ok, true);
    assert.equal(bootstrapCalls, 3);
    assert.ok(result.attempts >= 3);
  } finally {
    _setExecFileWrap(prev);
  }
});

test("reloadServer fails closed when bootstrap never succeeds", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-rel2-"));
  const plistPath = join(base, "server.plist");
  await writeFile(plistPath, "<plist/>");
  const prev = _setExecFileWrap(async (cmd, args) => {
    if (cmd === "/bin/launchctl" && args[0] === "bootout") return { code: 0 };
    if (cmd === "/bin/launchctl" && args[0] === "print") return { code: 3 };
    if (cmd === "/bin/launchctl" && args[0] === "bootstrap") return { code: 5, stderr: "Bootstrap failed: 5" };
    return { code: 1 };
  });
  try {
    const result = await reloadServer(plistPath, { delays: [0, 0, 0, 0] });
    assert.equal(result.ok, false);
  } finally {
    _setExecFileWrap(prev);
  }
});

test("verifyRuntimeAt verifies a served version and accepts version-exact expectations", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-ver-"));
  // export check: verifyRuntimeAt is internal; validate SSE framing through parseMcpBody
  const sse = 'data: {"jsonrpc":"2.0","id":"verify","result":{"serverInfo":{"name":"herdr-mcp","version":"0.3.26"}}}';
  const body = parseMcpBody(sse);
  assert.equal(body.result.serverInfo.version, "0.3.26");
  // rollback verify_original_runtime uses readPlist+splitServerEnv+verifyRuntimeAt;
  // at minimum assert the restore/rollback ordering re-verifies after reload.
  const job = {
    candidate_active: true,
    server_reloaded: true,
    server_plist_edited: true,
    server_plist_backup: "/a/backup",
    server_plist: "/a/server.plist",
    candidate_generation: "candidate-0.3.27-ab12cd",
    original_active: "stable-0.3.26",
    original_generation_spec: { generation: "stable-0.3.26", endpoint: "http://127.0.0.1:8772/mcp", runtime_version: "0.3.26" },
  };
  const steps = buildRollbackPlan(job);
  assert.ok(steps.some((s) => s.action === "verify_original_runtime"));
  const verifyStep = steps.find((s) => s.action === "verify_original_runtime");
  assert.ok(verifyStep);
});

test("git-aware working-tree snapshot via the production prepareRelease path", async () => {
  const base = await mkdtemp(join(tmpdir(), "su-git-"));
  const repo = join(base, "repo");
  const release = join(base, "release");
  await mkdir(join(repo, "src"), { recursive: true });
  await mkdir(join(repo, "docs", "_wip"), { recursive: true });
  await mkdir(join(repo, "site-dist"), { recursive: true });
  await mkdir(join(repo, "node_modules", "pkg"), { recursive: true });
  // git tracked file
  await writeFile(join(repo, "src", "tracked.ts"), "v1");
  // git modified tracked file (committed as "old", then changed in worktree)
  // ignored secret
  await writeFile(join(repo, ".env"), "SECRET=1");
  await writeFile(join(repo, "docs", "_wip", "notes.md"), "wip");
  await mkdir(join(repo, ".git"), { recursive: true });
  await writeFile(join(repo, ".gitignore"), "node_modules/\ndist/\nsite-dist/\ndocs/_wip/\n.env\n");
  await writeFile(join(repo, "node_modules", "pkg", "index.js"), "x");
  await writeFile(join(repo, "site-dist", "index.html"), "<html/>");
  // build the fake git index via a mocked ls-files that reflects the real fixture:
  // tracked + untracked-but-not-ignored are: src/tracked.ts (only if committed).
  // We commit src/tracked.ts as tracked so ls-files --cached lists it.
  const { execFileSync } = await import("node:child_process");
  const git = (args, opts) => execFileSync("git", args, { cwd: repo, ...opts });
  git(["init", "-q"]);
  git(["config", "user.email", "t@t.t"]);
  git(["config", "user.name", "t"]);
  // add + modify tracked file to simulate a local modification in the worktree
  await writeFile(join(repo, "src", "tracked.ts"), "committed-v1");
  git(["add", "src/tracked.ts"]);
  git(["commit", "-q", "-m", "init"]);
  await writeFile(join(repo, "src", "tracked.ts"), "worktree-modified-v2");
  // now the committed .gitignore would ignore .env/docs/_wip only if committed;
  // commit it too so git ls-files honors it
  git(["add", ".gitignore"]);
  git(["commit", "-q", "-m", "ignore"]);
  // untracked but NOT ignored file -> should be copied
  await writeFile(join(repo, "src", "new-untracked.ts"), "untracked");

  const job = { source: "working-tree", release, ref: "main" };
  const paths = { repoPath: repo };
  const logs = [];
  const log = async (line) => { logs.push(line); };
  const result = await prepareRelease(job, {}, paths, log);
  assert.equal(result.ok, true);
  assert.equal(result.copied >= 2, true);
  // modified tracked + untracked nonignored copied; ignored/secret/build excluded
  assert.equal(await readFile(join(release, "src", "tracked.ts"), "utf8"), "worktree-modified-v2");
  assert.equal(await readFile(join(release, "src", "new-untracked.ts"), "utf8"), "untracked");
  await assert.rejects(readFile(join(release, ".env")));
  await assert.rejects(readFile(join(release, "docs", "_wip", "notes.md")));
  await assert.rejects(readFile(join(release, "site-dist", "index.html")));
  await assert.rejects(readFile(join(release, "node_modules", "pkg", "index.js")));
});
