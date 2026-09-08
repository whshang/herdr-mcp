#!/usr/bin/env node

import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, isAbsolute, resolve } from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';
import { sampleProcessTree } from './process-metrics.mjs';

const ROOT = resolve(fileURLToPath(new URL('../..', import.meta.url)));

function usage(message) {
  if (message) console.error(`ERROR: ${message}`);
  console.error(`Usage: node scripts/perf/sample-process-tree.mjs --pid [label=]PID [options]\n\nOptions:\n  --pid [label=]PID     Repeat for one or more explicit process-tree roots\n  --samples <n>         Number of samples (default: 5)\n  --interval-ms <n>     Delay between samples (default: 500)\n  --no-pss              Disable Linux /proc smaps_rollup PSS sampling\n  --out <path>          Also write the JSON report to this path\n  --help                Show this help\n`);
  process.exit(message ? 2 : 0);
}

function parsePid(value) {
  const index = value.lastIndexOf('=');
  const label = index > 0 ? value.slice(0, index) : null;
  const rawPid = index > 0 ? value.slice(index + 1) : value;
  const pid = Number(rawPid);
  if (!Number.isInteger(pid) || pid <= 0) usage(`invalid PID: ${value}`);
  return { label: label || `pid-${pid}`, pid };
}

function parseArgs(argv) {
  const options = {
    roots: [],
    samples: 5,
    intervalMs: 500,
    includePss: process.platform === 'linux',
    out: null,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--help') usage();
    if (arg === '--no-pss') {
      options.includePss = false;
      continue;
    }
    const value = argv[index + 1];
    if (arg === '--pid') {
      if (!value) usage('--pid requires a value');
      options.roots.push(parsePid(value));
      index += 1;
      continue;
    }
    if (arg === '--samples') {
      if (!value) usage('--samples requires a value');
      options.samples = Number(value);
      index += 1;
      continue;
    }
    if (arg === '--interval-ms') {
      if (!value) usage('--interval-ms requires a value');
      options.intervalMs = Number(value);
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
  if (options.roots.length === 0) usage('at least one --pid is required');
  if (!Number.isInteger(options.samples) || options.samples < 1 || options.samples > 10_000) {
    usage('--samples must be an integer between 1 and 10000');
  }
  if (!Number.isInteger(options.intervalMs) || options.intervalMs < 25 || options.intervalMs > 60_000) {
    usage('--interval-ms must be an integer between 25 and 60000');
  }
  return options;
}

function percentile(values, percentileValue) {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const index = Math.min(
    sorted.length - 1,
    Math.max(0, Math.ceil((percentileValue / 100) * sorted.length) - 1),
  );
  return sorted[index];
}

function metricSummary(samples, key) {
  const values = samples.map((sample) => sample[key]).filter(Number.isFinite);
  if (values.length === 0) return null;
  return {
    min: Math.min(...values),
    p50: percentile(values, 50),
    p95: percentile(values, 95),
    max: Math.max(...values),
    last: values.at(-1),
  };
}

const options = parseArgs(process.argv.slice(2));
const rootPids = options.roots.map((root) => root.pid);
const samples = [];

for (let index = 0; index < options.samples; index += 1) {
  const sample = await sampleProcessTree(rootPids, {
    includePss: options.includePss,
    includeThreads: true,
    includeProcesses: index === options.samples - 1,
  });
  samples.push(sample);
  console.error(`[process-memory-bench] sample ${index + 1}/${options.samples}: rss_kib=${sample.rss_kib} pss_kib=${sample.pss_kib ?? 'n/a'} processes=${sample.process_count}`);
  if (index + 1 < options.samples) {
    await new Promise((resolveDelay) => setTimeout(resolveDelay, options.intervalMs));
  }
}

const componentSnapshots = {};
for (const root of options.roots) {
  componentSnapshots[root.label] = await sampleProcessTree([root.pid], {
    includePss: options.includePss,
    includeThreads: true,
    includeProcesses: true,
  });
}

const report = {
  schema: 'herdr-mcp/process-memory-benchmark/v1',
  generated_at: new Date().toISOString(),
  platform: process.platform,
  arch: process.arch,
  roots: options.roots,
  methodology: {
    samples: options.samples,
    interval_ms: options.intervalMs,
    rss: 'sum of ps RSS across the union of explicit root process trees',
    pss: options.includePss
      ? 'Linux /proc/<pid>/smaps_rollup Pss/Pss_Anon when readable'
      : null,
    threads: process.platform === 'darwin'
      ? 'ps -M count per process'
      : (process.platform === 'linux' ? 'ps nlwp field' : 'unavailable'),
    note: 'PIDs are explicit inputs; the benchmark does not infer runtime identity from process names.',
  },
  summary: {
    rss_kib: metricSummary(samples, 'rss_kib'),
    pss_kib: metricSummary(samples, 'pss_kib'),
    pss_anon_kib: metricSummary(samples, 'pss_anon_kib'),
    process_count: metricSummary(samples, 'process_count'),
    thread_count: metricSummary(samples, 'thread_count'),
  },
  samples,
  components: componentSnapshots,
};

const json = `${JSON.stringify(report, null, 2)}\n`;
if (options.out) {
  const outPath = isAbsolute(options.out) ? options.out : resolve(ROOT, options.out);
  await mkdir(dirname(outPath), { recursive: true });
  await writeFile(outPath, json);
  console.error(`[process-memory-bench] wrote ${outPath}`);
}
process.stdout.write(json);
