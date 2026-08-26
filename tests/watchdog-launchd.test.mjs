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
}) {
  const root = await mkdtemp(join(tmpdir(), 'herdr-watchdog-'));
  const home = join(root, 'home');
  const cfg = join(root, 'cfg');
  const log = join(root, 'launchctl.log');
  const printCount = join(root, 'launchctl-print-count');
  const launchctl = join(root, 'launchctl');
  const curl = join(root, 'curl');
  const lsof = join(root, 'lsof');
  const runtime = join(root, 'herdr-mcp');
  await mkdir(cfg, { recursive: true });
  await writeExecutable(
    launchctl,
    `#!/bin/bash
set -eu
echo "$*" >> ${JSON.stringify(log)}
if [[ "$1" == "print" ]]; then
  count=0
  [[ -f ${JSON.stringify(printCount)} ]] && count="$(cat ${JSON.stringify(printCount)})"
  count=$((count + 1))
  echo "$count" > ${JSON.stringify(printCount)}
  ${loaded ? ':' : 'exit 1'}
  ${unloadAfterPrints === null ? ':' : `[[ "$count" -le ${unloadAfterPrints} ]] || exit 1`}
  exit 0
fi
exit 0
`,
  );
  await writeExecutable(curl, `#!/bin/bash
printf '%s' ${JSON.stringify(healthCode)}
`);
  await writeExecutable(lsof, `#!/bin/bash
exit ${mutationActive ? 0 : 1}
`);
  const updateStatus = JSON.stringify({ job: { state: updateState } });
  await writeExecutable(runtime, `#!/bin/bash
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
  return { root, home, cfg, log, launchctl, curl, lsof, runtime };
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
      ...extraEnv,
    },
  });
}

test('watchdog pins the managed Rust service and removes Node process heuristics', async () => {
  assert.ok(script.includes('LABEL_SERVER="dev.herdr-mcp.server"'));
  assert.ok(script.includes('LABEL_WATCH="dev.herdr-mcp.health-watchdog"'));
  assert.ok(script.includes('dev.herdr-mcp.health-watchdog.plist'));
  assert.doesNotMatch(script, /LABEL_WATCH="dev\.herdr-mcp\.watchdog"/);
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
  assert.ok(script.includes('HERDR_MCP_WATCHDOG_INTERVAL_SEC:-15'));
  assert.ok(script.includes('HERDR_MCP_WATCHDOG_HEALTH_TIMEOUT_SEC:-2'));
  assert.ok(script.includes('--connect-timeout 1'));
  assert.ok(script.includes('HERDR_MCP_LSOF_BIN:-/usr/sbin/lsof'));
});

test('watchdog launchd install is periodic one-shot and does not KeepAlive-loop itself', () => {
  assert.ok(script.includes('runtime_bin="$CFG_DIR/watchdog.sh"'));
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
  const state = JSON.parse(await readFile(join(harness.cfg, 'watchdog.state.json'), 'utf8'));
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
  const state = JSON.parse(await readFile(join(harness.cfg, 'watchdog.state.json'), 'utf8'));
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
  const state = JSON.parse(await readFile(join(harness.cfg, 'watchdog.state.json'), 'utf8'));
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
    const state = JSON.parse(await readFile(join(harness.cfg, 'watchdog.state.json'), 'utf8'));
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
  const state = JSON.parse(await readFile(join(harness.cfg, 'watchdog.state.json'), 'utf8'));
  assert.equal(state.restarts_total, 1);
  assert.equal(state.last_action, 'cooldown');
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
