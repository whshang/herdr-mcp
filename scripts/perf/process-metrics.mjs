import { spawnSync } from 'node:child_process';
import { readFile } from 'node:fs/promises';
import process from 'node:process';

export function parsePsTable(text, { hasThreads = false } = {}) {
  const rows = [];
  for (const line of text.split(/\r?\n/)) {
    if (!line.trim()) continue;
    const match = hasThreads
      ? line.match(/^\s*(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(.*)$/)
      : line.match(/^\s*(\d+)\s+(\d+)\s+(\d+)\s+(.*)$/);
    if (!match) continue;
    rows.push(hasThreads
      ? {
          pid: Number(match[1]),
          ppid: Number(match[2]),
          rss_kib: Number(match[3]),
          threads: Number(match[4]),
          command: match[5],
        }
      : {
          pid: Number(match[1]),
          ppid: Number(match[2]),
          rss_kib: Number(match[3]),
          threads: null,
          command: match[4],
        });
  }
  return rows;
}

export function selectProcessTree(rows, rootPids) {
  const wanted = new Set(rootPids.map(Number).filter(Number.isInteger));
  let changed = true;
  while (changed) {
    changed = false;
    for (const row of rows) {
      if (!wanted.has(row.pid) && wanted.has(row.ppid)) {
        wanted.add(row.pid);
        changed = true;
      }
    }
  }
  return rows.filter((row) => wanted.has(row.pid));
}

function psSnapshot({ includeThreads }) {
  const hasThreads = includeThreads && process.platform === 'linux';
  const fields = hasThreads
    ? 'pid=,ppid=,rss=,nlwp=,command='
    : 'pid=,ppid=,rss=,command=';
  const result = spawnSync('ps', ['-axo', fields], {
    encoding: 'utf8',
    maxBuffer: 16 * 1024 * 1024,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`ps failed with status ${result.status}: ${(result.stderr ?? '').trim()}`);
  }
  return parsePsTable(result.stdout, { hasThreads });
}

function darwinThreadCount(pid) {
  const result = spawnSync('ps', ['-M', '-p', String(pid)], {
    encoding: 'utf8',
    maxBuffer: 1024 * 1024,
  });
  if (result.error || result.status !== 0) return null;
  const lines = result.stdout.split(/\r?\n/).filter((line) => line.trim());
  return Math.max(0, lines.length - 1);
}

export function parseLinuxSmapsRollup(text) {
  const values = new Map();
  for (const line of text.split(/\r?\n/)) {
    const match = line.match(/^([A-Za-z_]+):\s+(\d+)\s+kB$/);
    if (match) values.set(match[1], Number(match[2]));
  }
  return {
    pss_kib: values.get('Pss') ?? null,
    pss_anon_kib: values.get('Pss_Anon') ?? null,
  };
}

async function linuxPss(pid) {
  try {
    const text = await readFile(`/proc/${pid}/smaps_rollup`, 'utf8');
    return parseLinuxSmapsRollup(text);
  } catch {
    return { pss_kib: null, pss_anon_kib: null };
  }
}

export function summarizeProcessRows(rows) {
  const rss = rows.reduce((sum, row) => sum + (row.rss_kib ?? 0), 0);
  const knownThreads = rows.filter((row) => Number.isFinite(row.threads));
  const knownPss = rows.filter((row) => Number.isFinite(row.pss_kib));
  const knownPssAnon = rows.filter((row) => Number.isFinite(row.pss_anon_kib));
  return {
    process_count: rows.length,
    thread_count: knownThreads.length > 0
      ? knownThreads.reduce((sum, row) => sum + row.threads, 0)
      : null,
    thread_count_complete: rows.length > 0 && knownThreads.length === rows.length,
    rss_kib: rss,
    pss_kib: knownPss.length > 0
      ? knownPss.reduce((sum, row) => sum + row.pss_kib, 0)
      : null,
    pss_anon_kib: knownPssAnon.length > 0
      ? knownPssAnon.reduce((sum, row) => sum + row.pss_anon_kib, 0)
      : null,
  };
}

export async function sampleProcessTree(rootPids, {
  includePss = process.platform === 'linux',
  includeThreads = true,
  includeProcesses = false,
} = {}) {
  let rows = selectProcessTree(psSnapshot({ includeThreads }), rootPids);

  if (includeThreads && process.platform === 'darwin') {
    rows = rows.map((row) => ({ ...row, threads: darwinThreadCount(row.pid) }));
  }

  if (includePss && process.platform === 'linux') {
    rows = await Promise.all(rows.map(async (row) => ({ ...row, ...(await linuxPss(row.pid)) })));
  }

  const summary = summarizeProcessRows(rows);
  return {
    captured_at: new Date().toISOString(),
    platform: process.platform,
    root_pids: rootPids.map(Number),
    ...summary,
    ...(includeProcesses
      ? {
          processes: rows
            .slice()
            .sort((a, b) => (b.rss_kib ?? 0) - (a.rss_kib ?? 0))
            .map((row) => ({
              pid: row.pid,
              ppid: row.ppid,
              rss_kib: row.rss_kib,
              threads: row.threads,
              pss_kib: row.pss_kib ?? null,
              pss_anon_kib: row.pss_anon_kib ?? null,
              command: row.command,
            })),
        }
      : {}),
  };
}
