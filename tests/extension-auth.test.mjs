import { after, before, test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const TOKEN = "extension-auth-static-test-token";
const EXTENSION_ID = "dklcamincneeijhcelpkdbcekfemldii";
let server;
let port;
let base;
let sessionToken = "";

async function freePort() {
  return await new Promise((resolve, reject) => {
    const probe = net.createServer();
    probe.once("error", reject);
    probe.listen(0, "127.0.0.1", () => {
      const address = probe.address();
      const value = typeof address === "object" && address ? address.port : 0;
      probe.close((error) => error ? reject(error) : resolve(value));
    });
  });
}

async function waitReady(targetPort, proc) {
  const deadline = Date.now() + 10_000;
  for (;;) {
    if (proc.exitCode !== null) throw new Error(`server exited early: ${proc.exitCode}`);
    const ready = await new Promise((resolve) => {
      const socket = net.connect(targetPort, "127.0.0.1");
      socket.once("connect", () => { socket.destroy(); resolve(true); });
      socket.once("error", () => resolve(false));
    });
    if (ready) return;
    if (Date.now() >= deadline) throw new Error("server readiness timeout");
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}

before(async () => {
  port = await freePort();
  base = `http://127.0.0.1:${port}`;
  server = spawn("node", [path.join(__dirname, "..", "dist", "server.js")], {
    env: {
      ...process.env,
      HERDR_MCP_PORT: String(port),
      HERDR_MCP_TOKEN: TOKEN,
      HERDR_MCP_BASE_URL: "",
      HERDR_MCP_CONTRACT_PROFILE: "",
      HERDR_MCP_ALL_TOOLS: "",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  server.stdout.resume();
  server.stderr.resume();
  await waitReady(port, server);
});

after(async () => {
  if (!server || server.exitCode !== null) return;
  server.kill("SIGTERM");
  await new Promise((resolve) => {
    const timer = setTimeout(() => { server.kill("SIGKILL"); resolve(); }, 2000);
    server.once("exit", () => { clearTimeout(timer); resolve(); });
  });
});

test("extension session endpoint does not mint credentials anonymously", async () => {
  const response = await fetch(`${base}/extension/session`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ extension_id: EXTENSION_ID }),
  });
  assert.equal(response.status, 401);
});

test("extension session endpoint rejects a malformed extension id", async () => {
  const response = await fetch(`${base}/extension/session`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${TOKEN}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ extension_id: "not-an-extension-id" }),
  });
  assert.equal(response.status, 400);
  assert.equal((await response.json()).error, "invalid-extension-id");
});

test("static local bearer mints a short-lived extension credential", async () => {
  const response = await fetch(`${base}/extension/session`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${TOKEN}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ extension_id: EXTENSION_ID }),
  });
  assert.equal(response.status, 200);
  assert.match(response.headers.get("cache-control") || "", /no-store/);
  const body = await response.json();
  assert.equal(body.ok, true);
  assert.equal(body.auth_mode, "native_session");
  assert.match(body.token, /^herdr_ext_[A-Za-z0-9_-]+$/);
  assert.equal(body.expires_in, 600);
  assert.ok(Date.parse(body.expires_at) > Date.now());
  sessionToken = body.token;
});

test("extension credential authorizes push routes without exposing the static token", async () => {
  assert.ok(sessionToken);
  assert.notEqual(sessionToken, TOKEN);
  const response = await fetch(`${base}/push/mcp-activity?since=${Date.now() - 1000}`, {
    headers: { Authorization: `Bearer ${sessionToken}` },
  });
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.equal(body.ok, true);
});

test("extension credential is accepted by MCP auth but cannot mint another credential", async () => {
  const mcp = await fetch(`${base}/mcp`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${sessionToken}`,
      "Content-Type": "application/json",
      Accept: "application/json, text/event-stream",
      "Mcp-Protocol-Version": "2025-11-25",
      "X-Herdr-Client": "extension-auth-test",
    },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list", params: {} }),
  });
  assert.notEqual(mcp.status, 401);

  const remint = await fetch(`${base}/extension/session`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${sessionToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ extension_id: EXTENSION_ID }),
  });
  assert.equal(remint.status, 401);
});

test("anonymous push access remains denied and legacy static bearer remains compatible", async () => {
  const url = `${base}/push/mcp-activity?since=${Date.now() - 1000}`;
  assert.equal((await fetch(url)).status, 401);
  assert.equal((await fetch(url, { headers: { Authorization: `Bearer ${TOKEN}` } })).status, 200);
});
