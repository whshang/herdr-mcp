/**
 * Short-lived browser-extension credentials.
 *
 * The long-lived HERDR_MCP_TOKEN never needs to enter the Chrome extension.
 * A native-messaging host presents that static token to this loopback-only
 * endpoint and receives a short-lived bearer scoped by middleware usage to
 * local /mcp and /push routes.
 */
import { createHash, randomBytes, timingSafeEqual } from "node:crypto";
import type { Express, Request, Response } from "express";

const AUTH_TOKEN = process.env.HERDR_MCP_TOKEN ?? "";
const DEFAULT_TTL_MS = 10 * 60_000;
const MIN_TTL_MS = 60_000;
const MAX_TTL_MS = 60 * 60_000;
const configuredTtl = Number(process.env.HERDR_EXTENSION_SESSION_TTL_MS ?? DEFAULT_TTL_MS);
const SESSION_TTL_MS = Math.min(MAX_TTL_MS, Math.max(MIN_TTL_MS, Number.isFinite(configuredTtl) ? configuredTtl : DEFAULT_TTL_MS));
const SESSION_PREFIX = "herdr_ext_";

type SessionRecord = { expiresAt: number; extensionId: string };
const sessions = new Map<string, SessionRecord>();

function safeEqual(a: string, b: string): boolean {
  const aa = Buffer.from(a);
  const bb = Buffer.from(b);
  return aa.length === bb.length && timingSafeEqual(aa, bb);
}

function bearer(req: Request): string {
  const auth = req.get("authorization") ?? "";
  const match = /^Bearer\s+(.+)$/i.exec(auth);
  return match ? match[1].trim() : "";
}

function digest(token: string): string {
  return createHash("sha256").update(token).digest("hex");
}

function loopbackAddress(value: string | undefined): boolean {
  const address = String(value || "").toLowerCase();
  return address === "127.0.0.1" || address === "::1" || address.startsWith("::ffff:127.");
}

function isLoopbackRequest(req: Request): boolean {
  return loopbackAddress(req.socket.remoteAddress);
}

function pruneExpired(now = Date.now()): void {
  for (const [key, value] of sessions) {
    if (value.expiresAt <= now) sessions.delete(key);
  }
}

function staticBearerMatches(req: Request): boolean {
  if (!AUTH_TOKEN) return true;
  const presented = bearer(req);
  return presented !== "" && safeEqual(presented, AUTH_TOKEN);
}

export function extensionSessionBearerMatches(req: Request): boolean {
  if (!isLoopbackRequest(req)) return false;
  const presented = bearer(req);
  if (!presented.startsWith(SESSION_PREFIX)) return false;
  pruneExpired();
  const record = sessions.get(digest(presented));
  if (!record || record.expiresAt <= Date.now()) return false;
  return true;
}

export function registerExtensionAuthRoutes(app: Express): void {
  app.post("/extension/session", (req: Request, res: Response) => {
    res.set("Cache-Control", "no-store");
    if (!isLoopbackRequest(req)) {
      res.status(403).json({ ok: false, error: "loopback-required" });
      return;
    }
    if (!staticBearerMatches(req)) {
      res.status(401).json({ ok: false, error: "unauthorized" });
      return;
    }

    const extensionId = typeof req.body?.extension_id === "string" ? req.body.extension_id.trim() : "";
    if (!/^[a-p]{32}$/.test(extensionId)) {
      res.status(400).json({ ok: false, error: "invalid-extension-id" });
      return;
    }

    // An unset static token is the historical open-development mode. In that
    // mode no new credential is needed because /mcp and /push are already open.
    if (!AUTH_TOKEN) {
      res.status(200).json({ ok: true, token: "", token_type: "Bearer", expires_at: null, expires_in: 0, auth_mode: "open" });
      return;
    }

    pruneExpired();
    const token = `${SESSION_PREFIX}${randomBytes(32).toString("base64url")}`;
    const expiresAt = Date.now() + SESSION_TTL_MS;
    sessions.set(digest(token), { expiresAt, extensionId });
    res.status(200).json({
      ok: true,
      token,
      token_type: "Bearer",
      expires_at: new Date(expiresAt).toISOString(),
      expires_in: Math.floor(SESSION_TTL_MS / 1000),
      auth_mode: "native_session",
    });
  });
}
