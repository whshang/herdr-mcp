#!/usr/bin/env node
/**
 * G6/G7 dogfood default-instance public MCP UAT (2026-08-28).
 * Programmatic OAuth DCR+PKCE (ChatGPT-equivalent); MCP via edge-prod /mcp.
 * No secrets printed.
 */
import { createHash, randomBytes } from "node:crypto";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const ISSUER = "https://herdr-mcp.agentforme.cc.cd";
const MCP_URL = "https://herdr-edge-prod.whshang.workers.dev/mcp";
const RESOURCE = `${ISSUER}/mcp`;
const UA = "openai-mcp/1.0.0";
const OUT = path.join(path.dirname(fileURLToPath(import.meta.url)), "g67-dogfood-public-uat-20260828.json");

const evidence = {
  timestamp: new Date().toISOString(),
  instance: "default (dogfood)",
  runtime_version: "0.4.0-alpha.19",
  connector_urls: {
    workers_dev: MCP_URL,
    custom_domain: `${ISSUER}/mcp`,
    oauth_issuer: ISSUER,
  },
  steps: {},
  failures: [],
  verdict: "UNKNOWN",
};

function pkce() {
  const verifier = randomBytes(48).toString("base64url");
  const challenge = createHash("sha256").update(verifier).digest("base64url");
  return { verifier, challenge };
}

async function postJson(url, payload) {
  const r = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify(payload),
  });
  const text = await r.text();
  let json = null;
  try { json = JSON.parse(text); } catch { /* */ }
  return { status: r.status, json };
}

async function postForm(url, params) {
  const r = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams(params).toString(),
  });
  const text = await r.text();
  let json = null;
  try { json = JSON.parse(text); } catch { /* */ }
  return { status: r.status, json };
}

async function mcpRpc(token, method, params = {}, id = 1) {
  const r = await fetch(MCP_URL, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Accept: "application/json, text/event-stream",
      Authorization: `Bearer ${token}`,
      "User-Agent": UA,
    },
    body: JSON.stringify({ jsonrpc: "2.0", id, method, params }),
  });
  const text = await r.text();
  if (text.startsWith("event:") || text.startsWith("data:")) {
    const dataLine = text.split("\n").find((l) => l.startsWith("data:"));
    const payload = dataLine ? dataLine.slice(5).trim() : text;
    try { return { status: r.status, msg: JSON.parse(payload) }; } catch { return { status: r.status, raw: text.slice(0, 300) }; }
  }
  try { return { status: r.status, msg: JSON.parse(text) }; } catch { return { status: r.status, raw: text.slice(0, 300) }; }
}

async function oauthFlow() {
  const reg = await postJson(`${ISSUER}/oauth/register`, {
    client_name: "g67-uat-20260828",
    redirect_uris: ["https://chatgpt.com/connector/oauth/callback"],
    token_endpoint_auth_method: "none",
    grant_types: ["authorization_code", "refresh_token"],
    response_types: ["code"],
  });
  if (reg.status !== 201) throw new Error(`DCR failed ${reg.status}`);
  const clientId = reg.json.client_id;
  const { verifier, challenge } = pkce();
  const redirectUri = "https://chatgpt.com/connector/oauth/callback";
  const authUrl = `${ISSUER}/oauth/authorize?response_type=code&client_id=${encodeURIComponent(clientId)}` +
    `&redirect_uri=${encodeURIComponent(redirectUri)}&code_challenge=${challenge}` +
    `&code_challenge_method=S256&resource=${encodeURIComponent(RESOURCE)}&scope=mcp`;
  const au = await fetch(authUrl, { redirect: "manual" });
  if (au.status !== 302) throw new Error(`authorize failed ${au.status}`);
  const code = new URL(au.headers.get("location") ?? "").searchParams.get("code");
  const tok = await postForm(`${ISSUER}/oauth/token`, {
    grant_type: "authorization_code",
    code,
    redirect_uri: redirectUri,
    client_id: clientId,
    code_verifier: verifier,
    resource: RESOURCE,
  });
  if (tok.status !== 200) throw new Error(`token failed ${tok.status}`);
  return {
    accessToken: tok.json.access_token,
    tokenType: tok.json.token_type,
    scope: tok.json.scope,
    hasRefresh: !!tok.json.refresh_token,
  };
}

async function callTool(token, name, args, id) {
  const r = await mcpRpc(token, "tools/call", { name, arguments: args }, id);
  const content = r.msg?.result?.content;
  const text = Array.isArray(content) ? content.map((c) => c.text ?? "").join("") : "";
  let parsed = null;
  try { parsed = JSON.parse(text); } catch { parsed = { preview: text.slice(0, 200) }; }
  return { ok: !r.msg?.error && r.status === 200, status: r.status, parsed, error: r.msg?.error };
}

async function main() {
  const health = await fetch("https://herdr-edge-prod.whshang.workers.dev/health");
  const healthJson = await health.json().catch(() => ({}));
  evidence.steps.preflight = { health_http: health.status, contract_epoch: healthJson.contractEpoch ?? null };

  let accessToken;
  try {
    const oauth = await oauthFlow();
    accessToken = oauth.accessToken;
    evidence.steps.oauth = { result: "PASS", token_type: oauth.tokenType, scope: oauth.scope, refresh_issued: oauth.hasRefresh };
  } catch (e) {
    evidence.steps.oauth = { result: "FAIL", error: String(e.message ?? e) };
    evidence.failures.push("oauth");
    evidence.verdict = "BLOCKED";
    writeFileSync(OUT, JSON.stringify(evidence, null, 2));
    console.log(JSON.stringify(evidence, null, 2));
    process.exit(1);
  }

  const init = await mcpRpc(accessToken, "initialize", {
    protocolVersion: "2025-11-25",
    capabilities: {},
    clientInfo: { name: "openai-mcp", version: "1.0.0" },
  }, 1);
  evidence.steps.initialize = { http: init.status, ok: !init.msg?.error };

  const list = await mcpRpc(accessToken, "tools/list", {}, 2);
  const tools = list.msg?.result?.tools ?? [];
  const names = tools.map((t) => t.name).sort();
  evidence.steps.tools_list = { result: names.length === 18 ? "PASS" : "FAIL", tool_count: names.length, tool_names: names };
  if (names.length !== 18) evidence.failures.push("tools_list");

  const inspect = await callTool(accessToken, "herdr_inspect", {}, 10);
  evidence.steps.readonly_inspect = {
    result: inspect.ok ? "PASS" : "FAIL",
    workspace_count: inspect.parsed?.workspace_count ?? inspect.parsed?.workspaces?.length ?? null,
  };
  if (!inspect.ok) evidence.failures.push("readonly_inspect");

  const roots = [
    ...(inspect.parsed?.projects?.map((p) => p.root).filter(Boolean) ?? []),
    ...(inspect.parsed?.shared_projects?.map((p) => p.root ?? p.path).filter(Boolean) ?? []),
    ...(inspect.parsed?.workspaces?.map((w) => w.cwd).filter(Boolean) ?? []),
  ];
  const uatRoot = roots.find((r) => r.includes("herdr-mcp")) ?? roots[0] ?? null;
  evidence.steps.uat_root = uatRoot;

  const git = await callTool(accessToken, "herdr_git", { root: uatRoot, args: ["status", "--short"] }, 11);
  evidence.steps.readonly_git = { result: git.ok ? "PASS" : "PARTIAL", ok: git.ok };

  const listFs = await callTool(accessToken, "herdr_fs_list", { path: uatRoot ?? ".", limit: 5 }, 12);
  evidence.steps.readonly_fs_list = { result: listFs.ok ? "PASS" : "PARTIAL", ok: listFs.ok };

  let mutationOk = false;
  if (uatRoot) {
    const markerPath = `${uatRoot}/.herdr-ga-uat-marker.txt`;
    const m1 = await callTool(accessToken, "herdr_fs_write", {
      path: markerPath,
      content: `ga-uat-${Date.now()}\n`,
      create: true,
      overwrite: true,
      confirm_dirty: true,
    }, 20);
    mutationOk = m1.ok;
    evidence.steps.mutation = { result: m1.ok ? "PASS" : "FAIL", path: markerPath, reason: m1.parsed?.reason ?? null };
    const dup = await callTool(accessToken, "herdr_fs_write", {
      path: markerPath,
      content: "duplicate-block-test\n",
      create: true,
    }, 21);
    evidence.steps.mutation_no_duplicate = {
      result: !dup.ok || dup.parsed?.reason ? "PASS" : "FAIL",
      blocked_reason: dup.parsed?.reason ?? dup.error?.message ?? null,
    };
  } else {
    evidence.steps.mutation = { result: "BLOCKED", reason: "no_managed_root" };
    evidence.failures.push("mutation");
  }
  if (!mutationOk) evidence.failures.push("mutation");

  if (uatRoot) {
    const start = await callTool(accessToken, "herdr_exec_start", {
      root: uatRoot,
      command: "sleep 3 && echo ga-uat-exec-done",
    }, 30);
    const sessionId = start.parsed?.session_id ?? start.parsed?.id;
    evidence.steps.exec_start = { result: start.ok && sessionId ? "PASS" : "FAIL", session_id: sessionId, phase: start.parsed?.phase ?? null };
    let completed = false;
    let polls = 0;
    let lastPhase = null;
    let offset = 0;
    if (sessionId) {
      while (polls < 30 && !completed) {
        await new Promise((r) => setTimeout(r, 500));
        const read = await callTool(accessToken, "herdr_exec_read", { session_id: sessionId, offset }, 40 + polls);
        lastPhase = read.parsed?.phase ?? lastPhase;
        offset = read.parsed?.next_offset ?? offset;
        if (lastPhase === "completed") completed = true;
        polls++;
      }
      evidence.steps.exec_read = { result: completed ? "PASS" : "FAIL", polls, final_phase: lastPhase, next_offset: offset };
      if (!completed) evidence.failures.push("exec_read");
    } else {
      evidence.failures.push("exec_start");
    }
  }

  evidence.verdict = evidence.failures.length === 0 ? "PASS" : "PARTIAL";
  writeFileSync(OUT, JSON.stringify(evidence, null, 2));
  console.log(JSON.stringify(evidence, null, 2));
  process.exit(evidence.failures.length === 0 ? 0 : 1);
}

main().catch((e) => {
  evidence.steps.fatal = String(e);
  evidence.verdict = "BLOCKED";
  writeFileSync(OUT, JSON.stringify(evidence, null, 2));
  console.error(e);
  process.exit(1);
});
