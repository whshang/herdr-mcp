#!/usr/bin/env node
/**
 * L-4 / L-5 integration test — herdr_reap project gate + herdr_session project snapshot.
 *
 * SAFETY: runs against a TEMP MCP instance (port arg) and an ISOLATED herdr workspace
 * built from throwaway git repos under /tmp. It never touches existing workspaces
 * (e.g. wH). Cleanup in `finally` closes only the dedicated test workspace.
 *
 * Usage:
 *   node tests/manual/l45_reap_project.mjs <port> <token>
 */
import { execSync } from "node:child_process";
import { mkdirSync, rmSync, writeFileSync, readFileSync, realpathSync } from "node:fs";

const PORT = process.argv[2] ?? "9793";
const TOKEN = process.argv[3] ?? "testtoken";
const BASE = `http://127.0.0.1:${PORT}/mcp`;

// macOS /tmp is a symlink to /private/tmp; git resolves to the real path,
// so repos must compare against the realpath to match git's --show-toplevel.
mkdirSync("/tmp/herdr-l45-a", { recursive: true });
mkdirSync("/tmp/herdr-l45-b", { recursive: true });
const REPO_A = realpathSync("/tmp/herdr-l45-a");
const REPO_B = realpathSync("/tmp/herdr-l45-b");
const LABEL = `l45-test-${Date.now()}`;

function gitSetup(dir) {
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });
  execSync(`git init -q && git config user.email t@t && git config user.name t && echo hi > f.txt && git add . && git commit -qm init`, { cwd: dir });
}

let sessionId = null;
let wsId = null;
let rootPane = null;
let paneB = null;

// --- minimal MCP streamable-http client (stateful) ---
async function rpc(method, params = {}) {
  const body = { jsonrpc: "2.0", id: Math.floor(Math.random() * 1e9), method, params };
  const headers = {
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
    "Accept": "application/json, text/event-stream",
  };
  if (sessionId) headers["Mcp-Session-Id"] = sessionId;
  const res = await fetch(BASE, { method: "POST", headers, body: JSON.stringify(body) });
  if (!res.ok) throw new Error(`HTTP ${res.status} for ${method}`);
  const sid = res.headers.get("mcp-session-id");
  if (sid) sessionId = sid;
  const text = await res.text();
  const dataLines = text.split("\n").filter((l) => l.startsWith("data: ")).map((l) => l.slice(6)).map((l) => JSON.parse(l));
  const last = dataLines[dataLines.length - 1];
  if (!last) throw new Error(`no data in response for ${method}: ${text}`);
  return last;
}

async function tool(name, args) {
  const r = await rpc("tools/call", { name, arguments: args });
  if (r.error) throw new Error(`tools/call ${name} error: ${JSON.stringify(r.error)}`);
  const content = r.result?.content?.[0]?.text;
  return JSON.parse(content);
}

function sleep(ms) { return new Promise((r) => setTimeout(r, ms)); }

async function pollSnapshot(wsId, minProjects, minPanes) {
  for (let i = 0; i < 20; i++) {
    const insp = await tool("herdr_inspect", {});
    const panes = insp.panes.filter((p) => p.workspace === wsId);
    if (panes.length >= minPanes) return insp;
    await sleep(300);
  }
  return null;
}

function wsPanes(insp, wsId) {
  return (insp?.panes ?? []).filter((p) => p.workspace === wsId).map((p) => p.id);
}

let failed = false;
const checks = [];
function check(name, cond, extra = "") {
  checks.push({ name, ok: !!cond });
  console.log(`  ${cond ? "PASS" : "FAIL"}  ${name}${extra ? ` (${extra})` : ""}`);
  if (!cond) failed = true;
}

(async () => {
  try {
    console.log(`\n=== L-4/L-5 reap project-gate test on :${PORT} ===`);
    console.log(`temp repos: A=${REPO_A} B=${REPO_B}  session=${LABEL}`);
    gitSetup(REPO_A);
    gitSetup(REPO_B);

    // bootstrap MCP session
    const init = await rpc("initialize", { protocolVersion: "2025-03-26", capabilities: {}, clientInfo: { name: "l45-test", version: "1" } });
    console.log("  initialized:", init.result?.serverInfo?.name, init.result?.serverInfo?.version);
    await rpc("notifications/initialized", {});

    // --- L-5: create session rooted at repo A ---
    const created = await tool("herdr_session", { label: LABEL, cwd: REPO_A, resume: false });
    wsId = created.workspace_id;
    rootPane = created.root_pane;
    console.log("  created workspace:", wsId, "root_pane:", rootPane, "resumed:", created.resumed);
    check("herdr_session creates workspace", !!wsId && !!rootPane);
    check("create returns stored projects with repo A only", Array.isArray(created.projects)
      && created.projects.length === 1
      && created.projects[0].root === REPO_A, JSON.stringify(created.projects));

    // stored snapshot on disk matches
    const sessFile = `${process.env.HOME}/.config/herdr-mcp/sessions/${LABEL}.json`;
    const stored = JSON.parse(readFileSync(sessFile, "utf8"));
    check("stored session snapshot contains only repo A", Array.isArray(stored.projects)
      && stored.projects.length === 1 && stored.projects[0].root === REPO_A, JSON.stringify(stored.projects));

    // --- split a SECOND pane into the same workspace but cwd = repo B (cross-project) ---
    await pollSnapshot(wsId, 1, 1);
    const split = await tool("herdr_call", { method: "pane.split", params: { direction: "right", target_pane_id: rootPane, cwd: REPO_B, workspace_id: wsId } });
    paneB = split.result?.pane?.pane_id ?? split.result?.pane_id;
    console.log("  split cross-project pane:", paneB, "(cwd=repo B)");
    check("cross-project pane created", !!paneB);

    // wait until snapshot sees both panes
    const snap2 = await pollSnapshot(wsId, 1, 2);
    check("snapshot shows 2 panes (A + B)", !!snap2, snap2 ? `panes=${wsPanes(snap2, wsId).length}` : "no snapshot");

    // --- L-4: reap WITHOUT force -> multi_project_confirmation_required ---
    // (Since 39dcd4f the multi-project gate is stricter: a workspace with
    // MULTIPLE current projects can never be whole-closed by default — the
    // caller must pick roots via force_projects. This fires before any
    // project_mismatch check, which only applies to single-project workspaces.)
    const mismatch = await tool("herdr_reap", { session: LABEL, close_workspace: true });
    console.log("  reap(no force) ->", mismatch.reason);
    check("no-force reap returns multi_project_confirmation_required", mismatch.ok === false && mismatch.reason === "multi_project_confirmation_required");
    const mA = mismatch.projects.find((p) => p.root === REPO_A);
    const mB = mismatch.projects.find((p) => p.root === REPO_B);
    check("repo A matches_session=true", mA && mA.matches_session === true);
    check("repo B matches_session=false", mB && mB.matches_session === false);

    // both panes still alive after refusal (pane liveness, not agent count)
    const inspRefused = await tool("herdr_inspect", {});
    const aliveAfterRefusal = wsPanes(inspRefused, wsId);
    check("both panes still alive after refusal", aliveAfterRefusal.length === 2 && aliveAfterRefusal.includes(rootPane) && aliveAfterRefusal.includes(paneB), `panes=${aliveAfterRefusal}`);

    // --- L-4: reap with force_projects=[repo A] -> only A's pane closed ---
    const forced = await tool("herdr_reap", { session: LABEL, close_workspace: true, force_projects: [REPO_A] });
    console.log("  reap(force_projects=[A]) -> ok:", forced.ok, "closed_panes:", JSON.stringify(forced.closed_panes), "errors:", JSON.stringify(forced.close_errors || []));
    check("force_projects reap ok", forced.ok === true);
    check("only repo A pane closed", Array.isArray(forced.closed_panes) && forced.closed_panes.includes(rootPane) && !forced.closed_panes.includes(paneB), JSON.stringify(forced.closed_panes));

    // poll: repo A pane gone, repo B pane still alive (SnapshotCache event
    // propagation can lag — give it a 40×500ms = 20s budget)
    let inspFinal = null;
    for (let i = 0; i < 40; i++) {
      inspFinal = await tool("herdr_inspect", {});
      if (!wsPanes(inspFinal, wsId).includes(rootPane)) break;
      await sleep(500);
    }
    const finalPanes = wsPanes(inspFinal, wsId);
    const aGone = !finalPanes.includes(rootPane);
    const bAlive = finalPanes.includes(paneB);
    check("repo A pane is gone", aGone);
    check("repo B pane is STILL ALIVE (cross-project preserved)", bAlive, `panes=${finalPanes}`);

    console.log(`\n=== RESULT: ${failed ? "FAILED" : "ALL PASSED"} (${checks.length} checks) ===`);
  } catch (e) {
    failed = true;
    console.error("  TEST ERROR:", e && e.message ? e.message : e);
    console.error(e && e.stack ? e.stack.split("\n").slice(0, 4).join("\n") : "");
  } finally {
    // --- SAFETY CLEANUP: close ONLY the dedicated test workspace / its panes ---
    console.log("\n  cleanup: closing test workspace", wsId, "panes", JSON.stringify([rootPane, paneB].filter(Boolean)));
    try { await tool("herdr_reap", { session: LABEL, close_workspace: true, force_projects: [REPO_A, REPO_B] }); } catch {}
    try {
      for (const p of [paneB, rootPane].filter(Boolean)) {
        try { await tool("herdr_call", { method: "pane.close", params: { pane_id: p } }); } catch {}
      }
      if (wsId) await tool("herdr_call", { method: "workspace.close", params: { workspace_id: wsId } });
    } catch (e) { console.error("  cleanup error (non-fatal):", e.message); }
    rmSync(REPO_A, { recursive: true, force: true });
    rmSync(REPO_B, { recursive: true, force: true });
  }
  process.exit(failed ? 1 : 0);
})();
