import assert from 'node:assert/strict';
import { readFile, stat } from 'node:fs/promises';
import test from 'node:test';

const url = new URL('../bin/watchdog.sh', import.meta.url);
const script = await readFile(url, 'utf8');
const cli = await readFile(new URL('../bin/herdr-mcp', import.meta.url), 'utf8');

test('watchdog launchd install runs a config-dir copy instead of executing from Documents', async () => {
  assert.match(script, /runtime_bin="\$CFG_DIR\/watchdog\.sh"/);
  assert.match(script, /cp "\$source_bin" "\$runtime_bin"/);
  assert.match(script, /chmod 700 "\$runtime_bin"/);
  assert.match(script, /<string>\$\{runtime_bin\}<\/string>/);
  assert.match(script, /<key>HERDR_MCP_ROOT<\/key>/);
  assert.doesNotMatch(script, /<key>WorkingDirectory<\/key>/);
  const info = await stat(url);
  assert.notEqual(info.mode & 0o111, 0, 'watchdog source must remain executable');
});

test('watchdog runtime copy can resolve the repository through HERDR_MCP_ROOT', () => {
  assert.match(script, /ROOT="\$\{HERDR_MCP_ROOT:-\$\(cd/);
  assert.match(script, /cli = os\.path\.join\(os\.environ\["ROOT"\], "bin", "herdr-mcp"\)/);
});

test('watchdog local HTTP probe uses cheap server/discover instead of initialize', () => {
  assert.match(script, /\"method\":\"server\/discover\"/);
  assert.doesNotMatch(script, /\"method\":\"initialize\"/);
  assert.match(script, /curl -s -o \/dev\/null -w \"%\{http_code\}\" -m 3/);
});

test('watchdog and CLI normalize failed curl to one 000 code and use discover locally', () => {
  for (const [name, source] of [['watchdog', script], ['cli', cli]]) {
    assert.match(source, /code="?000"?/, `${name} normalizes failed curl to 000`);
    assert.match(source, /\"method\":\"server\/discover\"/, `${name} uses sessionless discover`);
  }
});

test('CLI tracks the exact launchd server job and delegates Rust service mutations', () => {
  assert.match(cli, /RUNTIME_BIN="\$HOME\/\.config\/herdr-mcp\/runtime\/current\/herdr-mcp"/);
  assert.match(cli, /launchctl list "\$LABEL"/);
  assert.match(cli, /"\$RUNTIME_BIN" service status/);
  assert.match(cli, /"\$RUNTIME_BIN" service "\$action"/);
  assert.match(cli, /run_rust_service start/);
  assert.match(cli, /run_rust_service stop/);
  assert.match(cli, /run_rust_service restart/);
  assert.doesNotMatch(cli, /pgrep -f "dist\/server\.js"/);
  assert.doesNotMatch(cli, /pkill -f "dist\/server\.js"/);
});
