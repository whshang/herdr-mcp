import assert from 'node:assert/strict';
import { chmod, mkdir, mkdtemp, readFile, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import test from 'node:test';

const url = new URL('../bin/watchdog.sh', import.meta.url);
const script = await readFile(url, 'utf8');
const cli = await readFile(new URL('../bin/herdr-mcp', import.meta.url), 'utf8');
const plannerSkill = await readFile(new URL('../assets/herdr-mcp-SKILL.md', import.meta.url), 'utf8');

async function writeExecutable(path, text) {
  await writeFile(path, text, { mode: 0o700 });
  await chmod(path, 0o700);
}

async function makeHarness({
  loaded,
  healthCode,
  mutationActive = false,
  updateState = 'succeeded',
  guardianState = null,
  unloadAfterPrints = null,
  linkLoaded = true,
  linkStatusJson = null,
  linkEvidence = '',
  linkUnloadAfterPrints = null,
}) {
  const root = await mkdtemp(join(tmpdir(), 'herdr-watchdog-'));
  const home = join(root, 'home');
  const cfg = join(root, 'cfg');
  const log = join(root, 'launchctl.log');
  const runtimeLog = join(root, 'runtime.log');
  const curlLog = join(root, 'curl.log');
  const serverPrintCount = join(root, 'launchctl-server-print-count');
  const linkPrintCount = join(root, 'launchctl-link-print-count');
  const linkEvidenceFile = join(root, 'link-evidence.txt');
  const launchctl = join(root, 'launchctl');
  const curl = join(root, 'curl');
  const lsof = join(root, 'lsof');
  const runtime = join(root, 'herdr-mcp');
  await mkdir(cfg, { recursive: true });
  await writeFile(linkEvidenceFile, linkEvidence);
  await writeExecutable(
    launchctl,
    `#!/bin/bash
set -eu
echo "$*" >> ${JSON.stringify(log)}
if [[ "$1" == "print" ]]; then
  label="\${2##*/}"
  if [[ "$label" == "dev.herdr-mcp.server" ]]; then
    count=0
    [[ -f ${JSON.stringify(serverPrintCount)} ]] && count="\$(cat ${JSON.stringify(serverPrintCount)})"
    count=\$((count + 1))
    echo "\$count" > ${JSON.stringify(serverPrintCount)}
    ${loaded ? ':' : 'exit 1'}
    ${unloadAfterPrints === null ? ':' : `[[ "$count" -le ${unloadAfterPrints} ]] || exit 1`}
    exit 0
  elif [[ "$label" == "dev.herdr-mcp.link-prod" ]]; then
    count=0
    [[ -f ${JSON.stringify(linkPrintCount)} ]] && count="\$(cat ${JSON.stringify(linkPrintCount)})"
    count=\$((count + 1))
    echo "\$count" > ${JSON.stringify(linkPrintCount)}
    ${linkLoaded ? ':' : 'exit 1'}
    ${linkUnloadAfterPrints === null ? ':' : `[[ "$count" -le ${linkUnloadAfterPrints} ]] || exit 1`}
    cat ${JSON.stringify(linkEvidenceFile)}
    exit 0
  fi
  exit 0
fi
exit 0
`,
  );
  await writeExecutable(curl, `#!/bin/bash
echo "$*" >> ${JSON.stringify(curlLog)}
printf '%s' ${JSON.stringify(healthCode)}
`);
  await writeExecutable(lsof, `#!/bin/bash
exit ${mutationActive ? 0 : 1}
`);
  const updateStatus = JSON.stringify({ job: { state: updateState } });
  const linkStatus = linkStatusJson ?? JSON.stringify({
    ok: true,
    production_owner: 'rust',
    agents: [{
      label: 'dev.herdr-mcp.link-prod',
      loaded: true,
      implementation: 'rust',
      points_at_repo_checkout: false,
    }],
    production_runtime_alignment: {
      current_generation: 'rust-7d7db9d2063970d2',
      active_generation: 'rust-7d7db9d2063970d2',
      loaded_launchd_generation: 'rust-7d7db9d2063970d2',
      runtime_control_active_matches_current: true,
      loaded_environment_stale: false,
    },
  });
  await writeExecutable(runtime, `#!/bin/bash
echo "$*" >> ${JSON.stringify(runtimeLog)}
if [[ "$1" == "link" && "$2" == "status" ]]; then
cat <<'JSON'
${linkStatus}
JSON
exit 0
fi
cat <<'JSON'
${updateStatus}
JSON
`);
  if (mutationActive) await writeFile(join(cfg, 'service-mutation.lock'), '');
  if (guardianState) {
    const guardianDir = join(cfg, 'guardians', 'gtx-test-123456');
    await mkdir(guardianDir, { recursive: true });
    await writeFile(join(guardianDir, 'transaction.json'), JSON.stringify({ state: guardianState }));
  }
  return { root, home, cfg, log, runtimeLog, curlLog, linkEvidenceFile, launchctl, curl, lsof, runtime };
}

function runOnce(harness, extraEnv = {}) {
  return spawnSync('/bin/bash', [new URL('../bin/watchdog.sh', import.meta.url).pathname, 'once'], {
    encoding: 'utf8',
    timeout: 5000,
    env: {
      ...process.env,
      HOME: harness.home,
      HERDR_MCP_CONFIG_DIR: harness.cfg,
      HERDR_MCP_LAUNCHCTL_BIN: harness.launchctl,
      HERDR_MCP_CURL_BIN: harness.curl,
      HERDR_MCP_LSOF_BIN: harness.lsof,
      HERDR_MCP_RUNTIME_BIN: harness.runtime,
      HERDR_MCP_WATCHDOG_FAIL_THRESHOLD: '2',
      HERDR_MCP_WATCHDOG_COOLDOWN_SEC: '0',
      HERDR_MCP_WATCHDOG_RESTART_BURST_LIMIT: '3',
      HERDR_MCP_WATCHDOG_RESTART_BACKOFF_SEC: '300',
      HERDR_MCP_LINK_WATCHDOG_FAIL_THRESHOLD: '2',
      HERDR_MCP_LINK_WATCHDOG_COOLDOWN_SEC: '0',
      ...extraEnv,
    },
  });
}

test('watchdog pins the managed Rust service and removes Node process heuristics', async () => {
  assert.ok(script.includes('LABEL_SERVER="dev.herdr-mcp.server"'));
  assert.ok(script.includes('LABEL_WATCH="dev.herdr-mcp.health-watchdog"'));
  assert.ok(script.includes('dev.herdr-mcp.health-watchdog.plist'));
  assert.doesNotMatch(script, /LABEL_WATCH="dev\.herdr-mcp\.watchdog"/);
  assert.ok(script.includes('STATE_FILE="$CFG_DIR/health-watchdog.state.json"'));
  assert.ok(script.includes('LOG_FILE="$CFG_DIR/health-watchdog.log"'));
  assert.doesNotMatch(script, /STATE_FILE="\$CFG_DIR\/watchdog\.state\.json"/);
  assert.doesNotMatch(script, /LOG_FILE="\$CFG_DIR\/watchdog\.log"/);
  assert.ok(script.includes('HERDR_MCP_RUNTIME_BIN:-$HOME/.config/herdr-mcp/runtime/current/herdr-mcp'));
  assert.ok(script.includes('HEALTH_URL="http://127.0.0.1:8772/health"'));
  assert.doesNotMatch(script, /dist\/server\.js/);
  assert.doesNotMatch(script, /pgrep/);
  assert.doesNotMatch(script, /pkill/);
  assert.doesNotMatch(script, /\$ROOT\/bin\/watchdog\.sh/);
  assert.doesNotMatch(script, /service (install|rollback)|update apply/);
  const info = await stat(url);
  assert.notEqual(info.mode & 0o111, 0, 'watchdog source must remain executable');
});

test('watchdog defaults are bounded for in-turn recovery', () => {
  assert.ok(script.includes('HERDR_MCP_WATCHDOG_FAIL_THRESHOLD:-2'));
  assert.ok(script.includes('HERDR_MCP_WATCHDOG_COOLDOWN_SEC:-60'));
  assert.ok(script.includes('HERDR_MCP_WATCHDOG_RESTART_BURST_LIMIT:-3'));
  assert.ok(script.includes('HERDR_MCP_WATCHDOG_RESTART_BACKOFF_SEC:-300'));
  assert.ok(script.includes('HERDR_MCP_WATCHDOG_INTERVAL_SEC:-15'));
  assert.ok(script.includes('HERDR_MCP_WATCHDOG_HEALTH_TIMEOUT_SEC:-2'));
  assert.ok(script.includes('--connect-timeout 1'));
  assert.ok(script.includes('HERDR_MCP_LSOF_BIN:-/usr/sbin/lsof'));
});

test('watchdog launchd install is periodic one-shot and does not KeepAlive-loop itself', () => {
  assert.ok(script.includes('runtime_bin="$CFG_DIR/health-watchdog.sh"'));
  assert.ok(script.includes('health-watchdog.launchd.out.log'));
  assert.ok(script.includes('health-watchdog.launchd.err.log'));
  assert.doesNotMatch(script, /runtime_bin="\$CFG_DIR\/watchdog\.sh"/);
  assert.match(script, /source_bin="\$\(cd .*BASH_SOURCE\[0\].*basename .*BASH_SOURCE\[0\]/s);
  assert.ok(script.includes('cp "$source_bin" "$runtime_bin"'));
  assert.ok(script.includes('chmod 700 "$runtime_bin"'));
  assert.ok(script.includes('<key>StartInterval</key>'));
  assert.ok(script.includes('<key>RunAtLoad</key>'));
  const plistStart = script.indexOf('cat >"$PLIST_WATCH"');
  const plistEnd = script.indexOf('EOF\n  "$LAUNCHCTL_BIN" bootout');
  assert.ok(plistStart >= 0 && plistEnd > plistStart, 'watchdog plist block must be present');
  const plistBlock = script.slice(plistStart, plistEnd);
  assert.doesNotMatch(plistBlock, /<key>KeepAlive<\/key>/);
  assert.doesNotMatch(script, /"\$LAUNCHCTL_BIN"\s+submit|\/bin\/launchctl\s+submit/);
  assert.ok(script.includes('"$LAUNCHCTL_BIN" enable "$(watchdog_target)"'));
  assert.ok(script.includes('"$LAUNCHCTL_BIN" bootstrap "gui/$(id -u)" "$PLIST_WATCH"'));
  assert.ok(script.includes('"$LAUNCHCTL_BIN" disable "$(watchdog_target)"'));
});

test('explicitly stopped server is never bootstrapped or kickstarted', async () => {
  const harness = await makeHarness({ loaded: false, healthCode: '000' });
  const result = runOnce(harness);
  assert.equal(result.status, 0, result.stderr);
  const launchctlLog = await readFile(harness.log, 'utf8');
  assert.match(launchctlLog, /^print /m);
  assert.doesNotMatch(launchctlLog, /kickstart|bootstrap/);
  const state = JSON.parse(await readFile(join(harness.cfg, 'health-watchdog.state.json'), 'utf8'));
  assert.equal(state.server_loaded, false);
  assert.equal(state.consecutive_fail, 0);
  assert.equal(state.last_action, 'stopped');
});

test('loaded unhealthy server requires two failures then kickstarts the same job', async () => {
  const harness = await makeHarness({ loaded: true, healthCode: '503' });
  const first = runOnce(harness);
  assert.equal(first.status, 0, first.stderr);
  let launchctlLog = await readFile(harness.log, 'utf8');
  assert.doesNotMatch(launchctlLog, /kickstart/);
  const second = runOnce(harness);
  assert.equal(second.status, 0, second.stderr);
  launchctlLog = await readFile(harness.log, 'utf8');
  assert.match(launchctlLog, /kickstart -k gui\/[0-9]+\/dev\.herdr-mcp\.server/);
  const state = JSON.parse(await readFile(join(harness.cfg, 'health-watchdog.state.json'), 'utf8'));
  assert.equal(state.last_action, 'kickstart');
  assert.equal(state.restarts_total, 1);
});

test('explicit stop racing the final health decision wins before kickstart', async () => {
  const harness = await makeHarness({ loaded: true, healthCode: '503', unloadAfterPrints: 1 });
  const result = runOnce(harness, { HERDR_MCP_WATCHDOG_FAIL_THRESHOLD: '1' });
  assert.equal(result.status, 0, result.stderr);
  const launchctlLog = await readFile(harness.log, 'utf8');
  assert.equal((launchctlLog.match(/^print /gm) || []).length, 2);
  assert.doesNotMatch(launchctlLog, /kickstart/);
  const state = JSON.parse(await readFile(join(harness.cfg, 'health-watchdog.state.json'), 'utf8'));
  assert.equal(state.server_loaded, false);
  assert.equal(state.last_action, 'stopped');
});

test('legitimate lifecycle activity suppresses health recovery', async () => {
  const cases = [
    [{ mutationActive: true }, 'mutation_active'],
    [{ updateState: 'queued' }, 'update_active'],
    [{ updateState: 'installing' }, 'update_active'],
    [{ guardianState: 'armed' }, 'guardian_active'],
    [{ guardianState: 'watching' }, 'guardian_active'],
    [{ guardianState: 'recovering' }, 'guardian_active'],
  ];
  for (const [options, reason] of cases) {
    const harness = await makeHarness({ loaded: true, healthCode: '503', ...options });
    const result = runOnce(harness);
    assert.equal(result.status, 0, `${reason}: ${result.stderr}`);
    const launchctlLog = await readFile(harness.log, 'utf8');
    assert.doesNotMatch(launchctlLog, /kickstart/, reason);
    const state = JSON.parse(await readFile(join(harness.cfg, 'health-watchdog.state.json'), 'utf8'));
    assert.equal(state.consecutive_fail, 0, reason);
    assert.equal(state.last_action, `suppressed_${reason}`, reason);
  }
});

test('restart cooldown prevents repeated kickstarts', async () => {
  const harness = await makeHarness({ loaded: true, healthCode: '503' });
  const env = { HERDR_MCP_WATCHDOG_COOLDOWN_SEC: '60' };
  for (let i = 0; i < 4; i += 1) {
    const result = runOnce(harness, env);
    assert.equal(result.status, 0, result.stderr);
  }
  const launchctlLog = await readFile(harness.log, 'utf8');
  assert.equal((launchctlLog.match(/kickstart -k/g) || []).length, 1);
  const state = JSON.parse(await readFile(join(harness.cfg, 'health-watchdog.state.json'), 'utf8'));
  assert.equal(state.restarts_total, 1);
  assert.equal(state.last_action, 'cooldown');
});

test('server restart storm is suppressed until a real healthy probe resets the circuit', async () => {
  const harness = await makeHarness({ loaded: true, healthCode: '503' });
  const env = {
    HERDR_MCP_WATCHDOG_FAIL_THRESHOLD: '1',
    HERDR_MCP_WATCHDOG_COOLDOWN_SEC: '0',
    HERDR_MCP_WATCHDOG_RESTART_BURST_LIMIT: '2',
    HERDR_MCP_WATCHDOG_RESTART_BACKOFF_SEC: '300',
  };
  for (let i = 0; i < 3; i += 1) {
    const result = runOnce(harness, env);
    assert.equal(result.status, 0, result.stderr);
  }
  let launchctlLog = await readFile(harness.log, 'utf8');
  assert.equal((launchctlLog.match(/kickstart -k/g) || []).length, 2);
  let state = JSON.parse(await readFile(join(harness.cfg, 'health-watchdog.state.json'), 'utf8'));
  assert.equal(state.recovery_attempts_without_health, 2);
  assert.equal(state.last_action, 'restart_storm_suppressed');
  assert.ok(state.restart_suppressed_until > state.updated_at);

  await writeExecutable(harness.curl, "#!/bin/bash\nprintf '200'\n");
  const healthy = runOnce(harness, env);
  assert.equal(healthy.status, 0, healthy.stderr);
  state = JSON.parse(await readFile(join(harness.cfg, 'health-watchdog.state.json'), 'utf8'));
  assert.equal(state.recovery_attempts_without_health, 0);
  assert.equal(state.restart_suppressed_until, 0);
  assert.equal(state.last_action, 'healthy');

  await writeExecutable(harness.curl, "#!/bin/bash\nprintf '503'\n");
  const recoveredEligibility = runOnce(harness, env);
  assert.equal(recoveredEligibility.status, 0, recoveredEligibility.stderr);
  launchctlLog = await readFile(harness.log, 'utf8');
  assert.equal((launchctlLog.match(/kickstart -k/g) || []).length, 3);
});

test('watchdog state writes are atomic and malformed state fails closed', async () => {
  assert.match(script, /tempfile\.mkstemp/);
  assert.match(script, /os\.fsync\(fh\.fileno\(\)\)/);
  assert.match(script, /os\.replace\(tmp, path\)/);
  assert.doesNotMatch(script, /with open\(path, "w"\)/);

  const harness = await makeHarness({ loaded: true, healthCode: '503' });
  await writeFile(join(harness.cfg, 'health-watchdog.state.json'), '{"recovery_attempts_without_health": 2');
  const result = runOnce(harness, {
    HERDR_MCP_WATCHDOG_FAIL_THRESHOLD: '1',
    HERDR_MCP_WATCHDOG_COOLDOWN_SEC: '0',
    HERDR_MCP_WATCHDOG_RESTART_BURST_LIMIT: '2',
    HERDR_MCP_WATCHDOG_RESTART_BACKOFF_SEC: '300',
  });
  assert.equal(result.status, 0, result.stderr);
  const launchctlLog = await readFile(harness.log, 'utf8');
  assert.doesNotMatch(launchctlLog, /kickstart -k/);
  const state = JSON.parse(await readFile(join(harness.cfg, 'health-watchdog.state.json'), 'utf8'));
  assert.equal(state.last_action, 'state_corrupt_suppressed');
  assert.equal(state.recovery_attempts_without_health, 2);
  assert.ok(state.restart_suppressed_until > state.updated_at);
});

test('herdr_skill policy pins bounded outage recovery and uncertain-mutation safety', () => {
  assert.match(plannerSkill, /RunAtLoad=true/);
  assert.match(plannerSkill, /KeepAlive=true/);
  assert.match(plannerSkill, /dev\.herdr-mcp\.health-watchdog/);
  assert.match(plannerSkill, /historical `dev\.herdr-mcp\.watchdog` identity/);
  assert.match(plannerSkill, /about \*\*5 seconds\*\*/);
  assert.match(plannerSkill, /about \*\*10 seconds\*\*/);
  assert.match(plannerSkill, /about \*\*20 seconds\*\*/);
  assert.match(plannerSkill, /roughly \*\*35 seconds\*\*/);
  assert.match(plannerSkill, /exactly three \*\*read-only\*\* reconnect attempts/);
  assert.match(plannerSkill, /retry_after_ms/);
  assert.match(plannerSkill, /retry_read_only_probe/);
  assert.match(plannerSkill, /backoff_ms=\[5000,10000,20000\]/);
  assert.match(plannerSkill, /delivery_state=not_delivered/);
  assert.match(plannerSkill, /agent_status_wait_timeout.*not.*offline/s);
  assert.match(plannerSkill, /\*\*never blindly resend it\*\*/);
  assert.match(plannerSkill, /workstation_info\.boot_id.*herdr_since\(cursor=0\)/s);
  assert.match(plannerSkill, /service restart/);
});

test('CLI still delegates Rust service lifecycle to the active runtime', () => {
  assert.match(cli, /RUNTIME_BIN="\$HOME\/\.config\/herdr-mcp\/runtime\/current\/herdr-mcp"/);
  assert.match(cli, /"\$RUNTIME_BIN" service status/);
  assert.match(cli, /"\$RUNTIME_BIN" service "\$action"/);
  assert.doesNotMatch(cli, /pgrep -f "dist\/server\.js"/);
  assert.doesNotMatch(cli, /pkill -f "dist\/server\.js"/);
});

// ---- Link health layer (server healthy + Link unhealthy/offline) ----

const LINK_MISMATCH = JSON.stringify({
  ok: true,
  production_owner: 'rust',
  agents: [{
    label: 'dev.herdr-mcp.link-prod',
    loaded: true,
    implementation: 'rust',
    points_at_repo_checkout: false,
    points_at_managed_runtime: true,
  }],
  production_runtime_alignment: {
    current_generation: 'rust-7d7db9d2063970d2',
    active_generation: 'rust-c286e4312263b688',
    loaded_launchd_generation: 'rust-c286e4312263b688',
    runtime_control_active_matches_current: false,
    loaded_environment_stale: true,
  },
});

const LINK_STALE_LOADED = JSON.stringify({
  ok: true,
  production_owner: 'rust',
  agents: [{
    label: 'dev.herdr-mcp.link-prod',
    loaded: true,
    implementation: 'rust',
    points_at_repo_checkout: false,
    points_at_managed_runtime: true,
  }],
  production_runtime_alignment: {
    current_generation: 'rust-7d7db9d2063970d2',
    active_generation: 'rust-7d7db9d2063970d2',
    loaded_launchd_generation: 'rust-c286e4312263b688',
    runtime_control_active_matches_current: true,
    loaded_environment_stale: true,
  },
});

const LINK_FOREIGN = JSON.stringify({
  ok: true,
  production_owner: 'rust',
  agents: [{
    label: 'dev.herdr-mcp.link-prod',
    loaded: true,
    implementation: 'rust',
    points_at_repo_checkout: false,
    points_at_managed_runtime: false,
  }],
  production_runtime_alignment: {
    current_generation: 'rust-7d7db9d2063970d2',
    active_generation: 'rust-c286e4312263b688',
    loaded_launchd_generation: 'rust-c286e4312263b688',
    runtime_control_active_matches_current: false,
    loaded_environment_stale: true,
  },
});

const LINK_MISSING_GEN = JSON.stringify({
  ok: true,
  production_owner: 'rust',
  agents: [{
    label: 'dev.herdr-mcp.link-prod',
    loaded: true,
    implementation: 'rust',
    points_at_repo_checkout: false,
    points_at_managed_runtime: true,
  }],
  production_runtime_alignment: {
    current_generation: null,
    active_generation: null,
    loaded_launchd_generation: null,
    runtime_control_active_matches_current: false,
    loaded_environment_stale: false,
  },
});

const LINK_HEALTHY = JSON.stringify({
  ok: true,
  production_owner: 'rust',
  agents: [{
    label: 'dev.herdr-mcp.link-prod',
    loaded: true,
    implementation: 'rust',
    points_at_repo_checkout: false,
    points_at_managed_runtime: true,
  }],
  production_runtime_alignment: {
    current_generation: 'rust-7d7db9d2063970d2',
    active_generation: 'rust-7d7db9d2063970d2',
    loaded_launchd_generation: 'rust-7d7db9d2063970d2',
    runtime_control_active_matches_current: true,
    loaded_environment_stale: false,
  },
});

async function readState(harness) {
  return JSON.parse(await readFile(join(harness.cfg, 'health-watchdog.state.json'), 'utf8'));
}

function linkKickstarts(launchctlLog) {
  return (launchctlLog.match(/kickstart -k gui\/[0-9]+\/dev\.herdr-mcp\.link-prod/g) || []).length;
}

function serverKickstarts(launchctlLog) {
  return (launchctlLog.match(/kickstart -k gui\/[0-9]+\/dev\.herdr-mcp\.server/g) || []).length;
}

test('healthy server + persistent active mismatch kickstarts only link-prod after threshold', async () => {
  const harness = await makeHarness({ loaded: true, healthCode: '200', linkStatusJson: LINK_MISMATCH });
  const first = runOnce(harness);
  assert.equal(first.status, 0, first.stderr);
  let log = await readFile(harness.log, 'utf8');
  assert.equal(linkKickstarts(log), 0);
  assert.equal(serverKickstarts(log), 0);
  const second = runOnce(harness);
  assert.equal(second.status, 0, second.stderr);
  log = await readFile(harness.log, 'utf8');
  assert.equal(linkKickstarts(log), 1);
  assert.equal(serverKickstarts(log), 0, 'server must never be touched by Link logic');
  const state = await readState(harness);
  assert.equal(state.link_last_action, 'link_kickstart');
  assert.equal(state.link_restarts_total, 1);
  assert.equal(state.link_active_matches_current, false);
});

test('stale loaded launchd env with active=current is degraded no-op, not a restart', async () => {
  const harness = await makeHarness({ loaded: true, healthCode: '200', linkStatusJson: LINK_STALE_LOADED });
  const result = runOnce(harness);
  assert.equal(result.status, 0, result.stderr);
  const log = await readFile(harness.log, 'utf8');
  assert.equal(linkKickstarts(log), 0);
  assert.equal(serverKickstarts(log), 0);
  const state = await readState(harness);
  assert.equal(state.link_last_action, 'link_degraded_startup_metadata');
  assert.equal(state.link_consecutive_fail, 0);
  assert.equal(state.link_loaded_environment_stale, true);
  assert.equal(state.link_active_matches_current, true);
});

test('foreign Rust binary (not managed runtime) is never mutated', async () => {
  const harness = await makeHarness({ loaded: true, healthCode: '200', linkStatusJson: LINK_FOREIGN });
  for (let i = 0; i < 3; i += 1) {
    const result = runOnce(harness);
    assert.equal(result.status, 0, result.stderr);
  }
  const log = await readFile(harness.log, 'utf8');
  assert.equal(linkKickstarts(log), 0);
  const state = await readState(harness);
  assert.equal(state.link_last_action, 'link_unowned');
  assert.equal(state.link_points_at_managed_runtime, false);
  assert.equal(state.link_consecutive_fail, 0);
});

test('missing active/current generation evidence is unobservable, no mutation', async () => {
  const harness = await makeHarness({ loaded: true, healthCode: '200', linkStatusJson: LINK_MISSING_GEN });
  for (let i = 0; i < 3; i += 1) {
    const result = runOnce(harness);
    assert.equal(result.status, 0, result.stderr);
  }
  const log = await readFile(harness.log, 'utf8');
  assert.equal(linkKickstarts(log), 0);
  const state = await readState(harness);
  assert.equal(state.link_last_action, 'link_unobservable');
  assert.equal(state.link_consecutive_fail, 0);
});

test('single crash with runs increment is not a storm; repeated growth is storm report-only', async () => {
  const evidence = 'last exit code = 1\nruns = 5\n';
  // Use a healthy alignment so the storm branch is exercised without any
  // active!=current kickstart interfering with the storm observation.
  const harness = await makeHarness({ loaded: true, healthCode: '200', linkStatusJson: LINK_HEALTHY, linkEvidence: evidence });
  // First observation: prev_runs absent -> no storm.
  let result = runOnce(harness);
  assert.equal(result.status, 0, result.stderr);
  let state = await readState(harness);
  assert.equal(state.link_restart_storm, false);
  assert.equal(state.link_last_action, 'link_healthy');
  // Second observation: runs grows 5->6 with non-zero exit -> storm streak 1, still not storm.
  await writeFile(harness.linkEvidenceFile, 'last exit code = 1\nruns = 6\n');
  result = runOnce(harness);
  assert.equal(result.status, 0, result.stderr);
  state = await readState(harness);
  assert.equal(state.link_restart_storm, false);
  // Third observation: runs grows 6->7 -> storm streak 2 -> storm, report-only, no kickstart.
  await writeFile(harness.linkEvidenceFile, 'last exit code = 1\nruns = 7\n');
  result = runOnce(harness);
  assert.equal(result.status, 0, result.stderr);
  state = await readState(harness);
  assert.equal(state.link_restart_storm, true);
  assert.equal(state.link_last_action, 'link_restart_storm');
  const log = await readFile(harness.log, 'utf8');
  assert.equal(linkKickstarts(log), 0, 'storm must never kickstart');
});

test('persistent mismatch cooldown allows only one link kickstart', async () => {
  const harness = await makeHarness({ loaded: true, healthCode: '200', linkStatusJson: LINK_MISMATCH });
  const env = { HERDR_MCP_LINK_WATCHDOG_COOLDOWN_SEC: '120' };
  for (let i = 0; i < 4; i += 1) {
    const result = runOnce(harness, env);
    assert.equal(result.status, 0, result.stderr);
  }
  const log = await readFile(harness.log, 'utf8');
  assert.equal(linkKickstarts(log), 1);
  const state = await readState(harness);
  assert.equal(state.link_restarts_total, 1);
  assert.equal(state.link_last_action, 'link_cooldown');
});

test('lifecycle activity suppresses Link recovery', async () => {
  const cases = [
    [{ mutationActive: true }, 'mutation_active'],
    [{ updateState: 'queued' }, 'update_active'],
    [{ updateState: 'installing' }, 'update_active'],
    [{ guardianState: 'armed' }, 'guardian_active'],
    [{ guardianState: 'watching' }, 'guardian_active'],
    [{ guardianState: 'recovering' }, 'guardian_active'],
  ];
  for (const [options, reason] of cases) {
    const harness = await makeHarness({ loaded: true, healthCode: '200', linkStatusJson: LINK_MISMATCH, ...options });
    const result = runOnce(harness);
    assert.equal(result.status, 0, `${reason}: ${result.stderr}`);
    const log = await readFile(harness.log, 'utf8');
    assert.equal(linkKickstarts(log), 0, reason);
    const state = await readState(harness);
    assert.equal(state.link_consecutive_fail, 0, reason);
    assert.equal(state.link_last_action, `link_suppressed_${reason}`, reason);
  }
});

test('explicitly unloaded link-prod is never resurrected', async () => {
  const unloadedStatus = JSON.stringify({
    ok: true,
    production_owner: 'rust',
    agents: [{
      label: 'dev.herdr-mcp.link-prod',
      loaded: false,
      implementation: 'rust',
      points_at_repo_checkout: false,
      points_at_managed_runtime: true,
    }],
    production_runtime_alignment: {
      current_generation: 'rust-7d7db9d2063970d2',
      active_generation: 'rust-c286e4312263b688',
      loaded_launchd_generation: 'rust-c286e4312263b688',
      runtime_control_active_matches_current: false,
      loaded_environment_stale: true,
    },
  });
  const harness = await makeHarness({ loaded: true, healthCode: '200', linkStatusJson: unloadedStatus, linkLoaded: false });
  for (let i = 0; i < 3; i += 1) {
    const result = runOnce(harness);
    assert.equal(result.status, 0, result.stderr);
  }
  const log = await readFile(harness.log, 'utf8');
  assert.equal(linkKickstarts(log), 0);
  assert.doesNotMatch(log, /bootstrap/);
  const state = await readState(harness);
  assert.equal(state.link_last_action, 'link_unloaded');
  assert.equal(state.link_consecutive_fail, 0);
});

test('healthy path invokes local link status and curl stays loopback-only', async () => {
  const harness = await makeHarness({ loaded: true, healthCode: '200', linkStatusJson: LINK_HEALTHY });
  const result = runOnce(harness);
  assert.equal(result.status, 0, result.stderr);
  const runtimeLog = await readFile(harness.runtimeLog, 'utf8');
  assert.match(runtimeLog, /link status/);
  const curlLog = await readFile(harness.curlLog, 'utf8');
  assert.match(curlLog, /127\.0\.0\.1:8772\/health/);
  assert.doesNotMatch(curlLog, /cloudflare|workers\.dev|github|oauth/i);
  const state = await readState(harness);
  assert.equal(state.link_last_action, 'link_healthy');
});

test('stale non-prod runtime-status fixture cannot affect canned healthy link status', async () => {
  const harness = await makeHarness({ loaded: true, healthCode: '200', linkStatusJson: LINK_HEALTHY });
  // A stale non-prod runtime-status.json must not influence the watchdog, which
  // reads only the Rust `link status` JSON (production path semantics).
  await writeFile(join(harness.cfg, 'runtime-status.json'), JSON.stringify({
    schema_version: 1,
    manager: { active_generation: 'rust-stale-nonprod' },
  }));
  const result = runOnce(harness);
  assert.equal(result.status, 0, result.stderr);
  const state = await readState(harness);
  assert.equal(state.link_last_action, 'link_healthy');
  assert.equal(state.link_active_generation, 'rust-7d7db9d2063970d2');
  assert.equal(state.link_consecutive_fail, 0);
});
