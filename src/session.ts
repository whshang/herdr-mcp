/**
 * Session store: durable label → workspace/handoff state (JSON files on disk).
 * Survives across MCP sessions and ChatGPT conversations.
 *
 * L-2: `cwd` renamed to `default_cwd` — semantic change from "the workspace's
 * authoritative directory" to "the default cwd for NEW panes created in this
 * session". The session's project set is derived from actual pane cwds at
 * reap time (L-5), not from this field.
 */
import * as fs from "node:fs";
import * as path from "node:path";
import { homedir } from "node:os";

export interface Handoff {
  summary: string;
  pending: string[];
  decisions: string[];
  saved_at: number;
}

export interface SessionProject {
  root: string;
  pane_ids: string[];
  /** L-1/P0-CRIT-3: dirty = uncommitted work present; changed_files = porcelain line count. */
  dirty?: boolean;
  changed_files?: number;
  /** P1-N: vcs + managed guardrails (unmanaged projects are never git-scanned). */
  vcs?: "git" | null;
  managed?: boolean;
}

export interface SessionData {
  label: string;
  workspace_id?: string;
  /** Default cwd for new panes. NOT the authoritative project root. */
  default_cwd?: string;
  root_pane?: string;
  created_at?: number;
  updated_at?: number;
  handoff?: Handoff;
  agents?: unknown[];
  /** L-5: snapshot of the project roots that existed when this session was created. */
  projects?: SessionProject[];
}

const SESSIONS_DIR = process.env.HERDR_MCP_STATE_DIR
  ?? path.join(homedir(), ".config", "herdr-mcp", "sessions");

function sessionPath(label: string): string {
  return path.join(SESSIONS_DIR, `${label.replace(/[/ ]/g, "_")}.json`);
}

export function get(label: string): SessionData | null {
  const p = sessionPath(label);
  if (!fs.existsSync(p)) return null;
  try {
    const raw = JSON.parse(fs.readFileSync(p, "utf-8")) as Record<string, unknown>;
    // Migrate: old sessions with `cwd` → `default_cwd`
    if ("cwd" in raw && !("default_cwd" in raw)) {
      raw["default_cwd"] = raw["cwd"];
      delete raw["cwd"];
    }
    return raw as unknown as SessionData;
  } catch {
    return null;
  }
}

export function save(label: string, data: Partial<SessionData>): void {
  fs.mkdirSync(SESSIONS_DIR, { recursive: true });
  const prev = get(label) ?? ({} as SessionData);
  const merged: SessionData = { ...prev, ...data, label, updated_at: Date.now() };
  fs.writeFileSync(sessionPath(label), JSON.stringify(merged, null, 2));
}

export function listAll(): SessionData[] {
  if (!fs.existsSync(SESSIONS_DIR)) return [];
  const out: SessionData[] = [];
  for (const f of fs.readdirSync(SESSIONS_DIR).sort()) {
    if (!f.endsWith(".json")) continue;
    try {
      out.push(get(f.replace(/\.json$/, "")) as SessionData);
    } catch { /* skip corrupt */ }
  }
  return out;
}
