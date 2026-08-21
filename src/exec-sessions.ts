/**
 * Background shell sessions for herdr_exec_start / read / kill.
 * Local child processes (not herdr utility panes). Persists a pid journal so
 * restarts can reap orphans left by detached:true spawns.
 *
 * Orphan reaping only kills PIDs that still carry HERDR_MCP_EXEC_ID=<session>
 * in their environment (PID-reuse safe).
 */
import { spawn, execSync, type ChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync, renameSync, existsSync } from "node:fs";
import * as path from "node:path";
import * as os from "node:os";

const MAX_BUFFER = 512 * 1024;
const SESSION_TTL_MS = 60 * 60_000;
const JOURNAL_DIR = process.env.HERDR_MCP_STATE_DIR
  ?? path.join(process.env.HOME ?? os.homedir(), ".config", "herdr-mcp");
const JOURNAL_PATH = path.join(JOURNAL_DIR, "exec-sessions.json");

type Chunk = { seq: number; stream: "stdout" | "stderr"; data: Buffer };

export type ExecSession = {
  id: string;
  cwd: string;
  command: string;
  startedAt: number;
  proc: ChildProcess;
  pid: number | null;
  chunks: Chunk[];
  nextSeq: number;
  stdoutBytes: number;
  stderrBytes: number;
  closed: boolean;
  exitCode: number | null;
  signal: NodeJS.Signals | null;
  truncated: boolean;
};

export type ExecSessionView = {
  session_id: string;
  cwd: string;
  command: string;
  started_at: string;
  running: boolean;
  exit_code: number | null;
  signal: string | null;
  truncated: boolean;
};

const sessions = new Map<string, ExecSession>();

type JournalEntry = { id: string; pid: number; cwd: string; command: string; startedAt: number };

function loadJournal(): JournalEntry[] {
  try {
    if (!existsSync(JOURNAL_PATH)) return [];
    const raw = JSON.parse(readFileSync(JOURNAL_PATH, "utf-8")) as { sessions?: JournalEntry[] };
    return Array.isArray(raw.sessions) ? raw.sessions : [];
  } catch {
    return [];
  }
}

function saveJournal(): void {
  try {
    mkdirSync(JOURNAL_DIR, { recursive: true });
    const entries: JournalEntry[] = [];
    for (const s of sessions.values()) {
      if (!s.closed && s.pid != null) {
        entries.push({ id: s.id, pid: s.pid, cwd: s.cwd, command: s.command.slice(0, 500), startedAt: s.startedAt });
      }
    }
    const tmp = JOURNAL_PATH + ".tmp";
    writeFileSync(tmp, JSON.stringify({ sessions: entries }, null, 2));
    renameSync(tmp, JOURNAL_PATH);
  } catch {
    /* best-effort */
  }
}

function pidAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

/** True only if the live process still carries our session env marker. */
function isOurOrphan(entry: JournalEntry): boolean {
  if (!pidAlive(entry.pid)) return false;
  try {
    // macOS/Linux: show env for the process; require exact session id marker.
    const dump = execSync(`ps eww -p ${entry.pid}`, {
      encoding: "utf-8",
      timeout: 2000,
      stdio: ["ignore", "pipe", "ignore"],
    });
    return dump.includes(`HERDR_MCP_EXEC_ID=${entry.id}`);
  } catch {
    return false;
  }
}

/** On boot: SIGTERM then SIGKILL journaled orphans that still match our marker. */
export function recoverExecSessionsOnBoot(): { reaped: number; skipped: number } {
  const prev = loadJournal();
  let reaped = 0;
  let skipped = 0;
  const toKill: JournalEntry[] = [];
  for (const e of prev) {
    if (!isOurOrphan(e)) { skipped += 1; continue; }
    toKill.push(e);
    try { process.kill(-e.pid, "SIGTERM"); } catch {
      try { process.kill(e.pid, "SIGTERM"); } catch { /* ignore */ }
    }
    reaped += 1;
  }
  try {
    mkdirSync(JOURNAL_DIR, { recursive: true });
    writeFileSync(JOURNAL_PATH, JSON.stringify({ sessions: [] }, null, 2));
  } catch { /* ignore */ }
  if (toKill.length > 0) {
    setTimeout(() => {
      for (const e of toKill) {
        if (!isOurOrphan(e)) continue;
        try { process.kill(-e.pid, "SIGKILL"); } catch {
          try { process.kill(e.pid, "SIGKILL"); } catch { /* ignore */ }
        }
      }
    }, 2000);
  }
  return { reaped, skipped };
}

function prune(): void {
  const now = Date.now();
  for (const [id, s] of sessions) {
    if (s.closed && now - s.startedAt > SESSION_TTL_MS) {
      sessions.delete(id);
    }
  }
  saveJournal();
}

function markClosed(s: ExecSession, code: number | null, signal: NodeJS.Signals | null): void {
  s.closed = true;
  s.exitCode = code;
  s.signal = signal;
  saveJournal();
}

function pushBuf(s: ExecSession, stream: "stdout" | "stderr", chunk: Buffer): void {
  let n = stream === "stdout" ? s.stdoutBytes : s.stderrBytes;
  if (n >= MAX_BUFFER) {
    s.truncated = true;
    return;
  }
  const room = MAX_BUFFER - n;
  const take = chunk.length > room ? chunk.subarray(0, room) : chunk;
  s.chunks.push({ seq: s.nextSeq++, stream, data: Buffer.from(take) });
  n += take.length;
  if (stream === "stdout") s.stdoutBytes = n;
  else s.stderrBytes = n;
  if (chunk.length > room) s.truncated = true;
}

export function startExecSession(opts: {
  command: string;
  cwd: string;
  env?: NodeJS.ProcessEnv;
}): ExecSession {
  prune();
  const id = `es_${randomUUID().slice(0, 12)}`;
  const proc = spawn("/bin/zsh", ["-lc", opts.command], {
    cwd: opts.cwd,
    env: { ...process.env, ...opts.env, HERDR_MCP_EXEC_ID: id },
    stdio: ["ignore", "pipe", "pipe"],
    detached: true,
  });
  const s: ExecSession = {
    id,
    cwd: opts.cwd,
    command: opts.command,
    startedAt: Date.now(),
    proc,
    pid: proc.pid ?? null,
    chunks: [],
    nextSeq: 0,
    stdoutBytes: 0,
    stderrBytes: 0,
    closed: false,
    exitCode: null,
    signal: null,
    truncated: false,
  };
  proc.stdout?.on("data", (c: Buffer) => pushBuf(s, "stdout", c));
  proc.stderr?.on("data", (c: Buffer) => pushBuf(s, "stderr", c));
  proc.on("close", (code, signal) => { markClosed(s, code, signal); });
  proc.on("error", () => { if (!s.closed) markClosed(s, null, null); });
  proc.unref();
  sessions.set(id, s);
  saveJournal();
  return s;
}

export function getExecSession(id: string): ExecSession | null {
  prune();
  return sessions.get(id) ?? null;
}

export function listExecSessions(): ExecSessionView[] {
  prune();
  return [...sessions.values()].map((s) => ({
    session_id: s.id,
    cwd: s.cwd,
    command: s.command.length > 200 ? s.command.slice(0, 200) + "…" : s.command,
    started_at: new Date(s.startedAt).toISOString(),
    running: !s.closed,
    exit_code: s.exitCode,
    signal: s.signal,
    truncated: s.truncated,
  }));
}

function bufferFor(s: ExecSession, stream: "stdout" | "stderr" | "both"): Buffer {
  if (stream === "both") {
    return Buffer.concat(s.chunks.map((c) => c.data));
  }
  return Buffer.concat(s.chunks.filter((c) => c.stream === stream).map((c) => c.data));
}

export function readExecSession(
  id: string,
  opts?: { stream?: "stdout" | "stderr" | "both"; offset?: number; limit?: number },
): {
  ok: true;
  session_id: string;
  running: boolean;
  exit_code: number | null;
  signal: string | null;
  truncated: boolean;
  stream: string;
  offset: number;
  text: string;
  next_offset: number;
  bytes_total: number;
} | { ok: false; reason: string } {
  const s = getExecSession(id);
  if (!s) return { ok: false, reason: "session_not_found" };
  const stream = opts?.stream ?? "both";
  const offset = Math.max(0, opts?.offset ?? 0);
  const limit = Math.min(Math.max(1, opts?.limit ?? 65536), 262144);
  const buf = bufferFor(s, stream);
  const slice = buf.subarray(offset, offset + limit);
  return {
    ok: true,
    session_id: id,
    running: !s.closed,
    exit_code: s.exitCode,
    signal: s.signal,
    truncated: s.truncated,
    stream,
    offset,
    text: slice.toString("utf-8"),
    next_offset: offset + slice.length,
    bytes_total: buf.length,
  };
}

export function killExecSession(id: string): {
  ok: true;
  session_id: string;
  killed: boolean;
  exit_code: number | null;
  signal: string | null;
} | { ok: false; reason: string } {
  const s = getExecSession(id);
  if (!s) return { ok: false, reason: "session_not_found" };
  if (s.closed) {
    return { ok: true, session_id: id, killed: false, exit_code: s.exitCode, signal: s.signal };
  }
  const pid = s.pid ?? s.proc.pid;
  try {
    if (pid) process.kill(-pid, "SIGTERM");
  } catch {
    try { s.proc.kill("SIGTERM"); } catch { /* ignore */ }
  }
  setTimeout(() => {
    if (!s.closed) {
      try {
        if (pid) process.kill(-pid, "SIGKILL");
      } catch {
        try { s.proc.kill("SIGKILL"); } catch { /* ignore */ }
      }
    }
  }, 1500);
  return { ok: true, session_id: id, killed: true, exit_code: null, signal: null };
}
