import { homedir } from "node:os";
import { delimiter, join } from "node:path";

/**
 * Build the PATH used by non-interactive workstation processes.
 *
 * macOS LaunchAgents intentionally have a small PATH, while interactive shells
 * commonly add user-installed CLI locations in .zshrc. herdr_exec_start and
 * local fallback shells are non-interactive, so without this normalization a
 * CLI can be visible in a Herdr pane but disappear in a background session.
 */
export function enrichedUserPath(env: NodeJS.ProcessEnv = process.env): string {
  const home = env.HOME?.trim() || homedir();
  const userBins = [
    join(home, ".local", "bin"),
    join(home, ".npm-global", "bin"),
    join(home, ".cargo", "bin"),
    join(home, ".opencode", "bin"),
    join(home, ".grok", "bin"),
  ];
  const systemFallbacks = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];
  const inherited = (env.PATH || "").split(delimiter).filter(Boolean);
  const seen = new Set<string>();
  const ordered: string[] = [];
  for (const entry of [...userBins, ...inherited, ...systemFallbacks]) {
    if (!entry || seen.has(entry)) continue;
    seen.add(entry);
    ordered.push(entry);
  }
  return ordered.join(delimiter);
}

export function enrichedUserEnv(env: NodeJS.ProcessEnv = process.env): NodeJS.ProcessEnv {
  return { ...env, PATH: enrichedUserPath(env) };
}
