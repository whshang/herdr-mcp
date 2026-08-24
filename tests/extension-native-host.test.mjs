import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { rmSync } from "node:fs";
import { createServer } from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(__dirname, "..");
const hostPath = path.join(root, "bin", "herdr-extension-host");
const extensionPath = path.join(root, "extension");
const extensionId = [...createHash("sha256").update(Buffer.from(extensionPath)).digest("hex").slice(0, 32)]
  .map((char) => "abcdefghijklmnop"[Number.parseInt(char, 16)])
  .join("");
const extensionOrigin = `chrome-extension://${extensionId}/`;

function encodeNativeMessage(value) {
  const body = Buffer.from(JSON.stringify(value), "utf8");
  const header = Buffer.alloc(4);
  header.writeUInt32LE(body.length, 0);
  return Buffer.concat([header, body]);
}

function parseNativeFrames(buffer) {
  const frames = [];
  let offset = 0;
  while (offset + 4 <= buffer.length) {
    const size = buffer.readUInt32LE(offset);
    offset += 4;
    if (offset + size > buffer.length) break;
    frames.push(JSON.parse(buffer.subarray(offset, offset + size).toString("utf8")));
    offset += size;
  }
  return frames;
}

async function withIpcServer(handler, fn) {
  const socketPath = path.join(os.tmpdir(), `herdr-native-host-${process.pid}-${Math.random().toString(16).slice(2)}.sock`);
  const server = createServer(handler);
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(socketPath, resolve);
  });
  try {
    return await fn(socketPath);
  } finally {
    await new Promise((resolve) => server.close(() => resolve()));
    try { rmSync(socketPath, { force: true }); } catch {}
  }
}

async function runHost(socketPath, message) {
  const child = spawn(process.execPath, [hostPath, extensionOrigin], {
    env: {
      ...process.env,
      HERDR_EXTENSION_IPC_SOCKET: socketPath,
      HERDR_MCP_TOKEN: "",
    },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const stdout = [];
  const stderr = [];
  child.stdout.on("data", (chunk) => stdout.push(chunk));
  child.stderr.on("data", (chunk) => stderr.push(chunk));
  child.stdin.end(encodeNativeMessage(message));
  const code = await new Promise((resolve) => child.once("exit", resolve));
  assert.equal(code, 0, Buffer.concat(stderr).toString("utf8"));
  return parseNativeFrames(Buffer.concat(stdout));
}

test("native host proxies request over tokenless Unix IPC", async () => {
  await withIpcServer((req, res) => {
    assert.equal(req.url, "/push/state");
    assert.equal(req.headers.authorization, undefined);
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ ok: true, transport: "socket" }));
  }, async (socketPath) => {
    const frames = await runHost(socketPath, {
      type: "request",
      base_url: "http://127.0.0.1:8772",
      path: "/push/state",
      method: "GET",
      headers: { Authorization: "Bearer must-be-stripped" },
    });
    assert.equal(frames.length, 1);
    assert.equal(frames[0].ok, true);
    assert.equal(frames[0].transport, "ipc");
    assert.equal(frames[0].status, 200);
    assert.deepEqual(JSON.parse(frames[0].body), { ok: true, transport: "socket" });
  });
});

test("native host carries SSE bytes over persistent tokenless Unix IPC", async () => {
  await withIpcServer((req, res) => {
    assert.equal(req.url, "/push/events");
    assert.equal(req.headers.authorization, undefined);
    res.writeHead(200, { "Content-Type": "text/event-stream" });
    res.write("event: hello\ndata: {\"ok\":true}\n\n");
    res.end();
  }, async (socketPath) => {
    const frames = await runHost(socketPath, {
      type: "stream",
      base_url: "http://127.0.0.1:8772",
      path: "/push/events",
    });
    assert.equal(frames[0].type, "stream_open");
    assert.equal(frames[0].transport, "ipc");
    assert.equal(frames[0].status, 200);
    const chunks = frames
      .filter((frame) => frame.type === "stream_chunk")
      .map((frame) => Buffer.from(frame.chunk_b64, "base64"));
    assert.equal(Buffer.concat(chunks).toString("utf8"), "event: hello\ndata: {\"ok\":true}\n\n");
    assert.equal(frames.at(-1).type, "stream_end");
  });
});

test("native host refuses arbitrary loopback proxy paths", async () => {
  await withIpcServer((_req, res) => res.end("unexpected"), async (socketPath) => {
    const frames = await runHost(socketPath, {
      type: "request",
      base_url: "http://127.0.0.1:8772",
      path: "/oauth/token",
      method: "GET",
    });
    assert.equal(frames.length, 1);
    assert.equal(frames[0].ok, false);
    assert.equal(frames[0].error, "proxy_path_not_allowed");
  });
});
