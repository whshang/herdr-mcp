import * as path from "node:path";
import * as os from "node:os";
import { readdir, stat, unlink } from "node:fs/promises";

export type UtilityPaneReadiness = {
  ready: boolean;
  shell_pid: number | null;
  foreground_process_group_id: number | null;
  foreground: Array<{ pid: number | null; name: string; cmdline: string }>;
};

/** POSIX single-quote escaping for shell command arguments. */
export function shellQuote(value: string): string {
  return "'" + String(value).replace(/'/g, `'\\''`) + "'";
}

/**
 * Commands submitted through herdr_exec run in a real TTY utility pane.
 * Disable implicit pagers so commands such as git log/diff/show and gh view
 * cannot leave the pane inside less and consume the next submitted command.
 */
export function buildUtilityExecScript(opts: {
  execShell: string;
  cwd: string | null;
  command: string;
}): string {
  return [
    `#!${opts.execShell}`,
    "set +e",
    "export PAGER=cat",
    "export GIT_PAGER=cat",
    "export GH_PAGER=cat",
    "export SYSTEMD_PAGER=cat",
    "export MANPAGER=cat",
    "export DELTA_PAGER=cat",
    // The outer launcher also removes the file. This trap covers manual runs
    // and normal exits where the outer launcher is interrupted after start.
    "trap 'rm -f -- \"$0\"' EXIT",
    opts.cwd ? `cd -- ${shellQuote(opts.cwd)} || exit 127` : "",
    opts.command,
  ].filter(Boolean).join("\n") + "\n";
}

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" ? value as Record<string, unknown> : {};
}

function finitePid(value: unknown): number | null {
  const n = Number(value);
  return Number.isFinite(n) && n > 0 ? n : null;
}

/**
 * A reusable utility pane is safe only while its interactive shell owns the
 * foreground process group. If less/git/gh/etc. still owns the TTY, sending
 * command text would be interpreted as interactive input instead of shell.
 */
export function utilityPaneReadiness(raw: unknown): UtilityPaneReadiness {
  const root = asRecord(raw);
  const info = asRecord(root["process_info"] ?? root);
  const shellPid = finitePid(info["shell_pid"]);
  const foregroundPgid = finitePid(info["foreground_process_group_id"]);
  const foreground = (Array.isArray(info["foreground_processes"]) ? info["foreground_processes"] : [])
    .map((entry) => {
      const rec = asRecord(entry);
      return {
        pid: finitePid(rec["pid"]),
        name: path.basename(String(rec["name"] ?? rec["argv0"] ?? "")),
        cmdline: String(rec["cmdline"] ?? ""),
      };
    });

  const shellOwnsForeground = shellPid !== null
    && foregroundPgid !== null
    && foregroundPgid === shellPid;
  const onlyShellForeground = foreground.length > 0
    && foreground.every((proc) => proc.pid === shellPid);

  return {
    ready: shellOwnsForeground && onlyShellForeground,
    shell_pid: shellPid,
    foreground_process_group_id: foregroundPgid,
    foreground,
  };
}

/** Remove stale temporary exec wrappers left by interrupted TTY sessions. */
export async function cleanupStaleUtilityScripts(tmpDir = process.env.TMPDIR || os.tmpdir()): Promise<number> {
  let removed = 0;
  let names: string[] = [];
  try {
    names = await readdir(tmpDir);
  } catch {
    return 0;
  }
  const cutoff = Date.now() - 24 * 60 * 60 * 1000;
  for (const name of names) {
    if (!name.startsWith("herdr-mcp-exec-") || !name.endsWith(".sh")) continue;
    const file = path.join(tmpDir, name);
    try {
      if ((await stat(file)).mtimeMs < cutoff) {
        await unlink(file);
        removed++;
      }
    } catch {
      // Best effort cleanup.
    }
  }
  return removed;
}
