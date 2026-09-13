import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));

test('Cargo intermediate build cache is isolated per worktree manifest path', async () => {
  const config = await readFile(join(ROOT, '.cargo', 'config.toml'), 'utf8');
  assert.match(
    config,
    /build-dir\s*=\s*"\{cargo-cache-home\}\/herdr-mcp-build\/\{workspace-path-hash\}"/,
  );
});
