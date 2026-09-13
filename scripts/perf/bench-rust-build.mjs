#!/usr/bin/env node

import { spawn, spawnSync } from 'node:child_process';
import { mkdir, mkdtemp, rm, stat, utimes, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';
import { sampleProcessTree } from './process-metrics.mjs';

const ROOT = resolve(fileURLToPath(new URL('../..', import.meta.url)));
const PHASES = {
  clippy: ['cargo', 'clippy', '--workspace', '--all-targets', '--all-features', '--', '-D', 'warnings'],
  test: ['cargo', 'test', '--workspace'],
  'test-no-run': ['cargo', 'test', '--workspace', '--no-run'],
};

function usage(message) {
  if (message) console.error(`ERROR: ${message}`);
  console.error(`Usage: node scripts/perf/bench-rust-build.mjs [options]\n\nOptions:\n  --phase <clippy|test|test-no-run>  Cargo phase to benchmark (default: clippy)\n  --touch <repo-relative-file>       Repeat for mtime invalidation scenarios\n  --sample-ms <n>                    Process-tree RSS sample interval (default: 100)\n  --ci-like                          Apply the 0.4.8 comparison profile: CARGO_INCREMENTAL=0 and RUSTFLAGS=-Dwarnings\n  --keep-target                      Preserve the isolated CARGO_TARGET_DIR\n  --out <path>                       Also write the JSON report to this path\n  --help                             Show this help\n`);
  process.exit(message ? 2 : 0);
}

function parseArgs(argv) {
  const options = {
    phase: 'clippy',
    touches: [],
    sampleMs: 100,
    ciLike: false,
    keepTarget: false,
    out: null,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--help') usage();
    if (arg === '--ci-like') {
      options.ciLike = true;
      continue;
    }
    if (arg === '--keep-target') {
      options.keepTarget = true;
      continue;
    }
    const value = argv[index + 1];
    if (arg === '--phase') {
      if (!value) usage('--phase requires a value');
      options.phase = value;
      index += 1;
      continue;
    }
    if (arg === '--touch') {
      if (!value) usage('--touch requires a path');
      options.touches.push(value);
      index += 1;
      continue;
    }
    if (arg === '--sample-ms') {
      if (!value) usage('--sample-ms requires a value');
      options.sampleMs = Number(value);
      index += 1;
      continue;
    }
    if (arg === '--out') {
      if (!value) usage('--out requires a path');
      options.out = value;
      index += 1;
      continue;
    }
    usage(`unknown argument: ${arg}`);
  }
  if (!PHASES[options.phase]) usage(`unsupported phase: ${options.phase}`);
  if (!Number.isInteger(options.sampleMs) || options.sampleMs < 25 || options.sampleMs > 5000) {
    usage('--sample-ms must be an integer between 25 and 5000');
  }
  return options;
}

function runText(command, args) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: 'utf8',
    maxBuffer: 8 * 1024 * 1024,
  });
  if (result.error || result.status !== 0) return null;
  return result.stdout.trim();
}

function boundedAppend(current, chunk, maxBytes = 24 * 1024) {
  const next = `${current}${chunk}`;
  if (Buffer.byteLength(next) <= maxBytes) return next;
  return next.slice(Math.max(0, next.length - maxBytes));
}

function sccacheStats(enabled) {
  if (!enabled) return null;
  const result = spawnSync('sccache', ['--show-stats', '--stats-format', 'json'], {
    encoding: 'utf8',
    maxBuffer: 4 * 1024 * 1024,
  });
  if (result.error || result.status !== 0) return null;
  try {
    return JSON.parse(result.stdout);
  } catch {
    return { raw: result.stdout.trim() };
  }
}

async function validateTouchPath(input) {
  const absolute = resolve(ROOT, input);
  if (absolute !== ROOT && !absolute.startsWith(`${ROOT}${sep}`)) {
    throw new Error(`touch path escapes repository root: ${input}`);
  }
  const info = await stat(absolute);
  if (!info.isFile()) throw new Error(`touch path is not a regular file: ${input}`);
  const repoRelative = relative(ROOT, absolute);
  const tracked = spawnSync('git', ['ls-files', '--error-unmatch', '--', repoRelative], {
    cwd: ROOT,
    stdio: 'ignore',
  });
  if (tracked.status !== 0) throw new Error(`touch path must be tracked by Git: ${repoRelative}`);
  const status = runText('git', ['status', '--porcelain', '--', repoRelative]);
  if (status === null) throw new Error(`unable to inspect Git status for ${repoRelative}`);
  if (status !== '') throw new Error(`touch path is already dirty: ${repoRelative}`);
  return { absolute, repoRelative, bytes: info.size };
}

async function runMeasured(label, argv, env, sampleMs) {
  const startedAt = new Date();
  const startedNs = process.hrtime.bigint();
  let stdoutTail = '';
  let stderrTail = '';
  let peakRssKib = 0;
  let peakProcessCount = 0;
  let samples = 0;
  let samplingError = null;

  console.error(`[rust-build-bench] start ${label}: ${argv.join(' ')}`);
  const child = spawn(argv[0], argv.slice(1), {
    cwd: ROOT,
    env,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout.setEncoding('utf8');
  child.stderr.setEncoding('utf8');
  child.stdout.on('data', (chunk) => { stdoutTail = boundedAppend(stdoutTail, chunk); });
  child.stderr.on('data', (chunk) => { stderrTail = boundedAppend(stderrTail, chunk); });

  let stopped = false;
  const sampler = (async () => {
    while (!stopped) {
      try {
        const snapshot = await sampleProcessTree([child.pid], {
          includePss: false,
          includeThreads: false,
          includeProcesses: false,
        });
        samples += 1;
        peakRssKib = Math.max(peakRssKib, snapshot.rss_kib ?? 0);
        peakProcessCount = Math.max(peakProcessCount, snapshot.process_count ?? 0);
      } catch (error) {
        samplingError = error?.message ?? String(error);
      }
      await new Promise((resolveDelay) => setTimeout(resolveDelay, sampleMs));
    }
  })();

  const exit = await new Promise((resolveExit) => {
    child.once('error', (error) => resolveExit({ code: null, signal: null, error }));
    child.once('exit', (code, signal) => resolveExit({ code, signal, error: null }));
  });
  stopped = true;
  await sampler;

  const elapsedMs = Number(process.hrtime.bigint() - startedNs) / 1e6;
  const result = {
    label,
    started_at: startedAt.toISOString(),
    elapsed_ms: Number(elapsedMs.toFixed(3)),
    exit_code: exit.code,
    signal: exit.signal,
    peak_process_tree_rss_kib: peakRssKib,
    peak_process_count: peakProcessCount,
    rss_sample_count: samples,
    rss_sample_interval_ms: sampleMs,
    sampling_error: samplingError,
    stdout_tail: stdoutTail.trim(),
    stderr_tail: stderrTail.trim(),
    spawn_error: exit.error?.message ?? null,
  };
  console.error(`[rust-build-bench] done ${label}: exit=${result.exit_code} elapsed_ms=${result.elapsed_ms} peak_rss_kib=${peakRssKib}`);
  return result;
}

const options = parseArgs(process.argv.slice(2));
const touchFiles = [];
for (const input of options.touches) touchFiles.push(await validateTouchPath(input));

const targetDir = await mkdtemp(join(tmpdir(), 'herdr-rust-build-bench-'));
const buildDir = join(targetDir, 'build-intermediates');
const command = PHASES[options.phase];
const env = {
  ...process.env,
  CARGO_TARGET_DIR: targetDir,
  // Keep the benchmark isolated even when the repository stores Cargo 1.97
  // intermediates under CARGO_HOME with per-worktree path hashing.
  CARGO_BUILD_BUILD_DIR: buildDir,
};
if (options.ciLike) {
  env.CARGO_INCREMENTAL = '0';
  env.RUSTFLAGS = '-Dwarnings';
}
const sccacheEnabled = Boolean(
  (env.RUSTC_WRAPPER && env.RUSTC_WRAPPER.includes('sccache'))
  || env.SCCACHE_GHA_ENABLED === 'true',
);

const report = {
  schema: 'herdr-mcp/rust-build-benchmark/v1',
  generated_at: new Date().toISOString(),
  repository_root: ROOT,
  commit: runText('git', ['rev-parse', 'HEAD']),
  branch: runText('git', ['branch', '--show-current']),
  platform: process.platform,
  arch: process.arch,
  node_version: process.version,
  cargo_version: runText('cargo', ['--version']),
  rustc_version: runText('rustc', ['--version']),
  phase: options.phase,
  command,
  methodology: {
    target_dir: targetDir,
    build_dir: buildDir,
    target_dir_isolated: true,
    build_dir_isolated: true,
    target_dir_preserved: options.keepTarget,
    cold_definition: 'empty isolated CARGO_TARGET_DIR; external compiler caches are not cleared',
    warm_definition: 'same command and target directory with no source mtime change',
    touched_file_definition: 'Git-clean tracked file mtime update only; file content is not changed',
    sample_interval_ms: options.sampleMs,
    peak_memory_metric: 'sum of RSS for the Cargo process tree sampled with ps',
    ci_like: options.ciLike,
    ci_like_reference: options.ciLike
      ? '0.4.8 comparison profile; current 1.0 main CI does not set these environment overrides'
      : null,
  },
  environment: {
    CARGO_INCREMENTAL: env.CARGO_INCREMENTAL ?? null,
    CARGO_BUILD_BUILD_DIR: env.CARGO_BUILD_BUILD_DIR,
    RUSTFLAGS: env.RUSTFLAGS ?? null,
    RUSTC_WRAPPER: env.RUSTC_WRAPPER ?? null,
    SCCACHE_GHA_ENABLED: env.SCCACHE_GHA_ENABLED ?? null,
  },
  touch_files: touchFiles.map(({ repoRelative, bytes }) => ({ path: repoRelative, bytes })),
  sccache: {
    enabled_by_environment: sccacheEnabled,
    before: sccacheStats(sccacheEnabled),
    after: null,
  },
  runs: [],
};

let failingCode = 0;
try {
  const cold = await runMeasured('cold', command, env, options.sampleMs);
  report.runs.push(cold);
  if (cold.exit_code !== 0) failingCode = cold.exit_code ?? 1;

  if (!failingCode) {
    const warm = await runMeasured('warm_noop', command, env, options.sampleMs);
    report.runs.push(warm);
    if (warm.exit_code !== 0) failingCode = warm.exit_code ?? 1;
  }

  for (const file of touchFiles) {
    if (failingCode) break;
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 25));
    const now = new Date();
    await utimes(file.absolute, now, now);
    const touched = await runMeasured(`touch:${file.repoRelative}`, command, env, options.sampleMs);
    report.runs.push(touched);
    const status = runText('git', ['status', '--porcelain', '--', file.repoRelative]);
    touched.git_content_still_clean = status === '';
    if (touched.exit_code !== 0) failingCode = touched.exit_code ?? 1;
  }
} finally {
  report.sccache.after = sccacheStats(sccacheEnabled);
  if (!options.keepTarget) await rm(targetDir, { recursive: true, force: true });
}

const json = `${JSON.stringify(report, null, 2)}\n`;
if (options.out) {
  const outPath = isAbsolute(options.out) ? options.out : resolve(ROOT, options.out);
  await mkdir(dirname(outPath), { recursive: true });
  await writeFile(outPath, json);
  console.error(`[rust-build-bench] wrote ${outPath}`);
}
process.stdout.write(json);
if (failingCode) process.exitCode = failingCode;
