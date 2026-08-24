import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import net from "node:net";
import test from "node:test";

import { computeContractHash } from "../dist/relay/contract.js";

const EPOCH2_HASH = "sha256:7da23ad2ec8e7703d6380062126ba797218bde9e7711138c6b3e0ca6592efbf8";

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

async function rpc(base, token, method, id) {
  const response = await fetch(`${base}/mcp`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
      Accept: "application/json, text/event-stream",
      "User-Agent": "epoch2-runtime-profile-test/1",
    },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id,
      method,
      params: method === "initialize"
        ? { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "epoch2-runtime-profile-test", version: "1" } }
        : {},
    }),
  });
  const text = await response.text();
  const raw = (response.headers.get("content-type") || "").includes("text/event-stream")
    ? text.split(/\r?\n/).filter((line) => line.startsWith("data: ")).map((line) => line.slice(6)).join("")
    : text;
  return { response, body: JSON.parse(raw) };
}

test("HERDR_MCP_CONTRACT_PROFILE=epoch2 advertises the exact frozen 18-tool contract including herdr_skill", async () => {
  const port = await freePort();
  const token = "epoch2-runtime-profile-token";
  const base = `http://127.0.0.1:${port}`;
  const child = spawn(process.execPath, ["dist/server.js"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      HERDR_MCP_PORT: String(port),
      HERDR_MCP_BASE_URL: base,
      HERDR_MCP_TOKEN: token,
      HERDR_MCP_CONTRACT_PROFILE: "epoch2",
      HERDR_SKILL_NETWORK: "0",
    },
    stdio: ["ignore", "ignore", "pipe"],
  });
  let stderr = "";
  child.stderr.on("data", (chunk) => { stderr += String(chunk); });
  try {
    let listed = null;
    for (let attempt = 0; attempt < 150; attempt += 1) {
      try {
        listed = await rpc(base, token, "tools/list", "epoch2-list");
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
    assert.equal(tools.length, 18);
    assert.equal(tools.some((tool) => tool.name === "herdr_skill"), true);
    assert.equal(computeContractHash(tools), EPOCH2_HASH);

    const initialized = await rpc(base, token, "initialize", "epoch2-init");
    assert.equal(initialized.response.status, 200);
    const instructions = initialized.body?.result?.instructions || "";
    assert.match(instructions, /herdr_skill/i);
    assert.doesNotMatch(instructions, /intentionally not exposed/i);
  } finally {
    child.kill("SIGTERM");
    await new Promise((resolve) => {
      const timer = setTimeout(resolve, 2000);
      child.once("exit", () => { clearTimeout(timer); resolve(); });
    });
  }
});

test("epoch2 profile fails closed when combined with the advanced all-tools surface", async () => {
  const port = await freePort();
  const child = spawn(process.execPath, ["dist/server.js"], {
    cwd: process.cwd(),
    env: {
      ...process.env,
      HERDR_MCP_PORT: String(port),
      HERDR_MCP_TOKEN: "epoch2-conflict-token",
      HERDR_MCP_CONTRACT_PROFILE: "epoch2",
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
