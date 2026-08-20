/**
 * Terminal output cleaning for human consumption (P1-K "clean" mode).
 * Shared by herdr_read/herdr_exec (server.ts) and the /push output snippet.
 *
 *  - Strip ANSI escape sequences
 *  - Drop spinner frames
 *  - Drop status-bar lines
 *  - Join soft-wrapped lines
 */

const ANSI_RE = /\x1b\[[0-9;]*[a-zA-Z]/g;
const ANSI_CSI_RE = /\x1b\[[?][0-9;]*[a-zA-Z]/g;
const SPINNER_RE = /⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇|⠏/;
const STATUS_BAR_RE = /deepseek-|token|R\d+[kK]|CH\d/;

export function cleanTerminalOutput(text: string): string {
  // Soft-wrap handling is owned by herdr's `recent_unwrapped` read source
  // (A-6: do NOT re-join wrapped lines client-side — that heuristic corrupts
  // CJK and code blocks). Here we only drop terminal chrome.
  const s = text.replace(ANSI_RE, "").replace(ANSI_CSI_RE, "");
  const out: string[] = [];
  for (const line of s.split("\n")) {
    // Drop spinner frame lines.
    if (SPINNER_RE.test(line)) continue;
    // Drop short trailing status-bar lines (model/token counters etc.).
    if (STATUS_BAR_RE.test(line) && line.trim().length > 0 && line.trim().length < 200 && /^[\s↑↓←→]/.test(line)) {
      continue;
    }
    out.push(line);
  }
  return out.join("\n");
}
