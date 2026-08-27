#!/usr/bin/env node

import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { performance } from 'node:perf_hooks';

const baseUrl = process.env.HERDR_BENCH_BASE_URL ?? 'http://127.0.0.1:18871/mcp';
const candidateUrl = process.env.HERDR_BENCH_CANDIDATE_URL ?? 'http://127.0.0.1:18872/mcp';
const token = process.env.HERDR_BENCH_TOKEN;
const root = process.env.HERDR_BENCH_ROOT ?? process.cwd();
const outputPath = process.env.HERDR_BENCH_OUT ?? join(root, 'docs', 'benchmarks', 'tool-performance-latest.json');
const baseCommit = process.env.HERDR_BENCH_BASE_COMMIT ?? 'baseline';
const candidateCommit = process.env.HERDR_BENCH_CANDIDATE_COMMIT ?? 'candidate';

if (!token) {
  throw new Error('HERDR_BENCH_TOKEN is required');
}

let requestId = 0;

async function callTool(url, name, args) {
  const body = JSON.stringify({
    jsonrpc: '2.0',
    id: ++requestId,
    method: 'tools/call',
    params: { name, arguments: args },
  });
  const started = performance.now();
  const response = await fetch(url, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${token}`,
      'content-type': 'application/json',
      accept: 'application/json',
      'user-agent': 'openai-mcp/herdr-benchmark',
    },
    body,
  });
  const text = await response.text();
  const elapsedMs = performance.now() - started;
  if (!response.ok) {
    throw new Error(`${name} HTTP ${response.status}: ${text.slice(0, 240)}`);
  }
  const payload = JSON.parse(text);
  if (payload.error || !payload.result) {
    throw new Error(`${name} JSON-RPC failure: ${text.slice(0, 320)}`);
  }
  return { elapsedMs, responseBytes: Buffer.byteLength(text) };
}

function percentile(values, p) {
  const sorted = [...values].sort((a, b) => a - b);
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1));
  return sorted[index];
}

function summarize(samples) {
  const times = samples.map((sample) => sample.elapsedMs);
  const bytes = samples.map((sample) => sample.responseBytes);
  return {
    samples: samples.length,
    p50_ms: Number(percentile(times, 50).toFixed(3)),
    p95_ms: Number(percentile(times, 95).toFixed(3)),
    min_ms: Number(Math.min(...times).toFixed(3)),
    max_ms: Number(Math.max(...times).toFixed(3)),
    median_response_bytes: percentile(bytes, 50),
  };
}

const specs = [
  {
    key: 'inspect',
    tool: 'herdr_inspect',
    args: {},
    samples: 24,
  },
  {
    key: 'since_wBH',
    tool: 'herdr_since',
    args: { cursor: 0, workspace: 'wBH' },
    samples: 24,
  },
  {
    key: 'fs_read_small',
    tool: 'herdr_fs_read',
    args: { path: join(root, 'README.md'), start_line: 1, end_line: 20, max_bytes: 8192 },
    samples: 30,
  },
  {
    key: 'fs_list_src',
    tool: 'herdr_fs_list',
    args: { path: join(root, 'crates', 'herdr-mcp', 'src'), recursive: false, max_entries: 200 },
    samples: 24,
  },
  {
    key: 'fs_grep_rust',
    tool: 'herdr_fs_grep',
    args: { root, pattern: 'herdr_fs_grep', glob: 'crates/**/*.rs', max_matches: 20 },
    samples: 16,
  },
  {
    key: 'git_status',
    tool: 'herdr_git',
    args: { root, action: 'status', max_bytes: 65536 },
    samples: 16,
  },
];

const coldMethods = {};
for (const [label, url] of [['baseline', baseUrl], ['candidate', candidateUrl]]) {
  coldMethods[label] = await callTool(url, 'herdr_methods', { query: 'worktree' });
}

const results = {};
for (const spec of specs) {
  const buckets = { baseline: [], candidate: [] };
  for (let index = 0; index < spec.samples; index += 1) {
    const order = index % 2 === 0
      ? [['baseline', baseUrl], ['candidate', candidateUrl]]
      : [['candidate', candidateUrl], ['baseline', baseUrl]];
    for (const [label, url] of order) {
      buckets[label].push(await callTool(url, spec.tool, spec.args));
    }
  }
  results[spec.key] = {
    tool: spec.tool,
    baseline: summarize(buckets.baseline),
    candidate: summarize(buckets.candidate),
  };
}

const cold = Object.fromEntries(Object.entries(coldMethods).map(([label, sample]) => [label, {
  elapsed_ms: Number(sample.elapsedMs.toFixed(3)),
  response_bytes: sample.responseBytes,
}]));

const report = {
  schema: 'herdr-mcp/tool-performance-benchmark/v1',
  generated_at: new Date().toISOString(),
  baseline: { commit: baseCommit, url: baseUrl },
  candidate: { commit: candidateCommit, url: candidateUrl },
  root,
  methodology: {
    transport: 'loopback MCP HTTP tools/call, openai-mcp stateless client',
    ordering: 'alternating baseline/candidate per sample',
    note: 'Local server timings isolate Rust/tool-path work; real Connector/model latency is measured separately after self-upgrade.',
  },
  cold_methods: cold,
  results,
};

await mkdir(dirname(outputPath), { recursive: true });
await writeFile(outputPath, `${JSON.stringify(report, null, 2)}\n`);
console.log(JSON.stringify(report, null, 2));
