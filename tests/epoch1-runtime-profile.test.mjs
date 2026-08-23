import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import net from "node:net";
import test from "node:test";

import { computeContractHash } from "../dist/relay/contract.js";

const EPOCH1_HASH = "sha256:3f23083ae31b977dad21b1ec9d6919c49e1067a27f7b7eea7bdd021b54770c0d";

async function freePort() {
  return await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      server.close((err) => err ? reject(err) : resolve(port));
    });
  });
}

async function toolsList(base, token) {
  const response = await fetch(`${base}/mcp`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
      Accept: "application/json, text/event-stream",
      "User-Agent": "epoch1-runtime-profile-test/1",
    },
    body: JSON.stringify({ jsonrpc: "2.0", id: "epoch1-list", method: "tools/list", params: {} }),
  });
  const text = await response.text();
  const raw = (response.headers.get("content-type") || "").includes("text/event-stream")
    ? text.split(/\r?\n/).filter((line) => line.startsWith("data: ")).map((line) => line.slice(6)).join("")
    : text;
  return { response, body: JSON.parse(raw) };
}

async function initialize(base, token) {
  const response = await fetch(`${base}/mcp`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
      Accept: "application/json, text/event-stream",
      "User-Agent": "epoch1-runtime-profile-test/1",
    },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: "epoch1-init",
      method: "initialize",
      params: {
        protocolVersion: "2025-06-18",
        capabilities: {},
        clientInfo: { name: "epoch1-runtime-profile-test", version: "1" },
      },
    }),
  });
  const text = await response.text();
  const raw = (response.headers.get("content-type") || "").includes("text/event-stream")
    ? text.split(/\r?\n/).filter((line) => line.startsWith("data: ")).map((line) => line.slice(6)).join("")
    : text;
  return { response, body: JSON.parse(raw) };
}

test("HERDR_MCP_CONTRACT_PROFILE=epoch1 advertises exact frozen 17-tool hash on newer runtime code", async () => {
  const port = await freePort();
  const token = "epoch1-runtime-profile-token";
  const base = `http://127.0.0.1:${port}`;
  const child = spawn(process.execPath, ["dist/server.js"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      HERDR_MCP_PORT: String(port),
      HERDR_MCP_BASE_URL: base,
      HERDR_MCP_TOKEN: token,
      HERDR_MCP_CONTRACT_PROFILE: "epoch1",
      HERDR_SKILL_NETWORK: "0",
    },
    stdio: ["ignore", "ignore", "pipe"],
  });
  let stderr = "";
  child.stderr.on("data", (chunk) => { stderr += String(chunk); });
  try {
    let listed = null;
    for (let attempt = 0; attempt < 50; attempt += 1) {
      try {
        listed = await toolsList(base, token);
        if (listed.response.status === 200) break;
      } catch {
        // startup race
      }
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    assert.ok(listed, `runtime did not start: ${stderr.slice(-1000)}`);
    assert.equal(listed.response.status, 200);
    const tools = listed.body?.result?.tools;
    assert.ok(Array.isArray(tools));
    assert.equal(tools.length, 17);
    assert.equal(tools.some((tool) => tool.name === "herdr_skill"), false);
    assert.equal(computeContractHash(tools), EPOCH1_HASH);

    const initialized = await initialize(base, token);
    assert.equal(initialized.response.status, 200);
    const instructions = initialized.body?.result?.instructions || "";
    assert.match(instructions, /herdr_skill tool is intentionally not exposed/i);
    assert.doesNotMatch(instructions, /then herdr_skill|Before herdr_call: herdr_skill/i);
    assert.match(instructions, /dsh --profile headless via herdr_exec_start/i);
    assert.doesNotMatch(instructions, /herdr_prompt\s+(?:an?\s+)?dsh\b/i);
  } finally {
    child.kill("SIGTERM");
    await new Promise((resolve) => {
      const timer = setTimeout(resolve, 2000);
      child.once("exit", () => { clearTimeout(timer); resolve(); });
    });
  }
});

test("epoch1 profile fails closed when combined with the advanced all-tools surface", async () => {
  const port = await freePort();
  const child = spawn(process.execPath, ["dist/server.js"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      HERDR_MCP_PORT: String(port),
      HERDR_MCP_BASE_URL: `http://127.0.0.1:${port}`,
      HERDR_MCP_TOKEN: "epoch1-conflict-token",
      HERDR_MCP_CONTRACT_PROFILE: "epoch1",
      HERDR_MCP_ALL_TOOLS: "1",
    },
    stdio: ["ignore", "ignore", "pipe"],
  });
  let stderr = "";
  child.stderr.on("data", (chunk) => { stderr += String(chunk); });
  const code = await new Promise((resolve) => child.once("exit", resolve));
  assert.notEqual(code, 0);
  assert.match(stderr, /cannot be combined with HERDR_MCP_ALL_TOOLS=1/);
});
