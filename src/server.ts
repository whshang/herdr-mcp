#!/usr/bin/env node
/**
 * herdr-mcp — Node.js (TypeScript) MCP server.
 *
 * Faithful rewrite of herdr_mcp/server.py on @modelcontextprotocol/sdk + express:
 *  - Default MCP tool surface (18): inspect/skill/call/since/prompt + fs_* + git + exec/exec_*.
 *  - HERDR_MCP_ALL_TOOLS=1 adds advanced/deprecated lifecycle tools (30 total).
 *  - Express on HERDR_MCP_PORT (default 8772).
 *  - OAuth DCR endpoints for Claude.ai / ChatGPT connectors.
 *  - Bearer auth on /mcp via HERDR_MCP_TOKEN.
 */
import express, { Express, Request, Response, NextFunction } from "express";
import { createHash, randomUUID } from "node:crypto";
import { recordMcpToolCall } from "./mcp-activity.js";
import { readFile, writeFile, realpath, readdir, stat, unlink, mkdir } from "node:fs/promises";
import * as path from "node:path";
import { exec, execSync, spawn, spawnSync } from "node:child_process";
import { promisify } from "node:util";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import * as z from "zod/v4";

import { HerdrClient, HerdrError, HerdrResult, NON_IDEMPOTENT_METHODS } from "./herdr.js";
import { snapFromListApis, fetchSessionSnapshot } from "./snap-fallback.js";
import { HERDR_RPC_TIMEOUT_MAX_MS, clampHerdrTimeout } from "./timeouts.js";
import { getSnapshotCache } from "./state.js";
import {
  filterVisibleAgents,
  redactPaneAgents,
  visibilityMeta,
} from "./agent-visibility.js";
import { cleanTerminalOutput } from "./clean.js";
import { validateMethodParams, listMethods, warmSchemaCache } from "./schema.js";
import { get as sessionGet, save as sessionSave, type SessionData, type SessionProject } from "./session.js";
import { waitForAgent } from "./wait.js";
import { registerPushRoutes } from "./push.js";
import {
  registerOAuthRoutes,
  mcpBearerAuth,
  oauthCors,
  isChatgptOAuthClientId,
  getRequestOAuthClientId,
} from "./oauth.js";
import { SERVER_NAME, SERVER_VERSION } from "./version.js";
import {
  isAgentStatusWaitTimeout,
  isHerdrControlPlaneTaskGroup,
  isTrueTransportFailure,
  unwrapControlPlaneMessage,
  buildStateObservation,
} from "./prompt-semantics.js";
import { parsePatch, applyUpdateHunks, PatchError } from "./patch.js";
import {
  startExecSession,
  readExecSession,
  killExecSession,
  listExecSessions,
  recoverExecSessionsOnBoot,
} from "./exec-sessions.js";
import { commitAtomic } from "./atomic-files.js";
import { runLocalShell } from "./local-exec.js";
import { fetchHerdrSkill, herdrSkillPointer } from "./herdr-skill.js";

/** Process boot id — returned by inspect/since so clients detect cursor reset. */
const BOOT_ID = randomUUID().slice(0, 12);

/** SDK-supported wire version used when ChatGPT advertises 2026-07-28. */
const SDK_WIRE_PROTOCOL = "2025-11-25";

// ---------------------------------------------------------------------------
// Config from environment (mirrors server.py)
// ---------------------------------------------------------------------------
const PORT = Number(process.env.HERDR_MCP_PORT ?? "8772");
const BASE_URL = process.env.HERDR_MCP_BASE_URL ?? ""; // e.g. https://xxxx.trycloudflare.com
const AUTH_TOKEN = process.env.HERDR_MCP_TOKEN ?? "";
// Tool-surface switch: default exposes the lean 18-tool surface; HERDR_MCP_ALL_TOOLS=1
// additionally registers advanced + deprecated lifecycle tools (wait/task/session/handoff/
// reap/parallel/read/explain/prompt_status/transcript/diff) — 30 total.
const ALL_TOOLS = process.env.HERDR_MCP_ALL_TOOLS === "1";
// E: authorization switches. READONLY blocks every mutating operation;
// WRITE_ROOTS (csv) limits mutations to listed roots (unset = all managed roots).
// P0-2: idempotency records for herdr_prompt — a replay with the same key
// returns the stored result instead of re-sending (prompt is non-idempotent).
const PROMPT_RECORD_TTL_MS = 10 * 60_000;
const promptRecords = new Map<string, { at: number; result: Record<string, unknown> }>();
function rememberPrompt(key: string, result: Record<string, unknown>): void {
  promptRecords.set(key, { at: Date.now(), result });
  if (promptRecords.size > 512) {
    const now = Date.now();
    for (const [k, v] of promptRecords) if (now - v.at > PROMPT_RECORD_TTL_MS) promptRecords.delete(k);
  }
}
const READONLY_MODE = process.env.HERDR_MCP_READONLY === "1";
const WRITE_ROOTS = (process.env.HERDR_MCP_WRITE_ROOTS ?? "")
  .split(",").map((s) => s.trim()).filter((s) => s.length > 0);
const SOCKET_PATH = process.env.HERDR_SOCKET_PATH;

// ---------------------------------------------------------------------------
// Build identity (P1-M): lets clients detect stale deployments
// ---------------------------------------------------------------------------
const BUILD_INFO = {
  commit: process.env.HERDR_MCP_BUILD_COMMIT ?? "dev",
  built_at: process.env.HERDR_MCP_BUILT_AT ?? new Date().toISOString(),
  started_at: new Date().toISOString(),
  pid: process.pid,
};

function buildInfo(): Record<string, unknown> {
  return {
    ...BUILD_INFO,
    server_version: SERVER_VERSION,
    started_at: BUILD_INFO.started_at,
    pid: process.pid,
    stale: BUILD_INFO.built_at > BUILD_INFO.started_at, // false in normal operation
  };
}

// ---------------------------------------------------------------------------
// Herdr client singleton (mirrors _client_get)
// ---------------------------------------------------------------------------
let _client: HerdrClient | null = null;
function clientGet(): HerdrClient {
  if (_client === null) {
    _client = new HerdrClient(SOCKET_PATH);
  }
  return _client;
}

/** Git helpers for project derivation (L-1). */
interface ProjectInfo {
  root: string;
  vcs: "git" | null;
  managed: boolean;
  dirty: boolean;
  changed_files: number;
  panes: string[];
  pane_ids: string[];
  cwds: string[];
}

/** HOME or '/' roots are never scanned / never considered managed (P1-N). */
function isUnmanagedRoot(root: string): boolean {
  const home = process.env.HOME;
  return root === "/" || (home !== undefined && root === home);
}

/** git rev-parse --show-toplevel; null if not a git repo (caller falls back to cwd). */
function gitToplevel(cwd: string): string | null {
  try {
    const out = execSync("git rev-parse --show-toplevel", {
      cwd, timeout: 100, stdio: ["ignore", "pipe", "ignore"],
    });
    return out.toString().trim() || null;
  } catch {
    return null;
  }
}

/**
 * git status --porcelain for a root: `dirty` = has uncommitted changes,
 * `changed_files` = number of non-empty porcelain lines. Only meaningful for
 * git repos; a non-git cwd is treated as clean.
 */
function gitStatus(root: string): { dirty: boolean; changed_files: number } {
  try {
    const out = execSync("git status --porcelain", {
      cwd: root, timeout: 500, stdio: ["ignore", "pipe", "ignore"],
    });
    const lines = out.toString().split("\n").filter((l) => l.trim().length > 0);
    return { dirty: lines.length > 0, changed_files: lines.length };
  } catch {
    return { dirty: false, changed_files: 0 };
  }
}

/**
 * Derive the git project (repo root) for every pane with a cwd, grouping panes
 * by project root. Per L-1: a git repo resolves to its toplevel; a non-git cwd
 * is itself the project root. `dirty` is computed once per project.
 */
function deriveProjects(snap: HerdrResult): Map<string, ProjectInfo> {
  // pane_id -> cwd (dedupe; agents preferred, panes fallback)
  const cwdPanes = new Map<string, string[]>();
  const addPane = (pane: string | null, cwd: string | null) => {
    if (!pane || !cwd) return;
    const arr = cwdPanes.get(cwd) ?? [];
    if (!arr.includes(pane)) arr.push(pane);
    cwdPanes.set(cwd, arr);
  };
  const agentsRaw = (snap["agents"] as unknown[]) ?? [];
  for (const a of agentsRaw) {
    const rec = (a ?? {}) as Record<string, unknown>;
    const pane = typeof rec["pane_id"] === "string" ? (rec["pane_id"] as string) : null;
    const cwd = typeof rec["cwd"] === "string" ? (rec["cwd"] as string)
      : typeof rec["foreground_cwd"] === "string" ? (rec["foreground_cwd"] as string) : null;
    addPane(pane, cwd);
  }
  const panesRaw = (snap["panes"] as unknown[]) ?? [];
  for (const p of panesRaw) {
    const rec = (p ?? {}) as Record<string, unknown>;
    const pane = typeof rec["pane_id"] === "string" ? (rec["pane_id"] as string) : null;
    const cwd = typeof rec["cwd"] === "string" ? (rec["cwd"] as string)
      : typeof rec["foreground_cwd"] === "string" ? (rec["foreground_cwd"] as string) : null;
    addPane(pane, cwd);
  }

  const projects = new Map<string, ProjectInfo>();
  for (const [cwd, panes] of cwdPanes) {
    const gitRoot = gitToplevel(cwd);
    const root = gitRoot ?? cwd; // non-git cwd is its own project root
    let proj = projects.get(root);
    if (!proj) {
      const vcs: "git" | null = gitRoot ? "git" : null;
      // P1-N: HOME/root or non-git cwds are unmanaged — no git scan, clean.
      const unmanaged = isUnmanagedRoot(root) || vcs === null;
      let st = { dirty: false, changed_files: 0 };
      if (!unmanaged) st = gitStatus(root);
      proj = {
        root, vcs, managed: !unmanaged, dirty: st.dirty, changed_files: st.changed_files,
        panes: [], pane_ids: [], cwds: [],
      };
      projects.set(root, proj);
    }
    for (const pane of panes) {
      if (!proj.panes.includes(pane)) proj.panes.push(pane);
    }
    proj.pane_ids = [...proj.panes];
    if (!proj.cwds.includes(cwd)) proj.cwds.push(cwd);
  }
  return projects;
}

/**
 * Current projects ({root, panes}) among the panes of a specific workspace.
 * Reuses deriveProjects (git-derived roots) and filters by workspace_id.
 */
function projectsForWorkspace(snap: HerdrResult, wsId: string): SessionProject[] {
  // pane_id -> workspace_id
  const paneWs = new Map<string, string>();
  for (const a of (snap["agents"] as unknown[]) ?? []) {
    const rec = (a ?? {}) as Record<string, unknown>;
    if (typeof rec["pane_id"] === "string") {
      paneWs.set(rec["pane_id"] as string, typeof rec["workspace_id"] === "string" ? (rec["workspace_id"] as string) : "");
    }
  }
  for (const p of (snap["panes"] as unknown[]) ?? []) {
    const rec = (p ?? {}) as Record<string, unknown>;
    if (typeof rec["pane_id"] === "string") {
      const pid = rec["pane_id"] as string;
      if (!paneWs.has(pid)) paneWs.set(pid, typeof rec["workspace_id"] === "string" ? (rec["workspace_id"] as string) : "");
    }
  }
  const byRoot = new Map<string, { root: string; pane_ids: string[]; dirty: boolean; changed_files: number; vcs: "git" | null; managed: boolean }>();
  for (const [, proj] of deriveProjects(snap)) {
    const panes = proj.pane_ids.filter((p) => paneWs.get(p) === wsId);
    if (panes.length > 0) byRoot.set(proj.root, { root: proj.root, pane_ids: panes, dirty: proj.dirty, changed_files: proj.changed_files, vcs: proj.vcs, managed: proj.managed });
  }
  return [...byRoot.values()].map((p) => ({ root: p.root, pane_ids: p.pane_ids, dirty: p.dirty, changed_files: p.changed_files, vcs: p.vcs, managed: p.managed }));
}

/**
 * L-5 create snapshot: after workspace.create the snapshot may lag behind. Poll
 * briefly until a pane in the workspace gains a derivable project; if still
 * empty, fall back to a project derived from the requested default cwd so the
 * session isn't left with zero projects (which would make every future project
 * look "new" at reap time).
 */
async function snapshotProjectsForCreate(c: HerdrClient, wsId: string, fallbackCwd: string): Promise<SessionProject[]> {
  for (let i = 0; i < 10; i++) {
    const snap = await c.snapshot();
    const projs = projectsForWorkspace(snap, wsId);
    if (projs.some((p) => p.pane_ids.length > 0)) return projs;
    await new Promise((r) => setTimeout(r, 150));
  }
  const root = fallbackCwd ? (gitToplevel(fallbackCwd) ?? fallbackCwd) : "";
  return root ? [{ root, pane_ids: [] }] : [];
}

/** Mirrors _project_snapshot in server.py. */
function projectSnapshot(snap: HerdrResult): Record<string, unknown> {
  const projects = deriveProjects(snap); // cache per-request (L-1)
  // pane_id -> project root for fast lookup
  const paneToRoot = new Map<string, string>();
  for (const [, proj] of projects) {
    for (const p of proj.pane_ids) paneToRoot.set(p, proj.root);
  }
  // pane_id -> workspace_id
  const paneToWs = new Map<string, string | null>();
  for (const a of (snap["agents"] as unknown[]) ?? []) {
    const rec = (a ?? {}) as Record<string, unknown>;
    if (typeof rec["pane_id"] === "string") {
      paneToWs.set(rec["pane_id"] as string, typeof rec["workspace_id"] === "string" ? (rec["workspace_id"] as string) : null);
    }
  }
  for (const p of (snap["panes"] as unknown[]) ?? []) {
    const rec = (p ?? {}) as Record<string, unknown>;
    if (typeof rec["pane_id"] === "string" && !paneToWs.has(rec["pane_id"] as string)) {
      paneToWs.set(rec["pane_id"] as string, typeof rec["workspace_id"] === "string" ? (rec["workspace_id"] as string) : null);
    }
  }

  const ws = (snap["workspaces"] as unknown[]) ?? [];

  // P1-M: global project_root -> workspace_ids[] across ALL workspaces.
  const rootToWorkspaces = new Map<string, Set<string>>();
  for (const [pane, root] of paneToRoot) {
    const wsId = paneToWs.get(pane);
    if (!wsId) continue;
    if (!rootToWorkspaces.has(root)) rootToWorkspaces.set(root, new Set());
    rootToWorkspaces.get(root)!.add(wsId);
  }

  const workspaces = ws.map((w) => {
    const wrec = (w ?? {}) as Record<string, unknown>;
    const wt = (wrec["worktree"] ?? {}) as Record<string, unknown>;
    const wsId = wrec["workspace_id"] as string;
    // Per-workspace projects: the distinct project roots among this workspace's panes.
    const projMap = new Map<string, { root: string; pane_ids: string[]; dirty: boolean; changed_files: number; vcs: "git" | null; managed: boolean }>();
    for (const [pane, root] of paneToRoot) {
      if (paneToWs.get(pane) !== wsId) continue;
      const proj = projects.get(root);
      const entry = projMap.get(root) ?? {
        root, pane_ids: [], dirty: proj?.dirty ?? false, changed_files: proj?.changed_files ?? 0,
        vcs: proj?.vcs ?? null, managed: proj?.managed ?? false,
      };
      if (!entry.pane_ids.includes(pane)) entry.pane_ids.push(pane);
      projMap.set(root, entry);
    }
    const projList = [...projMap.values()].map(({ root, pane_ids, dirty, changed_files, vcs, managed }) => ({
      root,
      pane_ids,
      dirty,
      changed_files,
      vcs,
      managed,
      // P1-M: other workspaces sharing this same project root.
      also_open_in: [...(rootToWorkspaces.get(root) ?? new Set())].filter((id) => id !== wsId).sort(),
    }));
    return {
      id: wsId,
      label: wrec["label"],
      cwd: wrec["cwd"] ?? (typeof wt === "object" ? wt["path"] : undefined),
      tabs: wrec["tab_count"],
      panes: wrec["pane_count"],
      focused: wrec["focused"],
      projects: projList,
      heterogeneous: projList.length > 1,
    };
  });

  // P1-M: top-level shared_projects — roots open in MORE THAN ONE workspace.
  const sharedProjects: { root: string; workspace_ids: string[]; pane_ids: string[]; dirty: boolean; managed: boolean; vcs: "git" | null }[] = [];
  for (const [root, wsSet] of rootToWorkspaces) {
    if (wsSet.size < 2) continue;
    const paneIds = projects.get(root)?.pane_ids ?? [];
    sharedProjects.push({
      root,
      workspace_ids: [...wsSet].sort(),
      pane_ids: paneIds,
      dirty: projects.get(root)?.dirty ?? false,
      managed: projects.get(root)?.managed ?? false,
      vcs: projects.get(root)?.vcs ?? null,
    });
  }
  sharedProjects.sort((a, b) => a.root.localeCompare(b.root));

  const tabsRaw = (snap["tabs"] as unknown[]) ?? [];
  const tabs = tabsRaw.map((t) => {
    const trec = (t ?? {}) as Record<string, unknown>;
    return { id: trec["tab_id"], workspace: trec["workspace_id"], label: trec["label"] };
  });

  const agentsRaw = (snap["agents"] as unknown[]) ?? [];
  const agentByPane = new Map<string, Record<string, unknown>>();
  for (const a of agentsRaw) {
    const rec = (a ?? {}) as Record<string, unknown>;
    if (typeof rec["pane_id"] === "string") agentByPane.set(rec["pane_id"] as string, rec);
  }

  const panesRaw = (snap["panes"] as unknown[]) ?? [];
  const panes = panesRaw.map((p) => {
    const prec = (p ?? {}) as Record<string, unknown>;
    const paneId = prec["pane_id"] as string;
    const arec = agentByPane.get(paneId);
    return {
      id: paneId,
      workspace: prec["workspace_id"],
      cwd: prec["cwd"] ?? prec["foreground_cwd"],
      agent: arec ? {
        name: arec["agent"], status: arec["agent_status"],
        terminal_title: arec["terminal_title"],
        state_change_seq: arec["state_change_seq"],
      } : null,
    };
  });

  const agentsAll = agentsRaw.map((a) => {
    const arec = (a ?? {}) as Record<string, unknown>;
    const sess = arec["agent_session"];
    return {
      name: arec["agent"],
      kind: arec["kind"] ?? arec["agent_kind"],
      pane: arec["pane_id"],
      status: arec["agent_status"] ?? arec["status"],
      workspace: arec["workspace_id"],
      cwd: arec["cwd"] ?? arec["foreground_cwd"],
      terminal_title: arec["terminal_title"],
      state_change_seq: arec["state_change_seq"],
      session_ref: typeof sess === "object" && sess !== null ? sess : null,
    };
  });
  const agents = filterVisibleAgents(agentsAll);
  const panesVisible = redactPaneAgents(panes);

  return {
    focused_workspace: snap["focused_workspace_id"],
    focused_pane: snap["focused_pane_id"],
    workspaces,
    tabs,
    panes: panesVisible,
    agents,
    ...visibilityMeta(agentsAll.length - agents.length),
    shared_projects: sharedProjects,
  };
}

// ---------------------------------------------------------------------------
// MCP result helpers
// ---------------------------------------------------------------------------
/** Wrap a plain JS value as a JSON text MCP tool result (matches Python's dict return). */
function toResult(data: unknown) {
  if (data && typeof data === "object" && (data as { ok?: unknown }).ok === false) {
    const d = data as Record<string, unknown>;
    const code = d["code"] ?? d["reason"] ?? d["failure"] ?? "-";
    const msg = String(d["message"] ?? d["hint"] ?? "").replace(/\s+/g, " ").slice(0, 160);
    console.log(
      `[herdr-mcp] ${new Date().toISOString()} tool_result ok=false code=${code}` +
        (d["context"] ? ` context=${d["context"]}` : "") +
        (msg ? ` message=${msg}` : ""),
    );
  }
  return {
    content: [{ type: "text" as const, text: JSON.stringify(data) }],
  };
}

/** P1-F: transparent transport / status-wait / control-plane errors for MCP clients. */
function herdrErrorResult(error: unknown, context?: string, failurePhase?: string) {
  const err = error instanceof HerdrError
    ? error
    : new HerdrError("unknown", error instanceof Error ? error.message : String(error));
  const detail = err.toDetail();
  const daemon = {
    socket_path: process.env.HERDR_SOCKET_PATH ?? `${process.env.HOME}/.config/herdr/herdr.sock`,
    mcp_pid: process.pid,
    build: buildInfo(),
  };
  const msg = detail.message || err.message;

  // Daemon waited for agent status after accept — not a socket transport failure.
  if (isAgentStatusWaitTimeout(msg)) {
    return toResult({
      ok: false,
      failure: "agent_status_wait_timeout",
      failure_phase: "post_submission_status_wait",
      submitted: "unknown",
      delivery_uncertain: true,
      context,
      ...detail,
      // Status-wait timeout after a mutation: never blind-retry
      retryable: false,
      hint: "submission may have succeeded — verify with herdr_inspect / herdr_since before re-sending",
      daemon,
    });
  }

  // Control-plane TaskGroup / ExceptionGroup (intermittent; agent/pane usually fine).
  if (isHerdrControlPlaneTaskGroup(msg)) {
    const rootMessage = unwrapControlPlaneMessage(msg);
    return toResult({
      ok: false,
      code: "snapshot_refresh_failed",
      failure: "herdr_internal",
      failure_phase: failurePhase ?? "control_plane_taskgroup",
      context,
      method: detail.method ?? context,
      request_id: `mcp-${process.pid}-${Date.now().toString(36)}`,
      retryable: true,
      message: rootMessage,
      error: {
        type: /ExceptionGroup/i.test(msg) ? "ExceptionGroup" : "TaskGroup",
        message: rootMessage,
        raw: msg.slice(0, 2000),
      },
      hint: "Shared herdr control-plane blip (snapshot/events/socket). Agent is usually still fine — retry the same read; do not blind-retry mutations.",
      daemon,
    });
  }

  return toResult({
    ok: false,
    failure: isTrueTransportFailure(detail.code, msg) ? "herdr_transport" : "herdr_error",
    context,
    ...detail,
    daemon,
  });
}

/**
 * Live session.snapshot with SnapshotCache fallback.
 * fs_* / inspect helpers must not surface bare ExceptionGroup when a cached
 * snapshot can still authorize managed roots.
 */
async function liveSnapshot(
  c: HerdrClient,
  context: string,
): Promise<
  | { ok: true; snap: HerdrResult; warnings: string[] }
  | { ok: false; result: ReturnType<typeof toResult> }
> {
  const cache = getSnapshotCache(c);
  await Promise.race([cache.whenReady(), new Promise<void>((r) => setTimeout(r, 800))]);
  const cached = cache.getSnapshot();
  const ws = cached["workspaces"];
  const panes = cached["panes"];
  const agents = cached["agents"];
  // Prefer any topology that can authorize managed roots — empty agent list is fine.
  const cacheUseful =
    (Array.isArray(ws) && ws.length > 0)
    || (Array.isArray(panes) && panes.length > 0)
    || (Array.isArray(agents) && agents.length > 0)
    || (Array.isArray(ws) && Array.isArray(panes)); // even empty arrays beat a hard TaskGroup
  try {
    const { snap, source } = await fetchSessionSnapshot(c);
    const warnings = source === "lists" ? ["snapshot_failed_used_list_apis"] : [];
    return { ok: true, snap, warnings };
  } catch (e) {
    if (cacheUseful) {
      return { ok: true, snap: cached, warnings: ["snapshot_refresh_failed_used_cache"] };
    }
    const assembled = await snapFromListApis(c);
    if (assembled) {
      return { ok: true, snap: assembled, warnings: ["snapshot_failed_used_list_apis"] };
    }
    return { ok: false, result: herdrErrorResult(e, context, "snapshot_refresh") };
  }
}

/**
 * Clean a herdr terminal read for human consumption (P1-K "clean" mode).
 * Moved to ./clean.ts (shared with the /push output snippet).
 */
export { cleanTerminalOutput } from "./clean.js";
/** Minimal agent state probe for herdr_prompt delivery evidence (never throws). */
async function agentStateOf(
  c: HerdrClient,
  target: string,
): Promise<{ pane_id: string | null; agent_status: string | null; state_change_seq: number | null } | null> {
  try {
    const r = await c.call("agent.get", { target }, 5000);
    const a = ((r["agent"] ?? r) ?? {}) as Record<string, unknown>;
    return {
      pane_id: typeof a["pane_id"] === "string" ? a["pane_id"] : null,
      agent_status: typeof a["agent_status"] === "string" ? a["agent_status"]
        : typeof a["status"] === "string" ? a["status"] : null,
      state_change_seq: typeof a["state_change_seq"] === "number" ? a["state_change_seq"] : null,
    };
  } catch {
    return null;
  }
}

/**
 * Resolve a herdr_diff `target` to the pane(s) to diff (mirrors wait.ts's
 * findAgent pane-resolution pattern):
 *  - pane_id (wH:p3)        -> that pane
 *  - agent name / ws:name   -> that agent's pane
 *  - workspace_id or label  -> all panes across that workspace
 */
function resolveDiffTargets(
  snap: HerdrResult,
  target: string,
): { kind: "single"; pane_ids: string[] } | { kind: "multi"; pane_ids: string[] } | {
  kind: "ambiguous"; candidates: { name: string | null; pane: string | null; workspace: string | null }[];
} | { kind: "not_found" } {
  // Entries from agents[] (they carry cwd/workspace_id), plus panes[] fallback.
  const entries: { pane: string | null; workspace: string | null; name: string | null; cwd: string | null }[] = [];
  const agentsRaw = (snap["agents"] as unknown[]) ?? [];
  for (const a of agentsRaw) {
    const rec = (a ?? {}) as Record<string, unknown>;
    entries.push({
      pane: typeof rec["pane_id"] === "string" ? (rec["pane_id"] as string) : null,
      workspace: typeof rec["workspace_id"] === "string" ? (rec["workspace_id"] as string) : null,
      name: typeof rec["agent"] === "string" ? (rec["agent"] as string) : null,
      cwd: typeof rec["cwd"] === "string" ? (rec["cwd"] as string)
        : typeof rec["foreground_cwd"] === "string" ? (rec["foreground_cwd"] as string) : null,
    });
  }
  const panesRaw = (snap["panes"] as unknown[]) ?? [];
  for (const p of panesRaw) {
    const rec = (p ?? {}) as Record<string, unknown>;
    const pid = typeof rec["pane_id"] === "string" ? (rec["pane_id"] as string) : null;
    if (!pid) continue;
    entries.push({
      pane: pid,
      workspace: typeof rec["workspace_id"] === "string" ? (rec["workspace_id"] as string) : null,
      name: null,
      cwd: typeof rec["cwd"] === "string" ? (rec["cwd"] as string)
        : typeof rec["foreground_cwd"] === "string" ? (rec["foreground_cwd"] as string) : null,
    });
  }

  // 1) pane_id direct match.
  const paneMatch = entries.find((e) => e.pane === target && e.cwd);
  if (paneMatch) return { kind: "single", pane_ids: [paneMatch.pane!] };


  // 2) workspace_id or label -> all panes in that workspace.
  const wsList = (snap["workspaces"] as unknown[]) ?? [];
  const wsMatch = wsList
    .map((w) => (w ?? {}) as Record<string, unknown>)
    .find((w) => w["workspace_id"] === target || w["label"] === target) ?? null;
  if (wsMatch) {
    const wsId = wsMatch["workspace_id"] as string;
    const paneIds = entries.filter((e) => e.workspace === wsId && e.cwd).map((e) => e.pane!).filter(Boolean);
    return { kind: paneIds.length > 1 ? "multi" : "single", pane_ids: paneIds };
  }

  // 3) agent name (optionally workspace:name).
  const scoped = target.includes(":");
  const sep = scoped ? target.indexOf(":") : -1;
  const wsPart = scoped ? target.slice(0, sep) : null;
  const namePart = scoped ? target.slice(sep + 1) : target;
  const byName = entries.filter((e) =>
    e.name === namePart && (!scoped || e.workspace === wsPart) && e.cwd,
  );
  if (byName.length === 1) return { kind: "single", pane_ids: [byName[0].pane!] };
  if (byName.length > 1) {
    return {
      kind: "ambiguous",
      candidates: byName.map((e) => ({ name: e.name, pane: e.pane, workspace: e.workspace })),
    };
  }
  return { kind: "not_found" };
}

// ---------------------------------------------------------------------------
function registerTools(server: McpServer): void {
  /** Remind web clients to read Herdr agent SKILL before pane/agent orchestration. */
  const SKILL_BEFORE_AGENT =
    "Before any agent operation (herdr_prompt or agent.* via herdr_call): call herdr_skill once per session first. ";

  server.registerTool(
    "herdr_methods",
    {
      description:
        SKILL_BEFORE_AGENT +
        "Discover herdr socket API methods and parameter schemas. " +
        "LIVE reflection from the installed herdr binary (herdr api schema, 60s cached). " +
        "Use this when you don't know the exact method or argument names (especially before agent.* calls).",
      inputSchema: {
        query: z.string().default("").describe("Optional case-insensitive filter: agent, pane.read, worktree, etc."),
      },
    },
    async ({ query }) => {
      try {
        const methods = listMethods(query);
        return toResult({ ok: true, count: methods.length, methods, source: "herdr api schema --json (live, 60s cache)" });
      } catch (e) {
        return toResult({ ok: false, reason: "schema_unavailable", message: e instanceof Error ? e.message : String(e) });
      }
    },
  );

  server.registerTool(
    "herdr_inspect",
    {
      description:
        "Check herdr connection and list workspaces (with cwd), tabs, panes, and agents in one call. " +
        "Typical session start: herdr_inspect → herdr_skill (read before any agent op) → work. " +
        "Also returns workstation_info: default_cwd hints, server/build, readonly/write_roots, " +
        "agent_skill pointer (call herdr_skill for latest upstream SKILL.md), " +
        "and a short exec_environment summary (PATH binaries relevant to local coding). " +
        "Agents come from the shared SnapshotCache (live events + 30s snapshot fallback) and carry " +
        "started_at + last_activity_at. If session.snapshot blips (TaskGroup), falls back to " +
        "workspace.list / pane.list / agent.list and sets warnings[] — do not treat that as a " +
        "repo blocker; keep using herdr_fs_* / herdr_git / herdr_exec. YOU (web) are the " +
        "planner/orchestrator. Prefer herdr_fs_* / herdr_exec / herdr_git before any herdr_prompt. " +
        "Agent lists soft-hide expensive kinds (Claude/OMP/Codex); only allowlisted workers " +
        "(pi, cline, opencode, anti) and auditors (droid, grok) appear — override with " +
        "HERDR_MCP_AGENT_ALLOW. herdr_prompt by known name/pane_id is NOT blocked. Prefer " +
        "explicit pane_id/workspace_id from this view.",
    },
    async () => {
      const c = clientGet();
      let pong: HerdrResult;
      try {
        pong = await c.ping();
      } catch (e) {
        return herdrErrorResult(e, "herdr_inspect", "ping");
      }
      const cache = getSnapshotCache(c);
      // Await the cache's first bootstrap (short cap) so an immediate inspect
      // doesn't read an empty state; fall back to a direct snapshot if the
      // cache can't bootstrap (daemon hiccup) — inspect never hangs / never throws.
      await Promise.race([cache.whenReady(), new Promise<void>((r) => setTimeout(r, 1500))]);
      const warnings: string[] = [];
      let snap = cache.getSnapshot();
      const hasAgents = Array.isArray(snap["agents"]) && (snap["agents"] as unknown[]).length > 0;
      const hasWs = Array.isArray(snap["workspaces"]) && (snap["workspaces"] as unknown[]).length > 0;
      const cacheThin = !hasAgents && !hasWs;
      if (cacheThin) {
        try {
          snap = await c.snapshot();
        } catch (e) {
          // Prefer any partial cache over a bare ExceptionGroup / TaskGroup to the client.
          const cached = cache.getSnapshot();
          const hasPartial = Array.isArray(cached["workspaces"]) || Array.isArray(cached["panes"])
            || Array.isArray(cached["agents"]);
          if (hasPartial) {
            snap = cached;
            warnings.push("snapshot_refresh_failed_used_cache");
          } else {
            const assembled = await snapFromListApis(c);
            if (assembled) {
              snap = assembled;
              warnings.push("snapshot_failed_used_list_apis");
            } else {
              return herdrErrorResult(e, "herdr_inspect", "snapshot_refresh");
            }
          }
        }
      }
      try {
        const view = projectSnapshot(snap);
        const enriched = filterVisibleAgents(cache.agentViews());
        const rawCount = ((snap["agents"] as unknown[]) ?? []).length;
        view["agents"] = enriched.length > 0 ? enriched : filterVisibleAgents(
          ((snap["agents"] as unknown[]) ?? []).map((a) => {
            const rec = (a ?? {}) as Record<string, unknown>;
            return {
              name: rec["agent"],
              kind: rec["kind"] ?? rec["agent_kind"],
              pane: rec["pane_id"],
              status: rec["agent_status"] ?? rec["status"],
              workspace: rec["workspace_id"],
              cwd: rec["cwd"] ?? rec["foreground_cwd"],
            };
          }),
        );
        Object.assign(view, visibilityMeta(Math.max(0, rawCount - (view["agents"] as unknown[]).length)));
        view["ok"] = true;
        view["herdr_version"] = pong["version"];
        view["protocol"] = pong["protocol"];
        view["build"] = buildInfo();
        const managed = managedRoots(snap);
        const which = (bin: string): string | null => {
          try {
            return execSync(`command -v ${bin}`, { timeout: 500, stdio: ["ignore", "pipe", "ignore"] })
              .toString().trim() || null;
          } catch { return null; }
        };
        view["workstation_info"] = {
          server_name: SERVER_NAME,
          server_version: SERVER_VERSION,
          boot_id: BOOT_ID,
          agent_skill: herdrSkillPointer(),
          default_cwd: typeof view["focused_pane"] === "string"
            ? ((view["panes"] as { id?: string; cwd?: string }[] | undefined)?.find((p) => p.id === view["focused_pane"])?.cwd
              ?? (view["workspaces"] as { focused?: boolean; cwd?: string }[] | undefined)?.find((w) => w.focused)?.cwd
              ?? null)
            : ((view["workspaces"] as { focused?: boolean; cwd?: string }[] | undefined)?.find((w) => w.focused)?.cwd ?? null),
          managed_git_roots: managed.sort(),
          readonly_mode: READONLY_MODE,
          write_roots: WRITE_ROOTS.length ? WRITE_ROOTS : null,
          agent_visibility: view["agent_visibility"],
          exec_sessions: listExecSessions(),
          exec_environment: {
            shell: process.env.SHELL ?? "/bin/zsh",
            node: process.version,
            path_has: {
              git: which("git"),
              rg: which("rg"),
              npm: which("npm"),
              python3: which("python3"),
            },
            hint: "Short sync shell: herdr_exec. Long jobs: herdr_exec_start → herdr_exec_read → herdr_exec_kill. Git facts: herdr_git. Patches: herdr_fs_patch.",
          },
        };
        if (warnings.length) view["warnings"] = warnings;
        if (cache.lastError) {
          view["cache_loop_error"] = cache.lastError.message;
        }
        return toResult(view);
      } catch (e) {
        return herdrErrorResult(e, "herdr_inspect", "project_snapshot");
      }
    },
  );

  server.registerTool(
    "herdr_skill",
    {
      description:
        "Read-only: fetch the latest Herdr agent SKILL.md from upstream (herdr master branch, " +
        "not version-pinned). Call ONCE per ChatGPT session BEFORE herdr_call or native " +
        "agent/pane orchestration — learn pane/agent IDs, lifecycle states, and CLI semantics. " +
        "Returns upstream herdr master when reachable; otherwise the bundled copy shipped " +
        "with herdr-mcp (origin field: network | cache | bundled). ChatGPT does not fetch " +
        "GitHub — only this server does. Cached ~1h; refresh=true bypasses cache; " +
        "HERDR_SKILL_NETWORK=0 skips network.",
      inputSchema: {
        refresh: z.boolean().default(false).describe("Bypass cache and re-fetch from upstream"),
      },
      annotations: { readOnlyHint: true },
    },
    async ({ refresh }) => {
      const result = await fetchHerdrSkill({ refresh });
      return toResult(result);
    },
  );

  server.registerTool(
    "herdr_call",
    {
      description:
        SKILL_BEFORE_AGENT +
        "Generic passthrough to the herdr socket API, VALIDATED against the live schema " +
        "(schema reflected from the installed herdr binary, 60s cache). Params are checked before " +
        "sending: missing required / wrong type / wrong enum -> invalid_params error (no socket " +
        "call); unknown params -> warnings. For unknown methods use herdr_methods; for agent.* " +
        "methods (agent.start, agent.prompt, pane.split, …) herdr_skill is mandatory once per session. " +
        "prefer explicit pane_id/workspace_id over bare names. " +
        "For agent.prompt prefer herdr_prompt (fire-and-forget + idempotency_key + delivery " +
        "evidence); do not pass wait on mutations unless you intentionally want submit+wait. " +
        "Never blind-retry a mutating call after failure — delivery may be uncertain; verify " +
        "with herdr_inspect / herdr_since first. Status-wait timeouts are failure " +
        "agent_status_wait_timeout (not herdr_transport).",
      inputSchema: {
        method: z.string().describe(
          "herdr socket method, e.g. pane.split, agent.start — read herdr_skill before agent.*",
        ),
        // ChatGPT/OpenAI rejects Zod's z.record → propertyNames + additionalProperties:{}.
        // Accept object or JSON string; advertise a plain string schema.
        params: z
          .preprocess((v) => {
            if (v === undefined || v === null) return undefined;
            if (typeof v === "object") return JSON.stringify(v);
            return v;
          }, z.string().optional())
          .describe("Method arguments as a JSON object string; omit for {}"),
      },
    },
    async ({ method, params }) => {
      if (READONLY_MODE && NON_IDEMPOTENT_METHODS.has(method)) {
        return toResult({ ok: false, reason: "readonly_mode", method,
          hint: "HERDR_MCP_READONLY=1 blocks side-effecting methods; read-only methods still pass" });
      }
      const c = clientGet();
      let given: Record<string, unknown> = {};
      if (typeof params === "string" && params.trim() !== "") {
        try {
          const parsed = JSON.parse(params) as unknown;
          if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
            given = parsed as Record<string, unknown>;
          } else {
            return toResult({ ok: false, code: "invalid_params", method, errors: ["params must be a JSON object"] });
          }
        } catch {
          return toResult({ ok: false, code: "invalid_params", method, errors: ["params is not valid JSON"] });
        }
      }
      // A-1: validate against the live schema BEFORE touching the socket.
      const v = validateMethodParams(method, given);
      if (!v.ok) {
        return toResult({ ok: false, code: "invalid_params", method, errors: v.errors, warnings: v.warnings });
      }
      try {
        const result = await c.call(method, given);
        return toResult({ ok: true, result, ...(v.warnings.length ? { warnings: v.warnings } : {}) });
      } catch (e) {
        return herdrErrorResult(e, `herdr_call:${method}`);
      }
    },
  );

  if (ALL_TOOLS) {
  server.registerTool(
    "herdr_wait",
    {
      description:
        SKILL_BEFORE_AGENT +
        "Block until a herdr agent reaches a settled lifecycle state, then return a read summary. " +
        "target: pane_id (e.g. wH:p1, unambiguous) or agent name (e.g. omp, must be unique). " +
        "If the bare name is ambiguous across workspaces, returns ambiguous_target with candidates; " +
        "re-call using a pane_id or workspace:name (e.g. wH:omp) to disambiguate.",
      inputSchema: {
        target: z.string().describe("pane_id (wH:p1, unambiguous) or agent name (must be unique; if ambiguous use pane_id or workspace:name)"),
        until: z.array(z.string()).default(["idle", "blocked", "done"]).describe("Settled states"),
        timeout_ms: z.number().default(60000).describe("Max time to wait in ms (capped at 60s)"),
      },
    },
    async ({ target, until, timeout_ms }) => {
      const c = clientGet();
      // Read agent state from the shared SnapshotCache (same source as herdr_inspect)
      // so wait and inspect can never disagree about a pane's status.
      const cache = getSnapshotCache(c);
      const result = await waitForAgent(
        c, target, [...until], clampHerdrTimeout(timeout_ms) / 1000,
        () => cache.getSnapshot(),
      );
      return toResult(result);
    },
  );

  const taskSessionSchema = {
    label: z.string().describe("Stable task label"),
    cwd: z.string().default("").describe("Working directory"),
    resume: z.boolean().default(true).describe("Resume existing task if present"),
  };
  const taskSessionHandler = async ({ label, cwd, resume }: { label: string; cwd: string; resume: boolean }) => {
      const c = clientGet();
      const existing = resume ? sessionGet(label) : null;
      if (existing && existing.workspace_id) {
        const wsId = existing.workspace_id;
        const snap = await c.snapshot();
        const agentsRaw = (snap["agents"] as unknown[]) ?? [];
        const agents = agentsRaw
          .filter((a) => (a as Record<string, unknown>)["workspace_id"] === wsId)
          .map((a) => {
            const rec = a as Record<string, unknown>;
            return { name: rec["agent"], pane: rec["pane_id"], status: rec["agent_status"] };
          });
        // L-5: return the stored snapshot AND the live current projects.
        const currentProjects = projectsForWorkspace(snap, wsId);
        return toResult({
          ok: true,
          session: label,
          workspace_id: wsId,
          cwd: existing.default_cwd,
          agents,
          handoff: existing.handoff,
          projects: existing.projects ?? [],
          current_projects: currentProjects,
          resumed: true,
        });
      }
      try {
        const r = await c.call("workspace.create", { cwd: cwd || null, label });
        const ws = (r["workspace"] as Record<string, unknown>) ?? r;
        const wsId = ws["workspace_id"] as SessionData["workspace_id"];
        const rootPane = ((r["root_pane"] as Record<string, unknown>) ?? {})["pane_id"];
        // L-5: snapshot the project roots that exist right after creation (with
        // a brief poll + fallback so the snapshot is never silently empty).
        const projSnap = await snapshotProjectsForCreate(c, wsId as string, cwd);
        sessionSave(label, { workspace_id: wsId, default_cwd: cwd, root_pane: rootPane as string | undefined, created_at: Date.now() / 1000, projects: projSnap });
        return toResult({
          ok: true,
          session: label,
          workspace_id: wsId,
          root_pane: rootPane,
          cwd,
          agents: [],
          handoff: null,
          projects: projSnap,
          resumed: false,
        });
      } catch (e) {
        const err = e instanceof HerdrError ? e : new HerdrError("error", String(e));
        return toResult({ ok: false, code: err.code, message: err.message });
      }
  };
  server.registerTool(
    "herdr_task",
    {
      description:
        "Call before workspace-creating work. Read-only operations (herdr_inspect, herdr_call " +
        "queries, herdr_wait) do NOT require a task. Creates or resumes a herdr workspace bound " +
        "to a stable task label (e.g. \"fix-auth-bug\"). Do NOT use conversation IDs. " +
        "(Renamed from herdr_session: 'session' collided with herdr server namespaces and " +
        "agent conversation transcripts.)",
      inputSchema: taskSessionSchema,
    },
    taskSessionHandler,
  );
  server.registerTool(
    "herdr_session",
    {
      description: "DEPRECATED alias of herdr_task (removed next version) — use herdr_task.",
      inputSchema: taskSessionSchema,
    },
    taskSessionHandler,
  );

  const taskHandoffSchema = {
    session: z.string().describe("Task label (as passed to herdr_task)"),
    summary: z.string().describe("What's done"),
    pending: z.array(z.string()).default([]).describe("What's pending"),
    decisions: z.array(z.string()).default([]).describe("Key decisions"),
  };
  const taskHandoffHandler = async ({ session, summary, pending, decisions }: { session: string; summary: string; pending: string[]; decisions: string[] }) => {
    const existing = sessionGet(session);
    const sess: Partial<SessionData> = existing ?? {};
    sess.handoff = {
      summary,
      pending: [...pending],
      decisions: [...decisions],
      saved_at: Date.now() / 1000,
    };
    if (!sess.workspace_id) {
      return toResult({
        ok: false,
        reason: "task_not_found",
        session,
        hint: "call herdr_task first to create the workspace",
      });
    }
    sessionSave(session, sess);
    return toResult({
      ok: true,
      session,
      saved: true,
      hint: `new conversation: call herdr_task("${session}", resume=true)`,
    });
  };
  server.registerTool(
    "herdr_task_handoff",
    {
      description:
        "Save a handoff note BEFORE your context fills up or the conversation ends. The next " +
        "conversation calls herdr_task(\"same-label\", resume=true) and gets this note + live " +
        "agent states. (Renamed from herdr_handoff.)",
      inputSchema: taskHandoffSchema,
    },
    taskHandoffHandler,
  );
  server.registerTool(
    "herdr_handoff",
    {
      description: "DEPRECATED alias of herdr_task_handoff (removed next version) — use herdr_task_handoff.",
      inputSchema: taskHandoffSchema,
    },
    taskHandoffHandler,
  );

  server.registerTool(
    "herdr_parallel",
    {
      description:
        SKILL_BEFORE_AGENT +
        "Start N agents in N panes within a session's workspace — true parallelism. Each agent: {kind, name, prompt, cwd?}. cwd defaults to session.default_cwd; pass a different cwd for cross-project work (orchestrator in A, implementer in B). Returns manifest with pane_ids/cwds; call herdr_wait on each.",
      inputSchema: {
        session: z.string().describe("Session label"),
        agents: z
          .array(
            z.object({
              kind: z.string().default("pi"),
              name: z.string(),
              prompt: z.string().default(""),
              cwd: z.string().optional().describe("Working directory for this agent's pane; defaults to session.default_cwd"),
            }),
          )
          .describe("Agents to start"),
      },
    },
    async ({ session, agents }) => {
      const c = clientGet();
      const sess = sessionGet(session);
      if (!sess) {
        return toResult({
          ok: false,
          reason: "session_not_found",
          session,
          hint: "call herdr_session first",
        });
      }
      // NOTE (fix over Python): server.py's herdr_parallel falls through its
      // for-loop and returns None. This TS version returns a proper manifest
      // ({ok, session, manifest}), matching the tool's own docstring. The tool
      // handlers return a JSON value either way, so callers are unaffected.
      const wsId = sess.workspace_id;
      const rootPane = sess.root_pane as string | undefined;
      const manifest: unknown[] = [];
      for (let i = 0; i < agents.length; i++) {
        const ag = agents[i];
        const kind = ag.kind ?? "pi";
        const name = ag.name ?? `agent-${i}`;
        const prompt = ag.prompt ?? "";
        const agentCwd = ag.cwd ?? sess.default_cwd ?? null;
        try {
          let paneId: string | undefined;
          if (i === 0 && rootPane) {
            paneId = rootPane;
          } else {
            const split = await c.call("pane.split", {
              pane_id: rootPane ?? undefined,
              direction: "right",
              cwd: agentCwd,
            });
            const pane = (split["pane"] as Record<string, unknown>) ?? split;
            paneId = pane["pane_id"] as string;
          }
          await new Promise((r) => setTimeout(r, 500)); // let the shell pane initialize
          await c.call("agent.start", { name, kind, pane_id: paneId, timeout_ms: 15000 });
          if (prompt) {
            await c.call("agent.prompt", { target: paneId, text: prompt, wait: null });
          }
          manifest.push({ name, pane: paneId, kind, cwd: agentCwd, status: "working" });
        } catch (e) {
          const err = e instanceof HerdrError ? e : new HerdrError("error", String(e));
          manifest.push({ name, error: err.code, message: err.message });
        }
      }
      return toResult({ ok: true, session, manifest });
    },
  );

  const taskReapSchema = {
    session: z.string().describe("Task label (as passed to herdr_task)"),
    close_workspace: z.boolean().default(true).describe("Close the workspace after collecting"),
    force: z.boolean().default(false).describe("Legacy bypass (single-project workspace only)"),
    force_projects: z.array(z.string()).default([]).describe("Project roots to close (selection confirmation); dirty roots among these STILL need confirm_dirty"),
    confirm_dirty: z.boolean().default(false).describe("Acknowledge closing roots that have uncommitted git changes (separate from project selection)"),
  };
  const taskReapHandler = async ({ session, close_workspace, force, force_projects, confirm_dirty }: { session: string; close_workspace: boolean; force: boolean; force_projects: string[]; confirm_dirty: boolean }) => {
      const c = clientGet();
      const sess = sessionGet(session);
      if (!sess) {
        return toResult({ ok: false, reason: "session_not_found", session });
      }
      const wsId = sess.workspace_id;
      if (!wsId) {
        return toResult({ ok: false, reason: "no_workspace", session, hint: "call herdr_session first" });
      }
      const snap = await c.snapshot();

      // ---- L-4/L-5 project gate: current projects vs stored snapshot ----
      const currentProjects = projectsForWorkspace(snap, wsId);
      const storedRoots = new Set<string>((sess.projects ?? []).map((p) => p.root));
      const gate = currentProjects.map((p) => ({
        root: p.root,
        panes: p.pane_ids,
        matches_session: storedRoots.has(p.root),
        dirty: p.dirty ?? false,
        changed_files: p.changed_files ?? 0,
      }));
      const hasNewProject = gate.some((p) => !p.matches_session);

      // ---- digest: collect each workspace agent's final output ----
      const digest: unknown[] = [];
      const agentsRaw = (snap["agents"] as unknown[]) ?? [];
      for (const a of agentsRaw) {
        const rec = a as Record<string, unknown>;
        if (rec["workspace_id"] !== wsId) continue;
        const paneId = rec["pane_id"] as string;
        const status = rec["agent_status"];
        let snippet = "";
        try {
          const r = await c.call("agent.read", {
            target: paneId,
            source: "recent_unwrapped",
            lines: 40,
            strip_ansi: true,
          });
          const rd = (r["read"] as Record<string, unknown>) ?? r;
          const text = (rd["content"] ?? rd["text"] ?? "") as string;
          snippet = text.slice(0, 500);
        } catch (e) {
          // read error -> empty snippet
          void e;
        }
        digest.push({ name: rec["agent"], pane: paneId, status, output: snippet });
      }

      // safety #3/#5: we can only ever close panes we can derive a project for in THIS workspace.
      const knownPaneIds = new Set<string>(currentProjects.flatMap((p) => p.pane_ids));
      // Every workspace pane id (that we can see) — used to guard whole-workspace close.
      const wsPaneIds = new Set<string>();
      const panesRaw = (snap["panes"] as unknown[]) ?? [];
      for (const p of panesRaw) {
        const rec = (p ?? {}) as Record<string, unknown>;
        if (rec["workspace_id"] !== wsId) continue;
        if (typeof rec["pane_id"] === "string") wsPaneIds.add(rec["pane_id"] as string);
      }
      for (const a of agentsRaw) {
        const rec = a as Record<string, unknown>;
        if (rec["workspace_id"] !== wsId) continue;
        if (typeof rec["pane_id"] === "string") wsPaneIds.add(rec["pane_id"] as string);
      }
      const unclassifiedPaneIds = [...wsPaneIds].filter((p) => !knownPaneIds.has(p));

      // ---- force_projects validation (safety #3): unknown roots are an error, never silent ----
      const requestedRoots = (force_projects ?? []).filter((r) => typeof r === "string" && r.length > 0);
      const currentRoots = new Set(currentProjects.map((p) => p.root));
      const unknownRoots = requestedRoots.filter((r) => !currentRoots.has(r));
      if (requestedRoots.length > 0 && unknownRoots.length > 0) {
        return toResult({
          ok: false,
          reason: "unknown_force_projects",
          session,
          unknown_projects: unknownRoots,
          projects: gate,
          hint: "listed project roots are not currently present in this workspace",
        });
      }

      // ---- decide close action (force_projects takes precedence unconditionally) ----
  let closeAction: { mode: "whole" } | { mode: "panes"; panes: string[] } | { mode: "none" } = { mode: "none" };
    if (close_workspace) {
      if (requestedRoots.length > 0) {
          // force_projects = SELECTION confirmation only (satisfies the
          // multi-project gate). It does NOT satisfy the dirty-workspace
          // confirmation: naming a root says "close THIS project", not "I
          // know it has N uncommitted files". A dirty root listed here still
          // requires confirm_dirty:true (P0-1).
          const rootSet = new Set(requestedRoots);
          const dirtyRequested = currentProjects.filter((p) => rootSet.has(p.root) && (p.dirty ?? false));
          if (dirtyRequested.length > 0 && !confirm_dirty) {
            return toResult({
              ok: false,
              reason: "dirty_projects",
              session,
              dirty_projects: dirtyRequested.map((p) => ({ root: p.root, panes: p.pane_ids, changed_files: p.changed_files ?? 0 })),
              projects: gate,
              hint: "force_projects selects which roots to close, but dirty roots still need confirm_dirty:true",
            });
          }
          const panes = currentProjects.filter((p) => rootSet.has(p.root)).flatMap((p) => p.pane_ids);
          if (panes.length === 0) {
            return toResult({
              ok: false,
              reason: "no_panes_for_force_projects",
              session,
              projects: gate,
              hint: "listed project roots have no panes in this workspace",
            });
          }
          closeAction = { mode: "panes", panes };
        } else if (gate.length > 1) {
          // P0-CRIT-2: a multi-project workspace can NEVER be whole-closed by default,
          // and legacy force=true is NOT allowed — the caller must pick roots explicitly.
          return toResult({
            ok: false,
            reason: "multi_project_confirmation_required",
            session,
            projects: gate,
            hint: "use force_projects=[root,...] to close selected projects",
          });
        } else {
          // Single-project workspace. proj is the one current project.
          const proj = currentProjects[0];
          if (force) {
            // Legacy force=true confirms the single project (its dirtiness is OK).
            closeAction = { mode: "panes", panes: proj?.pane_ids ?? [] };
          } else if (proj && (proj.dirty ?? false)) {
            // P0-CRIT-3: single dirty project, not explicitly confirmed -> refuse.
            return toResult({
              ok: false,
              reason: "dirty_projects",
              session,
              dirty_projects: [{ root: proj.root, panes: proj.pane_ids, changed_files: proj.changed_files ?? 0 }],
              projects: gate,
              hint: "explicitly list dirty roots in force_projects to close them",
            });
          } else if (hasNewProject) {
            // new project present and no force: refuse to close anything.
            return toResult({
              ok: false,
              reason: "project_mismatch",
              session,
              projects: gate,
              hint: "new projects appeared since session creation — use force_projects to selectively close",
            });
          } else {
            // gate passes and no force: whole-workspace close, but ONLY if every visible
            // workspace pane is classified (safety #5).
            if (unclassifiedPaneIds.length > 0) {
              return toResult({
                ok: false,
                reason: "unclassified_panes",
                session,
                unclassified_panes: unclassifiedPaneIds,
                projects: gate,
                hint: "some workspace panes have no derivable project — refuse whole-workspace close",
              });
            }
            closeAction = { mode: "whole" };
          }
        }
      }

      // ---- execute close (safety #4: track per-pane failures accurately) ----
      const closedPanes: string[] = [];
      const closeErrors: { pane: string; code: string }[] = [];
      let wholeWorkspaceClosed = false;
      let closeError: string | undefined;
      if (closeAction.mode !== "none") {
        try {
          if (closeAction.mode === "whole") {
            await c.call("workspace.close", { workspace_id: wsId });
            wholeWorkspaceClosed = true;
          } else {
            for (const paneId of closeAction.panes) {
              try {
                await c.call("pane.close", { pane_id: paneId });
                closedPanes.push(paneId);
              } catch (e) {
                const perr = e instanceof HerdrError ? e : new HerdrError("error", String(e));
                closeErrors.push({ pane: paneId, code: perr.code });
              }
            }
          }
        } catch (e) {
          const err = e instanceof HerdrError ? e : new HerdrError("error", String(e));
          closeError = err.code;
        }
      }
      const closed = closeAction.mode === "none"
        ? false
        : closeAction.mode === "whole"
          ? wholeWorkspaceClosed
          : closeErrors.length === 0 && (closeAction.panes.length === 0 || closedPanes.length === closeAction.panes.length);

      return toResult({
        ok: true,
        session,
        workspace_id: wsId,
        digest,
        projects: gate,
        closed,
        ...(closeAction.mode === "panes" ? { closed_panes: closedPanes } : {}),
        ...(closeErrors.length ? { close_errors: closeErrors } : {}),
        ...(closeError ? { close_error: closeError } : {}),
      });
  };
  server.registerTool(
    "herdr_task_reap",
    {
      description:
        "Reap a task: collect each agent's final status + output, then close its workspace. " +
        "Safety gates: dirty_projects (uncommitted changes in a to-be-closed project require " +
        "force_projects), multi_project_confirmation_required (heterogeneous workspaces must " +
        "pick roots via force_projects, whether heterogeneous at creation or after), and the " +
        "creation-time project snapshot. (Renamed from herdr_reap.)",
      inputSchema: taskReapSchema,
    },
    taskReapHandler,
  );
  server.registerTool(
    "herdr_reap",
    {
      description: "DEPRECATED alias of herdr_task_reap (removed next version) — use herdr_task_reap.",
      inputSchema: taskReapSchema,
    },
    taskReapHandler,
  );

  server.registerTool(
    "herdr_read",
    {
      description:
        SKILL_BEFORE_AGENT +
        "Read a herdr agent's recent output WITHOUT waiting for it to settle. " +
        "Wraps the herdr 'agent.read' socket call (recent_unwrapped, last N lines). " +
        "mode='clean' (default) strips ANSI/spinner/status-bar chrome; soft-wrap " +
        "unwrapping is owned by herdr's recent_unwrapped source (no client-side re-joining); " +
        "mode='raw' returns the terminal output as-is. " +
        "Use this to peek at what an agent is doing right now, or to collect output " +
        "after herdr_wait returns a read_error.",
      inputSchema: {
        target: z.string().describe("Agent name or pane_id to read"),
        lines: z.number().default(120).describe("Approx lines of recent output to return"),
        mode: z.enum(["raw", "clean"]).default("clean").describe("clean (default): stripped/joined text; raw: terminal output as-is"),
      },
    },
    async ({ target, lines, mode }) => {
      const c = clientGet();
      try {
        const r = await c.call(
          "agent.read",
          { target, source: "recent_unwrapped", lines, strip_ansi: true },
          10000,
        );
        const rd = (r["read"] as Record<string, unknown>) ?? r;
        let text = (rd["content"] ?? rd["text"] ?? rd["output"] ?? "") as string;
        if (mode === "clean") {
          text = cleanTerminalOutput(text);
        }
        return toResult({ ok: true, pane_id: rd["pane_id"], mode: mode ?? "clean", output: text });
      } catch (e) {
        const err = e instanceof HerdrError ? e : new HerdrError("error", String(e));
        return toResult({ ok: false, code: err.code, message: err.message });
      }
    },
  );

  server.registerTool(
    "herdr_explain",
    {
      description:
        SKILL_BEFORE_AGENT +
        "Wrap agent.explain — structured verdict for WHY an agent shows its current " +
        "state: final status, manifest source/version, matched rules with evidence, " +
        "skip reasons, screen_detection_skip_reason. Use to settle state disagreements " +
        "(e.g. inspect shows working while wait reports done) instead of guessing; also " +
        "the first check after a herdr_prompt timeout before re-sending.",
      inputSchema: {
        target: z.string().describe("Agent name or pane_id"),
      },
    },
    async ({ target }) => {
      const c = clientGet();
      try {
        const r = await c.call("agent.explain", { target }, 10000);
        return toResult({ ok: true, target, explain: r["explain"] ?? r });
      } catch (e) {
        const err = e instanceof HerdrError ? e : new HerdrError("error", String(e));
        return toResult({ ok: false, target, code: err.code, message: err.message });
      }
    },
  );
  }

  server.registerTool(
    "herdr_since",
    {
      description:
        "Incremental digest since a cursor — cheap conversation-resume primitive (❺). " +
        "When polling agents after herdr_prompt, herdr_skill should already have been read this session. " +
        "MCP clients only run when the user sends a message, so polling is not an option; " +
        "pass the cursor from your last call to get only NEW events. Returns: events[] " +
        "(pane/workspace/tab changes with cursor+at), current agents[] (status/started_at/" +
        "last_activity_at/cwd), workspaces[], and a new cursor. First call (cursor=0) returns " +
        "the recent tail. The server keeps a live events.subscribe stream (A-2) so this is a " +
        "single round-trip instead of inspect+read+explain. Events/agents carry explicit " +
        "pane_id/workspace_id — prefer those IDs over labels when addressing targets later.",
      inputSchema: {
        cursor: z.number().int().min(0).default(0).describe("Cursor from a previous herdr_since (0 = first call)"),
        workspace: z.string().optional().describe("Optional: filter events + agents to this workspace_id or label"),
      },
    },
    async ({ cursor, workspace }) => {
      const c = clientGet();
      try {
        const cache = getSnapshotCache(c);
        await Promise.race([cache.whenReady(), new Promise<void>((r) => setTimeout(r, 2000))]);
        const dig = cache.digestSince(cursor);
        let events = dig.events;
        let agents = dig.agents;
        let workspaces = dig.workspaces;
        const warnings: string[] = [];
        // Live digest empty after reconnect blip → one direct snapshot for agents, keep events from cache.
        if (agents.length === 0) {
          try {
            const snap = await c.snapshot();
            const snapAgents = ((snap["agents"] as unknown[]) ?? []) as Record<string, unknown>[];
            if (snapAgents.length > 0) {
              agents = snapAgents.map((rec) => ({
                name: typeof rec["agent"] === "string" ? rec["agent"] : null,
                pane: typeof rec["pane_id"] === "string" ? rec["pane_id"] : null,
                status: typeof rec["agent_status"] === "string" ? rec["agent_status"]
                  : typeof rec["status"] === "string" ? rec["status"] : null,
                workspace: typeof rec["workspace_id"] === "string" ? rec["workspace_id"] : null,
                cwd: typeof rec["cwd"] === "string" ? rec["cwd"] : null,
                started_at: null as string | null,
                last_activity_at: null as string | null,
                state_change_seq: rec["state_change_seq"],
              }));
              warnings.push("since_used_snapshot_fallback");
            }
            if (workspaces.length === 0 && Array.isArray(snap["workspaces"])) {
              workspaces = snap["workspaces"] as Record<string, unknown>[];
            }
          } catch (e) {
            if (isHerdrControlPlaneTaskGroup(e instanceof Error ? e.message : String(e))) {
              warnings.push("since_snapshot_fallback_taskgroup");
            } else {
              return herdrErrorResult(e, "herdr_since", "snapshot_fallback");
            }
          }
        }
        if (workspace) {
          const ids = new Set<string>();
          for (const w of workspaces) {
            const rec = w as Record<string, unknown>;
            if (rec["workspace_id"] === workspace || rec["label"] === workspace) {
              if (typeof rec["workspace_id"] === "string") ids.add(rec["workspace_id"] as string);
            }
          }
          if (ids.size === 0) ids.add(workspace);
          events = events.filter((e: { workspace_id?: string; pane_id?: string }) =>
            (e.workspace_id && ids.has(e.workspace_id)) || false);
          agents = agents.filter((a: { workspace?: string | null }) => a.workspace && ids.has(a.workspace));
        }
        const agentsBeforeHide = agents.length;
        agents = filterVisibleAgents(agents);
        const cursorReset = cursor > dig.cursor;
        if (cursorReset) warnings.push("cursor_reset_boot_or_rollover");
        return toResult({
          ok: true,
          boot_id: BOOT_ID,
          cursor: dig.cursor,
          cursor_reset: cursorReset,
          event_count: events.length,
          events,
          agents,
          workspaces: workspaces.map((w) => {
            const r = w as Record<string, unknown>;
            return { workspace_id: r["workspace_id"], label: r["label"], cwd: r["cwd"], panes: r["pane_count"], tabs: r["tab_count"] };
          }),
          ...visibilityMeta(agentsBeforeHide - agents.length),
          ...(warnings.length ? { warnings } : {}),
          hint: "save boot_id+cursor; if boot_id changes or cursor_reset=true, start from cursor 0",
        });
      } catch (e) {
        return herdrErrorResult(e, "herdr_since", "digest");
      }
    },
  );

  // -------------------------------------------------------------------------
  // Remote workstation layer (D) — the client is remote; herdr-mcp is its only
  // path to these files/terminals. Authorization gates (E) apply to mutations.
  // -------------------------------------------------------------------------

  /** E: gate for mutating workstation operations. null = allowed. */
  function mutationDenied(root: string | null, action: string): Record<string, unknown> | null {
    if (READONLY_MODE) {
      return { ok: false, reason: "readonly_mode", action,
        hint: "HERDR_MCP_READONLY=1 — all mutating operations are disabled" };
    }
    if (root && WRITE_ROOTS.length > 0) {
 const normalized = WRITE_ROOTS.map((w) => w.replace(/\/+$/, ""));
      const allowed = normalized.some((w) => root === w || root.startsWith(w + "/"));
      if (!allowed) {
        return { ok: false, reason: "root_not_whitelisted", action, root, write_roots: normalized,
          hint: "add this root to HERDR_MCP_WRITE_ROOTS, or unset it to allow all managed roots" };
      }
    }
    return null;
  }

  const SECRET_BASENAME_RE = /\.(env|pem|key|p12|pfx)$/i;
  const SECRET_PATH_RE = /(^|\/)(\.env[^/]*|id_(rsa|dsa|ecdsa|ed25519)[^/]*|[^/]*secret[^/]*|[^/]*token[^/]*|[^/]*credential[^/]*)$/i;
  function deniedSecretPath(p: string): boolean {
    if (p.endsWith("/.git/config")) return true;
    return SECRET_BASENAME_RE.test(path.basename(p)) || SECRET_PATH_RE.test(p);
  }

  function managedRoots(snap: HerdrResult): string[] {
    const roots: string[] = [];
    for (const [root, proj] of deriveProjects(snap)) {
      if (proj.managed && proj.vcs === "git") roots.push(root);
    }
    return roots;
  }
  function containingRoot(roots: string[], p: string): string | null {
    for (const root of roots) {
      if (p === root || p.startsWith(root.endsWith("/") ? root : root + "/")) return root;
    }
    return null;
  }

  /** Validate a path for fs tools: managed root + realpath containment + secret deny. */
  async function validateManagedFile(
    snap: HerdrResult, input: string, mustExist: boolean,
  ): Promise<{ ok: true; root: string; resolved: string; real: string } | { ok: false; err: Record<string, unknown> }> {
    const resolved = path.resolve(input);
    const roots = managedRoots(snap);
    const root = containingRoot(roots, resolved);
    if (!root) {
      return { ok: false, err: { ok: false, reason: "outside_managed_roots", path: resolved,
        managed_roots: roots.sort(),
        hint: "only paths inside git-backed project roots visible in the live snapshot are accessible" } };
    }
    if (deniedSecretPath(resolved)) {
      return { ok: false, err: { ok: false, reason: "secret_path_denied", path: resolved } };
    }
    let real: string;
    try {
      real = await realpath(resolved);
    } catch (e) {
      if (mustExist) return { ok: false, err: { ok: false, reason: "not_found", path: resolved, message: String(e) } };
      // new file: validate the parent instead
      try { real = path.resolve(await realpath(path.dirname(resolved)), path.basename(resolved)); }
      catch (e2) { return { ok: false, err: { ok: false, reason: "parent_not_found", path: resolved, message: String(e2) } }; }
    }
    if (containingRoot(roots, real) !== root) {
      return { ok: false, err: { ok: false, reason: "symlink_escape", path: resolved, real } };
    }
    return { ok: true, root, resolved, real };
  }

  /**
   * herdr_git only: when snapshot/managed-roots gate is unavailable during a
   * control-plane TaskGroup storm, still allow local git if the path is a real
   * repo under $HOME (or HERDR_MCP_WRITE_ROOTS when set).
   */
  async function validateGitRootLocalFallback(
    input: string,
  ): Promise<{ ok: true; root: string; resolved: string; real: string } | { ok: false; err: Record<string, unknown> }> {
    const resolved = path.resolve(input);
    if (deniedSecretPath(resolved)) {
      return { ok: false, err: { ok: false, reason: "secret_path_denied", path: resolved } };
    }
    let real: string;
    try {
      real = await realpath(resolved);
    } catch (e) {
      return { ok: false, err: { ok: false, reason: "not_found", path: resolved, message: String(e) } };
    }
    const top = gitToplevel(real);
    if (!top) {
      return { ok: false, err: { ok: false, reason: "not_a_git_repo", path: resolved } };
    }
    const home = process.env.HOME;
    const bases = WRITE_ROOTS.length > 0
      ? WRITE_ROOTS.map((w) => w.replace(/\/+$/, ""))
      : (home ? [home] : []);
    if (bases.length === 0 || !containingRoot(bases, top)) {
      return {
        ok: false,
        err: {
          ok: false,
          reason: "git_fallback_outside_home",
          path: resolved,
          toplevel: top,
          hint: "git local fallback only allows repos under $HOME (or HERDR_MCP_WRITE_ROOTS)",
        },
      };
    }
    return { ok: true, root: top, resolved, real: top };
  }

  function workingAgentsForRoot(snap: HerdrResult, root: string): { pane: string; agent: string | null }[] {
    const proj = deriveProjects(snap).get(root);
    const paneSet = new Set(proj?.pane_ids ?? []);
    const out: { pane: string; agent: string | null }[] = [];
    for (const a of (snap["agents"] as unknown[]) ?? []) {
      const rec = (a ?? {}) as Record<string, unknown>;
      if (rec["agent_status"] !== "working") continue;
      const pane = rec["pane_id"];
      if (typeof pane === "string" && paneSet.has(pane)) {
        out.push({ pane, agent: typeof rec["agent"] === "string" ? rec["agent"] : null });
      }
    }
    return out;
  }

  function fileDirty(root: string, file: string): boolean {
    try {
      const out = execSync(`git status --porcelain -- ${JSON.stringify(path.relative(root, file))}`,
        { cwd: root, timeout: 500, stdio: ["ignore", "pipe", "ignore"] }).toString();
      return out.trim().length > 0;
    } catch {
      return false;
    }
  }

  server.registerTool(
    "herdr_fs_read",
    {
      description:
        "PREFERRED way to read project source on the workstation. Reads a file from a MANAGED " +
        "(git) project root (remote-workstation layer — the client cannot reach these files " +
        "otherwise). Do not use herdr_prompt / omp / agent.read for ordinary file IO. Gates: " +
        "path must sit inside a git-backed project root from the live snapshot ($HOME / non-git " +
        "roots refused); secret-ish files (.env*, *.pem, id_rsa*, *.key, .git/config, " +
        "*secret*/*token*/*credential*) denied; budget defaults to 200 lines / 16KB — raise " +
        "explicitly (cap 256KB). Diff-sized reads are cheap; whole-source reads are not.",
      inputSchema: {
        path: z.string().describe("Absolute file path inside a managed project root"),
        start_line: z.number().int().min(1).optional().describe("1-based first line (default 1)"),
        end_line: z.number().int().min(1).optional().describe("1-based last line (default start+199)"),
        max_bytes: z.number().int().min(1).max(262144).optional().describe("Byte ceiling (default 16384, cap 262144)"),
      },
    },
    async ({ path: p, start_line, end_line, max_bytes }) => {
      const c = clientGet();
      const live = await liveSnapshot(c, "herdr_fs_read");
      if (!live.ok) return live.result;
      const snap = live.snap;
      const v = await validateManagedFile(snap, p, true);
      if (!v.ok) return toResult(v.err);
      let data: Buffer;
      try { data = await readFile(v.real); } catch (e) {
        return toResult({ ok: false, reason: "read_failed", path: v.resolved, message: String(e) });
      }
      const budget = max_bytes ?? 16384;
      const allLines = data.toString("utf-8").split("\n");
      const s0 = start_line ?? 1;
      const e0 = Math.min(end_line ?? s0 + 199, allLines.length);
      let content = allLines.slice(s0 - 1, e0).join("\n");
      let truncated = e0 < allLines.length;
      let truncated_by: "lines" | "bytes" | null = truncated ? "lines" : null;
      if (Buffer.byteLength(content, "utf-8") > budget) {
        // Keep only complete lines so next_start_line never skips a partial line's tail.
        const buf = Buffer.from(content, "utf-8").subarray(0, budget);
        const lastNl = buf.lastIndexOf(0x0a);
        content = lastNl >= 0 ? buf.subarray(0, lastNl).toString("utf-8") : "";
        truncated = true;
        truncated_by = "bytes";
      }
      const linesDelivered = content === "" ? 0 : content.split("\n").length;
      const next_start_line = truncated
        ? (truncated_by === "bytes"
          ? (linesDelivered === 0 ? s0 : s0 + linesDelivered)
          : e0 + 1)
        : null;
      return toResult({ ok: true, path: v.resolved, root: v.root,
        lines: {
          start: s0,
          end: truncated_by === "bytes"
            ? (linesDelivered === 0 ? s0 - 1 : s0 + linesDelivered - 1)
            : e0,
          total: allLines.length,
        },
        next_start_line, truncated_by, bytes: data.length, budget, truncated, content,
        ...(live.warnings.length ? { warnings: live.warnings } : {}) });
    },
  );

  server.registerTool(
    "herdr_fs_list",
    {
      description:
        "List a directory inside a MANAGED (git) project root on the workstation. " +
        "Gates: path must be an existing directory inside a git-backed project root " +
        "from the live snapshot (same validation as herdr_fs_read); secret-ish files " +
        "(.env*, *.pem, id_rsa*, *.key, .git/config, *secret*/*token*/*credential*) " +
        "are skipped; .git is always skipped. Returns name/type(file|dir|symlink)/size?/mtime? " +
        "per entry. recursive:true walks subdirectories (bounded by max_entries).",
      inputSchema: {
        path: z.string().describe("Absolute directory path inside a managed project root"),
        recursive: z.boolean().default(false).describe("Recursively list subdirectories (default false)"),
        glob: z.string().optional().describe("Optional glob filter on entry names (e.g. '*.ts')"),
        max_entries: z.number().int().min(1).max(2000).optional().describe("Max entries returned (default 200)"),
      },
    },
    async ({ path: p, recursive, glob, max_entries }) => {
      const c = clientGet();
      const live = await liveSnapshot(c, "herdr_fs_list");
      if (!live.ok) return live.result;
      const snap = live.snap;
      const v = await validateManagedFile(snap, p, true);
      if (!v.ok) return toResult(v.err);
      let st;
      try { st = await stat(v.real); } catch (e) {
        return toResult({ ok: false, reason: "stat_failed", path: v.resolved, message: String(e) });
      }
      if (!st.isDirectory()) {
        return toResult({ ok: false, reason: "not_a_directory", path: v.resolved });
      }
      const budget = max_entries ?? 200;
      const out: Record<string, unknown>[] = [];
      let truncated = false;
      const globRe = glob ? new RegExp("^" + glob.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".") + "$") : null;
      const walk = async (dir: string, depth: number): Promise<void> => {
        if (out.length >= budget) { truncated = true; return; }
        let entries;
        try { entries = await readdir(dir, { withFileTypes: true }); } catch { return; }
        for (const ent of entries) {
          if (out.length >= budget) { truncated = true; return; }
          const name = ent.name;
          if (name === ".git") continue;
          const full = path.join(dir, name);
          if (deniedSecretPath(full)) continue;
          const type = ent.isDirectory() ? "dir" : ent.isSymbolicLink() ? "symlink" : "file";
          if (type === "dir") {
            if (recursive) await walk(full, depth - 1);
            if (globRe && !globRe.test(name)) continue; // glob filters files; dirs only filtered when not recursing
          } else if (globRe && !globRe.test(name)) {
            continue;
          }
          const rec: Record<string, unknown> = {
            name,
            type,
            path: full,
            relative_path: path.relative(v.real, full) || ".",
          };
          if (type === "file") {
            try { const fs = await stat(full); rec.size = fs.size; rec.mtime = fs.mtime.toISOString(); } catch { /* ignore */ }
          }
          out.push(rec);
        }
      };
      await walk(v.real, recursive ? 64 : 0);
      return toResult({ ok: true, path: v.resolved, root: v.root, count: out.length, truncated, entries: out });
    },
  );

  server.registerTool(
    "herdr_fs_grep",
    {
      description:
        "Content-search inside a MANAGED (git) project root on the workstation. " +
        "Gates: root/path must be inside a git-backed project root from the live snapshot; " +
        "secret-ish files are excluded. Prefers ripgrep (rg) when available, else falls back " +
        "to a Node traversal. Returns matching lines with file/line/content; truncated:true " +
        "when the match budget or byte budget is hit.",
      inputSchema: {
        root: z.string().describe("Absolute directory path inside a managed project root to search"),
        pattern: z.string().describe("Search pattern (literal string, or regex when regex:true)"),
        regex: z.boolean().default(false).describe("Treat pattern as a regular expression (default false)"),
        glob: z.string().optional().describe("Optional glob filter on file names (e.g. '*.ts')"),
        max_matches: z.number().int().min(1).max(1000).optional().describe("Max matches returned (default 50)"),
        max_bytes: z.number().int().min(1).max(1048576).optional().describe("Per-file byte ceiling (default 65536)"),
        case_insensitive: z.boolean().default(false).describe("Case-insensitive match (default false)"),
      },
    },
    async ({ root: p, pattern, regex, glob, max_matches, max_bytes, case_insensitive }) => {
      const c = clientGet();
      const live = await liveSnapshot(c, "herdr_fs_grep");
      if (!live.ok) return live.result;
      const snap = live.snap;
      const v = await validateManagedFile(snap, p, true);
      if (!v.ok) return toResult(v.err);
      let st;
      try { st = await stat(v.real); } catch (e) {
        return toResult({ ok: false, reason: "stat_failed", path: v.resolved, message: String(e) });
      }
      if (!st.isDirectory()) {
        return toResult({ ok: false, reason: "not_a_directory", path: v.resolved });
      }
      const matchBudget = max_matches ?? 50;
      const byteBudget = max_bytes ?? 65536;
      const out: Record<string, unknown>[] = [];
      let truncated = false;
      const globRe = glob ? new RegExp("^" + glob.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".") + "$") : null;
      const re = regex ? new RegExp(pattern, case_insensitive ? "i" : "") : null;
      const lit = regex ? null : pattern;

      // Prefer ripgrep (rg) when available; stream lines and stop at budgets.
      const rgArgs = [
        "--line-number", "--no-heading", "--color", "never",
        ...(regex ? [] : ["-F"]),
        ...(case_insensitive ? ["-i"] : []),
        ...(glob ? ["-g", glob] : []),
        "--max-count", String(matchBudget),
        pattern, v.real,
      ];
      const rgOk = await new Promise<boolean>((resolve) => {
        let child;
        try { child = spawn("rg", rgArgs, { stdio: ["ignore", "pipe", "ignore"] }); }
        catch { resolve(false); return; }
        let carry = "";
        let totalBytes = 0;
        child.stdout.on("data", (d: Buffer) => {
          totalBytes += d.length;
          if (totalBytes > byteBudget * 8) { // hard ceiling on rg stdout
            truncated = true;
            try { child.kill("SIGTERM"); } catch { /* ignore */ }
            return;
          }
          carry += d.toString("utf-8");
          const parts = carry.split("\n");
          carry = parts.pop() ?? "";
          for (const line of parts) {
            if (!line.trim()) continue;
            if (out.length >= matchBudget) {
              truncated = true;
              try { child.kill("SIGTERM"); } catch { /* ignore */ }
              return;
            }
            const idx = line.indexOf(":");
            if (idx < 0) continue;
            const file = line.slice(0, idx);
            const rest = line.slice(idx + 1);
            const idx2 = rest.indexOf(":");
            if (idx2 < 0) continue;
            const lineNo = Number(rest.slice(0, idx2));
            const content = rest.slice(idx2 + 1);
            if (Buffer.byteLength(content, "utf-8") > byteBudget) { truncated = true; continue; }
            if (deniedSecretPath(file)) continue;
            out.push({ file, line: lineNo, content });
          }
        });
        child.on("error", () => resolve(false));
        child.on("close", () => resolve(true));
      });
      if (rgOk) {
        return toResult({ ok: true, root: v.resolved, count: out.length, truncated, matches: out, engine: "rg" });
      }

      const walk = async (dir: string): Promise<void> => {
        if (out.length >= matchBudget) { truncated = true; return; }
        let entries;
        try { entries = await readdir(dir, { withFileTypes: true }); } catch { return; }
        for (const ent of entries) {
          if (out.length >= matchBudget) { truncated = true; return; }
          const name = ent.name;
          if (name === ".git") continue;
          const full = path.join(dir, name);
          if (deniedSecretPath(full)) continue;
          if (ent.isDirectory()) { await walk(full); continue; }
          if (!ent.isFile()) continue;
          if (globRe && !globRe.test(name)) continue;
          let data: Buffer;
          try { data = await readFile(full); } catch { continue; }
          if (data.length > byteBudget) { truncated = true; continue; }
          const text = data.toString("utf-8");
          const lines = text.split("\n");
          for (let i = 0; i < lines.length; i++) {
            if (out.length >= matchBudget) { truncated = true; break; }
            const line = lines[i];
            const hit = re ? re.test(line) : (case_insensitive ? line.toLowerCase().includes(lit!.toLowerCase()) : line.includes(lit!));
            if (hit) out.push({ file: full, line: i + 1, content: line });
          }
        }
      };
      await walk(v.real);
      return toResult({ ok: true, root: v.resolved, count: out.length, truncated, matches: out, engine: "node" });
    },
  );

  server.registerTool(
    "herdr_fs_patch",
    {
      description:
        "Apply a coding-tools/Codex-style patch (*** Begin Patch / *** End Patch) inside a managed " +
        "git root. Prefer this over herdr_fs_edit for multi-hunk / multi-file edits. Paths in the " +
        "patch may be absolute or relative to root. dry_run:true validates without writing. " +
        "Same dirty/busy gates as herdr_fs_edit when applying.",
      inputSchema: {
        root: z.string().describe("Managed git project root (absolute)"),
        patch: z.string().describe("Full *** Begin Patch ... *** End Patch text"),
        dry_run: z.boolean().default(false),
        confirm_dirty: z.boolean().default(false),
        confirm_busy: z.boolean().default(false),
      },
    },
    async ({ root: rootIn, patch, dry_run, confirm_dirty, confirm_busy }) => {
      const c = clientGet();
      const live = await liveSnapshot(c, "herdr_fs_patch");
      if (!live.ok) return live.result;
      const snap = live.snap;
      const rootV = await validateManagedFile(snap, rootIn, true);
      if (!rootV.ok) return toResult(rootV.err);
      let st;
      try { st = await stat(rootV.real); } catch (e) {
        return toResult({ ok: false, reason: "stat_failed", path: rootV.resolved, message: String(e) });
      }
      if (!st.isDirectory()) return toResult({ ok: false, reason: "not_a_directory", path: rootV.resolved });
      // dry_run is read-only validation — allow under HERDR_MCP_READONLY
      if (!dry_run) {
        const gate = mutationDenied(rootV.root, "herdr_fs_patch");
        if (gate) return toResult(gate);
      }
      const working = workingAgentsForRoot(snap, rootV.root);
      if (working.length > 0 && !confirm_busy && !dry_run) {
        return toResult({ ok: false, reason: "agent_working", root: rootV.root, working,
          hint: "pass confirm_busy:true to force, or wait for idle" });
      }
      let ops;
      try { ops = parsePatch(patch); }
      catch (e) {
        const err = e instanceof PatchError ? e : new PatchError("PATCH_FAILED", String(e));
        return toResult({ ok: false, code: err.code, message: err.message, ...(err.details ?? {}) });
      }
      if (!ops.length) return toResult({ ok: false, code: "PATCH_FAILED", message: "No files were modified." });

      const resolveTarget = async (raw: string, mustExist: boolean) => {
        const abs = path.isAbsolute(raw) ? path.resolve(raw) : path.resolve(rootV.real, raw);
        return validateManagedFile(snap, abs, mustExist);
      };

      type Staged = { display: string; real: string; content: string | null; op: string };
      const staged: Staged[] = [];
      const summaries: string[] = [];
      let additions = 0;
      let removals = 0;

      try {
        for (const op of ops) {
          if (op.kind === "add") {
            const v = await resolveTarget(op.path, false);
            if (!v.ok) return toResult({ ...v.err, patch_op: "add" });
            try { await stat(v.real); return toResult({ ok: false, code: "PATCH_FAILED", message: "Cannot add file that already exists.", path: v.resolved }); }
            catch { /* expected missing */ }
            staged.push({ display: v.resolved, real: v.real, content: op.content, op: "add" });
            summaries.push(`A ${v.resolved}`);
            additions += op.content.split("\n").length;
          } else if (op.kind === "delete") {
            const v = await resolveTarget(op.path, true);
            if (!v.ok) return toResult({ ...v.err, patch_op: "delete" });
            const text = await readFile(v.real, "utf-8");
            staged.push({ display: v.resolved, real: v.real, content: null, op: "delete" });
            summaries.push(`D ${v.resolved}`);
            removals += text.split("\n").length;
          } else {
            const v = await resolveTarget(op.path, true);
            if (!v.ok) return toResult({ ...v.err, patch_op: "update" });
            const old = await readFile(v.real, "utf-8");
            const updated = applyUpdateHunks(old, op.hunks, v.resolved);
            for (const hunk of op.hunks) {
              for (const line of hunk) {
                if (line.startsWith("+")) additions += 1;
                if (line.startsWith("-")) removals += 1;
              }
            }
            if (op.move_to) {
              const dest = await resolveTarget(op.move_to, false);
              if (!dest.ok) return toResult({ ...dest.err, patch_op: "move" });
              staged.push({ display: v.resolved, real: v.real, content: null, op: "delete" });
              staged.push({ display: dest.resolved, real: dest.real, content: updated, op: "add" });
              summaries.push(`R ${v.resolved} -> ${dest.resolved}`);
            } else {
              staged.push({ display: v.resolved, real: v.real, content: updated, op: "update" });
              summaries.push(`M ${v.resolved}`);
            }
          }
        }
      } catch (e) {
        const err = e instanceof PatchError ? e : new PatchError("PATCH_FAILED", String(e));
        return toResult({ ok: false, code: err.code, message: err.message, ...(err.details ?? {}) });
      }

      if (!dry_run) {
        for (const s of staged) {
          if (s.content === null) {
            if (fileDirty(rootV.root, s.real) && !confirm_dirty) {
              return toResult({ ok: false, reason: "file_dirty_confirmation_required", path: s.display,
                hint: "re-send with confirm_dirty:true" });
            }
          } else if (s.op !== "add") {
            if (fileDirty(rootV.root, s.real) && !confirm_dirty) {
              return toResult({ ok: false, reason: "file_dirty_confirmation_required", path: s.display,
                hint: "re-send with confirm_dirty:true" });
            }
          }
        }
        try {
          await commitAtomic(staged.map((s) => ({ real: s.real, content: s.content })));
        } catch (e) {
          return toResult({
            ok: false,
            code: "PATCH_COMMIT_FAILED",
            message: e instanceof Error ? e.message : String(e),
            hint: "patch rolled back when possible; re-read files and regenerate",
          });
        }
      }

      return toResult({
        ok: true,
        dry_run,
        root: rootV.root,
        summary: summaries.join("\n"),
        affected_files: staged.map((s) => ({ path: s.display, operation: s.op })),
        additions,
        removals,
        ...(working.length ? { warnings: { working } } : {}),
        ...(live.warnings.length ? { cache_warnings: live.warnings } : {}),
      });
    },
  );

  server.registerTool(
    "herdr_fs_image",
    {
      description:
        "Read an image under a managed git root and return it as an MCP image (plus JSON metadata). " +
        "Use for screenshots/UI assets so the web model can see pixels without a local agent.",
      inputSchema: {
        path: z.string().describe("Absolute image path inside a managed project root"),
        max_bytes: z.number().int().min(1).max(8_000_000).optional()
          .describe("Byte ceiling (default 2097152)"),
      },
    },
    async ({ path: p, max_bytes }) => {
      const c = clientGet();
      const live = await liveSnapshot(c, "herdr_fs_image");
      if (!live.ok) return live.result;
      const v = await validateManagedFile(live.snap, p, true);
      if (!v.ok) return toResult(v.err);
      const budget = max_bytes ?? 2_097_152;
      let data: Buffer;
      try { data = await readFile(v.real); }
      catch (e) { return toResult({ ok: false, reason: "read_failed", path: v.resolved, message: String(e) }); }
      if (data.length > budget) {
        return toResult({ ok: false, reason: "image_too_large", path: v.resolved, bytes: data.length, max_bytes: budget });
      }
      const ext = path.extname(v.real).toLowerCase();
      const mime =
        ext === ".png" ? "image/png"
        : ext === ".jpg" || ext === ".jpeg" ? "image/jpeg"
        : ext === ".gif" ? "image/gif"
        : ext === ".webp" ? "image/webp"
        : null;
      if (!mime) {
        return toResult({ ok: false, reason: "unsupported_image", path: v.resolved, hint: "png/jpeg/gif/webp only" });
      }
      const meta = {
        ok: true,
        path: v.resolved,
        root: v.root,
        mime_type: mime,
        bytes: data.length,
      };
      return {
        content: [
          { type: "text" as const, text: JSON.stringify(meta) },
          { type: "image" as const, data: data.toString("base64"), mimeType: mime },
        ],
      };
    },
  );

  server.registerTool(
    "herdr_git",
    {
      description:
        "Deterministic git facts for a managed root — prefer this over herdr_prompt to a local agent. " +
        "action=status|diff|log. Runs local git (PAGER=cat); if herdr snapshot/managed-roots gate " +
        "blips (TaskGroup), still serves repos under $HOME with a warnings[] mark. " +
        "Web planner should call this itself to verify changes.",
      inputSchema: {
        root: z.string().describe("Managed git project root (absolute)"),
        action: z.enum(["status", "diff", "log"]).describe("Git action"),
        path: z.string().optional().describe("Optional path for diff (repo-relative or absolute under root)"),
        staged: z.boolean().default(false).describe("diff --staged when action=diff"),
        max_count: z.number().int().min(1).max(100).optional().describe("log -n (default 20)"),
        max_bytes: z.number().int().min(1).max(512_000).optional().describe("Output byte ceiling (default 65536)"),
      },
    },
    async ({ root: rootIn, action, path: pathIn, staged, max_count, max_bytes }) => {
      const c = clientGet();
      const live = await liveSnapshot(c, "herdr_git");
      const warnings: string[] = live.ok ? [...live.warnings] : [];
      let rootV: { ok: true; root: string; resolved: string; real: string } | { ok: false; err: Record<string, unknown> };

      if (live.ok) {
        rootV = await validateManagedFile(live.snap, rootIn, true);
        // Snapshot cache may be empty during control-plane storms — fall back to local git root.
        if (!rootV.ok && (rootV.err as { reason?: string }).reason === "outside_managed_roots") {
          const local = await validateGitRootLocalFallback(rootIn);
          if (local.ok) {
            rootV = local;
            warnings.push("git_local_without_managed_roots");
          }
        }
      } else {
        const local = await validateGitRootLocalFallback(rootIn);
        if (!local.ok) return live.result;
        rootV = local;
        warnings.push("git_local_after_snapshot_failure");
      }
      if (!rootV.ok) return toResult(rootV.err);
      if (gitToplevel(rootV.real) !== rootV.real && gitToplevel(rootV.real) !== rootV.root) {
        // allow if root is the git toplevel
        const top = gitToplevel(rootV.real);
        if (!top) return toResult({ ok: false, reason: "not_a_git_repo", root: rootV.resolved });
      }
      const budget = max_bytes ?? 65536;
      const args: string[] =
        action === "status" ? ["status", "--porcelain", "-b"]
        : action === "diff"
          ? ["diff", ...(staged ? ["--staged"] : []), ...(pathIn ? ["--", path.isAbsolute(pathIn) ? pathIn : pathIn] : [])]
          : ["log", `-n${max_count ?? 20}`, "--oneline", "--decorate"];
      const r = spawnSync("git", args, {
        cwd: rootV.real,
        timeout: 15_000,
        maxBuffer: Math.max(budget + 4096, 8_388_608),
        encoding: "utf-8",
        env: { ...process.env, PAGER: "cat", GIT_PAGER: "cat" },
      });
      if (r.error) {
        return toResult({ ok: false, root: rootV.root, action, message: String(r.error) });
      }
      let text = String(r.stdout ?? "");
      let truncated = false;
      if (Buffer.byteLength(text, "utf-8") > budget) {
        text = Buffer.from(text, "utf-8").subarray(0, budget).toString("utf-8");
        truncated = true;
      }
      // git diff without --exit-code returns 0 even with changes; nonzero = real error.
      if ((r.status ?? 0) !== 0) {
        return toResult({
          ok: false,
          root: rootV.root,
          action,
          exit_code: r.status,
          output: text,
          stderr: String(r.stderr ?? ""),
          ...(warnings.length ? { warnings } : {}),
        });
      }
      return toResult({
        ok: true,
        root: rootV.root,
        action,
        exit_code: 0,
        truncated,
        output: text,
        ...(r.stderr ? { stderr: String(r.stderr).slice(0, 2000) } : {}),
        ...(warnings.length ? { warnings } : {}),
      });
    },
  );

  server.registerTool(
    "herdr_exec_start",
    {
      description:
        "Start a long-running shell command in a managed project root as a background session " +
        "(local process, not the herdr utility pane). Returns session_id. Then poll with " +
        "herdr_exec_read and finish with herdr_exec_kill. For short commands prefer herdr_exec.",
      inputSchema: {
        root: z.string().describe("Managed git project root used as cwd"),
        command: z.string().describe("Shell command line"),
        confirm_busy: z.boolean().default(false),
      },
    },
    async ({ root: rootIn, command, confirm_busy }) => {
      const c = clientGet();
      const live = await liveSnapshot(c, "herdr_exec_start");
      if (!live.ok) return live.result;
      const rootV = await validateManagedFile(live.snap, rootIn, true);
      if (!rootV.ok) return toResult(rootV.err);
      const gate = mutationDenied(rootV.root, "herdr_exec_start");
      if (gate) return toResult(gate);
      const working = workingAgentsForRoot(live.snap, rootV.root);
      if (working.length > 0 && !confirm_busy) {
        return toResult({ ok: false, reason: "agent_working", root: rootV.root, working,
          hint: "pass confirm_busy:true to force" });
      }
      const s = startExecSession({ command, cwd: rootV.real });
      return toResult({
        ok: true,
        session_id: s.id,
        root: rootV.root,
        command,
        hint: "poll herdr_exec_read with session_id; herdr_exec_kill when done",
        ...(working.length ? { warnings: { working } } : {}),
      });
    },
  );

  server.registerTool(
    "herdr_exec_read",
    {
      description:
        "Read stdout/stderr from a herdr_exec_start session. Pass offset=next_offset to continue. " +
        "running=false when the process has exited.",
      inputSchema: {
        session_id: z.string(),
        stream: z.enum(["stdout", "stderr", "both"]).default("both"),
        offset: z.number().int().min(0).optional(),
        limit: z.number().int().min(1).max(262144).optional(),
      },
    },
    async ({ session_id, stream, offset, limit }) => {
      const r = readExecSession(session_id, { stream, offset, limit });
      if (!r.ok) return toResult(r);
      return toResult(r);
    },
  );

  server.registerTool(
    "herdr_exec_kill",
    {
      description: "Terminate a herdr_exec_start session (SIGTERM then SIGKILL).",
      inputSchema: {
        session_id: z.string(),
      },
    },
    async ({ session_id }) => {
      const r = killExecSession(session_id);
      if (!r.ok) return toResult({ ok: false, reason: r.reason });
      return toResult(r);
    },
  );

  server.registerTool(
    "herdr_exec",
    {
      description:
        "Run a shell command on the workstation inside the target workspace's persistent, " +
        "VISIBLE utility pane (label 'herdr-mcp:utility'; created once, reused — not headless, " +
        "observable in herdr). The command always runs inside a selected project root — explicit " +
        "project_root, or the workspace's single current project root — via an explicit subshell " +
        "cd; it never depends on the pane's current foreground_cwd. When the workspace has " +
        "MULTIPLE project roots and project_root is omitted, the call is REFUSED and returns " +
        "candidates (one of them is the exact value to pass next). Gated by HERDR_MCP_READONLY / " +
        "HERDR_MCP_WRITE_ROOTS. If any agent in that project is working, refused unless " +
        "confirm_busy:true (returns warnings.working). Freeform shell is NOT secret-path gated " +
        "(unlike herdr_fs_* — a command can still read .env); prefer fs tools for file IO. " +
        "Returns exit_code + effective_cwd/project_root + stripped output. " +
        "On timeout the command may still be running in the pane; partial output is returned " +
        "with ok:false code:exec_timeout. " +
        "If herdr control-plane TaskGroup/ExceptionGroup blocks pane ops BEFORE the command is " +
        "delivered, automatically falls back to a local zsh process (backend:local_fallback) — " +
        "same cwd/gates, no double-run. After send_text, never re-sends or falls back.",
      inputSchema: {
        workspace: z.string().describe("workspace_id or label (from herdr_inspect)"),
        command: z.string().describe("Shell command line to run in the utility pane"),
        project_root: z.string().optional().describe("Explicit project root within this workspace (workspaces[].projects[].root from herdr_inspect). REQUIRED when the workspace has multiple project roots; the command runs with this root as cwd via subshell cd"),
        timeout_ms: z.number().int().min(1).max(HERDR_RPC_TIMEOUT_MAX_MS).default(30000),
        confirm_busy: z.boolean().default(false).describe("Force exec even when an agent in the project is working (returns warnings.working)"),
      },
    },
    async ({ workspace: wsTarget, command, project_root, timeout_ms, confirm_busy }) => {
      const c = clientGet();
      const live = await liveSnapshot(c, "herdr_exec");
      if (!live.ok) return live.result;
      const snap = live.snap;
      const wsRec = ((snap["workspaces"] as unknown[]) ?? [])
        .map((w) => (w ?? {}) as Record<string, unknown>)
        .find((w) => w["workspace_id"] === wsTarget || w["label"] === wsTarget);
      if (!wsRec) return toResult({ ok: false, reason: "workspace_not_found", workspace: wsTarget });
      const wsId = wsRec["workspace_id"] as string;

      const currentProjects = projectsForWorkspace(snap, wsId);
      const roots = currentProjects.map((p) => p.root);
      let effectiveRoot: string | null = null;
      if (roots.length === 0) {
        return toResult({
          ok: false,
          reason: "project_root_required",
          workspace: wsId,
          candidates: [],
          current_projects: [],
          hint: "workspace has no current project root — create or attach a project, then re-call with project_root set to the returned root",
        });
      }
      if (project_root) {
        const want = path.resolve(project_root);
        const match = currentProjects.find((p) => p.root === want);
        if (!match) {
          return toResult({
            ok: false,
            reason: "project_root_not_in_workspace",
            workspace: wsId,
            project_root: want,
            candidates: roots,
            current_projects: currentProjects,
            hint: "project_root must be one of this workspace's current project roots — re-call with project_root set to one of candidates",
          });
        }
        effectiveRoot = match.root;
      } else if (roots.length > 1) {
        return toResult({
          ok: false,
          reason: "project_root_required",
          workspace: wsId,
          candidates: roots,
          current_projects: currentProjects,
          hint: "workspace has multiple project roots — re-call with project_root set to one of candidates",
        });
      } else {
        effectiveRoot = roots[0];
      }
      const execCwd = effectiveRoot;
      const gate = mutationDenied(execCwd, "herdr_exec");
      if (gate) return toResult(gate);
      const working = workingAgentsForRoot(snap, execCwd);
      if (working.length > 0 && !confirm_busy) {
        return toResult({
          ok: false, reason: "agent_working", root: execCwd, working,
          hint: "an agent in this project is working — pass confirm_busy:true to force, or wait for idle/done",
        });
      }

      const execTimeoutMs = clampHerdrTimeout(timeout_ms);
      const busyWarn = working.length > 0 ? { warnings: { working } } : {};

      const localFallback = async (reason: string) => {
        const local = await runLocalShell({
          command,
          cwd: execCwd!,
          timeoutMs: execTimeoutMs,
          maxOutputBytes: 8000,
        });
        if (local.timed_out) {
          return toResult({
            ok: false,
            code: "exec_timeout",
            backend: "local_fallback",
            fallback_reason: reason,
            workspace: wsId,
            command,
            effective_cwd: execCwd,
            project_root: effectiveRoot,
            exit_code: local.exit_code,
            output: local.output,
            hint: "local_fallback timed out — command may still be running as a local process",
            ...busyWarn,
          });
        }
        return toResult({
          ok: local.exit_code === 0,
          backend: "local_fallback",
          fallback_reason: reason,
          workspace: wsId,
          command,
          exit_code: local.exit_code,
          effective_cwd: execCwd,
          project_root: effectiveRoot,
          output: local.output,
          ...busyWarn,
        });
      };

      const panesRaw = ((snap["panes"] as unknown[]) ?? [])
        .map((p) => (p ?? {}) as Record<string, unknown>)
        .filter((p) => p["workspace_id"] === wsId);

      // Pre-send: resolve/create utility pane with TaskGroup retries, then local fallback.
      let paneId: string | null = null;
      let created = false;
      let preSendBlip: string | null = null;
      for (let attempt = 0; attempt < 3; attempt++) {
        try {
          const utilityLabel = panesRaw.find((p) => p["label"] === "herdr-mcp:utility")?.["pane_id"];
          paneId = typeof utilityLabel === "string" ? utilityLabel : null;
          created = false;
          if (!paneId) {
            // Re-read panes from a fresh snapshot when possible (label may have appeared).
            let seed = panesRaw[0]?.["pane_id"];
            try {
              const fresh = await c.snapshot();
              const freshPanes = ((fresh["panes"] as unknown[]) ?? [])
                .map((p) => (p ?? {}) as Record<string, unknown>)
                .filter((p) => p["workspace_id"] === wsId);
              const labeled = freshPanes.find((p) => p["label"] === "herdr-mcp:utility");
              if (typeof labeled?.["pane_id"] === "string") {
                paneId = labeled["pane_id"] as string;
              } else {
                seed = freshPanes[0]?.["pane_id"] ?? seed;
              }
            } catch (e) {
              if (!isHerdrControlPlaneTaskGroup(e instanceof Error ? e.message : String(e))) throw e;
              // keep seed from cache
            }
            if (!paneId) {
              const r = await c.call("pane.split",
                { ...(typeof seed === "string" ? { target_pane_id: seed } : {}), cwd: execCwd, direction: "right", focus: false },
                10000);
              const created0 = (r["pane"] ?? r) as Record<string, unknown>;
              paneId = (created0["pane_id"] ?? created0["id"] ?? null) as string | null;
              if (!paneId) return toResult({ ok: false, reason: "pane_split_failed", detail: r });
              created = true;
              try { await c.call("pane.rename", { pane_id: paneId, label: "herdr-mcp:utility" }, 5000); } catch { /* optional */ }
              try {
                await c.call("pane.wait_for_output",
                  { pane_id: paneId, source: "recent_unwrapped", match: { type: "regex", value: "[%#$>❯] ?$" }, timeout_ms: 5000 },
                  6000);
              } catch { /* best effort */ }
              await new Promise<void>((resolve) => setTimeout(resolve, 300));
            }
          }
          preSendBlip = null;
          break;
        } catch (e) {
          const msg = e instanceof Error ? e.message : String(e);
          if (isHerdrControlPlaneTaskGroup(msg)) {
            preSendBlip = msg;
            await new Promise<void>((r) => setTimeout(r, 100 + attempt * 200));
            continue;
          }
          const err = e instanceof HerdrError ? e : new HerdrError("error", msg);
          return toResult({ ok: false, code: err.code, message: err.message, hint: "failed to prepare utility pane" });
        }
      }
      if (preSendBlip || !paneId) {
        return localFallback(preSendBlip
          ? "control_plane_taskgroup_before_send"
          : "utility_pane_unavailable");
      }

      const nonce = randomUUID().slice(0, 8);
      const marker = `__HM_EXEC_${nonce}_EXIT_`;
      const shq = (s: string): string => "'" + s.replace(/'/g, `'\\''`) + "'";
      const scriptPath = path.join(
        process.env.TMPDIR || "/tmp",
        `herdr-mcp-exec-${nonce}.sh`,
      );
      const scriptBody = [
        "#!/bin/zsh",
        "set +e",
        effectiveRoot ? `cd -- ${shq(effectiveRoot)} || exit 127` : "",
        command,
      ].filter(Boolean).join("\n") + "\n";
      try {
        await writeFile(scriptPath, scriptBody, { encoding: "utf-8", mode: 0o700 });
      } catch (e) {
        return toResult({ ok: false, reason: "script_write_failed", message: String(e) });
      }
      const cmdline = `zsh ${shq(scriptPath)}; ec=$?; rm -f -- ${shq(scriptPath)}; printf '\\n${marker}%s__' "$ec"`;
      const readText = (rr: HerdrResult): string => {
        const rd = ((rr["read"] as Record<string, unknown>) ?? rr) as Record<string, unknown>;
        return String(rd["content"] ?? rd["text"] ?? rd["output"] ?? "");
      };

      let delivered = false;
      try {
        await c.call("pane.send_text", { pane_id: paneId, text: cmdline + "\n" }, 5000);
        delivered = true;
      } catch (e) {
        const err = e instanceof HerdrError ? e : new HerdrError("error", String(e));
        // Known undelivered: pane gone — recreate once and send (existing path).
        if (err.code === "pane_not_found" || err.code === "unknown_pane") {
          try {
            const fresh = await c.snapshot();
            const freshPanes = ((fresh["panes"] as unknown[]) ?? [])
              .map((p) => (p ?? {}) as Record<string, unknown>)
              .filter((p) => p["workspace_id"] === wsId && p["label"] === "herdr-mcp:utility");
            let nextId: string | null = null;
            if (freshPanes.length > 0 && typeof freshPanes[0]["pane_id"] === "string") {
              nextId = freshPanes[0]["pane_id"] as string;
            } else {
              const seedRaw = panesRaw[0]?.["pane_id"];
              const r2 = await c.call("pane.split",
                { ...(typeof seedRaw === "string" ? { target_pane_id: seedRaw } : {}), cwd: execCwd, direction: "right", focus: false }, 10000);
              const p2 = (r2["pane"] ?? r2) as Record<string, unknown>;
              nextId = (p2["pane_id"] ?? p2["id"] ?? null) as string | null;
              if (nextId) {
                try { await c.call("pane.rename", { pane_id: nextId, label: "herdr-mcp:utility" }, 5000); } catch { /* optional */ }
                created = true;
              }
            }
            if (!nextId) throw err;
            paneId = nextId;
            await c.call("pane.send_text", { pane_id: paneId, text: cmdline + "\n" }, 5000);
            delivered = true;
          } catch (e2) {
            const msg2 = e2 instanceof Error ? e2.message : String(e2);
            // Recreate/send never confirmed — safe to local-fallback only if control-plane
            // and we never marked delivered.
            if (!delivered && isHerdrControlPlaneTaskGroup(msg2)) {
              return localFallback("control_plane_taskgroup_pane_recover");
            }
            if (!delivered) {
              // pane_not_found after recover failed — command not sent
              return localFallback(`pane_recover_failed:${err.code}`);
            }
            const err2 = e2 instanceof HerdrError ? e2 : new HerdrError("error", msg2);
            return toResult({
              ok: false, code: err2.code, message: err2.message,
              workspace: wsId, pane_id: paneId, command,
              delivery: "uncertain",
              hint: "send may have reached the utility pane — check pane output; do not blind-retry",
              ...busyWarn,
            });
          }
        } else if (isHerdrControlPlaneTaskGroup(err.message)) {
          // send_text + TaskGroup: delivery uncertain — NEVER local_fallback / re-send
          return toResult({
            ok: false,
            code: "delivery_uncertain",
            failure: "herdr_internal",
            message: unwrapControlPlaneMessage(err.message),
            workspace: wsId, pane_id: paneId, command,
            hint: "pane.send_text hit control-plane TaskGroup — command may or may not have run; inspect utility pane or herdr_since, do not re-send the same command",
            ...busyWarn,
          });
        } else {
          return toResult({ ok: false, code: err.code, message: err.message, workspace: wsId, pane_id: paneId, command });
        }
      }

      try {
        await c.call("pane.wait_for_output",
          { pane_id: paneId, source: "recent_unwrapped", match: { type: "regex", value: `${marker}\\d+__` }, timeout_ms: execTimeoutMs },
          clampHerdrTimeout(execTimeoutMs + 10000));
      } catch (e) {
        const err = e instanceof HerdrError ? e : new HerdrError("error", String(e));
        let partial = "";
        try {
          const rr = await c.call("pane.read", { pane_id: paneId, source: "recent_unwrapped", lines: 80, strip_ansi: true }, 5000);
          partial = readText(rr);
        } catch { /* best effort */ }
        const timedOut = err.code === "timeout";
        return toResult({
          ok: false,
          code: timedOut ? "exec_timeout" : err.code,
          message: timedOut ? undefined : err.message,
          backend: "utility_pane",
          workspace: wsId, pane_id: paneId, command, effective_cwd: execCwd, project_root: effectiveRoot ?? null,
          partial_output: partial.slice(-4000),
          ...busyWarn,
          hint: timedOut
            ? "command may still be running in the utility pane — inspect it via pane.read"
            : `wait_for_output failed (${err.code}) — command was already sent; do not re-send`,
        });
      }
      let rr: HerdrResult;
      try {
        rr = await c.call("pane.read", { pane_id: paneId, source: "recent_unwrapped", lines: 200, strip_ansi: true }, 5000);
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        // Post-send read failure: no fallback (would double-run). One more wait already done.
        return toResult({
          ok: false,
          code: "pane_read_failed",
          message: isHerdrControlPlaneTaskGroup(msg) ? unwrapControlPlaneMessage(msg) : msg,
          backend: "utility_pane",
          workspace: wsId, pane_id: paneId, command,
          hint: "command was sent; retry herdr_call pane.read on this pane_id — do not re-run herdr_exec with the same command",
          ...busyWarn,
        });
      }
      const raw = readText(rr);
      let seg = raw;
      const cmdEcho = raw.lastIndexOf(cmdline);
      if (cmdEcho >= 0) seg = raw.slice(cmdEcho + cmdline.length);
      const mIdx = seg.indexOf(marker);
      if (mIdx >= 0) seg = seg.slice(0, mIdx);
      const full = raw.match(new RegExp(`${marker}(\\d+)__`));
      const exitCode = full ? Number(full[1]) : null;
      const output = cleanTerminalOutput(seg).trim();
      return toResult({
        ok: exitCode === 0,
        backend: "utility_pane",
        workspace: wsId, pane_id: paneId, created_utility_pane: created,
        command, exit_code: exitCode, effective_cwd: execCwd, project_root: effectiveRoot ?? null,
        output: output.slice(-8000),
        ...busyWarn,
      });
    },
  );

  server.registerTool(
    "herdr_fs_edit",
    {
      description:
        "Edit a file on the workstation by EXACT unique string replacement (never whole-file " +
        "overwrite). Gates: managed-root path validation (same as herdr_fs_read); if any agent " +
        "in the file's project is working -> refused by default (listed); confirm_busy:true forces " +
        "continue and returns warnings.working. If the file has uncommitted git changes -> " +
        "requires confirm_dirty:true. old_string must match exactly once.",
      inputSchema: {
        path: z.string().describe("Absolute file path inside a managed project root"),
        old_string: z.string().describe("Exact text to replace (must be unique in the file)"),
        new_string: z.string().describe("Replacement text"),
        confirm_dirty: z.boolean().default(false).describe("Acknowledge editing a git-dirty file"),
        confirm_busy: z.boolean().default(false).describe("Force edit even when an agent in the project is working (returns warnings.working)"),
      },
    },
    async ({ path: p, old_string, new_string, confirm_dirty, confirm_busy }) => {
      const c = clientGet();
      const live = await liveSnapshot(c, "herdr_fs_edit");
      if (!live.ok) return live.result;
      const snap = live.snap;
      const v = await validateManagedFile(snap, p, true);
      if (!v.ok) return toResult(v.err);
      const gate = mutationDenied(v.root, "herdr_fs_edit");
      if (gate) return toResult(gate);
      const working = workingAgentsForRoot(snap, v.root);
      if (working.length > 0 && !confirm_busy) {
        return toResult({ ok: false, reason: "agent_working", root: v.root, working,
          hint: "an agent in this project is working — pass confirm_busy:true to force, or wait for idle/done" });
      }
      let old: string;
      try { old = await readFile(v.real, "utf-8"); } catch (e) {
        return toResult({ ok: false, reason: "read_failed", path: v.resolved, message: String(e) });
      }
      const occurrences = old.split(old_string).length - 1;
      if (occurrences !== 1) {
        return toResult({ ok: false, reason: occurrences === 0 ? "old_string_not_found" : "old_string_not_unique",
          path: v.resolved, occurrences });
      }
      if (fileDirty(v.root, v.real) && !confirm_dirty) {
        return toResult({ ok: false, reason: "file_dirty_confirmation_required", path: v.resolved,
          hint: "file has uncommitted changes — re-send with confirm_dirty:true to proceed" });
      }
      const next = old.replace(old_string, new_string);
      try { await writeFile(v.real, next, "utf-8"); } catch (e) {
        return toResult({ ok: false, reason: "write_failed", path: v.resolved, message: String(e) });
      }
      return toResult({ ok: true, path: v.resolved, root: v.root, replaced: 1,
        bytes_before: Buffer.byteLength(old, "utf-8"), bytes_after: Buffer.byteLength(next, "utf-8"),
        ...(working.length > 0 ? { warnings: { working } } : {}) });
    },
  );

  server.registerTool(
    "herdr_fs_write",
    {
      description:
        "Create a new file (or explicitly overwrite a clean tracked one) on the workstation. " +
        "Same gates as herdr_fs_edit (managed root, no working agent by default, dirty needs " +
        "confirm). confirm_busy:true forces write even when an agent is working and returns " +
        "warnings.working. For surgical changes prefer herdr_fs_edit; this is for new files and " +
        "full rewrites.",
      inputSchema: {
        path: z.string().describe("Absolute target path inside a managed project root"),
        content: z.string().describe("Full file content"),
        overwrite: z.boolean().default(false).describe("Required true when overwriting an existing file"),
        confirm_dirty: z.boolean().default(false).describe("Acknowledge overwriting a git-dirty existing file"),
        confirm_busy: z.boolean().default(false).describe("Force write even when an agent in the project is working (returns warnings.working)"),
      },
    },
    async ({ path: p, content, overwrite, confirm_dirty, confirm_busy }) => {
      const c = clientGet();
      const live = await liveSnapshot(c, "herdr_fs_write");
      if (!live.ok) return live.result;
      const snap = live.snap;
      const v = await validateManagedFile(snap, p, false);
      if (!v.ok) return toResult(v.err);
      const gate = mutationDenied(v.root, "herdr_fs_write");
      if (gate) return toResult(gate);
      const working = workingAgentsForRoot(snap, v.root);
      if (working.length > 0 && !confirm_busy) {
        return toResult({ ok: false, reason: "agent_working", root: v.root, working,
          hint: "an agent in this project is working — pass confirm_busy:true to force, or wait for idle/done" });
      }
      let existed = false;
      try { await readFile(v.real); existed = true; } catch { existed = false; }
      if (existed && !overwrite) {
        return toResult({ ok: false, reason: "overwrite_confirmation_required", path: v.resolved,
          hint: "file exists — re-send with overwrite:true (and confirm_dirty:true if dirty)" });
      }
      if (existed && fileDirty(v.root, v.real) && !confirm_dirty) {
        return toResult({ ok: false, reason: "file_dirty_confirmation_required", path: v.resolved,
          hint: "existing file has uncommitted changes — re-send with confirm_dirty:true to overwrite" });
      }
      try { await writeFile(v.real, content, "utf-8"); } catch (e) {
        return toResult({ ok: false, reason: "write_failed", path: v.resolved, message: String(e) });
      }
      return toResult({ ok: true, path: v.resolved, root: v.root, created: !existed, overwritten: existed,
        bytes: Buffer.byteLength(content, "utf-8"),
        ...(working.length > 0 ? { warnings: { working } } : {}) });
    },
  );
  server.registerTool(
    "herdr_prompt",
    {
      description:
        SKILL_BEFORE_AGENT +
        "Send a prompt to a herdr agent via socket agent.prompt (NEVER pane.send_text). " +
        "Prefer herdr_fs_* / herdr_exec when the work is deterministic file/shell IO (no local API burn). " +
        "Target a cheap/fast worker (pi, flash, …) with a self-contained task; do NOT prompt " +
        "Claude/OMP/main to plan or to command other panes — the web client owns orchestration. " +
        "DEFAULT: fire-and-forget (omit wait); confirm with herdr_since / herdr_inspect. " +
        "Strongly prefer idempotency_key (replays return stored result; never auto-retried). " +
        "Returns delivery evidence: submitted, before/after, state_observation " +
        "({changed:true|false|\"unknown\", fresh}), plus legacy state_changed. Blocked target " +
        "-> status 'agent_blocked', submitted:false. Optional wait {until, timeout_ms} is " +
        "submit+wait; a status-wait timeout is failure_phase post_submission_status_wait " +
        "(not a socket transport failure) — verify before re-sending. Worker invariant: " +
        "project root == pane cwd == foreground cwd.",
      inputSchema: {
        target: z.string().describe("Agent name or pane_id to prompt (call herdr_skill first if new session)"),
        text: z.string().describe("Prompt text (multi-line/CJK safe; the server owns submission)"),
        idempotency_key: z.string().optional().describe(
          "STRONGLY RECOMMENDED client key; replay returns stored result without re-sending",
        ),
        wait: z
          .object({
            until: z.array(z.enum(["idle", "working", "blocked", "done", "unknown"])).optional()
              .describe("Agent statuses that end the wait"),
            timeout_ms: z.number().int().min(1).max(HERDR_RPC_TIMEOUT_MAX_MS).optional()
              .describe("Wait budget (max 60s); default 25s when wait is given"),
          })
          .optional()
          .describe("OPTIONAL submit+wait — omit for fire-and-forget (recommended)"),
      },
    },
    async ({ target, text, idempotency_key, wait }) => {
      const gate = mutationDenied(null, "herdr_prompt");
      if (gate) return toResult(gate);
      const c = clientGet();
      // P0-2: idempotent replay — never re-send a non-idempotent prompt.
      if (idempotency_key) {
        const rec = promptRecords.get(idempotency_key);
        if (rec && Date.now() - rec.at < PROMPT_RECORD_TTL_MS) {
          return toResult({ ...rec.result, idempotent_replay: true });
        }
      }
      let before: { pane_id: string | null; agent_status: string | null; state_change_seq: number | null } | null;
      try {
        before = await agentStateOf(c, target);
      } catch (e) {
        const err = e instanceof HerdrError ? e : new HerdrError("error", String(e));
        const result = { ok: false, target, failure_phase: "resolve_before", code: err.code, message: err.message, retryable: err.retryable };
        if (idempotency_key) rememberPrompt(idempotency_key, result);
        return toResult(result);
      }
      const params: Record<string, unknown> = { target, text, wait: wait ?? null };
      const callTimeout = clampHerdrTimeout(wait ? (wait.timeout_ms ?? 25000) + 5000 : 30000);
      let r: HerdrResult;
      try {
        r = await c.call("agent.prompt", params, callTimeout);
      } catch (e) {
        const err = e instanceof HerdrError ? e : new HerdrError("error", String(e));
        const resolveFail = err.code === "agent_not_found" || err.code === "unknown_agent" || err.code === "unknown_pane";
        const afterProbe = await agentStateOf(c, target);
        if (isAgentStatusWaitTimeout(err.message)) {
          const likelyWorking = afterProbe?.agent_status === "working";
          const seqMoved = !!(before && afterProbe
            && before.state_change_seq !== afterProbe.state_change_seq);
          const submitted = likelyWorking || seqMoved ? true : "unknown";
          const result = {
            ok: false,
            target,
            failure: "agent_status_wait_timeout",
            failure_phase: "post_submission_status_wait",
            submitted,
            delivery_uncertain: submitted === "unknown",
            resolved_pane: afterProbe?.pane_id ?? before?.pane_id ?? null,
            before: before ? { agent_status: before.agent_status, state_change_seq: before.state_change_seq } : null,
            after: afterProbe ? { agent_status: afterProbe.agent_status, state_change_seq: afterProbe.state_change_seq } : null,
            ...buildStateObservation({ before, after: afterProbe, waited: true }),
            code: err.code,
            message: err.message,
            retryable: false,
            hint: "status wait timed out after accept — verify with herdr_inspect / herdr_since before re-sending",
            wait: { completed: false, reason: "agent_status_timeout" },
          };
          // Safe to remember when we believe submission landed (blocks blind re-prompt)
          if (idempotency_key && submitted === true) {
            rememberPrompt(idempotency_key, {
              ok: true, target, status: "submitted", submitted: true,
              resolved_pane: result.resolved_pane, before: result.before, after: result.after,
              wait: { completed: false, reason: "agent_status_timeout" },
              idempotent_note: "first call timed out waiting for status; submission already landed",
            });
          }
          return toResult(result);
        }
        if (isHerdrControlPlaneTaskGroup(err.message)) {
          const likelyWorking = afterProbe?.agent_status === "working";
          const seqMoved = !!(before && afterProbe
            && before.state_change_seq !== afterProbe.state_change_seq);
          const submitted = likelyWorking || seqMoved ? true : "unknown";
          const rootMessage = unwrapControlPlaneMessage(err.message);
          return toResult({
            ok: false,
            target,
            failure: "herdr_internal",
            failure_phase: "control_plane_taskgroup",
            code: "control_plane_taskgroup",
            submitted,
            delivery_uncertain: submitted !== true,
            resolved_pane: afterProbe?.pane_id ?? before?.pane_id ?? null,
            before: before ? { agent_status: before.agent_status, state_change_seq: before.state_change_seq } : null,
            after: afterProbe ? { agent_status: afterProbe.agent_status, state_change_seq: afterProbe.state_change_seq } : null,
            ...buildStateObservation({ before, after: afterProbe, waited: !!wait }),
            message: rootMessage,
            error: {
              type: /ExceptionGroup/i.test(err.message) ? "ExceptionGroup" : "TaskGroup",
              message: rootMessage,
              raw: err.message.slice(0, 2000),
            },
            // Never blind-retry agent.prompt: delivery may have landed despite TaskGroup.
            retryable: false,
            hint: "herdr daemon control-plane TaskGroup blip on agent.prompt — pane/agent usually still fine. Check herdr_since / herdr_inspect (status/seq) before any re-prompt; do not treat this as agent dead or as a novo/repo failure.",
          });
        }
        const result = {
          ok: false, target, failure_phase: resolveFail ? "resolve" : "submit_or_response_lost",
          resolved_pane: before?.pane_id ?? null,
          before: before ? { agent_status: before.agent_status, state_change_seq: before.state_change_seq } : null,
          after: afterProbe ? { agent_status: afterProbe.agent_status, state_change_seq: afterProbe.state_change_seq } : null,
          code: err.code, message: err.message, retryable: err.retryable,
          hint: err.retryable
            ? "non-idempotent: a timeout may still have delivered — verify with herdr_explain/herdr_read before re-sending"
            : undefined,
        };
        return toResult(result);
      }
      const pr = (r["prompt"] as Record<string, unknown>) ?? r;
      const status = typeof pr["status"] === "string" ? (pr["status"] as string) : "submitted";
      let after = await agentStateOf(c, target);
      // seq settle: without a native wait the agent may not have bumped seq yet.
      // One short re-poll if unchanged — still NOT a guarantee (queued agents lag).
      if (!wait && before && after && before.state_change_seq === after.state_change_seq) {
        await new Promise<void>((resolve) => setTimeout(resolve, 250));
        after = await agentStateOf(c, target);
      }
      const obs = buildStateObservation({ before, after, waited: !!wait });
      const result = {
        ok: true, target,
        resolved_pane: after?.pane_id ?? before?.pane_id ?? null,
        status, submitted: status !== "agent_blocked",
        before: before ? { agent_status: before.agent_status, state_change_seq: before.state_change_seq } : null,
        after: after ? { agent_status: after.agent_status, state_change_seq: after.state_change_seq } : null,
        ...obs,
        seq_note: !wait ? "seq may lag; state_observation.changed=unknown does NOT prove non-delivery" : undefined,
        prompt: pr,
        ...(idempotency_key ? {} : { idempotency_hint: "pass idempotency_key on mutating prompts to make retries safe" }),
      };
      if (idempotency_key) rememberPrompt(idempotency_key, result);
      return toResult(result);
    },
  );

  if (ALL_TOOLS) {
  server.registerTool(
    "herdr_prompt_status",
    {
      description:
        "Inspect recorded herdr_prompt deliveries by idempotency_key or target (P0-2). " +
        "Use after a transport failure to check whether the prompt was accepted/submitted " +
        "before the response was lost. Returns matching records with accepted_at, submitted, " +
        "status, resolved_pane, before/after state.",
      inputSchema: {
        idempotency_key: z.string().optional(),
        target: z.string().optional(),
      },
    },
    async ({ idempotency_key, target }) => {
      const now = Date.now();
      const out: Record<string, unknown>[] = [];
      for (const [k, v] of promptRecords) {
        if (now - v.at > PROMPT_RECORD_TTL_MS) continue;
        if (idempotency_key && k !== idempotency_key) continue;
        if (target && v.result["target"] !== target) continue;
        out.push({ idempotency_key: k, accepted_at: v.at, ...v.result });
      }
      return toResult({ ok: true, count: out.length, records: out });
    },
  );


  server.registerTool(
    "herdr_transcript",
    {
      description:
        SKILL_BEFORE_AGENT +
        "Read the last N entries of an agent's session transcript (jsonl file). " +
        "Calls 'agent.get' to find the agent_session.value path, then reads the tail of that file. " +
        "Use to review the full conversation/actions an agent has taken (not just recent scrollback).",
      inputSchema: {
        target: z.string().describe("Agent name or pane_id"),
        lines: z.number().default(50).describe("Last N jsonl entries to return"),
      },
    },
    async ({ target, lines }) => {
      const c = clientGet();
      let r: HerdrResult;
      try {
        r = await c.call("agent.get", { target });
      } catch (e) {
        const err = e instanceof HerdrError ? e : new HerdrError("error", String(e));
        return toResult({ ok: false, code: err.code, message: err.message });
      }
      const agent = (r["agent"] ?? {}) as Record<string, unknown>;
      const sess = (agent["agent_session"] ?? {}) as Record<string, unknown>;
      const pathVal = sess["value"];
      if (typeof pathVal !== "string" || pathVal.length === 0) {
        return toResult({ ok: false, reason: "no_session", target });
      }
      try {
        const full = await readFile(pathVal, "utf-8");
        const entries = full
          .split(/\r?\n/)
          .filter((l) => l.trim().length > 0)
          .map((l) => {
            try {
              return JSON.parse(l);
            } catch {
              return l; // not a JSON line — keep raw string
            }
          });
        const last = entries.slice(-lines);
        return toResult({ ok: true, session_path: pathVal, lines: last });
      } catch (e) {
        const err = e instanceof Error ? e : new Error(String(e));
        return toResult({ ok: false, error: err.message, session_path: pathVal });
      }
    },
  );

  server.registerTool(
    "herdr_diff",
    {
      description:
        "Show git diff (stat + full) at a precise scope. target can be: a pane_id " +
        "(e.g. wH:p3), an agent name (resolves to that agent's pane), or a workspace " +
        "id/label (e.g. wH). Resolves to the git PROJECT ROOT(s) for the target's " +
        "panes and returns a projects array — one diff per distinct project, each " +
        "with its own 5000-char budget. Non-git cwds are treated as their own project. " +
        "Use to review uncommitted changes before merging.",
      inputSchema: {
        target: z.string().describe("pane_id (wH:p3), agent name, or workspace id/label (wH)"),
      },
    },
    async ({ target }) => {
      const execP = promisify(exec);
      const diffRoot = async (root: string): Promise<{ stat: string; diff: string; error?: string }> => {
        try {
          const { stdout: stat } = await execP("git diff --stat", { cwd: root, maxBuffer: 10 * 1024 * 1024 });
          const { stdout: diff } = await execP("git diff", { cwd: root, maxBuffer: 10 * 1024 * 1024 });
          return { stat, diff: diff.slice(0, 5000) };
        } catch (e) {
          const err = e instanceof Error ? e : new Error(String(e));
          return { stat: "", diff: "", error: err.message };
        }
      };

      let snapped: HerdrResult;
      try {
        snapped = await clientGet().snapshot();
      } catch (e) {
        const err = e instanceof HerdrError ? e : new HerdrError("error", String(e));
        return toResult({ ok: false, error: `snapshot failed: ${err.message}` });
      }
      const resolved = resolveDiffTargets(snapped, target);
      if (resolved.kind === "not_found") {
        return toResult({ ok: false, error: `target not found: ${target}`, target });
      }
      if (resolved.kind === "ambiguous") {
        return toResult({
          ok: false,
          reason: "ambiguous_target",
          target,
          candidates: resolved.candidates,
          hint: "multiple agents share this name — use pane_id or workspace:name to disambiguate",
        });
      }

      // Derive projects once (per-request cache) and map resolved panes -> roots.
      const projects = deriveProjects(snapped);
      const paneToRoot = new Map<string, string>();
      for (const [, proj] of projects) {
        for (const p of proj.pane_ids) paneToRoot.set(p, proj.root);
      }
      // Group the resolved panes by project root.
      const byRoot = new Map<string, { root: string; panes: string[]; dirty: boolean }>();
      for (const pane of resolved.pane_ids) {
        const root = paneToRoot.get(pane);
        if (!root) continue;
        const entry = byRoot.get(root) ?? { root, panes: [], dirty: projects.get(root)?.dirty ?? false };
        if (!entry.panes.includes(pane)) entry.panes.push(pane);
        byRoot.set(root, entry);
      }
      const projectEntries = [...byRoot.values()];
      const projResults: {
        project_root: string;
        panes: string[];
        stat: string;
        diff: string;
        dirty: boolean;
        changed_files: number;
        vcs: "git" | null;
        managed: boolean;
        error?: string;
        skipped?: string;
      }[] = [];
      for (const entry of projectEntries) {
        const pi = projects.get(entry.root);
        if (pi && !pi.managed) {
          // P1-N: unmanaged projects are never git-scanned.
          projResults.push({
            project_root: entry.root, panes: entry.panes, dirty: false, changed_files: 0,
            vcs: pi.vcs, managed: false, skipped: "unmanaged project", stat: "", diff: "",
          });
          continue;
        }
        const d = await diffRoot(entry.root);
        projResults.push({
          project_root: entry.root, panes: entry.panes, dirty: entry.dirty,
          changed_files: pi?.changed_files ?? 0,
          vcs: pi?.vcs ?? null, managed: pi?.managed ?? false,
          stat: d.stat, diff: d.diff, ...(d.error ? { error: d.error } : {}),
        });
      }
      return toResult({ ok: true, target, projects: projResults });
    },
  );
  }
}

// ---------------------------------------------------------------------------
// OAuth 2.1 / DCR / PKCE authorization server — see ./oauth.ts
// (RFC 8414/9728/7591/7636/9207; persistent registry; refresh rotation).
// Static HERDR_MCP_TOKEN remains valid on /mcp (Claude/curl compat) and the
// DCR + PKCE flow issues opaque tokens.

// ---------------------------------------------------------------------------
// MCP Streamable HTTP (stateful, per-session)
// ---------------------------------------------------------------------------
interface McpSession {
  server: McpServer;
  transport: StreamableHTTPServerTransport;
  tracked?: boolean;
}

const mcpSessions = new Map<string, McpSession>();

/**
 * Persistent stateless (OpenAI/ChatGPT) SSE probe streams. The connector
 * opens a GET and keeps it open; EOF is treated as "transport terminated",
 * so we hold the stream open with heartbeats until the client disconnects.
 * Each entry carries its heartbeat timer so close/abort can clear it and the
 * process can tear everything down on shutdown without a leak.
 */
const statelessSseStreams = new Set<{
  res: Response;
  hb: NodeJS.Timeout;
}>();

function clearStatelessSse(entry: { res: Response; hb: NodeJS.Timeout }): void {
  clearInterval(entry.hb);
  statelessSseStreams.delete(entry);
}


/**
 * Instructions advertised in BOTH the initialize result (official spec field;
 * McpServer options inject it via the SDK) and the server/discover answer.
 * Deliberately terse — points at the right tool per situation, never restates
 * every tool description.
 */
const SERVER_INSTRUCTIONS =
  "Herdr control plane for a WEB planner. Session start: herdr_inspect then herdr_skill (once, " +
  "latest upstream SKILL — tracks herdr master, not pinned). Ladder: (1) herdr_fs_read/list/grep/patch/image + " +
  "herdr_git + herdr_exec (short) / herdr_exec_start|read|kill (long) — zero local-agent API; " +
  "(2) only if agent reasoning is required, herdr_prompt a cheap/fast worker (pi, flash, cline, " +
  "opencode, anti) or auditor (droid, grok) with a self-contained task + idempotency_key; " +
  "inspect/since soft-hide Claude/OMP/Codex (HERDR_MCP_AGENT_ALLOW overrides; prompt by known id still works); " +
  "(3) YOU keep the plan — poll herdr_since / herdr_inspect and re-prompt workers yourself. " +
  "Do NOT herdr_prompt Claude/OMP/main to plan, research, or delegate to other panes. " +
  "Before herdr_call: herdr_skill + herdr_methods. Unknown native API: herdr_methods then herdr_call. " +
  "Before edits require project_root == pane cwd == foreground cwd; use explicit IDs, never UI focus. " +
  "Never blind-retry mutations after uncertain delivery.";

function mcpServerForSession(): McpServer {
  const server = new McpServer(
    { name: SERVER_NAME, version: SERVER_VERSION },
    { capabilities: {}, instructions: SERVER_INSTRUCTIONS },
  );
  registerTools(server);
  return server;
}

/**
 * Register a session in the map and make sure it is removed once its transport
 * closes (DELETE from the client, or any other close path). `server.connect`
 * installs its own `onclose`, so we chain ours after it instead of overwriting.
 * Idempotent: safe to call only once the transport has assigned its session id.
 */
function trackMcpSession(session: McpSession): void {
  const sid = session.transport.sessionId;
  if (session.tracked || !sid) return;
  session.tracked = true;
  const { transport, server } = session;
  const prevOnclose = transport.onclose;
  transport.onclose = () => {
    try {
      prevOnclose?.();
    } finally {
      if (transport.sessionId) {
        mcpSessions.delete(transport.sessionId);
      }
      void server.close();
    }
  };
  mcpSessions.set(sid, session);
}

function handleMcpGet(_req: Request, res: Response): void {
  res.status(405).json({ jsonrpc: "2.0", error: { code: -32000, message: "Method not allowed." }, id: null });
}

/**
 * Clients that MUST be served fully stateless (no Mcp-Session-Id at all):
 * ChatGPT / openai-mcp (UA observed in logs: `openai-mcp/1.0.0`). OpenAI's
 * connector stores the session id returned by initialize and reuses the stale
 * id after a server restart; the new process has no such session, the client
 * then reports JSON-RPC -32600 "Session terminated" and never recovers.
 * Serving it stateless end-to-end (initialize included) means no session id is
 * ever issued, so there is nothing that can go stale.
 *
 * Detection (any match → stateless), order independent of initialize body:
 *  1. User-Agent contains openai-mcp
 *  2. OAuth access-token client_id is a ChatGPT CIMD URL (UA may be stripped)
 *  3. initialize clientInfo name looks like ChatGPT/OpenAI
 */
function isStatelessClient(req: Request): boolean {
  const ua = (req.get("user-agent") ?? "").toLowerCase();
  if (ua.includes("openai-mcp")) return true;
  if (isChatgptOAuthClientId(getRequestOAuthClientId(req))) return true;
  const body = (req.body ?? {}) as Record<string, unknown>;
  if (!Array.isArray(body) && body["method"] === "initialize") {
    const params = (body["params"] ?? {}) as Record<string, unknown>;
    const ci = (params["clientInfo"] ?? {}) as Record<string, unknown>;
    const name = typeof ci["name"] === "string" ? ci["name"].toLowerCase() : "";
    if (name === "chatgpt" || name.includes("openai") || name.includes("chatgpt")) return true;
  }
  return false;
}

/**
 * ChatGPT discover may latch onto 2026-07-28; the SDK only speaks <= 2025-11-25.
 * Rewrite the wire header before the transport validates it, otherwise some
 * later tools/call batches 400 and the connector surfaces Session terminated.
 *
 * Must patch BOTH `req.headers` and `rawHeaders`: @hono/node-server builds the
 * Web Request from rawHeaders, so mutating only the headers object leaves the
 * unsupported version visible to the SDK (observed: 400 Unsupported protocol
 * version: 2026-07-28 while Express req.get already showed 2025-11-25).
 */
function normalizeProtocolVersionHeader(req: Request): void {
  const raw = req.get("mcp-protocol-version");
  if (!raw) return;
  if (raw !== "2026-07-28" && !raw.startsWith("2026-")) return;
  req.headers["mcp-protocol-version"] = SDK_WIRE_PROTOCOL;
  const rh = (req as Request & { rawHeaders?: string[] }).rawHeaders;
  if (!Array.isArray(rh)) return;
  for (let i = 0; i < rh.length; i += 2) {
    if (String(rh[i]).toLowerCase() === "mcp-protocol-version") {
      rh[i + 1] = SDK_WIRE_PROTOCOL;
    }
  }
}

async function handleStatelessMcpRequest(req: Request, res: Response): Promise<void> {
  normalizeProtocolVersionHeader(req);
  const body = (req.body ?? {}) as Record<string, unknown>;
  const method = !Array.isArray(req.body) && typeof body["method"] === "string" ? body["method"] : "";
  // ChatGPT registration (initialize / tools/list) historically completes on SSE.
  // 0.3.6 forced JSON for every openai POST; production then showed OAuth OK +
  // initialize 200, but NEVER a follow-up tools/list — connector connected with
  // zero schemas. Keep SSE for handshake/list; use JSON only for tools/call so
  // long tool payloads stay proxy-safe without breaking schema registration.
  const enableJsonResponse = method === "tools/call";
  const server = mcpServerForSession();
  const transport = new StreamableHTTPServerTransport({
    sessionIdGenerator: undefined,
    onsessionclosed: undefined,
    enableJsonResponse,
  });
  let closed = false;
  const shutdown = (): void => {
    if (closed) return;
    closed = true;
    void transport.close().catch(() => undefined);
    void server.close().catch(() => undefined);
  };
  try {
    await server.connect(transport);
    // Only tear down after the HTTP response fully finishes. Closing inside a
    // finally that races the SDK stream causes SDK `_closed` → 404/-32001
    // "Session not found" (ChatGPT: Session terminated) on in-flight work.
    res.on("finish", shutdown);
    res.on("close", shutdown);
    await transport.handleRequest(req, res, req.body);
  } catch (e) {
    console.error("[herdr-mcp] stateless request error:", e);
    shutdown();
    if (!res.headersSent) {
      res.status(500).json({ jsonrpc: "2.0", error: { code: -32603, message: String(e) }, id: null });
    }
  }
}

async function handleMcpRequest(req: Request, res: Response): Promise<void> {
  // Stateless clients never consult the session map: a stale Mcp-Session-Id on
  // a restarted server must not 404 — that 404/-32001 (surfacing as "Session
  // terminated" in the connector) is exactly the failure this fixes.
  const statelessClient = isStatelessClient(req);
  const sessionId = req.get("mcp-session-id");
  let session = !statelessClient && sessionId ? mcpSessions.get(sessionId) : undefined;
  // Soft-recover: openai/chatgpt with a stale sid must never take the 404 path,
  // even if classification somehow raced. Force session=undefined.
  if (statelessClient) {
    session = undefined;
  }

  // MCP 2026-07-28 clients (e.g. the Claude Connector) probe with the sessionless
  // bootstrap method "server/discover" before any initialize. The SDK (which
  // speaks <= 2025-11-25) rejects pre-initialize requests with HTTP 400 and the
  // connector then reports "no MCP server was found".
  //  - Claude: answer with a DiscoverResult advertising SDK-supported versions;
  //    it negotiates down to the legacy initialize handshake.
  //  - ChatGPT/openai-mcp (post-OAuth): sends discover with Mcp-Protocol-Version
  //    2026-07-28 and a Bearer token. Returning -32601 used to force a legacy
  //    initialize fallback for unauthenticated probes, but after OAuth the
  //    client stops with "Connection problem" and never calls initialize. Advertise
  //    SDK wire versions FIRST, keep 2026-07-28 in the list so discovery still
  //    completes; prefer negotiation onto 2025-11-25 for subsequent POSTs.
  const discoverBody = (req.body ?? {}) as Record<string, unknown>;
  if (!Array.isArray(req.body) && discoverBody["method"] === "server/discover") {
    const ua = (req.get("user-agent") ?? "").toLowerCase();
    const versions = ua.startsWith("openai-mcp") || isChatgptOAuthClientId(getRequestOAuthClientId(req))
      ? [SDK_WIRE_PROTOCOL, "2025-06-18", "2025-03-26", "2024-11-05", "2024-10-07", "2026-07-28"]
      : [SDK_WIRE_PROTOCOL, "2025-06-18", "2025-03-26", "2024-11-05", "2024-10-07"];
    res.status(200).json({
      jsonrpc: "2.0",
      id: discoverBody["id"] ?? null,
      result: {
        resultType: "complete",
        supportedVersions: versions,
        capabilities: { tools: { listChanged: true } },
        instructions: SERVER_INSTRUCTIONS,
        ttlMs: 3600000,
        cacheScope: "private",
        _meta: { "io.modelcontextprotocol/serverInfo": { name: SERVER_NAME, version: SERVER_VERSION } },
      },
    });
    return;
  }

  // A non-initialization request with an unknown session id -> 404 for
  // stateful clients only. Stateless/ChatGPT is exempt (and already forced
  // session=undefined above).
  if (!session && sessionId && !statelessClient) {
    res.status(404).json({ jsonrpc: "2.0", error: { code: -32001, message: "Session not found" }, id: null });
    return;
  }

  // Stateless clients (ChatGPT sends tools/call WITHOUT a session id even
  // after initialize) get a per-request stateless server: handle the single
  // request on a throwaway transport, then close. This path NEVER registers a
  // tracked session, so stateless clients can't leak map entries.
  const body = (req.body ?? {}) as Record<string, unknown>;
  const isNew = !session;
  const isInitialize = !Array.isArray(req.body) && body["method"] === "initialize";
  // Stateless branch: (a) any request from an openai-mcp / ChatGPT-OAuth client
  // — initialize INCLUDED, so the response never carries Mcp-Session-Id — or
  // (b) a sessionless non-initialize request (legacy). Never registers a session.
  if (statelessClient || (isNew && !isInitialize)) {
    await handleStatelessMcpRequest(req, res);
    return;
  }
  if (isNew) {
    normalizeProtocolVersionHeader(req);
    const server = mcpServerForSession();
    const transport = new StreamableHTTPServerTransport({
      sessionIdGenerator: () => randomUUID(),
      onsessionclosed: (cid) => {
        // Client sent DELETE / session ended: drop the map entry. onclose will
        // still run and also call server.close(), so keep both cheap.
        mcpSessions.delete(cid);
      },
    });
    try {
      await server.connect(transport);
    } catch (e) {
      res.status(500).json({ jsonrpc: "2.0", error: { code: -32603, message: String(e) }, id: null });
      return;
    }
    session = { server, transport };
  }

  const transport = session!.transport;
  try {
    normalizeProtocolVersionHeader(req);
    await transport.handleRequest(req, res, req.body);
    // Once the transport has assigned its session id (initialize), register it.
    if (isNew) {
      trackMcpSession(session!);
    }
  } catch (e) {
    console.error("[herdr-mcp] MCP request error:", e);
    if (!res.headersSent) {
      res.status(500).json({ jsonrpc: "2.0", error: { code: -32603, message: String(e) }, id: null });
    }
  }
}

function routes(app: Express): void {
  // --- MCP endpoint (Streamable HTTP) with Bearer auth ---
  app.use(express.json({ limit: "10mb" }));
  app.use(express.urlencoded({ extended: true, limit: "10mb" }));
  // ChatGPT connector UI discovers OAuth from the browser — needs CORS + OPTIONS.
  app.use(oauthCors);

  // --- Browser-extension push channel (herdr → web wake-up) ---
  // GET /push/events (SSE) + GET /push/state, same Bearer token as /mcp.
  registerPushRoutes(app, clientGet);

  // Access log: diagnose Connector protocol failures without logging prompts/tokens.
  // Emits start + finish lines per MCP GET/POST/DELETE. Never logs the
  // Authorization header, request body, prompt, or session contents. The
  // Mcp-Session-Id is emitted as "none" or a short SHA-256 fingerprint (never
  // the raw value); the response's own mcp-session-id is logged as present/none.
  app.use((req, res, next) => {
    const started = Date.now();
    const startedIso = new Date(started).toISOString();
    const requestId = randomUUID().slice(0, 8);
    // isStatelessClient reads req.body (already parsed by express.json above)
    // and optional OAuth client_id attached by mcpBearerAuth (set later for
    // /mcp — first pass may be UA-only; finish line re-checks).
    const rawSid = req.get("mcp-session-id") ?? "";
    const body = (req.body ?? {}) as Record<string, unknown>;
    const params = (body["params"] ?? {}) as Record<string, unknown>;
    const method = !Array.isArray(req.body) && typeof body["method"] === "string" ? body["method"] : "-";
    const tool = typeof params["name"] === "string" ? params["name"] : "-";
    // For herdr_call only: log the native method name (not args) so ChatGPT
    // "TaskGroup / omp died" reports can be correlated without body dumps.
    const toolArgs = (params["arguments"] ?? {}) as Record<string, unknown>;
    const callMethod =
      tool === "herdr_call" && typeof toolArgs["method"] === "string"
        ? String(toolArgs["method"]).slice(0, 64)
        : "";
    const toolExtra = callMethod ? ` call=${callMethod}` : "";
    const uaRaw = (req.get("user-agent") ?? "-");
    const uaFirst = uaRaw === "-" ? "-" : (uaRaw.split(/[\s;]/)[0] || "?");
    const ua = `${uaFirst}(${uaRaw.length})`;
    const protoIn = req.get("mcp-protocol-version") ?? "-";
    console.log(
      `[herdr-mcp] ${startedIso} rid=${requestId} START ${req.method} ${req.originalUrl}` +
        ` method=${method} tool=${tool}${toolExtra} ua=${ua} proto=${protoIn}` +
        ` sid=${rawSid ? "present" : "none"}`,
    );
    res.on("finish", () => { emitTrace(); });
    res.on("close", () => { emitTrace(); });
    let logged = false;
    function emitTrace(): void {
      if (logged) return; // finish + close may both fire for the same request
      logged = true;
      const stateless = isStatelessClient(req);
      const sid = rawSid
        ? (stateless ? "stale(skip)" : `hash:${createHash("sha256").update(rawSid).digest("hex").slice(0, 12)}`)
        : "none";
      let route = req.method === "GET"
        ? (rawSid
            ? (stateless ? "openai-probe(stale)" : (mcpSessions.has(rawSid) ? "sse-stream" : "unknown-session"))
            : (stateless ? "openai-probe" : "unknown-session"))
        : (stateless ? "stateless" : "stateful");
      const authz = req.get("authorization");
      const auth = authz ? (/^Bearer\s/i.test(authz) ? "bearer" : "other") : "none";
      const proto = req.get("mcp-protocol-version") ?? "-";
      const respSid = res.get("mcp-session-id") ? "present" : "none";
      const ct = res.get("content-type") ?? "-";
      const dur = Date.now() - started;
      const lookup = (!stateless && !!rawSid) ? (mcpSessions.has(rawSid) ? "hit" : "miss") : "skipped";
      console.log(
        `[herdr-mcp] ${new Date().toISOString()} rid=${requestId} ${req.method} ${req.originalUrl}` +
          ` -> ${res.statusCode} ${dur}ms method=${method} tool=${tool}${toolExtra} ua=${ua} proto=${proto}` +
          ` sid=${sid} stateless=${stateless} lookup=${lookup} route=${route}` +
          ` auth=${auth} resp-sid=${respSid} ct=${ct}`,
      );
      // Ring-buffer tools/call for extension idle-nudge (talk-without-tools).
      if (method === "tools/call" && typeof tool === "string" && tool !== "-") {
        recordMcpToolCall({
          at: started,
          tool,
          call: callMethod || null,
          ua: uaRaw === "-" ? "-" : uaRaw,
          status: res.statusCode,
        });
      }
    }
    next();
  });
  // OAuth 2.1 discovery / DCR / PKCE endpoints (RFC 8414/9728/7591) — see ./oauth.ts.
  registerOAuthRoutes(app);

  // --- MCP endpoint (Bearer auth) ---
  const mcpHandler = (req: Request, res: Response): void => {
    if (req.method === "GET") {
      // SSE stream open (Streamable HTTP): the client GETs with its session id
      // after initialize to receive server-initiated messages. Hand it to the
      // SDK transport; a GET without a known session is a 400 (not 405/501 —
      // clients treat a rejected probe as "runtime unavailable").
      const sid = req.get("mcp-session-id");
      const sess = sid ? mcpSessions.get(sid) : undefined;
      if (!sess) {
        // OpenAI/ChatGPT (openai-mcp UA) probes a NEW conversation with a
        // sessionless GET before any initialize. It must be a PERSISTENT SSE
        // stream: the connector keeps the GET open and treats an EOF as
        // "transport terminated" (it then refuses to send further requests).
        // Hold the stream open with heartbeats until the client disconnects.
        // No Mcp-Session-Id is issued and no mcpSessions entry is created.
        if (isStatelessClient(req)) {
          res.status(200);
          res.set({
            "Content-Type": "text/event-stream",
            "Cache-Control": "no-cache, no-transform",
            Connection: "keep-alive",
            "X-Accel-Buffering": "no",
          });
          res.flushHeaders();
          res.write(": connected\n\n");
          const hb = setInterval(() => {
            try {
              res.write(": keepalive\n\n");
            } catch {
              /* stream gone */
            }
          }, 15_000);
          const entry = { res, hb };
          statelessSseStreams.add(entry);
          // Cleanup on client disconnect / abort / error: clear the timer and
          // end the response safely. The access-log middleware listens on
          // res close/finish, so it records this GET with the final status.
          const cleanup = () => {
            clearStatelessSse(entry);
            if (!res.destroyed) {
              try { res.end(); } catch { /* already closed */ }
            }
          };
          req.on("close", cleanup);
          req.on("aborted", cleanup);
          res.on("close", cleanup);
          res.on("error", cleanup);
          return;
        }
        res.status(400).json({ jsonrpc: "2.0", error: { code: -32000, message: "Bad Request: session required for GET stream" }, id: null });
        return;
      }
      void sess.transport.handleRequest(req, res);
      return;
    }
    if (req.method === "POST" || req.method === "DELETE") {
      void handleMcpRequest(req, res);
      return;
    }
    res.status(405).end();
  };
  app.use("/mcp", mcpBearerAuth, mcpHandler);
  // ChatGPT/OpenAI completes OAuth against the canonical /mcp resource, then
  // currently probes and invokes JSON-RPC at the issuer root (`/`). Keep /mcp
  // canonical in discovery, but accept this root-path MCP alias. OAuth and
  // well-known routes were registered above, so they remain untouched.
  app.use("/", mcpBearerAuth, mcpHandler);
}

export function start(): void {
  warmSchemaCache();
  const app = express();
  routes(app);
  app.listen(PORT, "127.0.0.1", () => {
    const recovered = recoverExecSessionsOnBoot();
    console.log(`[herdr-mcp] listening on 127.0.0.1:${PORT} boot_id=${BOOT_ID}` +
      (recovered.reaped ? ` reaped_orphans=${recovered.reaped}` : ""));
  });
}

// Run when executed directly (not imported).
import { pathToFileURL } from "node:url";
const entry = process.argv[1] ? pathToFileURL(process.argv[1]).href : null;
if (entry && import.meta.url === entry) {
  start();
}
