import assert from 'node:assert/strict';
import { readFile, stat } from 'node:fs/promises';
import test from 'node:test';

const url = new URL('../bin/watchdog.sh', import.meta.url);
const script = await readFile(url, 'utf8');

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
