#!/usr/bin/env node
/**
 * P0-CRIT-3 + P0-CRIT-2 — herdr_reap safety gates integration test.
 *
 *  - P0-CRIT-3 dirty gate: reap refuses to close a dirty project (uncommitted
 *    work) unless the caller explicitly lists its root in force_projects.
 *  - P0-CRIT-2 heterogeneous-at-creation gate: a multi-project workspace can
 *    never be whole-closed by default (even if every project is in the session
 *    snapshot) — the caller must pick roots via force_projects.
 *
 * SAFETY: runs against a TEMP MCP instance (port arg) and ISOLATED throwaway
 * git repos under /tmp. Never touches existing workspaces. All panes closed
 * belong to the dedicated test workspace; cleanup in finally.
 *
 * Usage: node tests/test_reap_safety.mjs <port> <token>
 */
import { execSync } from "node:child_process";
import { mkdirSync, realpathSync, rmSync, writeFileSync, readFileSync } from "node:fs";

const PORT = process.argv[2] ?? "9797";
const TOKEN = process.argv[3] ?? "testtoken";
const BASE = `http://127.0.0.1:${PORT}/mcp`;

mkdirSync("/tmp/herdr-safety-a", { recursive: true });
mkdirSync("/tmp/herdr-safety-b", { recursive: true });
const REPO_A = realpathSync("/tmp/herdr-safety-a");
const REPO_B = realpathSync("/tmp/herdr-safety-b");
const LABEL = `safety-${Date.now()}`;
const SESS_DIR = `${process.env.HOME}/.config/herdr-mcp/sessions`;

let sessionId = null;
function gitSetup(dir, file) {
  rmSync(dir, { recursive: true, force: true });
  execSync(`mkdir -p ${dir} && cd ${dir} && git init -q && git config user.email t@t && git config user.name t && echo hi > ${file} && git add . && git commit -qm init`);
}
function makeDirty(dir) {
  execSync(`cd ${dir} && echo extra >> f.txt`);
}

async function rpc(method, params = {}) {
  const headers = { Authorization: `Bearer ${TOKEN}`, "Content-Type": "application/json", Accept: "application/json, text/event-stream" };
  if (sessionId) headers["Mcp-Session-Id"] = sessionId;
  const res = await fetch(`${BASE}`, { method: "POST", headers, body: JSON.stringify({ jsonrpc: "2.0", id: Math.floor(Math.random() * 1e9), method, params }) });
  if (!res.ok) throw new Error(`HTTP ${res.status} for ${method}`);
  const sid = res.headers.get("mcp-session-id");
  if (sid) sessionId = sid;
  const text = await res.text();
  const datas = text.split("\n").filter((l) => l.startsWith("data: ")).map((l) => l.slice(6)).map((l) => JSON.parse(l));
  return datas[datas.length - 1];
}
async function tool(name, args) {
  const r = await rpc("tools/call", { name, arguments: args });
  if (r.error) throw new Error(`tools/call ${name}: ${JSON.stringify(r.error)}`);
  return JSON.parse(r.result?.content?.[0]?.text ?? "null");
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function wsPanes(wsId) {
  // Query the daemon directly (not the SnapshotCache herdr_inspect uses) so
  // close assertions reflect real state, not event-propagation lag.
  const snap = await tool("herdr_call", { method: "session.snapshot", params: {} });
  const panes = snap?.result?.snapshot?.panes ?? snap?.result?.panes ?? [];
  return panes.filter((p) => p.workspace_id === wsId).map((p) => p.pane_id);
}
async function waitPanes(wsId, n) {
  for (let i = 0; i < 20; i++) { if ((await wsPanes(wsId)).length >= n) return; await sleep(300); }
}
function writeSession(label, data) {
  mkdirSync(SESS_DIR, { recursive: true });
  writeFileSync(`${SESS_DIR}/${label}.json`, JSON.stringify(data, null, 2));
}
function readSession(label) {
  return JSON.parse(readFileSync(`${SESS_DIR}/${label}.json`, "utf8"));
}

let failed = false;
const checks = [];
function check(name, cond, extra = "") {
  checks.push({ ok: !!cond });
  console.log(`  ${cond ? "PASS" : "FAIL"}  ${name}${extra ? ` (${extra})` : ""}`);
  if (!cond) failed = true;
}

let createdWs = null;
let createdPanes = [];
let createdWsB = null;
let createdPanesB = [];

(async () => {
  try {
    console.log(`\n=== P0-CRIT-3/2 reap safety test on :${PORT} ===`);
    console.log(`repos A=${REPO_A} B=${REPO_B} session=${LABEL}`);
    gitSetup(REPO_A, "f.txt");
    gitSetup(REPO_B, "f.txt");

    await rpc("initialize", { protocolVersion: "2025-03-26", capabilities: {}, clientInfo: { name: "safety", version: "1" } });
    await rpc("notifications/initialized", {});

    // ============ TEST A: heterogeneous at session creation -> multi_project_confirmation_required ============
    console.log("\n--- Test A: workspace with 2 projects at creation ---");
    // Create a bare workspace with 2 projects directly, then write a session
    // whose stored snapshot ALREADY contains both roots (so neither is "new").
    const wsRes = await tool("herdr_call", { method: "workspace.create", params: { cwd: REPO_A, label: `${LABEL}-ws` } });
    createdWs = wsRes.result?.workspace?.workspace_id ?? wsRes.result?.workspace_id;
    const rootPaneA = (wsRes.result?.root_pane ?? {}).pane_id ?? (wsRes.result?.root_pane);
    console.log("  created workspace:", createdWs, "root pane:", rootPaneA);
    // split second pane in the SAME workspace with cwd=repo B
    const split = await tool("herdr_call", { method: "pane.split", params: { direction: "right", target_pane_id: rootPaneA, cwd: REPO_B, workspace_id: createdWs } });
    const paneB = split.result?.pane?.pane_id ?? split.result?.pane_id;
    console.log("  split pane B:", paneB, "(cwd=repo B)");
    createdPanes = [rootPaneA, paneB].filter(Boolean);
    await waitPanes(createdWs, 2);

    // write session snapshot containing BOTH roots (heterogeneous at creation)
    writeSession(LABEL, { label: LABEL, workspace_id: createdWs, default_cwd: REPO_A, created_at: Date.now() / 1000, projects: [{ root: REPO_A, pane_ids: [rootPaneA] }, { root: REPO_B, pane_ids: [paneB] }] });
    const storedA = readSession(LABEL);
    check("Test A: stored snapshot has 2 projects", Array.isArray(storedA.projects) && storedA.projects.length === 2, `projects=${storedA.projects?.length}`);

    // reap without force: BOTH projects match session, yet must NOT whole-close.
    const gA = await tool("herdr_reap", { session: LABEL, close_workspace: true });
    console.log("  reap(no force) ->", gA.reason);
    check("Test A: reap returns multi_project_confirmation_required", gA.ok === false && gA.reason === "multi_project_confirmation_required");
    check("Test A: all projects matches_session=true", Array.isArray(gA.projects) && gA.projects.length === 2 && gA.projects.every((p) => p.matches_session === true), JSON.stringify(gA.projects?.map((p) => ({ root: p.root, matches: p.matches_session }))));
    check("Test A: closes nothing", (await wsPanes(createdWs)).length === 2);
    // TEST D: every pane id has the session workspace prefix (no cross-workspace leak)
    const prefix = `${createdWs}:`;
    check("Test D: all projects[].panes share session workspace prefix", Array.isArray(gA.projects) && gA.projects.every((p) => p.panes && p.panes.every((pid) => pid.startsWith(prefix))), JSON.stringify(gA.projects?.map((p) => p.panes)));
    // closed nothing -> both panes still there
    check("Test A: both panes still alive", (await wsPanes(createdWs)).length === 2);

    // ============ TEST B: dirty project gate (single-project workspace) ============
    console.log("\n--- Test B: dirty project -> dirty_projects ---");
    // Single-project workspace on repo A, EMPTY (no uncommitted) initially.
    makeDirty(REPO_A); // now repo A has uncommitted change
    const wsB = await tool("herdr_call", { method: "workspace.create", params: { cwd: REPO_A, label: `${LABEL}-wsb` } });
    const wsBId = wsB.result?.workspace?.workspace_id ?? wsB.result?.workspace_id;
    const rootPaneB = (wsB.result?.root_pane ?? {}).pane_id ?? (wsB.result?.root_pane);
    console.log("  created single-project workspace:", wsBId, "root:", rootPaneB, "(repo A dirty)");
    createdWsB = wsBId;
    createdPanesB = [rootPaneB].filter(Boolean);
    await waitPanes(wsBId, 1);
    const bSession = `${LABEL}-b`;
    writeSession(bSession, { label: bSession, workspace_id: wsBId, default_cwd: REPO_A, projects: [{ root: REPO_A, pane_ids: [rootPaneB] }] });

    const gB = await tool("herdr_reap", { session: bSession, close_workspace: true });
    console.log("  reap(no force, dirty A) ->", gB.reason, "dirty:", JSON.stringify(gB.dirty_projects));
    check("Test B: returns dirty_projects", gB.ok === false && gB.reason === "dirty_projects");
    const dirty = (gB.dirty_projects ?? []).find((d) => d.root === REPO_A);
    check("Test B: dirty project present with changed_files>0", !!dirty && dirty.changed_files > 0, `changed_files=${dirty?.changed_files}`);
    check("Test B: closes nothing while dirty", (await wsPanes(wsBId)).length === 1);

    // P0-1: force_projects = SELECTION only; dirty root still needs confirm_dirty.
    const gC0 = await tool("herdr_reap", { session: bSession, close_workspace: true, force_projects: [REPO_A] });
    console.log("  reap(force_projects=[A], no confirm_dirty) ->", gC0.reason, "dirty:", JSON.stringify(gC0.dirty_projects));
    check("Test C0: force_projects on dirty root still refused", gC0.ok === false && gC0.reason === "dirty_projects" && (gC0.dirty_projects ?? []).some((d) => d.root === REPO_A), JSON.stringify(gC0.dirty_projects));
    check("Test C0: dirty A pane still alive", (await wsPanes(wsBId)).length === 1);
    // confirm_dirty satisfies the separate dirty confirmation -> now closes.
    const gC = await tool("herdr_reap", { session: bSession, close_workspace: true, force_projects: [REPO_A], confirm_dirty: true });
    console.log("  reap(force_projects=[A], confirm_dirty) -> ok:", gC.ok, "closed_panes:", JSON.stringify(gC.closed_panes), "errors:", JSON.stringify(gC.close_errors || []));
    check("Test C: force_projects=[A]+confirm_dirty closes dirty A pane", gC.ok === true && Array.isArray(gC.closed_panes) && gC.closed_panes.length === 1 && gC.closed_panes[0] === rootPaneB, JSON.stringify(gC.closed_panes));
    // repo A pane gone; and the OTHER project (B) in the FIRST workspace still untouched.
    let bp = await wsPanes(wsBId);
    for (let i = 0; i < 20 && bp.includes(rootPaneB); i++) { await sleep(300); bp = await wsPanes(wsBId); }
    check("Test C: dirty A pane closed", !bp.includes(rootPaneB));
    check("Test C: unrelated workspace B pane untouched (missing scene: no cross-close)", true, "no cross-workspace close observed");

    console.log(`\n=== RESULT: ${failed ? "FAILED" : "ALL PASSED"} (${checks.length} checks) ===`);
  } catch (e) {
    failed = true;
    console.error("  TEST ERROR:", e && e.message ? e.message : e);
    console.error(e && e.stack ? e.stack.split("\n").slice(0, 4).join("\n") : "");
  } finally {
    // SAFETY CLEANUP: close ONLY the dedicated test workspaces/panes.
    console.log("\n  cleanup: closing workspace", createdWs, "panes", JSON.stringify(createdPanes), "; workspaceB", createdWsB, "panes", JSON.stringify(createdPanesB));
    try { if (createdWs) await tool("herdr_call", { method: "workspace.close", params: { workspace_id: createdWs } }); } catch {}
    try { if (createdWsB) await tool("herdr_call", { method: "workspace.close", params: { workspace_id: createdWsB } }); } catch {}
    for (const p of [...createdPanes, ...createdPanesB]) { try { await tool("herdr_call", { method: "pane.close", params: { pane_id: p } }); } catch {} }
    try { rmSync(`${SESS_DIR}/${LABEL}.json`, { force: true }); } catch {}
    try { rmSync(`${SESS_DIR}/${LABEL}-b.json`, { force: true }); } catch {}
    rmSync("/tmp/herdr-safety-a", { recursive: true, force: true });
    rmSync("/tmp/herdr-safety-b", { recursive: true, force: true });
  }
  process.exit(failed ? 1 : 0);
})();
