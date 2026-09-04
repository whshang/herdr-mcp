import type { Env } from "./env.js";
import { createOAuthIdentity, createRs256AccessTokenVerifier, hashOpaqueToken } from "./oauth-edge.js";
import { issueRs256AccessJwt, randomBase64UrlToken } from "./oauth-token-crypto.js";

const CLIENT_PREFIX = "client:";
const ACCESS_PREFIX = "access:";
const REFRESH_PREFIX = "refresh:";
const CODE_PREFIX = "code:";
const APPROVAL_PREFIX = "approval:";
const GRANT_PREFIX = "grant:";
const CONNECTOR_PREFIX = "connector:";
const SIGNING_KEY = "signing:key:v1";
const MAX_IMPORT_BYTES = 256 * 1024;
const MAX_IMPORT_RECORDS = 512;
const MAX_STRING = 4096;
const MAX_ACTIVE_PENDING_TOTAL = 128;
const MAX_ACTIVE_PENDING_PER_CLIENT = 4;
const MAX_UNAPPROVED_DCR_CLIENTS = 256;
const UNAPPROVED_DCR_TTL_MS = 60 * 60_000;

export interface OAuthClientRecord {
  client_secret_hash: string | null;
  redirect_uris: string[];
  token_endpoint_auth_method: "none" | "client_secret_post";
  grant_types: string[];
  scope: string;
  client_name?: string;
  issued_at: number;
}

export interface OAuthTokenRecord {
  client_id: string;
  connector_id?: string;
  grant_generation?: number;
  resource: string;
  scope: string;
  expires_at: number;
}

export interface OAuthCodeRecord {
  client_id: string;
  connector_id?: string;
  grant_generation?: number;
  redirect_uri: string;
  code_challenge: string;
  resource: string;
  expires_at: number;
}

export interface OAuthApprovalRecord {
  client_id: string;
  connector_id?: string;
  auth_source?: "dcr" | "cimd" | "chatgpt_cimd";
  redirect_uri: string;
  code_challenge: string;
  resource: string;
  scope: string;
  state: string;
  approval_code_hash: string;
  resume_hash: string;
  created_at_ms: number;
  expires_at_ms: number;
  attempts: number;
  status: "pending" | "approved" | "locked" | "rejected";
  approved_at_ms?: number;
  approved_by?: string;
  rejected_at_ms?: number;
  rejected_by?: string;
}

export interface OAuthConnectorRecord {
  connector_id: string;
  client_id: string;
  status: "active" | "revoked";
  principal_type: "connector";
  capabilities: ["mcp_access"];
  resource: string;
  scope: string;
  redirect_uri: string;
  auth_source: "dcr" | "cimd" | "chatgpt_cimd" | "legacy";
  grant_generation: number;
  alias?: string;
  approved_at_ms: number;
  approved_by: string;
  last_token_issued_at_ms?: number;
  token_issue_count?: number;
  revoked_at_ms?: number;
  revoked_by?: string;
}

export interface OAuthConnectorGrantRecord {
  client_id: string;
  status: "active" | "revoked";
  principal_type?: "connector" | "automation";
  resource?: string;
  scope?: string;
  /** Bound device_id for automation grants; validated as canonical dev_<26-char> ULID. */
  device_id?: string;
  device_name?: string;
  approved_at_ms?: number;
  approved_by?: string;
  last_token_issued_at_ms?: number;
  token_issue_count?: number;
  last_rotated_at_ms?: number;
  last_rotated_by?: string;
  revoked_at_ms?: number;
  revoked_by?: string;
}

type StoredJwk = JsonWebKey & { kid?: string; alg?: string; use?: string };

interface SigningKeyRecord {
  kid: string;
  private_jwk: StoredJwk;
  public_jwk: StoredJwk;
  created_at: number;
}

interface IssuePairInput {
  client_id: string;
  connector_id?: string;
  grant_generation?: number;
  resource: string;
  now_sec?: number;
  access_ttl_sec?: number;
  refresh_ttl_sec?: number;
}

type ParsedIssuePairInput = Required<Omit<IssuePairInput, "connector_id" | "grant_generation">>
  & Pick<IssuePairInput, "connector_id" | "grant_generation">;

interface ImportBody {
  clients?: Record<string, unknown>;
  tokens?: Record<string, unknown>;
  refresh?: Record<string, unknown>;
  /** Optional production RS256 key pair to adopt as the signing key (continuity). */
  signing_key?: unknown;
  overwrite?: boolean;
  now_sec?: number;
}

const SIGNING_PROBE = new TextEncoder().encode("herdr-mcp-signing-key-probe");

function json(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: { "content-type": "application/json", "cache-control": "no-store" },
  });
}

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function boundedString(value: unknown, max = MAX_STRING): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= max;
}

function finiteEpoch(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

function validMapKey(key: string): boolean {
  return key.length > 0 && key.length <= 4096 && !key.includes("\u0000");
}

export function normalizeOAuthClient(value: unknown): OAuthClientRecord | null {
  if (!record(value)) return null;
  const redirects = value.redirect_uris;
  if (!Array.isArray(redirects) || redirects.length > 32) return null;
  const redirect_uris: string[] = [];
  for (const uri of redirects) {
    if (!boundedString(uri, 4096)) return null;
    redirect_uris.push(uri);
  }
  const method = value.token_endpoint_auth_method === "client_secret_post" ? "client_secret_post" : "none";
  const grantsRaw = value.grant_types;
  const grant_types = Array.isArray(grantsRaw)
    ? grantsRaw.filter((item): item is string => typeof item === "string" && item.length <= 128).slice(0, 16)
    : ["authorization_code", "refresh_token"];
  const scope = typeof value.scope === "string" && value.scope.length > 0 && value.scope.length <= 512 ? value.scope : "mcp";
  const secretHash = value.client_secret_hash;
  if (!(secretHash === null || (typeof secretHash === "string" && secretHash.length <= 256))) return null;
  const issuedAt = finiteEpoch(value.issued_at) ? value.issued_at : Math.floor(Date.now() / 1000);
  const clientName = typeof value.client_name === "string" && value.client_name.length <= 512 ? value.client_name : undefined;
  return {
    client_secret_hash: secretHash,
    redirect_uris,
    token_endpoint_auth_method: method,
    grant_types,
    scope,
    ...(clientName !== undefined ? { client_name: clientName } : {}),
    issued_at: issuedAt,
  };
}

export function normalizeOAuthToken(value: unknown, nowSec = Math.floor(Date.now() / 1000)): OAuthTokenRecord | null {
  if (!record(value)) return null;
  if (!boundedString(value.client_id, 4096) || !boundedString(value.resource, 4096)) return null;
  if (typeof value.scope !== "string" || value.scope.length > 512) return null;
  if (!finiteEpoch(value.expires_at) || value.expires_at <= nowSec) return null;
  const connectorId = typeof value.connector_id === "string" && /^conn_[A-Za-z0-9_-]{8,128}$/.test(value.connector_id)
    ? value.connector_id
    : undefined;
  const grantGeneration = Number.isSafeInteger(value.grant_generation) && (value.grant_generation as number) > 0
    ? value.grant_generation as number
    : undefined;
  if ((connectorId === undefined) !== (grantGeneration === undefined)) return null;
  return {
    client_id: value.client_id,
    ...(connectorId ? { connector_id: connectorId, grant_generation: grantGeneration } : {}),
    resource: value.resource,
    scope: value.scope || "mcp",
    expires_at: value.expires_at,
  };
}

export function normalizeOAuthCode(value: unknown, nowMs = Date.now()): OAuthCodeRecord | null {
  if (!record(value)) return null;
  if (!boundedString(value.client_id, 4096)) return null;
  if (!boundedString(value.redirect_uri, 4096)) return null;
  if (!boundedString(value.code_challenge, 256)) return null;
  if (!boundedString(value.resource, 4096)) return null;
  if (!finiteEpoch(value.expires_at) || value.expires_at <= nowMs) return null;
  const connectorId = typeof value.connector_id === "string" && /^conn_[A-Za-z0-9_-]{8,128}$/.test(value.connector_id)
    ? value.connector_id
    : undefined;
  const grantGeneration = Number.isSafeInteger(value.grant_generation) && (value.grant_generation as number) > 0
    ? value.grant_generation as number
    : undefined;
  if ((connectorId === undefined) !== (grantGeneration === undefined)) return null;
  return {
    client_id: value.client_id,
    ...(connectorId ? { connector_id: connectorId, grant_generation: grantGeneration } : {}),
    redirect_uri: value.redirect_uri,
    code_challenge: value.code_challenge,
    resource: value.resource,
    expires_at: value.expires_at,
  };
}

function normalizeOAuthApproval(value: unknown, nowMs = Date.now()): OAuthApprovalRecord | null {
  if (!record(value)) return null;
  if (!boundedString(value.client_id, 4096)) return null;
  if (!boundedString(value.redirect_uri, 4096)) return null;
  if (!boundedString(value.code_challenge, 256)) return null;
  if (!boundedString(value.resource, 4096)) return null;
  if (typeof value.scope !== "string" || value.scope.length === 0 || value.scope.length > 512) return null;
  if (typeof value.state !== "string" || value.state.length > 4096) return null;
  if (!boundedString(value.approval_code_hash, 256) || !boundedString(value.resume_hash, 256)) return null;
  if (!finiteEpoch(value.created_at_ms) || !finiteEpoch(value.expires_at_ms) || value.expires_at_ms <= nowMs) return null;
  if (!Number.isSafeInteger(value.attempts) || (value.attempts as number) < 0 || (value.attempts as number) > 5) return null;
  if (value.status !== "pending" && value.status !== "approved" && value.status !== "locked") return null;
  const approvedAt = finiteEpoch(value.approved_at_ms) ? value.approved_at_ms as number : undefined;
  const approvedBy = typeof value.approved_by === "string" && value.approved_by.length <= 4096 ? value.approved_by : undefined;
  const connectorId = typeof value.connector_id === "string" && /^conn_[A-Za-z0-9_-]{8,128}$/.test(value.connector_id)
    ? value.connector_id
    : undefined;
  const authSource = value.auth_source === "dcr" || value.auth_source === "cimd" || value.auth_source === "chatgpt_cimd"
    ? value.auth_source
    : undefined;
  return {
    client_id: value.client_id,
    ...(connectorId ? { connector_id: connectorId } : {}),
    ...(authSource ? { auth_source: authSource } : {}),
    redirect_uri: value.redirect_uri,
    code_challenge: value.code_challenge,
    resource: value.resource,
    scope: value.scope,
    state: value.state,
    approval_code_hash: value.approval_code_hash,
    resume_hash: value.resume_hash,
    created_at_ms: value.created_at_ms,
    expires_at_ms: value.expires_at_ms,
    attempts: value.attempts as number,
    status: value.status,
    ...(approvedAt !== undefined ? { approved_at_ms: approvedAt } : {}),
    ...(approvedBy !== undefined ? { approved_by: approvedBy } : {}),
  };
}

function normalizeConnector(value: unknown): OAuthConnectorRecord | null {
  if (!record(value)) return null;
  if (typeof value.connector_id !== "string" || !/^conn_[A-Za-z0-9_-]{8,128}$/.test(value.connector_id)) return null;
  if (!boundedString(value.client_id, 4096)) return null;
  if (value.status !== "active" && value.status !== "revoked") return null;
  if (value.principal_type !== "connector") return null;
  if (!Array.isArray(value.capabilities) || value.capabilities.length !== 1 || value.capabilities[0] !== "mcp_access") return null;
  if (!boundedString(value.resource, 4096) || value.scope !== "mcp" || !boundedString(value.redirect_uri, 4096)) return null;
  if (value.auth_source !== "dcr" && value.auth_source !== "cimd" && value.auth_source !== "chatgpt_cimd" && value.auth_source !== "legacy") return null;
  if (!Number.isSafeInteger(value.grant_generation) || (value.grant_generation as number) <= 0) return null;
  if (!finiteEpoch(value.approved_at_ms) || !boundedString(value.approved_by, 4096)) return null;
  const alias = typeof value.alias === "string" && value.alias.length > 0 && value.alias.length <= 256 ? value.alias : undefined;
  const lastTokenIssuedAt = finiteEpoch(value.last_token_issued_at_ms) ? value.last_token_issued_at_ms as number : undefined;
  const tokenIssueCount = Number.isSafeInteger(value.token_issue_count) && (value.token_issue_count as number) >= 0
    ? value.token_issue_count as number
    : undefined;
  const revokedAt = finiteEpoch(value.revoked_at_ms) ? value.revoked_at_ms as number : undefined;
  const revokedBy = boundedString(value.revoked_by, 4096) ? value.revoked_by : undefined;
  return {
    connector_id: value.connector_id,
    client_id: value.client_id,
    status: value.status,
    principal_type: "connector",
    capabilities: ["mcp_access"],
    resource: value.resource,
    scope: "mcp",
    redirect_uri: value.redirect_uri,
    auth_source: value.auth_source,
    grant_generation: value.grant_generation as number,
    approved_at_ms: value.approved_at_ms,
    approved_by: value.approved_by,
    ...(alias ? { alias } : {}),
    ...(lastTokenIssuedAt !== undefined ? { last_token_issued_at_ms: lastTokenIssuedAt } : {}),
    ...(tokenIssueCount !== undefined ? { token_issue_count: tokenIssueCount } : {}),
    ...(revokedAt !== undefined ? { revoked_at_ms: revokedAt } : {}),
    ...(revokedBy !== undefined ? { revoked_by: revokedBy } : {}),
  };
}

function normalizeConnectorGrant(value: unknown): OAuthConnectorGrantRecord | null {
  if (!record(value)) return null;
  if (!boundedString(value.client_id, 4096)) return null;
  if (value.status !== "active" && value.status !== "revoked") return null;
  const principalType = value.principal_type === "connector" || value.principal_type === "automation"
    ? value.principal_type
    : undefined;
  const resource = boundedString(value.resource, 4096) ? value.resource : undefined;
  const scope = typeof value.scope === "string" && value.scope.length > 0 && value.scope.length <= 512 ? value.scope : undefined;
  const approvedAt = finiteEpoch(value.approved_at_ms) ? value.approved_at_ms as number : undefined;
  const approvedBy = boundedString(value.approved_by, 4096) ? value.approved_by : undefined;
  const lastTokenIssuedAt = finiteEpoch(value.last_token_issued_at_ms) ? value.last_token_issued_at_ms as number : undefined;
  const tokenIssueCount = Number.isSafeInteger(value.token_issue_count) && (value.token_issue_count as number) >= 0
    ? value.token_issue_count as number
    : undefined;
  const lastRotatedAt = finiteEpoch(value.last_rotated_at_ms) ? value.last_rotated_at_ms as number : undefined;
  const lastRotatedBy = boundedString(value.last_rotated_by, 4096) ? value.last_rotated_by : undefined;
  if (value.status === "active" && (!resource || !scope || approvedAt === undefined || !approvedBy)) return null;
  const deviceId = typeof value.device_id === "string" && /^dev_[0-9A-HJKMNP-TV-Z]{26}$/i.test(value.device_id)
    ? value.device_id
    : undefined;
  const deviceName = boundedString(value.device_name, 256) ? value.device_name : undefined;
  // An active automation grant MUST be bound to exactly one enrolled device.
  if (principalType === "automation" && value.status === "active") {
    if (!deviceId) return null;
    if (deviceName === undefined) return null;
  }
  const revokedAt = finiteEpoch(value.revoked_at_ms) ? value.revoked_at_ms as number : undefined;
  const revokedBy = typeof value.revoked_by === "string" && value.revoked_by.length <= 4096 ? value.revoked_by : undefined;
  return {
    client_id: value.client_id,
    status: value.status,
    ...(principalType !== undefined ? { principal_type: principalType } : {}),
    ...(resource !== undefined ? { resource } : {}),
    ...(scope !== undefined ? { scope } : {}),
    ...(deviceId !== undefined ? { device_id: deviceId } : {}),
    ...(deviceName !== undefined ? { device_name: deviceName } : {}),
    ...(approvedAt !== undefined ? { approved_at_ms: approvedAt } : {}),
    ...(approvedBy !== undefined ? { approved_by: approvedBy } : {}),
    ...(lastTokenIssuedAt !== undefined ? { last_token_issued_at_ms: lastTokenIssuedAt } : {}),
    ...(tokenIssueCount !== undefined ? { token_issue_count: tokenIssueCount } : {}),
    ...(lastRotatedAt !== undefined ? { last_rotated_at_ms: lastRotatedAt } : {}),
    ...(lastRotatedBy !== undefined ? { last_rotated_by: lastRotatedBy } : {}),
    ...(revokedAt !== undefined ? { revoked_at_ms: revokedAt } : {}),
    ...(revokedBy !== undefined ? { revoked_by: revokedBy } : {}),
  };
}

async function boundedJson(request: Request): Promise<{ ok: true; value: unknown } | { ok: false; status: number }> {
  const declared = Number(request.headers.get("content-length") ?? "0");
  if (Number.isFinite(declared) && declared > MAX_IMPORT_BYTES) return { ok: false, status: 413 };
  const text = await request.text();
  if (new TextEncoder().encode(text).byteLength > MAX_IMPORT_BYTES) return { ok: false, status: 413 };
  try {
    return { ok: true, value: JSON.parse(text) };
  } catch {
    return { ok: false, status: 400 };
  }
}

export class OAuthStoreDO {
  private signingKeyPromise: Promise<SigningKeyRecord> | undefined;

  constructor(private readonly state: DurableObjectState, private readonly env: Env) {
    void this.env;
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (!url.pathname.startsWith("/internal/oauth/")) return json({ ok: false, code: "not_found" }, 404);

    if (request.method === "GET" && url.pathname === "/internal/oauth/stats") return this.stats();
    if (request.method === "POST" && url.pathname === "/internal/oauth/import") return this.importState(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/client/get") return this.getClient(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/client/put") return this.putClient(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/access/get") return this.getToken(request, ACCESS_PREFIX);
    if (request.method === "POST" && url.pathname === "/internal/oauth/access/put") return this.putToken(request, ACCESS_PREFIX);
    if (request.method === "POST" && url.pathname === "/internal/oauth/refresh/get") return this.getToken(request, REFRESH_PREFIX);
    if (request.method === "POST" && url.pathname === "/internal/oauth/refresh/put") return this.putToken(request, REFRESH_PREFIX);
    if (request.method === "POST" && url.pathname === "/internal/oauth/refresh/consume") return this.consumeRefresh(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/code/put") return this.putCode(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/code/consume") return this.consumeCode(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/approval/put") return this.putApproval(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/approval/get") return this.getApproval(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/approval/approve") return this.approveApproval(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/approval/consume") return this.consumeApproval(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/grant/get") return this.getGrant(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/grant/revoke") return this.revokeGrant(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/connector/get") return this.getConnector(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/connector/list") return this.listConnectors();
    if (request.method === "POST" && url.pathname === "/internal/oauth/connector/revoke") return this.revokeConnector(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/connector/revoke-client") return this.revokeClientConnectors(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/automation/create") return this.createAutomation(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/automation/list") return this.listAutomations();
    if (request.method === "POST" && url.pathname === "/internal/oauth/automation/rotate") return this.rotateAutomation(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/automation/token/issue") return this.issueAutomationAccess(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/signing/ensure") return this.signingPublicKey();
    if (request.method === "POST" && url.pathname === "/internal/oauth/access/verify") return this.verifyAccess(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/token/issue") return this.issuePair(request);
    if (request.method === "POST" && url.pathname === "/internal/oauth/refresh/exchange") return this.exchangeRefresh(request);
    return json({ ok: false, code: "not_found" }, 404);
  }

  private async body(request: Request): Promise<Record<string, unknown> | null> {
    const parsed = await boundedJson(request);
    return parsed.ok && record(parsed.value) ? parsed.value : null;
  }

  private async getClient(request: Request): Promise<Response> {
    const body = await this.body(request);
    const clientId = body?.client_id;
    if (!boundedString(clientId, 4096)) return json({ ok: false, code: "bad_request" }, 400);
    const value = await this.state.storage.get<OAuthClientRecord>(CLIENT_PREFIX + clientId);
    return value ? json({ ok: true, record: value }) : json({ ok: false, code: "not_found" }, 404);
  }

  private async putClient(request: Request): Promise<Response> {
    const body = await this.body(request);
    const clientId = body?.client_id;
    const normalized = normalizeOAuthClient(body?.record);
    if (!boundedString(clientId, 4096) || !normalized) return json({ ok: false, code: "bad_request" }, 400);
    // Use the authoritative request/handler clock so DCR cleanup and the
    // persisted issued_at share one time source (no dual-clock drift).
    const nowMs = finiteEpoch(body?.now_ms) ? body!.now_ms as number : Date.now();

    if (clientId.startsWith("dcr-") && !(await this.state.storage.get(CLIENT_PREFIX + clientId))) {
      const [clients, approvals, connectors, grants] = await Promise.all([
        this.state.storage.list<OAuthClientRecord>({ prefix: CLIENT_PREFIX }),
        this.state.storage.list<OAuthApprovalRecord>({ prefix: APPROVAL_PREFIX }),
        this.state.storage.list<OAuthConnectorRecord>({ prefix: CONNECTOR_PREFIX }),
        this.state.storage.list<OAuthConnectorGrantRecord>({ prefix: GRANT_PREFIX }),
      ]);
      const protectedClientIds = new Set<string>();
      for (const raw of approvals.values()) {
        if (record(raw) && boundedString(raw.client_id, 4096) && finiteEpoch(raw.expires_at_ms) && raw.expires_at_ms > nowMs) {
          protectedClientIds.add(raw.client_id);
        }
      }
      for (const raw of connectors.values()) {
        const connector = normalizeConnector(raw);
        if (connector) protectedClientIds.add(connector.client_id);
      }
      for (const raw of grants.values()) {
        const grant = normalizeConnectorGrant(raw);
        if (grant) protectedClientIds.add(grant.client_id);
      }

      let retained = 0;
      for (const [key, raw] of clients) {
        const existingId = key.slice(CLIENT_PREFIX.length);
        if (!existingId.startsWith("dcr-")) continue;
        const existing = normalizeOAuthClient(raw);
        if (!existing) continue;
        const expiredUnapproved = existing.issued_at * 1000 + UNAPPROVED_DCR_TTL_MS <= nowMs
          && !protectedClientIds.has(existingId);
        if (expiredUnapproved) {
          await this.state.storage.delete(key);
          continue;
        }
        retained += 1;
      }
      if (retained >= MAX_UNAPPROVED_DCR_CLIENTS) {
        return json({ ok: false, code: "dcr_capacity_reached", retryable: true }, 429);
      }
    }

    await this.state.storage.put(CLIENT_PREFIX + clientId, normalized);
    return json({ ok: true });
  }

  private async getToken(request: Request, prefix: string): Promise<Response> {
    const body = await this.body(request);
    const hash = body?.hash;
    const nowSec = finiteEpoch(body?.now_sec) ? body!.now_sec as number : Math.floor(Date.now() / 1000);
    if (!boundedString(hash, 4096)) return json({ ok: false, code: "bad_request" }, 400);
    const key = prefix + hash;
    const value = await this.state.storage.get<OAuthTokenRecord>(key);
    if (!value) return json({ ok: false, code: "not_found" }, 404);
    if (value.expires_at <= nowSec) {
      await this.state.storage.delete(key);
      return json({ ok: false, code: "expired" }, 404);
    }
    return json({ ok: true, record: value });
  }

  private async putToken(request: Request, prefix: string): Promise<Response> {
    const body = await this.body(request);
    const hash = body?.hash;
    const nowSec = finiteEpoch(body?.now_sec) ? body!.now_sec as number : Math.floor(Date.now() / 1000);
    const normalized = normalizeOAuthToken(body?.record, nowSec);
    if (!boundedString(hash, 4096) || !normalized) return json({ ok: false, code: "bad_request" }, 400);
    await this.state.storage.put(prefix + hash, normalized);
    return json({ ok: true });
  }

  private async consumeRefresh(request: Request): Promise<Response> {
    const body = await this.body(request);
    const hash = body?.hash;
    const nowSec = finiteEpoch(body?.now_sec) ? body!.now_sec as number : Math.floor(Date.now() / 1000);
    if (!boundedString(hash, 4096)) return json({ ok: false, code: "bad_request" }, 400);
    const key = REFRESH_PREFIX + hash;
    let value: OAuthTokenRecord | undefined;
    await this.state.storage.transaction(async (txn) => {
      value = await txn.get<OAuthTokenRecord>(key);
      if (value !== undefined) await txn.delete(key);
    });
    if (!value) return json({ ok: false, code: "not_found" }, 404);
    if (value.expires_at <= nowSec) return json({ ok: false, code: "expired" }, 404);
    return json({ ok: true, record: value });
  }

  private async putCode(request: Request): Promise<Response> {
    const body = await this.body(request);
    const hash = body?.hash;
    const nowMs = finiteEpoch(body?.now_ms) ? body!.now_ms as number : Date.now();
    const normalized = normalizeOAuthCode(body?.record, nowMs);
    if (!boundedString(hash, 4096) || !normalized) return json({ ok: false, code: "bad_request" }, 400);
    await this.state.storage.put(CODE_PREFIX + hash, normalized);
    return json({ ok: true });
  }

  private async consumeCode(request: Request): Promise<Response> {
    const body = await this.body(request);
    const hash = body?.hash;
    const nowMs = finiteEpoch(body?.now_ms) ? body!.now_ms as number : Date.now();
    if (!boundedString(hash, 4096)) return json({ ok: false, code: "bad_request" }, 400);
    const key = CODE_PREFIX + hash;
    let value: OAuthCodeRecord | undefined;
    await this.state.storage.transaction(async (txn) => {
      value = await txn.get<OAuthCodeRecord>(key);
      if (value !== undefined) await txn.delete(key);
    });
    if (!value) return json({ ok: false, code: "not_found" }, 404);
    if (value.expires_at <= nowMs) return json({ ok: false, code: "expired" }, 404);
    return json({ ok: true, record: value });
  }

  private async putApproval(request: Request): Promise<Response> {
    const body = await this.body(request);
    const requestId = body?.request_id;
    const nowMs = finiteEpoch(body?.now_ms) ? body!.now_ms as number : Date.now();
    const normalized = normalizeOAuthApproval(body?.record, nowMs);
    if (!boundedString(requestId, 256) || !normalized) return json({ ok: false, code: "bad_request" }, 400);
    const key = APPROVAL_PREFIX + requestId;
    if (await this.state.storage.get(key) !== undefined) return json({ ok: false, code: "already_exists" }, 409);

    const rows = await this.state.storage.list<OAuthApprovalRecord>({ prefix: APPROVAL_PREFIX });
    let totalPending = 0;
    let clientPending = 0;
    for (const [existingKey, raw] of rows) {
      if (!record(raw) || !finiteEpoch(raw.expires_at_ms) || raw.expires_at_ms <= nowMs) {
        await this.state.storage.delete(existingKey);
        continue;
      }
      if (raw.status !== "pending" && raw.status !== "locked") continue;
      totalPending += 1;
      if (raw.client_id === normalized.client_id) clientPending += 1;
      if (
        raw.status === "pending" &&
        raw.client_id === normalized.client_id &&
        raw.redirect_uri === normalized.redirect_uri &&
        raw.code_challenge === normalized.code_challenge &&
        raw.resource === normalized.resource &&
        raw.scope === normalized.scope &&
        raw.state === normalized.state
      ) {
        return json({
          ok: false,
          code: "duplicate_pending",
          existing_request_id: existingKey.slice(APPROVAL_PREFIX.length),
          expires_at_ms: raw.expires_at_ms,
        }, 409);
      }
    }
    if (clientPending >= MAX_ACTIVE_PENDING_PER_CLIENT) {
      return json({ ok: false, code: "client_pending_limit", retryable: true }, 429);
    }
    if (totalPending >= MAX_ACTIVE_PENDING_TOTAL) {
      return json({ ok: false, code: "pending_capacity_reached", retryable: true }, 429);
    }

    await this.state.storage.put(key, normalized);
    return json({ ok: true });
  }

  private async getApproval(request: Request): Promise<Response> {
    const body = await this.body(request);
    const requestId = body?.request_id;
    const nowMs = finiteEpoch(body?.now_ms) ? body!.now_ms as number : Date.now();
    if (!boundedString(requestId, 256)) return json({ ok: false, code: "bad_request" }, 400);
    const key = APPROVAL_PREFIX + requestId;
    const current = await this.state.storage.get<OAuthApprovalRecord>(key);
    if (!current) return json({ ok: false, code: "not_found" }, 404);
    if (current.expires_at_ms <= nowMs) {
      await this.state.storage.delete(key);
      return json({ ok: false, code: "expired" }, 404);
    }
    return json({ ok: true, record: current });
  }

  private async approveApproval(request: Request): Promise<Response> {
    const body = await this.body(request);
    const requestId = body?.request_id;
    const codeHash = body?.code_hash;
    const approver = body?.approver;
    const nowMs = finiteEpoch(body?.now_ms) ? body!.now_ms as number : Date.now();
    if (!boundedString(requestId, 256) || !boundedString(codeHash, 256) || !boundedString(approver, 4096)) {
      return json({ ok: false, code: "bad_request" }, 400);
    }
    const key = APPROVAL_PREFIX + requestId;
    let result: { ok: boolean; code?: string; record?: OAuthApprovalRecord } = { ok: false, code: "not_found" };
    await this.state.storage.transaction(async (txn) => {
      const current = await txn.get<OAuthApprovalRecord>(key);
      if (!current) return;
      if (current.expires_at_ms <= nowMs) {
        await txn.delete(key);
        result = { ok: false, code: "expired" };
        return;
      }
      if (current.status === "locked") {
        result = { ok: false, code: "locked" };
        return;
      }
      if (current.approval_code_hash !== codeHash) {
        const attempts = Math.min(5, current.attempts + 1);
        await txn.put(key, { ...current, attempts, status: attempts >= 5 ? "locked" : current.status });
        result = { ok: false, code: attempts >= 5 ? "locked" : "invalid_code" };
        return;
      }
      if (current.status === "approved") {
        result = { ok: true, record: current };
        return;
      }
      const connectorId = current.connector_id ?? `conn_${randomBase64UrlToken().slice(0, 22)}`;
      const approved: OAuthApprovalRecord = {
        ...current,
        connector_id: connectorId,
        status: "approved",
        approved_at_ms: nowMs,
        approved_by: approver,
      };
      const connector: OAuthConnectorRecord = {
        connector_id: connectorId,
        client_id: current.client_id,
        status: "active",
        principal_type: "connector",
        capabilities: ["mcp_access"],
        resource: current.resource,
        scope: current.scope,
        redirect_uri: current.redirect_uri,
        auth_source: current.auth_source ?? "legacy",
        grant_generation: 1,
        approved_at_ms: nowMs,
        approved_by: approver,
        token_issue_count: 0,
      };
      await txn.put(key, approved);
      await txn.put(CONNECTOR_PREFIX + connectorId, connector);
      result = { ok: true, record: approved };
    });
    if (!result.ok) {
      const status = result.code === "invalid_code" ? 403
        : result.code === "locked" ? 423
          : 404;
      return json({ ok: false, code: result.code }, status);
    }
    return json({ ok: true, record: result.record });
  }

  private async consumeApproval(request: Request): Promise<Response> {
    const body = await this.body(request);
    const requestId = body?.request_id;
    const resumeHash = body?.resume_hash;
    const nowMs = finiteEpoch(body?.now_ms) ? body!.now_ms as number : Date.now();
    if (!boundedString(requestId, 256) || !boundedString(resumeHash, 256)) return json({ ok: false, code: "bad_request" }, 400);
    const key = APPROVAL_PREFIX + requestId;
    let result: { ok: boolean; code?: string; record?: OAuthApprovalRecord } = { ok: false, code: "not_found" };
    await this.state.storage.transaction(async (txn) => {
      const current = await txn.get<OAuthApprovalRecord>(key);
      if (!current) return;
      if (current.expires_at_ms <= nowMs) {
        await txn.delete(key);
        result = { ok: false, code: "expired" };
        return;
      }
      if (current.resume_hash !== resumeHash) {
        result = { ok: false, code: "invalid_resume" };
        return;
      }
      if (current.status === "pending") {
        result = { ok: false, code: "pending" };
        return;
      }
      if (current.status === "locked") {
        result = { ok: false, code: "locked" };
        return;
      }
      await txn.delete(key);
      result = { ok: true, record: current };
    });
    if (!result.ok) {
      const status = result.code === "pending" ? 202
        : result.code === "invalid_resume" ? 403
          : result.code === "locked" ? 423
            : 404;
      return json({ ok: false, code: result.code }, status);
    }
    return json({ ok: true, record: result.record });
  }

  private async getGrant(request: Request): Promise<Response> {
    const body = await this.body(request);
    const clientId = body?.client_id;
    if (!boundedString(clientId, 4096)) return json({ ok: false, code: "bad_request" }, 400);
    const value = await this.state.storage.get<OAuthConnectorGrantRecord>(GRANT_PREFIX + clientId);
    return value ? json({ ok: true, record: value }) : json({ ok: false, code: "not_found" }, 404);
  }

  private async revokeGrant(request: Request): Promise<Response> {
    const body = await this.body(request);
    const clientId = body?.client_id;
    const revokedBy = body?.revoked_by;
    const nowMs = finiteEpoch(body?.now_ms) ? body!.now_ms as number : Date.now();
    if (!boundedString(clientId, 4096) || !boundedString(revokedBy, 4096)) return json({ ok: false, code: "bad_request" }, 400);
    const key = GRANT_PREFIX + clientId;
    const current = await this.state.storage.get<OAuthConnectorGrantRecord>(key);
    const [client, connectors, refresh, access] = await Promise.all([
      this.state.storage.get<OAuthClientRecord>(CLIENT_PREFIX + clientId),
      this.state.storage.list<OAuthConnectorRecord>({ prefix: CONNECTOR_PREFIX }),
      this.state.storage.list<OAuthTokenRecord>({ prefix: REFRESH_PREFIX }),
      this.state.storage.list<OAuthTokenRecord>({ prefix: ACCESS_PREFIX }),
    ]);
    const deletes: string[] = [];
    for (const [tokenKey, token] of refresh) if (token.client_id === clientId) deletes.push(tokenKey);
    for (const [tokenKey, token] of access) if (token.client_id === clientId) deletes.push(tokenKey);
    // The legacy client_id kill-switch fences legacy pre-v0.4.6 tokens. If an
    // approved Connector instance (CONNECTOR_PREFIX) already exists for this
    // client, this is a real connector revocation (not a brand-new legacy
    // tombstone), so legacy_tombstone_created must be false.
    const hasConnector = [...connectors.values()].some((raw) => {
      const connector = normalizeConnector(raw);
      return connector !== null && connector.client_id === clientId;
    });
    const revoked: OAuthConnectorGrantRecord = {
      ...(current ?? { client_id: clientId }),
      status: "revoked",
      revoked_at_ms: nowMs,
      revoked_by: revokedBy,
    };
    await this.state.storage.put(key, revoked);
    if (deletes.length > 0) await Promise.all(deletes.map((tokenKey) => this.state.storage.delete(tokenKey)));
    return json({
      ok: true,
      client_id: clientId,
      revoked_at_ms: nowMs,
      deleted_tokens: deletes.length,
      legacy_tombstone_created: current === undefined && client === undefined && !hasConnector,
    });
  }

  private async getConnector(request: Request): Promise<Response> {
    const body = await this.body(request);
    const connectorId = body?.connector_id;
    if (typeof connectorId !== "string" || !/^conn_[A-Za-z0-9_-]{8,128}$/.test(connectorId)) {
      return json({ ok: false, code: "bad_request" }, 400);
    }
    const connector = normalizeConnector(await this.state.storage.get<OAuthConnectorRecord>(CONNECTOR_PREFIX + connectorId));
    if (!connector) return json({ ok: false, code: "not_found" }, 404);
    const client = normalizeOAuthClient(await this.state.storage.get<OAuthClientRecord>(CLIENT_PREFIX + connector.client_id));
    return json({
      ok: true,
      connector: {
        ...connector,
        client_name: client?.client_name ?? null,
      },
    });
  }

  private async listConnectors(): Promise<Response> {
    const rows = await this.state.storage.list<OAuthConnectorRecord>({ prefix: CONNECTOR_PREFIX });
    const connectors: Array<Record<string, unknown>> = [];
    for (const raw of rows.values()) {
      const connector = normalizeConnector(raw);
      if (!connector) continue;
      const client = normalizeOAuthClient(await this.state.storage.get<OAuthClientRecord>(CLIENT_PREFIX + connector.client_id));
      connectors.push({ ...connector, client_name: client?.client_name ?? null });
      if (connectors.length >= 256) break;
    }
    connectors.sort((a, b) => Number(b.approved_at_ms ?? 0) - Number(a.approved_at_ms ?? 0));
    return json({ ok: true, connectors });
  }

  private async revokeConnector(request: Request): Promise<Response> {
    const body = await this.body(request);
    const connectorId = body?.connector_id;
    const revokedBy = body?.revoked_by;
    const nowMs = finiteEpoch(body?.now_ms) ? body!.now_ms as number : Date.now();
    if (
      typeof connectorId !== "string" || !/^conn_[A-Za-z0-9_-]{8,128}$/.test(connectorId) ||
      !boundedString(revokedBy, 4096)
    ) return json({ ok: false, code: "bad_request" }, 400);
    const key = CONNECTOR_PREFIX + connectorId;
    let outcome: OAuthConnectorRecord | null = null;
    await this.state.storage.transaction(async (txn) => {
      const current = normalizeConnector(await txn.get<OAuthConnectorRecord>(key));
      if (!current) return;
      const revoked: OAuthConnectorRecord = current.status === "revoked"
        ? current
        : {
            ...current,
            status: "revoked",
            grant_generation: current.grant_generation + 1,
            revoked_at_ms: nowMs,
            revoked_by: revokedBy,
          };
      if (current.status !== "revoked") await txn.put(key, revoked);
      outcome = revoked;
    });
    if (!outcome) return json({ ok: false, code: "not_found" }, 404);

    const [refresh, access] = await Promise.all([
      this.state.storage.list<OAuthTokenRecord>({ prefix: REFRESH_PREFIX }),
      this.state.storage.list<OAuthTokenRecord>({ prefix: ACCESS_PREFIX }),
    ]);
    const deletes: string[] = [];
    for (const [tokenKey, token] of refresh) if (token.connector_id === connectorId) deletes.push(tokenKey);
    for (const [tokenKey, token] of access) if (token.connector_id === connectorId) deletes.push(tokenKey);
    if (deletes.length > 0) await Promise.all(deletes.map((tokenKey) => this.state.storage.delete(tokenKey)));
    return json({ ok: true, connector_id: connectorId, revoked_at_ms: nowMs, deleted_tokens: deletes.length });
  }

  private async revokeClientConnectors(request: Request): Promise<Response> {
    const body = await this.body(request);
    const clientId = body?.client_id;
    const revokedBy = body?.revoked_by;
    const nowMs = finiteEpoch(body?.now_ms) ? body!.now_ms as number : Date.now();
    if (!boundedString(clientId, 4096) || !boundedString(revokedBy, 4096)) {
      return json({ ok: false, code: "bad_request" }, 400);
    }
    const rows = await this.state.storage.list<OAuthConnectorRecord>({ prefix: CONNECTOR_PREFIX });
    let revokedConnectors = 0;
    for (const [key, raw] of rows) {
      const connector = normalizeConnector(raw);
      if (!connector || connector.client_id !== clientId || connector.status === "revoked") continue;
      await this.state.storage.put(key, {
        ...connector,
        status: "revoked",
        grant_generation: connector.grant_generation + 1,
        revoked_at_ms: nowMs,
        revoked_by: revokedBy,
      } satisfies OAuthConnectorRecord);
      revokedConnectors += 1;
    }

    const currentGrant = await this.state.storage.get<OAuthConnectorGrantRecord>(GRANT_PREFIX + clientId);
    const client = await this.state.storage.get<OAuthClientRecord>(CLIENT_PREFIX + clientId);
    await this.state.storage.put(GRANT_PREFIX + clientId, {
      ...(currentGrant ?? { client_id: clientId }),
      status: "revoked",
      revoked_at_ms: nowMs,
      revoked_by: revokedBy,
    } satisfies OAuthConnectorGrantRecord);

    const [refresh, access] = await Promise.all([
      this.state.storage.list<OAuthTokenRecord>({ prefix: REFRESH_PREFIX }),
      this.state.storage.list<OAuthTokenRecord>({ prefix: ACCESS_PREFIX }),
    ]);
    const deletes: string[] = [];
    for (const [tokenKey, token] of refresh) if (token.client_id === clientId) deletes.push(tokenKey);
    for (const [tokenKey, token] of access) if (token.client_id === clientId) deletes.push(tokenKey);
    if (deletes.length > 0) await Promise.all(deletes.map((tokenKey) => this.state.storage.delete(tokenKey)));
    return json({
      ok: true,
      client_id: clientId,
      revoked_connectors: revokedConnectors,
      deleted_tokens: deletes.length,
      legacy_tombstone_created: !client && !currentGrant && revokedConnectors === 0,
    });
  }

  private async createAutomation(request: Request): Promise<Response> {
    const body = await this.body(request);
    const clientId = body?.client_id;
    const clientSecretHash = body?.client_secret_hash;
    const clientName = body?.client_name;
    const resource = body?.resource;
    const scope = body?.scope;
    const createdBy = body?.created_by;
    const deviceId = body?.device_id;
    const deviceName = body?.device_name;
    const nowMs = finiteEpoch(body?.now_ms) ? body!.now_ms as number : Date.now();
    if (
      !boundedString(clientId, 256) || !clientId.startsWith("svc_") ||
      !boundedString(clientSecretHash, 256) || !boundedString(clientName, 256) ||
      !boundedString(resource, 4096) || scope !== "mcp" || !boundedString(createdBy, 4096) ||
      typeof deviceId !== "string" || !/^dev_[0-9A-HJKMNP-TV-Z]{26}$/i.test(deviceId) ||
      typeof deviceName !== "string" || deviceName.length === 0 || deviceName.length > 256
    ) return json({ ok: false, code: "bad_request" }, 400);

    let created = false;
    await this.state.storage.transaction(async (txn) => {
      if (await txn.get(CLIENT_PREFIX + clientId)) return;
      const client: OAuthClientRecord = {
        client_secret_hash: clientSecretHash,
        redirect_uris: [],
        token_endpoint_auth_method: "client_secret_post",
        grant_types: ["client_credentials"],
        scope: "mcp",
        client_name: clientName,
        issued_at: Math.floor(nowMs / 1000),
      };
      const grant: OAuthConnectorGrantRecord = {
        client_id: clientId,
        status: "active",
        principal_type: "automation",
        resource,
        scope: "mcp",
        device_id: deviceId,
        device_name: deviceName,
        approved_at_ms: nowMs,
        approved_by: createdBy,
        token_issue_count: 0,
      };
      await txn.put(CLIENT_PREFIX + clientId, client);
      await txn.put(GRANT_PREFIX + clientId, grant);
      created = true;
    });
    return created
      ? json({ ok: true, client_id: clientId, client_name: clientName, device_id: deviceId, device_name: deviceName, created_at_ms: nowMs })
      : json({ ok: false, code: "automation_client_exists" }, 409);
  }

  private async listAutomations(): Promise<Response> {
    const grants = await this.state.storage.list<OAuthConnectorGrantRecord>({ prefix: GRANT_PREFIX });
    const automations: Array<Record<string, unknown>> = [];
    for (const [key, rawGrant] of grants) {
      const grant = normalizeConnectorGrant(rawGrant);
      if (!grant || grant.principal_type !== "automation") continue;
      const clientId = key.slice(GRANT_PREFIX.length);
      const client = normalizeOAuthClient(await this.state.storage.get<OAuthClientRecord>(CLIENT_PREFIX + clientId));
      if (!client) continue;
      automations.push({
        client_id: clientId,
        name: client.client_name ?? clientId,
        status: grant.status,
        device_id: grant.device_id ?? null,
        device_name: grant.device_name ?? null,
        created_at_ms: grant.approved_at_ms ?? client.issued_at * 1000,
        created_by: grant.approved_by ?? null,
        last_token_issued_at_ms: grant.last_token_issued_at_ms ?? null,
        token_issue_count: grant.token_issue_count ?? 0,
        last_rotated_at_ms: grant.last_rotated_at_ms ?? null,
        last_rotated_by: grant.last_rotated_by ?? null,
        revoked_at_ms: grant.revoked_at_ms ?? null,
      });
      if (automations.length >= 256) break;
    }
    automations.sort((a, b) => Number(b.created_at_ms ?? 0) - Number(a.created_at_ms ?? 0));
    return json({ ok: true, automations });
  }

  private async rotateAutomation(request: Request): Promise<Response> {
    const body = await this.body(request);
    const clientId = body?.client_id;
    const clientSecretHash = body?.client_secret_hash;
    const rotatedBy = body?.rotated_by;
    const nowMs = finiteEpoch(body?.now_ms) ? body!.now_ms as number : Date.now();
    if (!boundedString(clientId, 256) || !boundedString(clientSecretHash, 256) || !boundedString(rotatedBy, 4096)) {
      return json({ ok: false, code: "bad_request" }, 400);
    }
    const outcome: { value: "rotated" | "not_found" | "not_automation" | "revoked" } = { value: "not_found" };
    await this.state.storage.transaction(async (txn) => {
      const client = normalizeOAuthClient(await txn.get<OAuthClientRecord>(CLIENT_PREFIX + clientId));
      const grant = normalizeConnectorGrant(await txn.get<OAuthConnectorGrantRecord>(GRANT_PREFIX + clientId));
      if (!client || !grant) return;
      if (grant.principal_type !== "automation") { outcome.value = "not_automation"; return; }
      if (grant.status !== "active") { outcome.value = "revoked"; return; }
      await txn.put(CLIENT_PREFIX + clientId, { ...client, client_secret_hash: clientSecretHash });
      await txn.put(GRANT_PREFIX + clientId, {
        ...grant,
        last_rotated_at_ms: nowMs,
        last_rotated_by: rotatedBy,
      });
      outcome.value = "rotated";
    });
    if (outcome.value === "rotated") return json({ ok: true, client_id: clientId, rotated_at_ms: nowMs });
    if (outcome.value === "revoked") return json({ ok: false, code: "automation_client_revoked" }, 409);
    if (outcome.value === "not_automation") return json({ ok: false, code: "not_automation_client" }, 409);
    return json({ ok: false, code: "automation_client_not_found" }, 404);
  }

  private async issueAutomationAccess(request: Request): Promise<Response> {
    const body = await this.body(request);
    const clientId = body?.client_id;
    const resource = body?.resource;
    const nowSec = finiteEpoch(body?.now_sec) ? Math.floor(body!.now_sec as number) : Math.floor(Date.now() / 1000);
    const accessTtlSec = finiteEpoch(body?.access_ttl_sec) ? Math.floor(body!.access_ttl_sec as number) : 3600;
    if (!boundedString(clientId, 256) || !boundedString(resource, 4096) || accessTtlSec < 60 || accessTtlSec > 3600) {
      return json({ ok: false, code: "bad_request" }, 400);
    }
    const initial = normalizeConnectorGrant(await this.state.storage.get<OAuthConnectorGrantRecord>(GRANT_PREFIX + clientId));
    if (!initial || initial.status !== "active" || initial.principal_type !== "automation" || initial.resource !== resource || initial.scope !== "mcp") {
      return json({ ok: false, code: "invalid_grant" }, 400);
    }
    const key = await this.ensureSigningKey();
    const privateKey = await crypto.subtle.importKey(
      "jwk",
      key.private_jwk,
      { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
      false,
      ["sign"],
    );
    const issuer = this.env.OAUTH_ISSUER?.replace(/\/+$/, "");
    if (!issuer) return json({ ok: false, code: "oauth_issuer_missing" }, 500);
    const deviceId = initial.device_id;
    const accessToken = await issueRs256AccessJwt(
      privateKey,
      issuer,
      resource,
      clientId,
      accessTtlSec,
      {
        nowSec,
        additionalClaims: {
          key_id: key.kid,
          principal_type: "automation",
          ...(deviceId ? { device_id: deviceId } : {}),
        },
      },
    );
    let committed = false;
    await this.state.storage.transaction(async (txn) => {
      const current = normalizeConnectorGrant(await txn.get<OAuthConnectorGrantRecord>(GRANT_PREFIX + clientId));
      if (!current || current.status !== "active" || current.principal_type !== "automation" || current.resource !== resource) return;
      await txn.put(GRANT_PREFIX + clientId, {
        ...current,
        last_token_issued_at_ms: nowSec * 1000,
        token_issue_count: (current.token_issue_count ?? 0) + 1,
      });
      committed = true;
    });
    if (!committed) return json({ ok: false, code: "invalid_grant" }, 400);
    return json({
      ok: true,
      token: {
        access_token: accessToken,
        token_type: "Bearer",
        expires_in: accessTtlSec,
        scope: "mcp",
        key_id: key.kid,
      },
    });
  }

  private async grantAllowsAccess(clientId: string): Promise<boolean> {
    const grant = await this.state.storage.get<OAuthConnectorGrantRecord>(GRANT_PREFIX + clientId);
    // Missing means a pre-v0.4.6 legacy grant. Preserve ordinary MCP access,
    // but approval authority is never inferred from a missing grant record.
    return grant?.status !== "revoked";
  }

  private async connectorAllowsAccess(
    clientId: string,
    connectorId: string | undefined,
    grantGeneration: number | undefined,
  ): Promise<boolean> {
    if (!(await this.grantAllowsAccess(clientId))) return false;
    if (!connectorId && grantGeneration === undefined) return this.grantAllowsAccess(clientId);
    if (!connectorId || !grantGeneration) return false;
    const connector = normalizeConnector(
      await this.state.storage.get<OAuthConnectorRecord>(CONNECTOR_PREFIX + connectorId),
    );
    return connector?.status === "active"
      && connector.client_id === clientId
      && connector.grant_generation === grantGeneration;
  }

  private async automationAllowsAccess(
    clientId: string,
    deviceId: string | undefined,
  ): Promise<boolean> {
    // An automation token is valid only while its grant is active AND still
    // bound to the exact enrolled device it was created for. Re-verified on
    // every access so revoke or re-binding immediately fences issued JWTs.
    const grant = normalizeConnectorGrant(
      await this.state.storage.get<OAuthConnectorGrantRecord>(GRANT_PREFIX + clientId),
    );
    return grant?.status === "active"
      && grant.principal_type === "automation"
      && grant.device_id !== undefined
      && grant.device_id === deviceId;
  }

  private async recordConnectorTokenIssue(
    connectorId: string | undefined,
    grantGeneration: number | undefined,
    nowMs: number,
  ): Promise<void> {
    if (!connectorId || !grantGeneration) return;
    const key = CONNECTOR_PREFIX + connectorId;
    await this.state.storage.transaction(async (txn) => {
      const connector = normalizeConnector(await txn.get<OAuthConnectorRecord>(key));
      if (!connector || connector.status !== "active" || connector.grant_generation !== grantGeneration) return;
      await txn.put(key, {
        ...connector,
        last_token_issued_at_ms: nowMs,
        token_issue_count: (connector.token_issue_count ?? 0) + 1,
      } satisfies OAuthConnectorRecord);
    });
  }

  private async ensureSigningKey(): Promise<SigningKeyRecord> {
    if (this.signingKeyPromise) return this.signingKeyPromise;
    this.signingKeyPromise = (async () => {
      const existing = await this.state.storage.get<SigningKeyRecord>(SIGNING_KEY);
      if (existing?.kid && existing.private_jwk && existing.public_jwk) return existing;
      const pair = await crypto.subtle.generateKey(
        {
          name: "RSASSA-PKCS1-v1_5",
          modulusLength: 2048,
          publicExponent: new Uint8Array([1, 0, 1]),
          hash: "SHA-256",
        },
        true,
        ["sign", "verify"],
      ) as CryptoKeyPair;
      const kid = randomBase64UrlToken().slice(0, 22);
      const privateJwk = await crypto.subtle.exportKey("jwk", pair.privateKey) as JsonWebKey;
      const publicJwk = await crypto.subtle.exportKey("jwk", pair.publicKey) as JsonWebKey;
      const decorate = (jwk: JsonWebKey): StoredJwk => ({ ...jwk, kid, alg: "RS256", use: "sig" } as StoredJwk);
      const record: SigningKeyRecord = {
        kid,
        private_jwk: decorate(privateJwk),
        public_jwk: decorate(publicJwk),
        created_at: Math.floor(Date.now() / 1000),
      };
      await this.state.storage.put(SIGNING_KEY, record);
      return record;
    })();
    try {
      return await this.signingKeyPromise;
    } catch (error) {
      this.signingKeyPromise = undefined;
      throw error;
    }
  }

  private async signingPublicKey(): Promise<Response> {
    const key = await this.ensureSigningKey();
    return json({ ok: true, kid: key.kid, public_jwk: key.public_jwk, created_at: key.created_at });
  }

  private async verifyAccess(request: Request): Promise<Response> {
    const body = await this.body(request);
    const token = body?.token;
    const nowSec = finiteEpoch(body?.now_sec) ? body!.now_sec as number : Math.floor(Date.now() / 1000);
    if (!boundedString(token, 16384)) return json({ ok: false, code: "bad_request" }, 400);
    const issuer = this.env.OAUTH_ISSUER?.replace(/\/+$/, "");
    if (!issuer) return json({ ok: false, code: "oauth_not_configured" }, 503);

    if (token.includes(".")) {
      try {
        const signing = await this.ensureSigningKey();
        const publicKey = await crypto.subtle.importKey(
          "jwk",
          signing.public_jwk,
          { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
          false,
          ["verify"],
        );
        const verifier = createRs256AccessTokenVerifier(createOAuthIdentity(issuer), publicKey);
        const verdict = await verifier.verify(token, nowSec);
        if (verdict.ok) {
          if (!verdict.clientId) return json({ ok: false, code: "invalid_token" }, 401);
          if (verdict.principalType === "automation") {
            if (!(await this.automationAllowsAccess(verdict.clientId, verdict.deviceId))) {
              return json({ ok: false, code: "invalid_token" }, 401);
            }
          } else if (!(await this.connectorAllowsAccess(verdict.clientId, verdict.connectorId, verdict.grantGeneration))) {
            return json({ ok: false, code: "invalid_token" }, 401);
          }
          return json({
            ok: true,
            client_id: verdict.clientId,
            connector_id: verdict.connectorId ?? null,
            grant_generation: verdict.grantGeneration ?? null,
            principal_type: verdict.principalType ?? null,
            device_id: verdict.deviceId ?? null,
            source: "edge_jwt",
          });
        }
      } catch {
        // Continue to the opaque compatibility lookup below.
      }
    }

    const hash = await hashOpaqueToken(token);
    const key = ACCESS_PREFIX + hash;
    const legacy = await this.state.storage.get<OAuthTokenRecord>(key);
    if (!legacy) return json({ ok: false, code: "invalid_token" }, 401);
    if (legacy.expires_at <= nowSec) {
      await this.state.storage.delete(key);
      return json({ ok: false, code: "invalid_token" }, 401);
    }
    if (!(await this.connectorAllowsAccess(legacy.client_id, legacy.connector_id, legacy.grant_generation))) {
      return json({ ok: false, code: "invalid_token" }, 401);
    }
    return json({
      ok: true,
      client_id: legacy.client_id,
      connector_id: legacy.connector_id ?? null,
      grant_generation: legacy.grant_generation ?? null,
      source: "opaque",
    });
  }

  private parseIssueInput(body: Record<string, unknown> | null): ParsedIssuePairInput | null {
    if (!body || !boundedString(body.client_id, 4096) || !boundedString(body.resource, 4096)) return null;
    const nowSec = finiteEpoch(body.now_sec) ? body.now_sec as number : Math.floor(Date.now() / 1000);
    const accessTtl = finiteEpoch(body.access_ttl_sec) ? body.access_ttl_sec as number : 86400;
    const refreshTtl = finiteEpoch(body.refresh_ttl_sec) ? body.refresh_ttl_sec as number : 2592000;
    if (accessTtl < 60 || accessTtl > 7 * 86400 || refreshTtl < 300 || refreshTtl > 90 * 86400) return null;
    const connectorId = typeof body.connector_id === "string" && /^conn_[A-Za-z0-9_-]{8,128}$/.test(body.connector_id)
      ? body.connector_id
      : undefined;
    const grantGeneration = Number.isSafeInteger(body.grant_generation) && (body.grant_generation as number) > 0
      ? body.grant_generation as number
      : undefined;
    if ((connectorId === undefined) !== (grantGeneration === undefined)) return null;
    return {
      client_id: body.client_id,
      ...(connectorId ? { connector_id: connectorId, grant_generation: grantGeneration } : {}),
      resource: body.resource,
      now_sec: nowSec,
      access_ttl_sec: accessTtl,
      refresh_ttl_sec: refreshTtl,
    };
  }

  private async createPair(input: ParsedIssuePairInput): Promise<Record<string, unknown>> {
    const key = await this.ensureSigningKey();
    const privateKey = await crypto.subtle.importKey(
      "jwk",
      key.private_jwk,
      { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
      false,
      ["sign"],
    );
    const issuer = this.env.OAUTH_ISSUER?.replace(/\/+$/, "");
    if (!issuer) throw new Error("oauth-store: OAUTH_ISSUER is required for token issuance");
    const accessToken = await issueRs256AccessJwt(
      privateKey,
      issuer,
      input.resource,
      input.client_id,
      input.access_ttl_sec,
      {
        nowSec: input.now_sec,
        additionalClaims: {
          key_id: key.kid,
          ...(input.connector_id
            ? { connector_id: input.connector_id, grant_generation: input.grant_generation }
            : {}),
        },
      },
    );
    const refreshToken = randomBase64UrlToken();
    const refreshHash = await hashOpaqueToken(refreshToken);
    const refreshRecord: OAuthTokenRecord = {
      client_id: input.client_id,
      ...(input.connector_id
        ? { connector_id: input.connector_id, grant_generation: input.grant_generation }
        : {}),
      resource: input.resource,
      scope: "mcp",
      expires_at: input.now_sec + input.refresh_ttl_sec,
    };
    await this.state.storage.put(REFRESH_PREFIX + refreshHash, refreshRecord);
    await this.recordConnectorTokenIssue(input.connector_id, input.grant_generation, input.now_sec * 1000);
    return {
      access_token: accessToken,
      token_type: "Bearer",
      expires_in: input.access_ttl_sec,
      refresh_token: refreshToken,
      scope: "mcp",
      key_id: key.kid,
    };
  }

  private async issuePair(request: Request): Promise<Response> {
    const input = this.parseIssueInput(await this.body(request));
    if (!input) return json({ ok: false, code: "bad_request" }, 400);
    if (!(await this.connectorAllowsAccess(input.client_id, input.connector_id, input.grant_generation))) {
      return json({ ok: false, code: "invalid_grant" }, 400);
    }
    return json({ ok: true, token: await this.createPair(input) });
  }

  private async exchangeRefresh(request: Request): Promise<Response> {
    const body = await this.body(request);
    const hash = body?.hash;
    const input = this.parseIssueInput(body);
    if (!boundedString(hash, 4096) || !input) return json({ ok: false, code: "bad_request" }, 400);
    const key = REFRESH_PREFIX + hash;
    let previous: OAuthTokenRecord | undefined;
    await this.state.storage.transaction(async (txn) => {
      previous = await txn.get<OAuthTokenRecord>(key);
      if (previous !== undefined) await txn.delete(key);
    });
    if (!previous) return json({ ok: false, code: "invalid_grant" }, 400);
    if (previous.expires_at <= input.now_sec) return json({ ok: false, code: "invalid_grant" }, 400);
    if (previous.client_id !== input.client_id || previous.resource !== input.resource) {
      return json({ ok: false, code: "invalid_grant" }, 400);
    }
    if (!(await this.connectorAllowsAccess(previous.client_id, previous.connector_id, previous.grant_generation))) {
      return json({ ok: false, code: "invalid_grant" }, 400);
    }
    return json({
      ok: true,
      token: await this.createPair({
        ...input,
        ...(previous.connector_id
          ? { connector_id: previous.connector_id, grant_generation: previous.grant_generation }
          : {}),
      }),
    });
  }

  /**
   * Adopt a production RS256 signing key pair through the authenticated,
   * bounded import path. Validation happens fully before any store mutation.
   * Errors are generic and never expose kid/JWK fields/PEM/exception text.
   */
  private async importSigningKey(
    raw: unknown,
    overwrite: boolean,
    nowSec: number,
  ): Promise<{ ok: true; imported: number; skipped: number } | { ok: false }> {
    if (!record(raw)) return { ok: false };
    const kid = raw.kid;
    const privateJwk = raw.private_jwk;
    const publicJwk = raw.public_jwk;
    const created = finiteEpoch(raw.created_at) ? raw.created_at as number : nowSec;
    if (!boundedString(kid, 256) || !record(privateJwk) || !record(publicJwk)) return { ok: false };

    // Reject any JWK fields that would make this unsafe to store (we never
    // want a public key leaking a private key's presence). The store keeps
    // the private JWK internally but never returns it.
    const validRsa = (j: Record<string, unknown>, wantPrivate: boolean): boolean => {
      if (j.kty !== "RSA") return false;
      if (typeof j.n !== "string" || j.n.length === 0 || j.n.length > 1024) return false;
      if (typeof j.e !== "string" || j.e.length === 0 || j.e.length > 64) return false;
      if (j.alg !== undefined && j.alg !== "RS256") return false;
      if (j.use !== undefined && j.use !== "sig") return false;
      if (wantPrivate) {
        if (typeof j.d !== "string" || j.d.length === 0) return false;
      } else {
        if (j.d !== undefined) return false; // public key must not carry d
      }
      return true;
    };
    if (!validRsa(privateJwk, true) || !validRsa(publicJwk, false)) return { ok: false };

    // Public/private n and e must match.
    if (privateJwk.n !== publicJwk.n || privateJwk.e !== publicJwk.e) return { ok: false };

    // Verify the pair actually works: import, sign a non-secret probe, verify.
    try {
      const privateKey = await crypto.subtle.importKey(
        "jwk",
        privateJwk as unknown as JsonWebKey,
        { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
        false,
        ["sign"],
      );
      const publicKey = await crypto.subtle.importKey(
        "jwk",
        publicJwk as unknown as JsonWebKey,
        { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
        false,
        ["verify"],
      );
      const signature = new Uint8Array(
        await crypto.subtle.sign("RSASSA-PKCS1-v1_5", privateKey, SIGNING_PROBE),
      );
      const valid = await crypto.subtle.verify("RSASSA-PKCS1-v1_5", publicKey, signature, SIGNING_PROBE);
      if (!valid) return { ok: false };
    } catch {
      return { ok: false };
    }

    // Existing key and overwrite not requested → skip (preserve continuity).
    if (!overwrite) {
      const existing = await this.state.storage.get<SigningKeyRecord>(SIGNING_KEY);
      if (existing?.kid && existing.private_jwk && existing.public_jwk) {
        return { ok: true, imported: 0, skipped: 1 };
      }
    }

    const localRecord: SigningKeyRecord = {
      kid,
      private_jwk: privateJwk as unknown as StoredJwk,
      public_jwk: publicJwk as unknown as StoredJwk,
      created_at: created,
    };
    await this.state.storage.put(SIGNING_KEY, localRecord);
    // Drop any cached in-memory signing key so ensureSigningKey() re-reads the
    // newly adopted pair on the next issuance/rotation.
    this.signingKeyPromise = undefined;
    return { ok: true, imported: 1, skipped: 0 };
  }

  private async importState(request: Request): Promise<Response> {
    const parsed = await boundedJson(request);
    if (!parsed.ok) return json({ ok: false, code: parsed.status === 413 ? "payload_too_large" : "bad_request" }, parsed.status);
    if (!record(parsed.value)) return json({ ok: false, code: "bad_request" }, 400);
    const body = parsed.value as ImportBody;
    const nowSec = finiteEpoch(body.now_sec) ? body.now_sec! : Math.floor(Date.now() / 1000);
    const overwrite = body.overwrite === true;
    const maps = [body.clients ?? {}, body.tokens ?? {}, body.refresh ?? {}];
    if (!maps.every(record)) return json({ ok: false, code: "bad_request" }, 400);
    const total = maps.reduce((sum, map) => sum + Object.keys(map).length, 0)
      + (body.signing_key !== undefined ? 1 : 0);
    if (total > MAX_IMPORT_RECORDS) return json({ ok: false, code: "too_many_records" }, 413);

    const result = {
      clients: { imported: 0, skipped: 0, invalid: 0 },
      tokens: { imported: 0, skipped: 0, invalid: 0 },
      refresh: { imported: 0, skipped: 0, invalid: 0 },
      signing_key: { imported: 0, skipped: 0 },
    };

    // Adopt a production RS256 signing key pair BEFORE mutating any other
    // state so an invalid pair never leaves partially-imported records behind.
    if (body.signing_key !== undefined) {
      const signing = await this.importSigningKey(body.signing_key, overwrite, nowSec);
      if (!signing.ok) return json({ ok: false, code: "invalid_signing_key" }, 400);
      result.signing_key = { imported: signing.imported, skipped: signing.skipped };
    }

    const stage = async (
      input: Record<string, unknown>,
      prefix: string,
      kind: "clients" | "tokens" | "refresh",
    ): Promise<void> => {
      for (const [rawKey, rawValue] of Object.entries(input)) {
        if (!validMapKey(rawKey)) {
          result[kind].invalid += 1;
          continue;
        }
        const normalized = kind === "clients"
          ? normalizeOAuthClient(rawValue)
          : normalizeOAuthToken(rawValue, nowSec);
        if (!normalized) {
          result[kind].invalid += 1;
          continue;
        }
        const key = prefix + rawKey;
        if (!overwrite) {
          const existing = await this.state.storage.get(key);
          if (existing !== undefined) {
            result[kind].skipped += 1;
            continue;
          }
        }
        await this.state.storage.put(key, normalized);
        result[kind].imported += 1;
      }
    };

    await stage(body.clients ?? {}, CLIENT_PREFIX, "clients");
    await stage(body.tokens ?? {}, ACCESS_PREFIX, "tokens");
    await stage(body.refresh ?? {}, REFRESH_PREFIX, "refresh");
    return json({ ok: true, overwrite, total, result });
  }

  private async stats(): Promise<Response> {
    const [clients, access, refresh, codes, approvals, grants] = await Promise.all([
      this.state.storage.list({ prefix: CLIENT_PREFIX }),
      this.state.storage.list({ prefix: ACCESS_PREFIX }),
      this.state.storage.list({ prefix: REFRESH_PREFIX }),
      this.state.storage.list({ prefix: CODE_PREFIX }),
      this.state.storage.list({ prefix: APPROVAL_PREFIX }),
      this.state.storage.list({ prefix: GRANT_PREFIX }),
    ]);
    return json({
      ok: true,
      clients: clients.size,
      access: access.size,
      refresh: refresh.size,
      codes: codes.size,
      approvals: approvals.size,
      grants: grants.size,
    });
  }
}
