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

test('user iterates locally on release-like code | Given the workspace Cargo profiles | When a developer builds release-fast | Then dependencies are optimized for dev and the official release profile is unchanged', async () => {
  const manifest = await readFile(join(ROOT, 'Cargo.toml'), 'utf8');

  // Dev builds stay incremental for workspace crates but run optimized deps.
  const devDeps = manifest.match(/\[profile\.dev\.package\."\*"\]([\s\S]*?)(?:\n\[|$)/);
  assert.ok(devDeps, 'dev dependency profile must exist');
  assert.match(devDeps[1], /opt-level\s*=\s*1/);
  assert.match(devDeps[1], /debug\s*=\s*false/);

  // The official release profile is the shipped artifact contract.
  const release = manifest.match(/\[profile\.release\]([\s\S]*?)(?:\n\[|$)/);
  assert.ok(release, 'release profile must exist');
  assert.match(release[1], /lto\s*=\s*"thin"/);
  assert.match(release[1], /codegen-units\s*=\s*1/);
  assert.match(release[1], /strip\s*=\s*"symbols"/);

  // release-fast is the local-only, faster-linking variant.
  const fast = manifest.match(/\[profile\.release-fast\]([\s\S]*?)(?:\n\[|$)/);
  assert.ok(fast, 'release-fast profile must exist for local iteration');
  assert.match(fast[1], /inherits\s*=\s*"release"/);
  assert.match(fast[1], /lto\s*=\s*false/);
  assert.match(fast[1], /codegen-units\s*=\s*16/);
  assert.doesNotMatch(fast[1], /strip\s*=/, 'strip stays inherited from the release profile');
});
