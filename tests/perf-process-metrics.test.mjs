import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmod, mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

import {
  parseLinuxSmapsRollup,
  parsePsTable,
  selectProcessTree,
  summarizeProcessRows,
} from '../scripts/perf/process-metrics.mjs';

test('parsePsTable parses process rows without thread counts', () => {
  const rows = parsePsTable(`\n  10  1  2048 cargo test --workspace\n  11 10  4096 rustc --crate-name herdr_mcp\n`);
  assert.deepEqual(rows, [
    { pid: 10, ppid: 1, rss_kib: 2048, threads: null, command: 'cargo test --workspace' },
    { pid: 11, ppid: 10, rss_kib: 4096, threads: null, command: 'rustc --crate-name herdr_mcp' },
  ]);
});

test('parsePsTable parses Linux nlwp thread counts', () => {
  const rows = parsePsTable(' 10  1  2048  3 cargo test --workspace\n', { hasThreads: true });
  assert.deepEqual(rows[0], {
    pid: 10,
    ppid: 1,
    rss_kib: 2048,
    threads: 3,
    command: 'cargo test --workspace',
  });
});

test('selectProcessTree includes descendants but excludes unrelated processes', () => {
  const rows = [
    { pid: 10, ppid: 1, rss_kib: 100, threads: 1, command: 'root' },
    { pid: 11, ppid: 10, rss_kib: 200, threads: 2, command: 'child' },
    { pid: 12, ppid: 11, rss_kib: 300, threads: 3, command: 'grandchild' },
    { pid: 20, ppid: 1, rss_kib: 400, threads: 4, command: 'other' },
  ];
  assert.deepEqual(selectProcessTree(rows, [10]).map((row) => row.pid), [10, 11, 12]);
  assert.deepEqual(summarizeProcessRows(selectProcessTree(rows, [10])), {
    process_count: 3,
    thread_count: 6,
    thread_count_complete: true,
    rss_kib: 600,
    pss_kib: null,
    pss_anon_kib: null,
  });
});

test('parseLinuxSmapsRollup extracts PSS metrics when available', () => {
  assert.deepEqual(parseLinuxSmapsRollup(`Rss:                900 kB\nPss:                700 kB\nPss_Anon:           500 kB\n`), {
    pss_kib: 700,
    pss_anon_kib: 500,
  });
});

test('Rust build benchmark accepts a tracked repo-relative touch path', async () => {
  const fakeBin = await mkdtemp(join(tmpdir(), 'herdr-perf-fake-bin-'));
  const fakeCargo = join(fakeBin, 'cargo');
  await writeFile(fakeCargo, '#!/bin/sh\necho "cargo 1.0.0-fake"\nexit 0\n');
  await chmod(fakeCargo, 0o755);

  const result = spawnSync(process.execPath, [
    'scripts/perf/bench-rust-build.mjs',
    '--phase', 'test-no-run',
    '--sample-ms', '25',
    '--touch', 'crates/herdr-mcp/src/test_env.rs',
  ], {
    cwd: process.cwd(),
    env: { ...process.env, PATH: `${fakeBin}:${process.env.PATH}` },
    encoding: 'utf8',
    maxBuffer: 4 * 1024 * 1024,
  });

  assert.equal(result.status, 0, result.stderr);
  const report = JSON.parse(result.stdout);
  assert.equal(report.repository_root, process.cwd());
  assert.deepEqual(report.touch_files.map((entry) => entry.path), ['crates/herdr-mcp/src/test_env.rs']);
  assert.deepEqual(report.runs.map((entry) => entry.label), [
    'cold',
    'warm_noop',
    'touch:crates/herdr-mcp/src/test_env.rs',
  ]);
  assert.equal(report.runs.at(-1).git_content_still_clean, true);
});
