/**
 * artifact-relay.ts — short-lived private R2 relay for generated images.
 *
 * This is not an MCP tool and not a public bucket. Upload requires the existing
 * Edge MCP/OAuth or Link shared-secret bearer. Download/delete require the
 * one-time object capability (or the same upload principal). Object IDs are
 * random; MIME, size, magic, and expiry are enforced here.
 */

import { authenticateStaticMcpBearer, constantTimeEqual } from "./auth.js";
import type { Env } from "./env.js";
import {
  ARTIFACT_CAPABILITY_BYTES,
  ARTIFACT_ID_BYTES,
  ARTIFACT_KEY_PREFIX,
  ARTIFACT_MIME_TYPES,
  ARTIFACT_TTL_MS,
  MAX_ARTIFACT_BYTES,
} from "./limits.js";
import { authenticateMcpRequest, type OAuthMcpAuthDeps } from "./oauth-mcp-auth.js";
import { readBytesBounded } from "./payload.js";

const enc = new TextEncoder();
const ARTIFACT_ID_RE = /^[a-f0-9]{32}$/;

export interface ArtifactAuthDeps extends OAuthMcpAuthDeps {
  nowMs?: number;
}

export type ArtifactJson = {
  ok: false;
  code: string;
  retryable: boolean;
  message?: string;
};

function jsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store" },
  });
}

function fail(status: number, code: string, message: string, retryable = false): Response {
  const body: ArtifactJson = { ok: false, code, retryable, message };
  return jsonResponse(body, status);
}

export function artifactObjectKey(id: string): string {
  return `${ARTIFACT_KEY_PREFIX}${id}`;
}

export function imageMagicMatches(mimeType: string, data: Uint8Array): boolean {
  switch (mimeType) {
    case "image/png":
      return data.length >= 8 &&
        data[0] === 0x89 && data[1] === 0x50 && data[2] === 0x4e && data[3] === 0x47 &&
        data[4] === 0x0d && data[5] === 0x0a && data[6] === 0x1a && data[7] === 0x0a;
    case "image/jpeg":
      return data.length >= 3 && data[0] === 0xff && data[1] === 0xd8 && data[2] === 0xff;
    case "image/gif":
      return data.length >= 6 &&
        data[0] === 0x47 && data[1] === 0x49 && data[2] === 0x46 && data[3] === 0x38 &&
        (data[4] === 0x37 || data[4] === 0x39) && data[5] === 0x61;
    case "image/webp":
      return data.length >= 12 &&
        data[0] === 0x52 && data[1] === 0x49 && data[2] === 0x46 && data[3] === 0x46 &&
        data[8] === 0x57 && data[9] === 0x45 && data[10] === 0x42 && data[11] === 0x50;
    default:
      return false;
  }
}

export function normalizeArtifactMime(raw: string | null): string | null {
  const mime = (raw ?? "").split(";")[0].trim().toLowerCase();
  return (ARTIFACT_MIME_TYPES as readonly string[]).includes(mime) ? mime : null;
}

export function randomHex(byteCount: number): string {
  const bytes = new Uint8Array(byteCount);
  crypto.getRandomValues(bytes);
  return hex(bytes);
}

export function hex(bytes: Uint8Array): string {
  let out = "";
  for (const byte of bytes) out += byte.toString(16).padStart(2, "0");
  return out;
}

export async function sha256Hex(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return hex(new Uint8Array(digest));
}

export function capabilityMatches(presented: string, storedHashHex: string): boolean {
  if (!presented || !storedHashHex || presented.length > 128 || storedHashHex.length !== 64) {
    return false;
  }
  if (![...presented].every((ch) => /[A-Fa-f0-9]/.test(ch))) return false;
  const expected = enc.encode(storedHashHex.toLowerCase());
  const actual = enc.encode(presented.toLowerCase());
  // Compare hashes, not the raw capability. Callers pass sha256(presented).
  return constantTimeEqual(actual, expected);
}

function bearerToken(request: Request): string | null {
  const raw = request.headers.get("authorization") ?? "";
  const match = /^Bearer\s+([^\s]+)$/i.exec(raw.trim());
  return match ? match[1] : null;
}

export async function authorizeArtifactMutator(
  request: Request,
  env: Env,
  deps: ArtifactAuthDeps = {},
): Promise<boolean> {
  const mcp = await authenticateMcpRequest(request, env, deps);
  if (mcp.ok) return true;
  return authenticateStaticMcpBearer(request, env.LINK_SHARED_SECRET).ok;
}

function parseArtifactId(pathname: string): string | null {
  const match = /^\/artifacts\/([^/]+)$/.exec(pathname);
  if (!match) return null;
  const id = match[1];
  return ARTIFACT_ID_RE.test(id) ? id : null;
}

function metadataExpired(metadata: Record<string, string> | undefined, nowMs: number): boolean {
  const raw = metadata?.expires_at_ms;
  const expires = raw ? Number.parseInt(raw, 10) : Number.NaN;
  return !Number.isFinite(expires) || expires <= nowMs;
}

async function authorizeObjectAccess(
  request: Request,
  env: Env,
  metadata: Record<string, string> | undefined,
  deps: ArtifactAuthDeps,
): Promise<boolean> {
  const token = bearerToken(request);
  if (token) {
    const presentedHash = await sha256Hex(enc.encode(token));
    if (capabilityMatches(presentedHash, metadata?.capability_sha256 ?? "")) return true;
  }
  return authorizeArtifactMutator(request, env, deps);
}

export async function handleArtifactRequest(
  request: Request,
  env: Env,
  deps: ArtifactAuthDeps = {},
): Promise<Response | null> {
  const url = new URL(request.url);
  if (url.pathname !== "/artifacts" && !url.pathname.startsWith("/artifacts/")) return null;

  const nowMs = deps.nowMs ?? Date.now();
  if (request.method === "POST" && url.pathname === "/artifacts") {
    return uploadArtifact(request, env, nowMs, deps);
  }
  const id = parseArtifactId(url.pathname);
  if (!id) {
    if (url.pathname === "/artifacts") {
      return fail(405, "method_not_allowed", "POST /artifacts to upload; GET or DELETE /artifacts/:id");
    }
    return fail(400, "artifact_id_invalid", "artifact id must be a 32-character hexadecimal object id");
  }
  if (request.method === "GET") return downloadArtifact(request, env, id, nowMs, deps);
  if (request.method === "DELETE") return deleteArtifact(request, env, id, nowMs, deps);
  return fail(405, "method_not_allowed", "POST /artifacts to upload; GET or DELETE /artifacts/:id");
}

async function uploadArtifact(
  request: Request,
  env: Env,
  nowMs: number,
  deps: ArtifactAuthDeps,
): Promise<Response> {
  if (!await authorizeArtifactMutator(request, env, deps)) {
    return fail(401, "artifact_auth_failed", "artifact upload requires Edge MCP/OAuth or Link bearer");
  }
  if (!env.ARTIFACT_BUCKET) {
    return fail(503, "artifact_store_unavailable", "artifact R2 binding is not configured", true);
  }
  const mime = normalizeArtifactMime(request.headers.get("content-type"));
  if (!mime) {
    return fail(415, "artifact_mime_unsupported", "supported MIME types are image/png, image/jpeg, image/gif, and image/webp");
  }
  const read = await readBytesBounded(request, MAX_ARTIFACT_BYTES);
  if (!read.ok) {
    return fail(read.code === "payload_too_large" ? 413 : 400, read.code, read.reason);
  }
  if (read.bytes.byteLength === 0) {
    return fail(400, "artifact_size_invalid", "artifact body must not be empty");
  }
  if (!imageMagicMatches(mime, read.bytes)) {
    return fail(415, "artifact_format_mismatch", "image bytes do not match the declared MIME type");
  }

  const id = randomHex(ARTIFACT_ID_BYTES);
  const capability = randomHex(ARTIFACT_CAPABILITY_BYTES);
  const expiresAtMs = nowMs + ARTIFACT_TTL_MS;
  const digest = await sha256Hex(read.bytes);
  const capabilityHash = await sha256Hex(enc.encode(capability));
  const key = artifactObjectKey(id);
  await env.ARTIFACT_BUCKET.put(key, read.bytes, {
    httpMetadata: { contentType: mime },
    customMetadata: {
      mime_type: mime,
      sha256: digest,
      bytes: String(read.bytes.byteLength),
      expires_at_ms: String(expiresAtMs),
      capability_sha256: capabilityHash,
    },
  });
  return jsonResponse({
    ok: true,
    id,
    mime_type: mime,
    bytes: read.bytes.byteLength,
    sha256: digest,
    expires_at_ms: expiresAtMs,
    ttl_ms: ARTIFACT_TTL_MS,
    capability,
    path: `/artifacts/${id}`,
  }, 201);
}

async function loadObject(env: Env, id: string, nowMs: number) {
  const object = await env.ARTIFACT_BUCKET!.get(artifactObjectKey(id));
  if (!object) return { kind: "missing" as const };
  const metadata = object.customMetadata ?? {};
  if (metadataExpired(metadata, nowMs)) {
    await env.ARTIFACT_BUCKET!.delete(artifactObjectKey(id));
    return { kind: "expired" as const };
  }
  return { kind: "ok" as const, object, metadata };
}

async function requireArtifactCredentials(
  request: Request,
  env: Env,
  deps: ArtifactAuthDeps,
): Promise<Response | null> {
  if (bearerToken(request) || await authorizeArtifactMutator(request, env, deps)) return null;
  return fail(401, "artifact_auth_failed", "artifact access requires the object capability or Edge auth");
}

async function downloadArtifact(
  request: Request,
  env: Env,
  id: string,
  nowMs: number,
  deps: ArtifactAuthDeps,
): Promise<Response> {
  const denied = await requireArtifactCredentials(request, env, deps);
  if (denied) return denied;
  if (!env.ARTIFACT_BUCKET) {
    return fail(503, "artifact_store_unavailable", "artifact R2 binding is not configured", true);
  }
  const loaded = await loadObject(env, id, nowMs);
  if (loaded.kind === "missing") return fail(404, "artifact_not_found", "artifact is missing or expired");
  if (loaded.kind === "expired") return fail(404, "artifact_expired", "artifact capability has expired");
  if (!await authorizeObjectAccess(request, env, loaded.metadata, deps)) {
    return fail(401, "artifact_auth_failed", "artifact download requires the object capability or Edge auth");
  }
  const bytes = new Uint8Array(await loaded.object.arrayBuffer());
  return new Response(bytes, {
    status: 200,
    headers: {
      "content-type": loaded.metadata.mime_type || loaded.object.httpMetadata?.contentType || "application/octet-stream",
      "cache-control": "no-store",
      "x-artifact-id": id,
      "x-artifact-sha256": loaded.metadata.sha256 ?? "",
      "x-artifact-expires-at-ms": loaded.metadata.expires_at_ms ?? "",
    },
  });
}

async function deleteArtifact(
  request: Request,
  env: Env,
  id: string,
  nowMs: number,
  deps: ArtifactAuthDeps,
): Promise<Response> {
  const denied = await requireArtifactCredentials(request, env, deps);
  if (denied) return denied;
  if (!env.ARTIFACT_BUCKET) {
    return fail(503, "artifact_store_unavailable", "artifact R2 binding is not configured", true);
  }
  const loaded = await loadObject(env, id, nowMs);
  if (loaded.kind === "missing") return fail(404, "artifact_not_found", "artifact is missing or expired");
  if (loaded.kind === "expired") return jsonResponse({ ok: true, deleted: true, expired: true });
  if (!await authorizeObjectAccess(request, env, loaded.metadata, deps)) {
    return fail(401, "artifact_auth_failed", "artifact delete requires the object capability or Edge auth");
  }
  await env.ARTIFACT_BUCKET.delete(artifactObjectKey(id));
  return jsonResponse({ ok: true, deleted: true, id });
}

export async function sweepExpiredArtifacts(bucket: R2Bucket, nowMs: number): Promise<{ scanned: number; deleted: number }> {
  let cursor: string | undefined;
  let scanned = 0;
  let deleted = 0;
  do {
    const page = await bucket.list({ prefix: ARTIFACT_KEY_PREFIX, cursor, limit: 100 });
    for (const entry of page.objects) {
      scanned += 1;
      const object = await bucket.head(entry.key);
      const metadata = object?.customMetadata ?? {};
      if (!object || metadataExpired(metadata, nowMs)) {
        await bucket.delete(entry.key);
        deleted += 1;
      }
    }
    cursor = page.truncated ? page.cursor : undefined;
  } while (cursor);
  return { scanned, deleted };
}
