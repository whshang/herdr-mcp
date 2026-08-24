/**
 * Synchronous-style local shell for herdr_exec fallback when the herdr
 * utility-pane control plane is unavailable (TaskGroup / ExceptionGroup).
 * Same temp-script pattern as the utility-pane path (heredoc-safe).
 */
import { spawn } from "node:child_process";
import { writeFile, unlink } from "node:fs/promises";
import * as path from "node:path";
import { randomUUID } from "node:crypto";
import { tmpdir } from "node:os";
import { enrichedUserEnv } from "./user-path.js";
import { resolveExecShell } from "./exec-sessions.js";

export type LocalExecResult = {
  exit_code: number | null;
  signal: string | null;
  output: string;
  timed_out: boolean;
  script_path: string;
};

function shq(s: string): string {
  return "'" + s.replace(/'/g, `'\\''`) + "'";
}

/**
 * Run `command` under an available login shell (zsh → bash → sh) with
 * cwd = `cwd`. stdout and stderr are merged in arrival order into `output`
 * (capped). The shell is resolved the same way as background exec sessions so
 * the fallback backend works on Linux CI and Linux Herdr workstations too.
 */
export async function runLocalShell(opts: {
  command: string;
  cwd: string;
  timeoutMs: number;
  maxOutputBytes?: number;
}): Promise<LocalExecResult> {
  const maxBytes = opts.maxOutputBytes ?? 8000;
  const nonce = randomUUID().slice(0, 8);
  const scriptPath = path.join(tmpdir(), `herdr-mcp-local-${nonce}.sh`);
  const shell = resolveExecShell(process.env);
  const body = [
    `#!${shell}`,
    "set +e",
    `cd -- ${shq(opts.cwd)} || exit 127`,
    opts.command,
  ].join("\n") + "\n";
  await writeFile(scriptPath, body, { encoding: "utf-8", mode: 0o700 });

  const chunks: Buffer[] = [];
  let total = 0;
  let truncated = false;

  const push = (c: Buffer) => {
    if (total >= maxBytes) {
      truncated = true;
      return;
    }
    const room = maxBytes - total;
    const take = c.length > room ? c.subarray(0, room) : c;
    chunks.push(Buffer.from(take));
    total += take.length;
    if (c.length > room) truncated = true;
  };

  return new Promise((resolve) => {
    const proc = spawn(shell, [scriptPath], {
      cwd: opts.cwd,
      env: enrichedUserEnv(process.env),
      stdio: ["ignore", "pipe", "pipe"],
      // POSIX: make the shell the leader of its own process group so timeout
      // cleanup can kill only this command tree. Never use `kill 0` from a
      // non-detached child: it can signal the parent and unrelated siblings.
      detached: process.platform !== "win32",
    });
    let timedOut = false;
    let exitCode: number | null = null;
    let signal: string | null = null;
    const killCommandTree = (sig: NodeJS.Signals) => {
      if (proc.pid && process.platform !== "win32") {
        try {
          process.kill(-proc.pid, sig);
          return;
        } catch { /* child may already have exited */ }
      }
      try { proc.kill(sig); } catch { /* ignore */ }
    };
    const timer = setTimeout(() => {
      timedOut = true;
      killCommandTree("SIGTERM");
      setTimeout(() => {
        killCommandTree("SIGKILL");
      }, 800);
    }, Math.max(1, opts.timeoutMs));

    proc.stdout?.on("data", (c: Buffer) => push(c));
    proc.stderr?.on("data", (c: Buffer) => push(c));
    proc.on("close", (code, sig) => {
      clearTimeout(timer);
      exitCode = code;
      signal = sig;
      void unlink(scriptPath).catch(() => {});
      let output = Buffer.concat(chunks).toString("utf-8");
      if (truncated) output += "\n…[truncated]";
      resolve({
        exit_code: exitCode,
        signal,
        output,
        timed_out: timedOut,
        script_path: scriptPath,
      });
    });
    proc.on("error", () => {
      clearTimeout(timer);
      void unlink(scriptPath).catch(() => {});
      resolve({
        exit_code: null,
        signal: null,
        output: Buffer.concat(chunks).toString("utf-8"),
        timed_out: timedOut,
        script_path: scriptPath,
      });
    });
  });
}
