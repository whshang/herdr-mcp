// fs_browse tests: herdr_fs_list + herdr_fs_grep + confirm_busy escape on
// fs_edit/fs_write. Uses a MOCK herdr socket (net server) that answers
// session.snapshot with a snapshot whose managed git root is a temp dir, so
// the fs tools' managed-root gate resolves deterministically without a live
// herdr. Server runs from dist/ on an ephemeral port with a temp token.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import * as net from "node:net";
import * as path from "node:path";
import * as os from "node:os";
import * as fs from "node:fs";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PORT = 9831;
const TOKEN = "fs-browse-token";
const BASE = `http://127.0.0.1:${PORT}`;

let root;          // temp managed git root
let sockPath;      // mock herdr socket path
let sockServer;    // net server
let server;        // MCP server proc
let sessionId = null;

function waitReady(port) {
  return new Promise((resolve, reject) => {
    const deadline = Date.now() + 15000;
    const poll = setInterval(() => {
      const probe = net.connect(port, "127.0.0.1");
      probe.on("connect", () => { clearInterval(poll); probe.destroy(); resolve(); });
      probe.on("error", () => {
        if (Date.now() > deadline) { clearInterval(poll); reject(new Error("server not ready")); return; }
        probe.destroy();
      });
    }, 150);
  });
}

function waitSocket(sock) {
  return new Promise((resolve, reject) => {
    const deadline = Date.now() + 15000;
    const poll = setInterval(() => {
      try { fs.accessSync(sock); clearInterval(poll); resolve(); }
      catch { if (Date.now() > deadline) { clearInterval(poll); reject(new Error("socket not ready")); } }
    }, 100);
  });
}

async function stopServer(proc) {
  if (!proc || proc.exitCode !== null) return;
  proc.kill("SIGTERM");
  await new Promise((resolve) => {
    const t = setTimeout(() => { proc.kill("SIGKILL"); resolve(); }, 3000);
    proc.once("exit", () => { clearTimeout(t); resolve(); });
  });
}

before(async () => {
  // temp managed git root with a few files
  root = fs.mkdtempSync(path.join(os.tmpdir(), "herdr-fs-"));
  root = fs.realpathSync(root); // /var -> /private/var so realpath containment matches
  fs.mkdirSync(path.join(root, "src"), { recursive: true });
  fs.mkdirSync(path.join(root, ".git"), { recursive: true });
  fs.writeFileSync(path.join(root, "README.md"), "hello world\nsecond line\n");
  fs.writeFileSync(path.join(root, "src", "a.ts"), "export const x = 1;\n// TODO fix\n");
  fs.writeFileSync(path.join(root, "src", "b.ts"), "export const y = 2;\n// TODO fix\n");
  fs.writeFileSync(path.join(root, ".env"), "SECRET=abc\n"); // secret — must be skipped
  fs.writeFileSync(path.join(root, "notes.txt"), "plain text\n");
  // make it a real git repo so deriveProjects marks it managed (vcs=git)
  const { execSync } = await import("node:child_process");
  execSync("git init -q", { cwd: root });
  execSync("git add -A", { cwd: root });
  execSync("git -c user.email=t@t -c user.name=t commit -qm init", { cwd: root });

  // mock herdr socket: answer session.snapshot with a managed git root
  sockPath = path.join(os.tmpdir(), `herdr-fs-${process.pid}.sock`);
  sockServer = net.createServer((sock) => {
    let buf = "";
    sock.on("data", (d) => {
      buf += d.toString("utf-8");
      let nl;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, nl); buf = buf.slice(nl + 1);
        let req;
        try { req = JSON.parse(line); } catch { continue; }
        if (req.method === "session.snapshot") {
          const snap = {
            agents: [{ pane_id: "wH:p1", workspace_id: "wH", cwd: root, agent_status: "working", agent: "pi" }],
            panes: [{ pane_id: "wH:p1", workspace_id: "wH", cwd: root }],
            workspaces: [{ workspace_id: "wH", label: "wH", projects: [{ root }] }],
          };
          sock.write(JSON.stringify({ id: req.id, result: { snapshot: snap } }) + "\n");
        } else if (req.method === "ping") {
          sock.write(JSON.stringify({ id: req.id, result: {} }) + "\n");
        } else {
          sock.write(JSON.stringify({ id: req.id, result: {} }) + "\n");
        }
      }
    });
  });
  await new Promise((r) => sockServer.listen(sockPath, r));
  await waitSocket(sockPath);

  server = spawn("node", [path.join(__dirname, "..", "dist", "server.js")], {
    env: { ...process.env, HERDR_MCP_PORT: String(PORT), HERDR_MCP_TOKEN: TOKEN, HERDR_SOCKET_PATH: sockPath },
    stdio: ["ignore", "pipe", "pipe"],
  });
  server.stdout.on("data", () => {});
  server.stderr.on("data", () => {});
  await waitReady(PORT);
});

after(async () => {
  await stopServer(server);
  if (sockServer) await new Promise((r) => sockServer.close(r));
  try { fs.rmSync(root, { recursive: true, force: true }); } catch {}
  try { fs.rmSync(sockPath, { force: true }); } catch {}
});

function headers(extra = {}) {
  return { Authorization: `Bearer ${TOKEN}`, "Content-Type": "application/json",
    Accept: "application/json, text/event-stream", ...extra };
}

async function parseRpc(res) {
  const text = await res.text();
  if (text.trimStart().startsWith("{")) return JSON.parse(text);
  const datas = text.split("\n").filter((l) => l.startsWith("data: ")).map((l) => l.slice(6));
  return JSON.parse(datas[datas.length - 1]);
}

async function rpc(method, params = {}, opts = {}) {
  const h = headers();
  if (sessionId && !opts.noSession) h["Mcp-Session-Id"] = sessionId;
  else if (opts.noSession) delete h["Mcp-Session-Id"];
  const res = await fetch(`${BASE}${opts.path ?? "/"}`, {
    method: "POST", headers: h,
    body: JSON.stringify({ jsonrpc: "2.0", id: Math.floor(Math.random() * 1e9), method, params }),
  });
  const sid = res.headers.get("mcp-session-id");
  if (sid) sessionId = sid;
  return { status: res.status, msg: await parseRpc(res) };
}

async function tool(name, args = {}, opts = {}) {
  const r = await rpc("tools/call", { name, arguments: args }, opts);
  assert.equal(r.msg.error, undefined, `tools/call ${name} error: ${JSON.stringify(r.msg.error)}`);
  return JSON.parse(r.msg.result.content[0].text);
}

test("tools/list exposes the 18 lean tools incl. herdr_fs_list + herdr_fs_grep", async () => {
  const saved = sessionId; sessionId = null;
  try {
    await rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "fs-t", version: "1" } }, { noSession: true });
    const list = await rpc("tools/list", {}, { noSession: true });
    const names = list.msg.result.tools.map((t) => t.name);
    assert.equal(names.length, 18, `default tools/list must be 18, got ${names.length}: ${names.join(",")}`);
    assert.ok(names.includes("herdr_skill"), "herdr_skill registered");
    assert.ok(names.includes("herdr_fs_list"), "herdr_fs_list missing");
    assert.ok(names.includes("herdr_fs_grep"), "herdr_fs_grep missing");
    assert.ok(names.includes("herdr_fs_write"), "herdr_fs_write missing");
    const write = list.msg.result.tools.find((t) => t.name === "herdr_fs_write");
    assert.ok(write?.inputSchema?.properties?.overwrite, "herdr_fs_write.overwrite must be in tools/list schema");
  } finally { sessionId = saved; }
});

test("herdr_fs_list: lists dir, skips .git and secret files", async () => {
  const r = await tool("herdr_fs_list", { path: root });
  assert.equal(r.ok, true, JSON.stringify(r));
  const names = r.entries.map((e) => e.name);
  assert.ok(names.includes("README.md"), "README.md missing");
  assert.ok(names.includes("src"), "src missing");
  assert.ok(!names.includes(".git"), ".git must be skipped");
  assert.ok(!names.includes(".env"), ".env (secret) must be skipped");
  const readme = r.entries.find((e) => e.name === "README.md");
  assert.equal(readme.type, "file");
  assert.ok(typeof readme.size === "number" && readme.size > 0, "file size missing");
});

test("herdr_fs_list: recursive + glob", async () => {
  const r = await tool("herdr_fs_list", { path: root, recursive: true, glob: "*.ts" });
  assert.equal(r.ok, true);
  const names = r.entries.map((e) => e.name);
  assert.ok(names.includes("a.ts") && names.includes("b.ts"), `recursive glob missed ts files: ${names.join(",")}`);
  assert.ok(!names.includes("README.md"), "glob must filter non-ts");
});

test("herdr_fs_list: non-managed path rejected", async () => {
  const r = await tool("herdr_fs_list", { path: "/tmp" });
  assert.equal(r.ok, false);
  assert.equal(r.reason, "outside_managed_roots");
});

test("herdr_fs_grep: literal match, secret excluded", async () => {
  const r = await tool("herdr_fs_grep", { root, pattern: "TODO fix" });
  assert.equal(r.ok, true, JSON.stringify(r));
  assert.equal(r.count, 2, `expected 2 TODO matches, got ${r.count}`);
  for (const m of r.matches) {
    assert.ok(!m.file.includes(".env"), "secret file must be excluded");
    assert.ok(m.file.endsWith(".ts"), `unexpected file ${m.file}`);
  }
});

test("herdr_fs_grep: regex + case_insensitive", async () => {
  const r = await tool("herdr_fs_grep", { root, pattern: "TODO", regex: true, case_insensitive: true });
  assert.equal(r.ok, true);
  assert.equal(r.count, 2);
});

test("herdr_fs_grep: budget truncation sets truncated", async () => {
  const r = await tool("herdr_fs_grep", { root, pattern: "", max_matches: 1 });
  assert.equal(r.ok, true);
  assert.equal(r.count, 1);
  assert.equal(r.truncated, true);
});

test("fs_edit/fs_write: default refuses working agent, confirm_busy forces with warnings.working", async () => {
  // working agent present in mock snapshot for this root
  const editRefuse = await tool("herdr_fs_edit", { path: path.join(root, "README.md"), old_string: "hello", new_string: "hi" });
  assert.equal(editRefuse.ok, false);
  assert.equal(editRefuse.reason, "agent_working");

  const writeRefuse = await tool("herdr_fs_write", { path: path.join(root, "new.txt"), content: "x" });
  assert.equal(writeRefuse.ok, false);
  assert.equal(writeRefuse.reason, "agent_working");

  // confirm_busy forces through with warnings.working
  const editOk = await tool("herdr_fs_edit", { path: path.join(root, "README.md"), old_string: "hello", new_string: "hi", confirm_busy: true });
  assert.equal(editOk.ok, true, JSON.stringify(editOk));
  assert.ok(editOk.warnings?.working?.length > 0, "edit must carry warnings.working");
  assert.equal(fs.readFileSync(path.join(root, "README.md"), "utf-8").startsWith("hi"), true);

  const writeOk = await tool("herdr_fs_write", { path: path.join(root, "new.txt"), content: "x", confirm_busy: true });
  assert.equal(writeOk.ok, true, JSON.stringify(writeOk));
  assert.ok(writeOk.warnings?.working?.length > 0, "write must carry warnings.working");
  assert.equal(fs.readFileSync(path.join(root, "new.txt"), "utf-8"), "x");
});

test("herdr_exec: default refuses working agent (confirm_busy escapes)", async () => {
  const refuse = await tool("herdr_exec", { workspace: "wH", command: "echo hi" });
  assert.equal(refuse.ok, false, JSON.stringify(refuse));
  assert.equal(refuse.reason, "agent_working");
  assert.ok(Array.isArray(refuse.working) && refuse.working.length > 0, "working list expected");
});
