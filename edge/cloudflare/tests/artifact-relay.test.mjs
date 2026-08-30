import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  ARTIFACT_TTL_MS,
  MAX_ARTIFACT_BYTES,
} from "../dist/limits.js";
import {
  artifactObjectKey,
  handleArtifactRequest,
  imageMagicMatches,
  isImageMime,
  normalizeArtifactMime,
  randomHex,
  sha256Hex,
  sweepExpiredArtifacts,
} from "../dist/artifact-relay.js";
import worker from "../dist/index.js";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..");
const PNG = Uint8Array.from(Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
  "base64",
));
const JPEG = Uint8Array.from([0xff, 0xd8, 0xff, 0xd9]);
const GIF = Uint8Array.from(Buffer.from("GIF89a\x01\x00\x01\x00\x00\x00\x00;"));
const WEBP = Uint8Array.from(Buffer.from("RIFF\x04\x00\x00\x00WEBP"));
const SECRET = "dev-artifact-secret";
const LINK_SECRET = "link-artifact-secret";

class MemoryR2 {
  constructor() {
    this.store = new Map();
  }

  async put(key, value, options = {}) {
    const bytes = value instanceof Uint8Array ? value.slice() : new Uint8Array(value);
    this.store.set(key, {
      key,
      bytes,
      size: bytes.byteLength,
      customMetadata: { ...(options.customMetadata || {}) },
      httpMetadata: { ...(options.httpMetadata || {}) },
    });
    return {};
  }

  _record(key) {
    const row = this.store.get(key);
    if (!row) return null;
    return {
      key: row.key,
      size: row.size,
      customMetadata: { ...row.customMetadata },
      httpMetadata: { ...row.httpMetadata },
      arrayBuffer: async () => row.bytes.slice().buffer,
    };
  }

  async get(key) {
    return this._record(key);
  }

  async head(key) {
    const row = this._record(key);
    if (!row) return null;
    const { arrayBuffer: _ignored, ...meta } = row;
    return meta;
  }

  async delete(key) {
    this.store.delete(key);
  }

  async list({ prefix = "", cursor, limit = 100 } = {}) {
    const keys = [...this.store.keys()].filter((key) => key.startsWith(prefix)).sort();
    const start = cursor ? Number.parseInt(cursor, 10) || 0 : 0;
    const slice = keys.slice(start, start + limit);
    const next = start + slice.length;
    return {
      objects: slice.map((key) => ({ key })),
      truncated: next < keys.length,
      cursor: next < keys.length ? String(next) : undefined,
    };
  }
}

function env(overrides = {}) {
  const stub = { fetch: async () => new Response(JSON.stringify({ ok: false }), { status: 401 }) };
  return {
    DEV_MCP_BEARER_SECRET: SECRET,
    LINK_SHARED_SECRET: LINK_SECRET,
    ARTIFACT_BUCKET: new MemoryR2(),
    OAUTH_STORE_DO: {
      idFromName(name) { return name; },
      get() { return stub; },
    },
    WORKSTATION_DO: {
      idFromName(name) { return name; },
      get() { return stub; },
    },
    ...overrides,
  };
}

function authHeaders(mime = "image/png", token = SECRET) {
  return {
    authorization: `Bearer ${token}`,
    "content-type": mime,
  };
}

async function upload(state, bytes = PNG, headers = authHeaders()) {
  return handleArtifactRequest(
    new Request("https://edge.example/artifacts", {
      method: "POST",
      headers,
      body: bytes,
    }),
    state,
  );
}

test("artifact magic accepts png/jpeg/gif/webp and rejects mismatch", () => {
  assert.equal(imageMagicMatches("image/png", PNG), true);
  assert.equal(imageMagicMatches("image/jpeg", JPEG), true);
  assert.equal(imageMagicMatches("image/gif", GIF), true);
  assert.equal(imageMagicMatches("image/webp", WEBP), true);
  assert.equal(imageMagicMatches("image/png", JPEG), false);
  assert.equal(imageMagicMatches("image/svg+xml", PNG), false);
});

test("non-image allowlist and octet-stream are accepted without image magic", async () => {
  const state = env();
  const cases = [
    { mime: "text/plain", bytes: Uint8Array.from(Buffer.from("hello artifact\n")) },
    { mime: "application/json", bytes: Uint8Array.from(Buffer.from('{"ok":true}')) },
    { mime: "application/octet-stream", bytes: Uint8Array.from([0x00, 0x01, 0x02, 0xff]) },
    { mime: "application/zip", bytes: Uint8Array.from(Buffer.from("PK\x03\x04fake")) },
  ];
  for (const { mime, bytes } of cases) {
    const created = await (await upload(state, bytes, authHeaders(mime))).json();
    assert.equal(created.ok, true, mime);
    assert.equal(created.mime_type, mime, mime);
    assert.equal(created.bytes, bytes.byteLength, mime);
    const digest = await sha256Hex(bytes);
    assert.equal(created.sha256, digest, mime);

    const downloaded = await handleArtifactRequest(
      new Request(`https://edge.example/artifacts/${created.id}`, {
        headers: { authorization: `Bearer ${created.capability}` },
      }),
      state,
    );
    assert.equal(downloaded.status, 200, mime);
    assert.equal(downloaded.headers.get("content-type"), mime, mime);
    assert.equal(downloaded.headers.get("content-disposition"), `attachment; filename="${created.id}"`, mime);
    assert.equal(downloaded.headers.get("x-content-type-options"), "nosniff", mime);
    assert.equal(downloaded.headers.get("x-artifact-sha256"), digest, mime);
    assert.deepEqual([...new Uint8Array(await downloaded.arrayBuffer())], [...bytes], mime);
  }
});

test("active-content MIME declarations are rejected", async () => {
  const state = env();
  for (const mime of [
    "text/html",
    "application/xhtml+xml",
    "image/svg+xml",
    "text/javascript",
    "application/javascript",
    "application/xml",
    "application/x-sh",
    "application/x-msdownload",
  ]) {
    const response = await upload(state, PNG, authHeaders(mime));
    assert.equal(response.status, 415, mime);
    assert.equal((await response.json()).code, "artifact_mime_unsupported", mime);
  }
});

test("normalizeArtifactMime and isImageMime classify the allowlist", () => {
  assert.equal(normalizeArtifactMime("image/png"), "image/png");
  assert.equal(normalizeArtifactMime("text/plain; charset=utf-8"), "text/plain");
  assert.equal(normalizeArtifactMime("application/octet-stream"), "application/octet-stream");
  assert.equal(normalizeArtifactMime("text/html"), null);
  assert.equal(normalizeArtifactMime("image/svg+xml"), null);
  assert.equal(normalizeArtifactMime("application/x-sh"), null);
  assert.equal(isImageMime("image/png"), true);
  assert.equal(isImageMime("text/plain"), false);
  assert.equal(isImageMime("application/octet-stream"), false);
});

test("artifact upload requires existing Edge MCP or Link bearer", async () => {
  const state = env();
  const denied = await upload(state, PNG, { "content-type": "image/png" });
  assert.equal(denied.status, 401);
  const body = await denied.json();
  assert.equal(body.code, "artifact_auth_failed");

  const mcp = await upload(state);
  assert.equal(mcp.status, 201);
  const created = await mcp.json();
  assert.equal(created.ok, true);
  assert.match(created.id, /^[a-f0-9]{32}$/);
  assert.equal(created.mime_type, "image/png");
  assert.equal(typeof created.capability, "string");
  assert.equal(created.capability.length, 64);

  const viaLink = await upload(env(), PNG, authHeaders("image/png", LINK_SECRET));
  assert.equal(viaLink.status, 201);
});

test("artifact IDs are random opaque 128-bit values, not sequential names", async () => {
  const state = env();
  const ids = new Set();
  for (let i = 0; i < 8; i += 1) {
    const created = await (await upload(state)).json();
    ids.add(created.id);
    assert.equal(created.path, `/artifacts/${created.id}`);
    assert.equal(created.id.includes("png"), false);
    assert.equal(created.id.includes("artifact-"), false);
  }
  assert.equal(ids.size, 8);
  assert.equal(randomHex(16).length, 32);
});

test("artifact upload rejects oversized, empty, and unsupported MIME bodies", async () => {
  const state = env();
  const tooLarge = await handleArtifactRequest(
    new Request("https://edge.example/artifacts", {
      method: "POST",
      headers: { ...authHeaders(), "content-length": String(MAX_ARTIFACT_BYTES + 1) },
      body: PNG,
    }),
    state,
  );
  assert.equal(tooLarge.status, 413);
  assert.equal((await tooLarge.json()).code, "payload_too_large");

  const empty = await upload(state, new Uint8Array());
  assert.equal(empty.status, 400);
  assert.equal((await empty.json()).code, "artifact_size_invalid");

  const svg = await upload(state, PNG, authHeaders("image/svg+xml"));
  assert.equal(svg.status, 415);
  assert.equal((await svg.json()).code, "artifact_mime_unsupported");

  const mismatch = await upload(state, JPEG, authHeaders("image/png"));
  assert.equal(mismatch.status, 415);
  assert.equal((await mismatch.json()).code, "artifact_format_mismatch");
});

test("artifact download/delete are capability-scoped and expire", async () => {
  const state = env();
  const now = 1_000_000;
  const created = await (await handleArtifactRequest(
    new Request("https://edge.example/artifacts", {
      method: "POST",
      headers: authHeaders(),
      body: PNG,
    }),
    state,
    { nowMs: now },
  )).json();
  const digest = await sha256Hex(PNG);

  const withCapability = await handleArtifactRequest(
    new Request(`https://edge.example/artifacts/${created.id}`, {
      headers: { authorization: `Bearer ${created.capability}` },
    }),
    state,
    { nowMs: now + 1 },
  );
  assert.equal(withCapability.status, 200);
  assert.equal(withCapability.headers.get("content-type"), "image/png");
  assert.equal(withCapability.headers.get("x-artifact-sha256"), digest);
  assert.deepEqual([...new Uint8Array(await withCapability.arrayBuffer())], [...PNG]);

  const wrongCap = await handleArtifactRequest(
    new Request(`https://edge.example/artifacts/${created.id}`, {
      headers: { authorization: "Bearer deadbeef" },
    }),
    state,
    { nowMs: now + 1 },
  );
  assert.equal(wrongCap.status, 401);

  const expired = await handleArtifactRequest(
    new Request(`https://edge.example/artifacts/${created.id}`, {
      headers: { authorization: `Bearer ${created.capability}` },
    }),
    state,
    { nowMs: now + ARTIFACT_TTL_MS + 1 },
  );
  assert.equal(expired.status, 404);
  assert.equal((await expired.json()).code, "artifact_expired");
  assert.equal(state.ARTIFACT_BUCKET.store.has(artifactObjectKey(created.id)), false);
});

test("artifact delete removes the object and sweep drops expired leftovers", async () => {
  const state = env();
  const now = 5_000_000;
  const live = await (await handleArtifactRequest(
    new Request("https://edge.example/artifacts", { method: "POST", headers: authHeaders(), body: PNG }),
    state,
    { nowMs: now },
  )).json();
  const stale = await (await handleArtifactRequest(
    new Request("https://edge.example/artifacts", { method: "POST", headers: authHeaders(), body: PNG }),
    state,
    { nowMs: now - ARTIFACT_TTL_MS - 10 },
  )).json();

  const deleted = await handleArtifactRequest(
    new Request(`https://edge.example/artifacts/${live.id}`, {
      method: "DELETE",
      headers: { authorization: `Bearer ${live.capability}` },
    }),
    state,
    { nowMs: now },
  );
  assert.equal(deleted.status, 200);
  assert.equal((await deleted.json()).deleted, true);
  assert.equal(state.ARTIFACT_BUCKET.store.has(artifactObjectKey(live.id)), false);

  const swept = await sweepExpiredArtifacts(state.ARTIFACT_BUCKET, now);
  assert.equal(swept.deleted >= 1, true);
  assert.equal(state.ARTIFACT_BUCKET.store.has(artifactObjectKey(stale.id)), false);
});

test("artifact routes are Worker-private and have no browser-extension coupling", async () => {
  const state = env();
  const unauth = await worker.fetch(new Request("https://edge.example/artifacts", { method: "POST", body: PNG }), state);
  assert.equal(unauth.status, 401);

  const created = await (await worker.fetch(
    new Request("https://edge.example/artifacts", { method: "POST", headers: authHeaders(), body: PNG }),
    state,
  )).json();
  const listed = await worker.fetch(
    new Request("https://edge.example/artifacts", { headers: { authorization: `Bearer ${SECRET}` } }),
    state,
  );
  assert.equal(listed.status, 405);

  const missingPublic = await worker.fetch(new Request(`https://edge.example/artifacts/${created.id}`), state);
  assert.equal(missingPublic.status, 401);

  const srcRoot = path.join(ROOT, "edge/cloudflare/src");
  const files = ["artifact-relay.ts", "index.ts", "env.ts"];
  for (const file of files) {
    const source = readFileSync(path.join(srcRoot, file), "utf8");
    assert.doesNotMatch(source, /extension\//);
    assert.doesNotMatch(source, /chrome\.runtime/);
    assert.doesNotMatch(source, /nativeMessaging/);
  }
});

test("missing R2 binding fails closed after auth", async () => {
  const state = env({ ARTIFACT_BUCKET: undefined });
  const response = await upload(state);
  assert.equal(response.status, 503);
  assert.equal((await response.json()).code, "artifact_store_unavailable");
});
