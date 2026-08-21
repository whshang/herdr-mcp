// Transport regression tests (P0): the exact failure classes seen from
// ChatGPT/Claude connectors against herdr-mcp.
//
//  1. stateless: initialize, then tools/list + tools/call WITHOUT Mcp-Session-Id
//  2. stateful: initialize -> carry Mcp-Session-Id through consecutive calls
//  3. keep-alive reuse: raw socket, sequential initialize -> tools/list ->
//     tools/call on ONE connection with the session id carried — responses are
//     byte-length framed and MUST NOT be concatenated / leaked into the next
//     unit (the router Bad-request-syntax desync class). Bodies contain
//     multi-byte UTF-8 (CJK / em-dashes), so framing is asserted on Buffers at
//     byte offsets, never on chunk-decoded strings.
//  4. 100 sequential requests alternating light tools
//  5. reconnect: drop everything, re-initialize, stateless call
//  6. HERDR_MCP_ALL_TOOLS=1 restores the full 30-tool surface (advanced +
//     deprecated), while the default surface stays exactly 17.
//
// Server runs from dist/ on an ephemeral port with a temp token. Never 8772.
// All child processes are terminated in after()/finally; no process outlives
// the test file.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import * as net from "node:net";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const PORT = 9817;
const ALL_PORT = PORT + 1;
const TOKEN = "transport-test-token";
const BASE = `http://127.0.0.1:${PORT}`;

let server;
let sessionId = null;

// OpenAI/ChatGPT connector UA — must be served fully stateless (initialize
// included): no Mcp-Session-Id is ever issued, so a stale id after a server
// restart can never surface as -32600 "Session terminated".
const OPENAI_UA = "openai-mcp/1.0.0";

/** Poll a TCP port until a server accepts connections (or reject after 15s). */
function waitReady(port) {
  return new Promise((resolve, reject) => {
    const deadline = Date.now() + 15000;
    const poll = setInterval(() => {
      const probe = net.connect(port, "127.0.0.1");
      probe.on("connect", () => { clearInterval(poll); probe.destroy(); resolve(); });
      probe.on("error", () => {
        if (Date.now() > deadline) {
          clearInterval(poll);
          reject(new Error(`server on 127.0.0.1:${port} did not become ready in 15s`));
          return;
        }
        probe.destroy();
      });
    }, 150);
  });
}

async function spawnServer(port, extraEnv = {}) {
  const proc = spawn("node", [path.join(__dirname, "..", "dist", "server.js")], {
    env: { ...process.env, HERDR_MCP_PORT: String(port), HERDR_MCP_TOKEN: TOKEN, ...extraEnv },
    stdio: ["ignore", "pipe", "pipe"],
  });
  proc.stdout.on("data", () => {}); // drain — a full pipe would block the child
  proc.stderr.on("data", () => {}); // drain
  await waitReady(port);
  return proc;
}

/** SIGTERM -> await exit -> SIGKILL fallback. Never leaves a child behind. */
async function stopServer(proc) {
  if (!proc || proc.exitCode !== null) return;
  proc.kill("SIGTERM");
  await new Promise((resolve) => {
    const killTimer = setTimeout(() => { proc.kill("SIGKILL"); resolve(); }, 3000);
    proc.once("exit", () => { clearTimeout(killTimer); resolve(); });
  });
}

before(async () => {
  server = await spawnServer(PORT);
});

after(async () => {
  await stopServer(server);
});

function headers(extra = {}) {
  return {
    Authorization: `Bearer ${TOKEN}`,
    "Content-Type": "application/json",
    Accept: "application/json, text/event-stream",
    ...extra,
  };
}

/** Parse a Streamable HTTP response body: JSON or SSE data lines -> last JSON-RPC message. */
async function parseRpc(res) {
  const text = await res.text();
  if (text.trimStart().startsWith("{")) return JSON.parse(text);
  const datas = text.split("\n").filter((l) => l.startsWith("data: ")).map((l) => l.slice(6));
  assert.ok(datas.length > 0, `no JSON-RPC payload in response: ${text.slice(0, 200)}`);
  return JSON.parse(datas[datas.length - 1]);
}

async function rpc(method, params = {}, opts = {}) {
  const base = opts.base ?? BASE;
  const h = headers();
  if (opts.ua) h["User-Agent"] = opts.ua;
  if (opts.sid !== undefined) h["Mcp-Session-Id"] = opts.sid;
  else if (sessionId && !opts.noSession) h["Mcp-Session-Id"] = sessionId;
  else if (opts.noSession) delete h["Mcp-Session-Id"];
  const res = await fetch(`${base}${opts.path ?? "/"}`, {
    method: "POST", headers: h,
    body: JSON.stringify({ jsonrpc: "2.0", id: Math.floor(Math.random() * 1e9), method, params }),
  });
  const sid = res.headers.get("mcp-session-id");
  if (sid) sessionId = sid;
  return { status: res.status, res, msg: await parseRpc(res) };
}

async function tool(name, args = {}, opts = {}) {
  const r = await rpc("tools/call", { name, arguments: args }, opts);
  assert.equal(r.msg.error, undefined, `tools/call ${name} error: ${JSON.stringify(r.msg.error)}`);
  return JSON.parse(r.msg.result.content[0].text);
}

test("stateless: initialize then calls WITHOUT session id (ChatGPT pattern)", async () => {
  // fresh state: do not reuse module sessionId
  const savedSid = sessionId;
  sessionId = null;
  try {
    const init = await rpc("initialize",
      { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "stateless-t", version: "1" } },
      { noSession: true });
    assert.equal(init.status, 200);
    assert.ok(init.msg.result?.serverInfo?.name, "initialize result missing serverInfo");
    // ChatGPT sends tools/call with NO session header at all
    const list = await rpc("tools/list", {}, { noSession: true });
    assert.equal(list.status, 200);
    assert.ok(Array.isArray(list.msg.result?.tools));
    assert.equal(list.msg.result.tools.length, 17, `default tools/list must expose exactly 17 tools, got ${list.msg.result.tools.length}: ${list.msg.result.tools.map((t) => t.name).join(",")}`);
    for (let i = 0; i < 3; i++) {
      const insp = await tool("herdr_methods", { query: "ping" }, { noSession: true });
      assert.equal(insp.ok, true, `stateless herdr_methods #${i}: ${JSON.stringify(insp).slice(0, 120)}`);
      assert.ok(insp.count >= 1);
    }
  } finally {
    sessionId = savedSid;
  }
});

test("stateful: initialize -> session id honored on consecutive calls", async () => {
  const savedSid = sessionId;
  sessionId = null;
  try {
    const init = await rpc("initialize",
      { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "stateful-t", version: "1" } });
    assert.ok(sessionId, "initialize did not return Mcp-Session-Id");
    const note = await rpc("notifications/initialized", {});
    // notifications: 202 accepted (or 200 with empty body depending on SDK version) — no JSON-RPC error payload
    assert.ok(note.status === 202 || note.status === 200, `notification status ${note.status}`);
    const list = await rpc("tools/list", {});
    assert.equal(list.status, 200);
    assert.ok(list.msg.result.tools.length > 0);
    const m = await tool("herdr_methods", { query: "" });
    assert.equal(m.ok, true);
    // unknown session id -> explicit 404 Session not found, NOT a silent tool failure
    const bad = await fetch(`${BASE}/`, {
      method: "POST",
      headers: headers({ "Mcp-Session-Id": "definitely-not-a-session" }),
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list", params: {} }),
    });
    assert.equal(bad.status, 404);
    const badMsg = await parseRpc(bad);
    assert.equal(badMsg.error.code, -32001);
  } finally {
    sessionId = savedSid;
  }
});

test("keep-alive: initialize -> tools/list -> tools/call strictly framed on ONE raw TCP connection", async () => {
  // Drive HTTP by hand so keep-alive framing is asserted, not assumed by fetch.
  // The transport sets Content-Length on JSON and SSE responses alike, so each
  // unit is byte-length framed. Bodies contain multi-byte UTF-8 (CJK / em
  // dashes in tool descriptions), so the parser MUST accumulate Buffers and
  // slice at byte offsets: decoding each chunk to a string corrupts byte
  // accounting whenever a chunk boundary splits a multibyte character and the
  // parser then waits forever for a body that can never complete (the original
  // P0 hang this test guards against).
  const conn = net.connect(PORT, "127.0.0.1");
  conn.setNoDelay(true);
  let buf = Buffer.alloc(0);
  const responses = [];
  let waiter = null; // { resolve, reject }
  let waiterTimer = null;
  let fatal = null;

  const failParse = (err) => {
    fatal = err;
    if (waiter) {
      const w = waiter; waiter = null;
      clearTimeout(waiterTimer);
      w.reject(err);
    }
  };
  const onResponse = (r) => {
    responses.push(r);
    if (waiter) {
      const w = waiter; waiter = null;
      clearTimeout(waiterTimer);
      w.resolve(responses.shift());
    }
  };
  const parseLoop = () => {
    const HTTP_START = Buffer.from("HTTP/1.1");
    // Decode a chunked body at `start`; null while incomplete. Returns the
    // decoded bytes and the absolute end offset (incl. terminating 0-chunk).
    const decodeChunked = (b, start) => {
      let i = start;
      const parts = [];
      while (true) {
        const nl = b.indexOf("\r\n", i);
        if (nl < 0) return null; // size line incomplete
        const size = parseInt(b.subarray(i, nl).toString("utf-8").split(";")[0].trim(), 16);
        if (Number.isNaN(size)) return null;
        i = nl + 2;
        if (size === 0) {
          // 0-chunk: consume trailers up to the terminating empty line
          while (true) {
            const te = b.indexOf("\r\n", i);
            if (te < 0) return null;
            if (te === i) return { body: Buffer.concat(parts), end: te + 2 };
            i = te + 2;
          }
        }
        if (b.length < i + size + 2) return null; // chunk data incomplete
        parts.push(b.subarray(i, i + size));
        i += size + 2;
      }
    };
    while (true) {
      if (buf.length === 0) return;
      if (!(buf.length >= HTTP_START.length && buf.subarray(0, HTTP_START.length).equals(HTTP_START))) {
        // tolerate keep-alive CRLF padding; anything else = desync
        if (!/^[\r\n]/.test(buf.slice(0, 2).toString())) {
          throw new Error(`connection desynced — next bytes are not a status line: ${JSON.stringify(buf.slice(0, 120).toString())}`);
        }
        buf = buf.subarray(2);
        continue;
      }
      const hEnd = buf.indexOf("\r\n\r\n");
      if (hEnd < 0) return; // header incomplete — wait for more bytes
      const head = buf.slice(0, hEnd).toString("utf-8");
      const sm = /HTTP\/1\.1 (\d+)/.exec(head);
      if (!sm) throw new Error(`bad status line: ${head.slice(0, 80)}`);
      const cl = /Content-Length:\s*(\d+)/i.exec(head);
      const chunked = /Transfer-Encoding:\s*chunked/i.test(head);
      let bodyBuf;
      let total;
      if (cl) {
        total = hEnd + 4 + Number(cl[1]);
        if (buf.length < total) return; // body incomplete — wait for more bytes
        bodyBuf = buf.slice(hEnd + 4, total);
      } else if (chunked) {
        const dec = decodeChunked(buf, hEnd + 4);
        if (!dec) return; // chunk stream incomplete — wait for more bytes
        bodyBuf = dec.body;
        total = dec.end;
      } else {
        throw new Error(`response framing unknown (no Content-Length, no chunked): ${head.slice(0, 120)}`);
      }
      const body = bodyBuf.toString("utf-8");
      const sid = /^mcp-session-id:\s*(\S+)/im.exec(head)?.[1] ?? null;
      buf = buf.slice(total);
      onResponse({ status: Number(sm[1]), body, sid });
    }
  };
  conn.on("data", (chunk) => {
    buf = Buffer.concat([buf, chunk]);
    try { parseLoop(); } catch (e) { failParse(e); }
  });
  conn.on("error", (e) => failParse(new Error(`raw connection error: ${e.message}`)));
  conn.on("close", () => {
    if (waiter) {
      const w = waiter; waiter = null;
      clearTimeout(waiterTimer);
      w.reject(new Error("raw connection closed before the next response"));
    }
  });
  await new Promise((r) => conn.once("connect", r));

  const nextResponse = (timeoutMs = 20000) => new Promise((resolve, reject) => {
    if (fatal) return reject(fatal);
    if (responses.length > 0) return resolve(responses.shift());
    waiterTimer = setTimeout(() => {
      waiter = null;
      reject(new Error(`timeout waiting for next response (buffered=${responses.length}, fatal=${fatal?.message ?? "none"})`));
    }, timeoutMs);
    waiter = { resolve, reject };
  });

  let sid = null;
  const mkReq = (method, params, extraHeaders = "") => {
    const body = JSON.stringify({ jsonrpc: "2.0", id: Math.floor(Math.random() * 1e9), method, params });
    return `POST / HTTP/1.1\r\nHost: t\r\nAuthorization: Bearer ${TOKEN}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n${extraHeaders}${sid ? `Mcp-Session-Id: ${sid}\r\n` : ""}Connection: keep-alive\r\nContent-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`;
  };
  async function roundTrip(method, params, extraHeaders = "") {
    conn.write(mkReq(method, params, extraHeaders));
    const r = await nextResponse();
    if (r.sid) sid = r.sid; // capture session id from initialize for later calls
    return r;
  }
  const parseMsg = (body) => JSON.parse(/^data: (.+)$/m.exec(body)?.[1] ?? body);

  try {
    // 1) initialize — assigns the session id carried by every later request
    const init = await roundTrip("initialize",
      { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "keepalive-t", version: "1" } });
    assert.equal(init.status, 200);
    assert.ok(parseMsg(init.body).result?.serverInfo?.name, "initialize result missing serverInfo");
    assert.ok(sid, "initialize did not assign Mcp-Session-Id on the raw connection");

    // 2) tools/list with the session id — exactly the default 17-tool surface
    const list = await roundTrip("tools/list", {});
    assert.equal(list.status, 200);
    const listMsg = parseMsg(list.body);
    assert.ok(Array.isArray(listMsg.result?.tools), "tools/list result missing tools array");
    assert.equal(listMsg.result.tools.length, 17, `default tools/list must be exactly 17, got ${listMsg.result.tools.length}`);

    // 3) tools/call + interleaved methods on the SAME connection (stateful sid)
    for (let i = 0; i < 5; i++) {
      const u = await roundTrip("tools/call", { name: "herdr_methods", arguments: { query: "ping" } });
      assert.equal(u.status, 200, `interleave #${i} status`);
      const msg = parseMsg(u.body);
      assert.ok(msg.result !== undefined || msg.error !== undefined, "non-JSON-RPC body");
      assert.ok(!/Bad request|<!DOCTYPE/i.test(u.body), `protocol garbage leaked into body: ${u.body.slice(0, 120)}`);
    }

    // 4) a malformed-JSON body must yield a clean framed 400 that does NOT
    //    desync the next request on the same connection (the router
    //    Bad-request-syntax class, asserted against Express itself).
    conn.write(`POST / HTTP/1.1\r\nHost: t\r\nAuthorization: Bearer ${TOKEN}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n${sid ? `Mcp-Session-Id: ${sid}\r\n` : ""}Connection: keep-alive\r\nContent-Length: 7\r\n\r\n{broken`);
    const bad = await nextResponse();
    assert.equal(bad.status, 400, `malformed JSON must be a clean 400, got ${bad.status}`);
    const after = await roundTrip("tools/list", {});
    assert.equal(after.status, 200);
    assert.ok(parseMsg(after.body).result, "request after 400 got corrupted");
  } finally {
    conn.destroy();
  }
});

test("stress: 100 sequential requests alternating light tools, no transport errors", async () => {
  const savedSid = sessionId;
  sessionId = null;
  try {
    const init = await rpc("initialize",
      { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "stress-t", version: "1" } });
    assert.equal(init.status, 200);
    let errs = 0;
    for (let i = 0; i < 100; i++) {
      const which = i % 3;
      if (which === 0) {
        const r = await rpc("tools/list", {});
        if (r.status !== 200 || !Array.isArray(r.msg.result?.tools)) errs++;
      } else if (which === 1) {
        const r = await tool("herdr_methods", { query: "ping" });
        if (r.ok !== true) errs++;
      } else {
        const r = await tool("herdr_methods", { query: "agent.start" });
        if (r.ok !== true) errs++;
      }
    }
    assert.equal(errs, 0, `${errs} of 100 sequential requests failed`);
  } finally {
    sessionId = savedSid;
  }
});

test("reconnect: transport dropped -> fresh initialize + stateless call works", async () => {
  const savedSid = sessionId;
  sessionId = null;
  try {
    // simulate client losing its session (server restart class): use an expired sid
    const stale = await fetch(`${BASE}/`, {
      method: "POST",
      headers: headers({ "Mcp-Session-Id": "stale-session-id" }),
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list", params: {} }),
    });
    assert.equal(stale.status, 404, "stale session must be an explicit 404, not a tool failure");
    // client falls back to a clean initialize
    const init = await rpc("initialize",
      { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "reconnect-t", version: "1" } },
      { noSession: true });
    assert.equal(init.status, 200);
    const insp = await tool("herdr_methods", { query: "" }, { noSession: true });
    assert.equal(insp.ok, true);
  } finally {
    sessionId = savedSid;
  }
});

test("GET / without session -> 400 (not 405/501), GET /mcp/ alias parity", async () => {
  const r1 = await fetch(`${BASE}/`, { headers: headers() });
  assert.equal(r1.status, 400);
  const r2 = await fetch(`${BASE}/mcp`, { headers: headers() });
  assert.equal(r2.status, 400);
});

// OpenAI/ChatGPT GET probe recovery: a NEW conversation probes with a
// sessionless GET (openai-mcp UA, no Mcp-Session-Id) before any initialize.
// A 400 here is surfaced by the connector as "invalid_mcp_response" and the
// conversation never recovers. The server must answer with a well-framed,
// short SSE 200 — a valid text/event-stream that creates no session and
// closes immediately (no long-lived connection leak) — while non-OpenAI
// sessionless GETs keep the standard 400/stateful behavior above.
test("openai-mcp UA: sessionless GET probe on / and /mcp -> 200 persistent SSE, no session, no EOF", async () => {
  const savedSid = sessionId;
  sessionId = null;
  try {
    for (const p of ["/", "/mcp"]) {
      const probe = await fetch(`${BASE}${p}`, {
        method: "GET",
        headers: { ...headers(), "User-Agent": OPENAI_UA },
        signal: AbortSignal.timeout(5000),
      });
      assert.equal(probe.status, 200, `openai-mcp GET probe on ${p} must be 200, got ${probe.status}`);
      assert.match(probe.headers.get("content-type") ?? "", /text\/event-stream/i,
        `openai-mcp GET probe on ${p} must be framed as text/event-stream`);
      assert.equal(probe.headers.get("mcp-session-id"), null,
        `openai-mcp GET probe on ${p} must NOT create a session`);
      assert.equal(probe.headers.get("cache-control"), "no-cache, no-transform");
      // Read the FIRST chunk only (never await EOF — the stream stays open).
      // The first bytes must be the keepalive comment, not a JSON-RPC error
      // the connector could misread as invalid_mcp_response.
      const reader = probe.body.getReader();
      const { value } = await reader.read();
      const first = new TextDecoder().decode(value ?? new Uint8Array());
      assert.doesNotMatch(first, /jsonrpc/, `probe first chunk on ${p} must not carry JSON-RPC: ${first}`);
      assert.match(first, /^: connected/, `probe first chunk on ${p} must be the SSE comment`);
      // Abort — the server must clean up the persistent stream without a leak.
      reader.releaseLock();
      probe.body.cancel();
    }
    // The probe must not have created a session: a follow-up stateless
    // initialize still returns NO Mcp-Session-Id, and tools/list works.
    const init = await rpc("initialize", openaiInit, { noSession: true, ua: OPENAI_UA });
    assert.equal(init.status, 200);
    assert.equal(init.res.headers.get("mcp-session-id"), null);
    const list = await rpc("tools/list", {}, { noSession: true, ua: OPENAI_UA });
    assert.equal(list.status, 200);
    assert.equal(list.msg.result.tools.length, 17);
  } finally {
    sessionId = savedSid;
  }
});

test("GET /mcp with a valid session opens SSE stream", async () => {
  const savedSid = sessionId;
  sessionId = null;
  let controller;
  try {
    const init = await rpc("initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "get-sse-t", version: "1" },
    });
    assert.equal(init.status, 200);
    assert.ok(sessionId, "initialize did not return Mcp-Session-Id for GET SSE");
    controller = new AbortController();
    const stream = await fetch(`${BASE}/mcp`, {
      method: "GET",
      headers: headers({ "Mcp-Session-Id": sessionId }),
      signal: controller.signal,
    });
    assert.equal(stream.status, 200);
    assert.match(stream.headers.get("content-type") ?? "", /text\/event-stream/i);
    controller.abort();
  } finally {
    controller?.abort();
    sessionId = savedSid;
  }
});

test("server/discover (sessionless bootstrap probe) answered, never desyncs", async () => {
  const r = await fetch(`${BASE}/`, {
    method: "POST",
    headers: headers(),
    body: JSON.stringify({ jsonrpc: "2.0", id: 7, method: "server/discover", params: {} }),
  });
  assert.equal(r.status, 200);
  const msg = await parseRpc(r);
  assert.ok(msg.result !== undefined || msg.error !== undefined);
  // follow-up request on a NEW connection still fine
  const list = await rpc("tools/list", {}, { noSession: true });
  assert.equal(list.status, 200);
});

test("HERDR_MCP_ALL_TOOLS=1 restores the full 30-tool surface (advanced + deprecated)", async () => {
  const allSrv = await spawnServer(ALL_PORT, { HERDR_MCP_ALL_TOOLS: "1" });
  try {
    const list = await rpc("tools/list", {}, { base: `http://127.0.0.1:${ALL_PORT}`, noSession: true });
    assert.equal(list.status, 200);
    const names = list.msg.result.tools.map((t) => t.name);
    assert.equal(names.length, 30, `all-tools mode must register all 30 tools, got ${names.length}: ${names.join(",")}`);
    const advanced = ["herdr_wait", "herdr_task", "herdr_task_handoff", "herdr_parallel", "herdr_task_reap",
      "herdr_read", "herdr_explain", "herdr_prompt_status", "herdr_transcript", "herdr_diff"];
    const deprecated = ["herdr_session", "herdr_handoff", "herdr_reap"];
    for (const n of [...advanced, ...deprecated]) {
      assert.ok(names.includes(n), `HERDR_MCP_ALL_TOOLS=1 must register ${n}`);
    }
    // the default 17 remain registered too
    const defaults = ["herdr_inspect", "herdr_since", "herdr_methods", "herdr_call", "herdr_prompt",
      "herdr_fs_read", "herdr_fs_list", "herdr_fs_grep", "herdr_fs_write", "herdr_fs_edit", "herdr_exec"];
    for (const n of defaults) {
      assert.ok(names.includes(n), `all-tools mode must keep ${n}`);
    }
  } finally {
    await stopServer(allSrv);
  }
});

// ---------------------------------------------------------------------------
// ChatGPT / openai-mcp stateless transport ("Session terminated" root fix):
// the connector stores the Mcp-Session-Id from initialize and reuses the stale
// id after a server restart; the new process has no such session and the client
// reports JSON-RPC -32600 "Session terminated" without recovering. openai-mcp
// UA traffic is therefore served stateless END-TO-END — initialize included —
// so no session id is ever issued and there is nothing that can go stale.
// ---------------------------------------------------------------------------

const openaiInit = {
  protocolVersion: "2025-06-18",
  capabilities: {},
  clientInfo: { name: "ChatGPT", version: "1" },
};

const openaiToolCall = (name, args = {}) =>
  tool(name, args, { noSession: true, ua: OPENAI_UA });

test("openai-mcp UA: initialize returns NO Mcp-Session-Id on / and /mcp (stateless end-to-end)", async () => {
  const savedSid = sessionId;
  sessionId = null;
  try {
    for (const p of ["/", "/mcp"]) {
      const init = await rpc("initialize", openaiInit, { noSession: true, ua: OPENAI_UA, path: p });
      assert.equal(init.status, 200, `initialize on ${p}`);
      assert.equal(init.res.headers.get("mcp-session-id"), null,
        `openai-mcp initialize on ${p} must NOT return Mcp-Session-Id — issuing one is what goes stale after restart`);
      assert.ok(init.msg.result?.serverInfo?.name, `initialize result missing serverInfo on ${p}`);
      assert.equal(init.msg.result?.serverInfo?.version, "0.3.18", `initialize serverInfo.version on ${p} must be 0.3.18`);
      assert.equal(typeof init.msg.result?.instructions, "string",
        `initialize must carry the instructions field on ${p}`);
    }
  } finally {
    sessionId = savedSid;
  }
});

test("openai-mcp UA: initialize -> tools/list -> tools/call consecutive, stale sid tolerated", async () => {
  const savedSid = sessionId;
  sessionId = null;
  try {
    const init = await rpc("initialize", openaiInit, { noSession: true, ua: OPENAI_UA });
    assert.equal(init.status, 200);
    assert.equal(init.res.headers.get("mcp-session-id"), null);
    const list = await rpc("tools/list", {}, { noSession: true, ua: OPENAI_UA });
    assert.equal(list.status, 200);
    assert.equal(list.msg.result.tools.length, 17,
      `openai-mcp tools/list must expose exactly 17 tools, got ${list.msg.result.tools.length}`);
    // tools/call while still carrying a (now-meaningless) session id header — the
    // restart class: the server must serve it statelessly, never 404 -32001.
    const m = await tool("herdr_methods", { query: "ping" },
      { noSession: true, ua: OPENAI_UA, sid: "stale-session-id" });
    assert.equal(m.ok, true, `stateless tools/call failed: ${JSON.stringify(m).slice(0, 160)}`);
    // notifications/initialized with a stale sid must not error either
    const note = await fetch(`${BASE}/`, {
      method: "POST",
      headers: headers({ "User-Agent": OPENAI_UA, "Mcp-Session-Id": "stale-session-id" }),
      body: JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized", params: {} }),
    });
    assert.ok(note.status === 200 || note.status === 202 || note.status === 204,
      `openai-mcp notification status ${note.status}`);
  } finally {
    sessionId = savedSid;
  }
});

test("openai-mcp UA: server restart on the same port -> stateless tools/call still succeeds (no Session terminated)", async () => {
  const savedSid = sessionId;
  sessionId = null;
  try {
    // handshake against the live server
    const init = await rpc("initialize", openaiInit, { noSession: true, ua: OPENAI_UA });
    assert.equal(init.status, 200);
    assert.equal(init.res.headers.get("mcp-session-id"), null);
    const before = await openaiToolCall("herdr_methods", { query: "ping" });
    assert.equal(before.ok, true);

    // kill + restart on the SAME port — the new process has zero sessions.
    await stopServer(server);
    server = await spawnServer(PORT);

    // The client's stale-session tools/call must still succeed: no 404/-32001,
    // which is exactly what surfaced as JSON-RPC -32600 "Session terminated".
    const after = await tool("herdr_methods", { query: "ping" },
      { noSession: true, ua: OPENAI_UA, sid: "pre-restart-stale-session" });
    assert.equal(after.ok, true, `stateless tools/call after restart failed: ${JSON.stringify(after).slice(0, 200)}`);
    const list = await rpc("tools/list", {}, { noSession: true, ua: OPENAI_UA, sid: "pre-restart-stale-session" });
    assert.equal(list.status, 200);
    assert.equal(list.msg.result.tools.length, 17, `tools/list after restart must stay 17`);
    // a fresh initialize after restart is stateless too
    const init2 = await rpc("initialize", openaiInit, { noSession: true, ua: OPENAI_UA });
    assert.equal(init2.res.headers.get("mcp-session-id"), null);
  } finally {
    sessionId = savedSid;
  }
});

test("openai-mcp UA: 100 sequential stateless calls all succeed", async () => {
  const savedSid = sessionId;
  sessionId = null;
  try {
    let errs = 0;
    for (let i = 0; i < 100; i++) {
      if (i % 2 === 0) {
        const r = await rpc("tools/list", {}, { noSession: true, ua: OPENAI_UA });
        if (r.status !== 200 || !Array.isArray(r.msg.result?.tools)) errs++;
      } else {
        const r = await openaiToolCall("herdr_methods", { query: "ping" });
        if (r.ok !== true) errs++;
      }
    }
    assert.equal(errs, 0, `${errs} of 100 openai-mcp stateless requests failed`);
  } finally {
    sessionId = savedSid;
  }
});

test("stateful client (non-openai UA): initialize still returns Mcp-Session-Id; stale sid still 404 -32001", async () => {
  const savedSid = sessionId;
  sessionId = null;
  try {
    const ua = "claude-connector/1.0";
    for (const p of ["/", "/mcp"]) {
      const init = await rpc("initialize",
        { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "claude-desktop", version: "1" } },
        { noSession: true, ua, path: p });
      assert.equal(init.status, 200, `stateful initialize on ${p}`);
      assert.ok(init.res.headers.get("mcp-session-id"),
        `non-openai initialize on ${p} must return Mcp-Session-Id`);
      const list = await rpc("tools/list", {}, { path: p });
      assert.equal(list.status, 200, `stateful tools/list on ${p} with its session`);
      // unknown sid stays a spec-correct 404 + -32001 for stateful clients
      const bad = await fetch(`${BASE}${p}`, {
        method: "POST",
        headers: headers({ "User-Agent": ua, "Mcp-Session-Id": "definitely-not-a-session" }),
        body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list", params: {} }),
      });
      assert.equal(bad.status, 404, `stale stateful sid on ${p} must 404`);
      const badMsg = await parseRpc(bad);
      assert.equal(badMsg.error.code, -32001);
    }
  } finally {
    sessionId = savedSid;
  }
});

test("openai-mcp UA: server/discover advertises SDK wire first and keeps 2026-07-28", async () => {
  for (const p of ["/", "/mcp"]) {
    const r = await fetch(`${BASE}${p}`, {
      method: "POST",
      headers: headers({
        "User-Agent": OPENAI_UA,
        "Mcp-Protocol-Version": "2026-07-28",
      }),
      body: JSON.stringify({ jsonrpc: "2.0", id: 7, method: "server/discover", params: {} }),
    });
    assert.equal(r.status, 200, `discover on ${p}`);
    const msg = await parseRpc(r);
    assert.equal(msg.error, undefined, `openai-mcp discover on ${p} must not be an error`);
    assert.ok(Array.isArray(msg.result?.supportedVersions), `supportedVersions on ${p}`);
    assert.ok(
      msg.result.supportedVersions.includes("2026-07-28"),
      `openai-mcp discover on ${p} must advertise 2026-07-28`,
    );
    assert.equal(
      msg.result.supportedVersions[0],
      "2025-11-25",
      `openai-mcp discover on ${p} must prefer SDK wire version first`,
    );
  }
});

test("openai-mcp UA: initialize/tools.list stay SSE; tools/call uses JSON", async () => {
  const savedSid = sessionId;
  sessionId = null;
  try {
    const init = await fetch(`${BASE}/mcp`, {
      method: "POST",
      headers: headers({ "User-Agent": OPENAI_UA, Accept: "application/json, text/event-stream" }),
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: openaiInit }),
    });
    assert.equal(init.status, 200);
    assert.match(init.headers.get("content-type") ?? "", /text\/event-stream/i,
      "initialize must stay SSE so ChatGPT continues to tools/list");
    assert.equal(init.headers.get("mcp-session-id"), null);
    const list = await fetch(`${BASE}/mcp`, {
      method: "POST",
      headers: headers({
        "User-Agent": OPENAI_UA,
        Accept: "application/json, text/event-stream",
        "Mcp-Protocol-Version": "2025-11-25",
      }),
      body: JSON.stringify({ jsonrpc: "2.0", id: 2, method: "tools/list", params: {} }),
    });
    assert.equal(list.status, 200);
    assert.match(list.headers.get("content-type") ?? "", /text\/event-stream/i,
      "tools/list must stay SSE for schema registration");
    const listMsg = await parseRpc(list);
    assert.equal(listMsg.result.tools.length, 17);
    assert.ok(listMsg.result.tools.every((t) => t.inputSchema), "every tool needs inputSchema");
    const call = await fetch(`${BASE}/mcp`, {
      method: "POST",
      headers: headers({
        "User-Agent": OPENAI_UA,
        Accept: "application/json, text/event-stream",
        "Mcp-Protocol-Version": "2026-07-28",
      }),
      body: JSON.stringify({
        jsonrpc: "2.0",
        id: 9,
        method: "tools/call",
        params: { name: "herdr_methods", arguments: { query: "ping" } },
      }),
    });
    assert.equal(call.status, 200);
    assert.match(call.headers.get("content-type") ?? "", /application\/json/i,
      "tools/call may use JSON for proxy-safe payloads");
    const msg = await parseRpc(call);
    assert.ok(msg.result?.content?.[0]?.text, "tools/call must return tool content");
    assert.doesNotMatch(JSON.stringify(msg), /-32001|Session not found|Session terminated/);
  } finally {
    sessionId = savedSid;
  }
});

// Non-OpenAI discover advertises the serverInfo identity (version).
test("server/discover (non-openai) result _meta serverInfo.version is 0.3.18", async () => {
  for (const p of ["/", "/mcp"]) {
    const r = await fetch(`${BASE}${p}`, {
      method: "POST",
      headers: headers({ "User-Agent": "claude-connector/1.0" }),
      body: JSON.stringify({ jsonrpc: "2.0", id: 8, method: "server/discover", params: {} }),
    });
    assert.equal(r.status, 200, `discover on ${p}`);
    const msg = await parseRpc(r);
    const si = msg.result?._meta?.["io.modelcontextprotocol/serverInfo"];
    assert.ok(si, `discover _meta serverInfo missing on ${p}`);
    assert.equal(si.name, "herdr-mcp", `discover serverInfo.name on ${p}`);
    assert.equal(si.version, "0.3.18", `discover serverInfo.version on ${p} must be 0.3.18, got ${si.version}`);
  }
});

// Cache-key regression: an OpenAI client that previously cached a 0.2.0
// catalog MUST, upon seeing the new 0.3.18 identity (initialize serverInfo +
// mcp.json + discover), re-run tools/list and obtain the current 17 tools.
// We assert all identity surfaces agree on 0.3.18 and that a fresh tools/list
// returns exactly the 17 default tools (i.e. a re-fetch after a version bump
// does NOT resurrect the old 22-tool surface).
test("cache-key regression: 0.3.18 identity consistent, fresh tools/list = 17 (no stale 22)", async () => {
  const savedSid = sessionId;
  sessionId = null;
  try {
    // initialize -> serverInfo.version
    for (const p of ["/", "/mcp"]) {
      const init = await rpc("initialize", openaiInit, { noSession: true, ua: OPENAI_UA, path: p });
      assert.equal(init.msg.result?.serverInfo?.version, "0.3.18", `initialize ${p} version`);
      const list = await rpc("tools/list", {}, { noSession: true, ua: OPENAI_UA, path: p });
      const names = list.msg.result.tools.map((t) => t.name);
      assert.equal(names.length, 17, `fresh tools/list on ${p} must be 17 after version bump, got ${names.length}`);
      assert.ok(!names.includes("herdr_session") && !names.includes("herdr_reap"),
        `fresh tools/list on ${p} must NOT contain 22-only tools`);
    }
    // mcp.json version
    const card = await fetch(`${BASE}/.well-known/mcp.json`, { headers: { Authorization: `Bearer ${TOKEN}` } });
    assert.equal(card.status, 200);
    const cardJson = await card.json();
    assert.equal(cardJson.version, "0.3.18", `mcp.json version must be 0.3.18, got ${cardJson.version}`);
  } finally {
    sessionId = savedSid;
  }
});

// Poisoned-session full-chain regression: an OpenAI/ChatGPT conversation whose
// transport has latched onto a stale Mcp-Session-Id (e.g. after a server
// restart) must still complete the WHOLE MCP flow — GET probe, initialize,
// tools/list, tools/call herdr_inspect, tools/call herdr_since — with that old
// sid carried on EVERY step. Every response must be 200/202, must NOT echo an
// mcp-session-id, and must never surface -32600/-32001 / "Session terminated".
const POISON = "poisoned-stale-session-0001";
async function openaiRpcRaw(path, payload, sid) {
  const h = headers({ "User-Agent": OPENAI_UA });
  if (sid) h["Mcp-Session-Id"] = sid;
  else delete h["Mcp-Session-Id"];
  const res = await fetch(`${BASE}${path}`, {
    method: "POST", headers: h,
    body: JSON.stringify(payload),
  });
  const sidResp = res.headers.get("mcp-session-id");
  return { status: res.status, sidResp, msg: await parseRpc(res), res };
}
async function openaiChain(path) {
  const out = {};
  // GET probe with poisoned sid — read first chunk then cancel (no EOF await).
  const probe = await fetch(`${BASE}${path}`, {
    method: "GET", headers: { ...headers(), "User-Agent": OPENAI_UA, "Mcp-Session-Id": POISON },
    signal: AbortSignal.timeout(5000),
  });
  out.probe = { status: probe.status, ct: probe.headers.get("content-type"), sid: probe.headers.get("mcp-session-id") };
  const reader = probe.body.getReader();
  const { value } = await reader.read();
  const first = new TextDecoder().decode(value ?? new Uint8Array());
  out.probeFirst = first;
  reader.releaseLock();
  await probe.body.cancel();
  // GET probe again (round coverage on GET) — also read-first then abort.
  const probe2 = await fetch(`${BASE}${path}`, {
    method: "GET", headers: { ...headers(), "User-Agent": OPENAI_UA, "Mcp-Session-Id": POISON },
    signal: AbortSignal.timeout(5000),
  });
  out.probe2 = probe2.status;
  await probe2.body?.cancel();
  // initialize (carries poisoned sid)
  const init = await openaiRpcRaw(path, { jsonrpc: "2.0", id: 1, method: "initialize", params: openaiInit }, POISON);
  out.init = { status: init.status, sid: init.sidResp, ok: !!init.msg.result?.serverInfo };
  // tools/list
  const list = await openaiRpcRaw(path, { jsonrpc: "2.0", id: 2, method: "tools/list", params: {} }, POISON);
  out.list = { status: list.status, sid: list.sidResp, tools: list.msg.result?.tools?.length };
  // tools/call herdr_inspect
  const insp = await openaiRpcRaw(path, { jsonrpc: "2.0", id: 3, method: "tools/call", params: { name: "herdr_inspect", arguments: {} } }, POISON);
  out.inspect = { status: insp.status, sid: insp.sidResp, text: insp.msg.result?.content?.[0]?.text };
  // tools/call herdr_since
  const since = await openaiRpcRaw(path, { jsonrpc: "2.0", id: 4, method: "tools/call", params: { name: "herdr_since", arguments: { cursor: 0, workspace: "w44" } } }, POISON);
  out.since = { status: since.status, sid: since.sidResp, text: since.msg.result?.content?.[0]?.text };
  return out;
}
function assertPoisonChain(out, label) {
  for (const [k, v] of Object.entries(out)) {
    // probeFirst is a raw string chunk, not a {status} wrapper — skip it here
    // (asserted separately below).
    if (k === "probeFirst") continue;
    const st = typeof v === "object" && v !== null && "status" in v ? v.status : v;
    assert.equal(st, 200, `${label} ${k} must be 200, got ${st}`);
  }
  assert.equal(out.probe.sid, null, `${label} GET probe must not return a session id`);
  assert.match(out.probe.ct ?? "", /text\/event-stream/i, `${label} GET probe must be SSE`);
  assert.match(out.probeFirst ?? "", /^: connected/, `${label} probe first chunk must be the SSE comment`);
  assert.doesNotMatch(out.probeFirst ?? "", /jsonrpc/, `${label} probe first chunk must not carry JSON-RPC`);
  assert.equal(out.init.sid, null, `${label} initialize must not return a session id`);
  assert.equal(out.list.sid, null, `${label} tools/list must not return a session id`);
  assert.equal(out.inspect.sid, null, `${label} herdr_inspect must not return a session id`);
  assert.equal(out.since.sid, null, `${label} herdr_since must not return a session id`);
  assert.doesNotMatch(JSON.stringify(out), /-32600|-32001|Session terminated|invalid_mcp_response/, `${label} must not surface termination markers`);
  assert.ok(JSON.parse(out.inspect.text).focused_workspace, `${label} inspect must return focused_workspace`);
  assert.ok(out.since.text.includes("cursor"), `${label} since must return cursor`);
}

test("openai-mcp UA poisoned-session: stale sid carried on EVERY step, GET/init/list/inspect/since all 200, no sid, two rounds", async () => {
  const savedSid = sessionId;
  sessionId = null;
  try {
    for (let round = 1; round <= 2; round++) {
      for (const p of ["/", "/mcp"]) {
        const out = await openaiChain(p);
        assertPoisonChain(out, `round${round}:${p}`);
      }
    }
  } finally {
    sessionId = savedSid;
  }
});

test("openai-mcp UA poisoned-session: full chain STILL 200 after server restart (no Session terminated)", async () => {
  const savedSid = sessionId;
  sessionId = null;
  try {
    await stopServer(server);
    server = await spawnServer(PORT);
    for (const p of ["/", "/mcp"]) {
      const out = await openaiChain(p);
      assertPoisonChain(out, `restart:${p}`);
    }
  } finally {
    sessionId = savedSid;
  }
});

test("stateful client (non-openai UA): poisoned/stale sid still 404 -32001 (reverse guard)", async () => {
  const savedSid = sessionId;
  sessionId = null;
  try {
    const ua = "claude-connector/1.0";
    const sid = "poisoned-stateful-sid";
    // GET with poisoned sid -> must stay 400 (not 200 SSE)
    const probe = await fetch(`${BASE}/`, {
      method: "GET", headers: headers({ "User-Agent": ua, "Mcp-Session-Id": sid }),
      signal: AbortSignal.timeout(5000),
    });
    assert.equal(probe.status, 400, `stateful GET with unknown sid must 400, got ${probe.status}`);
    // POST tools/list with poisoned sid -> must 404 -32001
    const bad = await fetch(`${BASE}/`, {
      method: "POST", headers: headers({ "User-Agent": ua, "Mcp-Session-Id": sid }),
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list", params: {} }),
    });
    assert.equal(bad.status, 404, `stateful POST unknown sid must 404`);
    const badMsg = await parseRpc(bad);
    assert.equal(badMsg.error.code, -32001);
  } finally {
    sessionId = savedSid;
  }
});

// ---------------------------------------------------------------------------
// Real-client state machine (Grok verifier repro): a ChatGPT-like transport
// keeps the GET SSE stream open and, when it hits EOF, marks itself
// "terminated" and refuses to send the NEXT request (inspect 200 -> since
// never leaves the client). With a PERSISTENT server SSE (no EOF until the
// client aborts), the second tools/call must still be sent and succeed.
//
// We use the actual @modelcontextprotocol/sdk StreamableHTTPClientTransport
// and hold the GET reader open (never awaiting EOF) to exercise the same
// connection semantics as the connector.
// ---------------------------------------------------------------------------

test("real SDK client: persistent GET SSE held open; inspect -> since both actually sent; abort -> server cleans up", async () => {
  const savedSid = sessionId;
  sessionId = null;
  let heldGet = null;
  try {
    const transport = new StreamableHTTPClientTransport(new URL(`${BASE}/`), {
      requestInit: { headers: { Authorization: `Bearer ${TOKEN}`, "User-Agent": OPENAI_UA } },
    });
    const messages = [];
    const errors = [];
    transport.onmessage = (m) => { messages.push(m); };
    transport.onerror = (e) => { errors.push(e?.message ?? String(e)); };
    await transport.start();

    // initialize (stateless: no session id)
    await transport.send({
      jsonrpc: "2.0", id: "init", method: "initialize",
      params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "ChatGPT", version: "1" } },
    });
    await new Promise((r) => setTimeout(r, 300));
    assert.equal(transport.sessionId, undefined, "openai-mcp initialize must NOT yield a session id");

    // notifications/initialized -> SDK client opens the persistent GET SSE.
    // We hold that GET open (persistent SSE). Grab its response and keep a
    // reader so it does not get GC'd; never await EOF.
    const probe = await fetch(`${BASE}/`, {
      method: "GET",
      headers: { Authorization: `Bearer ${TOKEN}`, "User-Agent": OPENAI_UA },
    });
    heldGet = probe;
    assert.equal(probe.status, 200, "persistent GET must be 200");
    const reader = probe.body.getReader();
    const { value } = await reader.read();
    assert.match(new TextDecoder().decode(value ?? new Uint8Array()), /^: connected/, "first chunk must be SSE comment");
    // keep reader open; the stream must NOT EOF

    // first tools/call -> herdr_inspect
    await transport.send({
      jsonrpc: "2.0", id: "insp", method: "tools/call",
      params: { name: "herdr_inspect", arguments: {} },
    });
    await new Promise((r) => setTimeout(r, 700));
    // second tools/call -> herdr_since (the call that used to never go out)
    await transport.send({
      jsonrpc: "2.0", id: "since", method: "tools/call",
      params: { name: "herdr_since", arguments: { cursor: 0, workspace: "w44" } },
    });
    await new Promise((r) => setTimeout(r, 900));

    const byId = Object.fromEntries(messages.filter((m) => m.id !== undefined).map((m) => [m.id, m]));
    assert.ok(byId["insp"]?.result, "herdr_inspect result must be received by the client");
    assert.ok(JSON.parse(byId.insp.result.content[0].text).focused_workspace, "inspect must carry focused_workspace");
    assert.ok(byId["since"] && byId.since.result?.content?.[0]?.text?.includes("cursor"),
      "herdr_since must be received (the second call actually went out), got ids: " + Object.keys(byId).join(","));
    assert.equal(errors.length, 0, `no client errors: ${errors.join("; ")}`);

    // cleanup: abort the held GET -> server must tear down the stream
    reader.releaseLock();
    await probe.body.cancel();
    await transport.close();
    // A tiny wait to let the server process the abort cleanly.
    await new Promise((r) => setTimeout(r, 200));
  } finally {
    await heldGet?.body?.cancel().catch(() => undefined);
    sessionId = savedSid;
  }
});

// ChatGPT-like transport EOF=>terminated simulator: proves that with a
// PERSISTENT server stream the client does NOT hit EOF (so it never flips to
// "terminated"), and that the second call still reaches the server. If the
// server sent a short stream, this simulator's reader would EOF.

test("openai-mcp tools/list schemas have no ChatGPT-hostile JSON Schema markers", async () => {
  const savedSid = sessionId;
  sessionId = null;
  try {
    await rpc("initialize", openaiInit, { noSession: true, ua: OPENAI_UA });
    const list = await rpc("tools/list", {}, { noSession: true, ua: OPENAI_UA });
    assert.equal(list.status, 200);
    const tools = list.msg.result.tools;
    assert.equal(tools.length, 17);
    const blob = JSON.stringify(tools);
    assert.doesNotMatch(blob, /"propertyNames"/, "propertyNames breaks ChatGPT tool registration");
    assert.doesNotMatch(blob, /"additionalProperties"\s*:\s*\{/, "additionalProperties:{} breaks ChatGPT");
    assert.doesNotMatch(blob, /"exclusiveMinimum"/, "exclusiveMinimum breaks ChatGPT");
    const call = tools.find((x) => x.name === "herdr_call");
    assert.ok(call);
    assert.equal(call.inputSchema.properties.params.type, "string",
      "herdr_call.params must advertise string (not z.record object)");
  } finally {
    sessionId = savedSid;
  }
});

test("ChatGPT-like EOF simulator: persistent server stream stays open (no EOF), second call still out of band", async () => {
  const savedSid = sessionId;
  sessionId = null;
  try {
    // Simulate the connector: open GET, hold it, expect NO EOF. Then issue
    // inspect and since via plain POSTs (the connector's send path) while the
    // GET is still held open.
    const probe = await fetch(`${BASE}/`, {
      method: "GET", headers: { ...headers(), "User-Agent": OPENAI_UA, "Mcp-Session-Id": "sim-eof-stale" },
      signal: AbortSignal.timeout(5000),
    });
    assert.equal(probe.status, 200);
    const reader = probe.body.getReader();
    let eof = false;
    let first = "";
    // Read exactly ONE chunk. With persistent SSE the stream stays open (no
    // EOF); a short-SSE server would EOF here and the simulator would flip to
    // "terminated" — the exact failure we are guarding against. We must NOT
    // wait for a second chunk: persistent streams only emit a heartbeat every
    // 15s, so a multi-read loop would block.
    {
      const { value, done } = await reader.read();
      if (done) eof = true;
      else first = new TextDecoder().decode(value ?? new Uint8Array());
    }
    assert.equal(eof, false, "persistent SSE must NOT EOF while held open (short-SSE would EOF -> transport 'terminated')");
    assert.match(first ?? "", /^: connected/);

    // Now the two sequential POSTs must BOTH go out and succeed while the GET
    // is still open (the real failure was since never being sent).
    const insp = await openaiRpcRaw("/", { jsonrpc: "2.0", id: "insp", method: "tools/call", params: { name: "herdr_inspect", arguments: {} } }, "stale-eof-sid");
    assert.equal(insp.status, 200, "inspect POST must be 200");
    assert.ok(JSON.parse(insp.msg.result.content[0].text).focused_workspace);
    const since = await openaiRpcRaw("/", { jsonrpc: "2.0", id: "since", method: "tools/call", params: { name: "herdr_since", arguments: { cursor: 0, workspace: "w44" } } }, "stale-eof-sid");
    assert.equal(since.status, 200, "since POST must be 200 (proves second call left the client)");
    assert.ok(since.msg.result.content[0].text.includes("cursor"));

    // cleanup: abort GET, server cleans up
    reader.releaseLock();
    await probe.body.cancel();
  } finally {
    sessionId = savedSid;
  }
});
