/**
 * Persisted OAuth Durable Object record shapes and their pure
 * validation/normalization compatibility rules.
 *
 * This module is deliberately side-effect free. It owns no storage keys or
 * prefixes, no transactions, no Durable Object state, and no issuance or
 * consumption path: `oauth-store-do.ts` stays the single authority for
 * persisted OAuth state. Record shapes, defaults, bounds, regexes, sorting and
 * legacy compatibility here are the persisted-format contract, so they must
 * keep byte-for-byte behavior.
 */

const MAX_STRING = 4096;

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
  last_used_at_ms?: number;
  last_token_issued_at_ms?: number;
  token_issue_count?: number;
  revoked_at_ms?: number;
  revoked_by?: string;
  revocation_reason?: string;
}

export interface OAuthConnectorGrantRecord {
  client_id: string;
  status: "active" | "revoked";
  /** Explicit client-wide kill switch. Missing on older records for compatibility. */
  revocation_scope?: "client";
  can_approve_connectors?: boolean;
  webchat_control?: OAuthWebChatControlGrant[];
  page_assist?: OAuthPageAssistGrant[];
  principal_type?: "connector" | "automation";
  resource?: string;
  scope?: string;
  connector_id?: string;
  grant_generation?: number;
  /** Bound device_id for automation grants; validated as canonical dev_<26-char> ULID. */
  device_id?: string;
  device_name?: string;
  approved_at_ms?: number;
  approved_by?: string;
  last_used_at_ms?: number;
  last_token_issued_at_ms?: number;
  token_issue_count?: number;
  last_rotated_at_ms?: number;
  last_rotated_by?: string;
  revoked_at_ms?: number;
  revoked_by?: string;
}

export interface OAuthWebChatControlGrant {
  connector_id: string;
  grant_generation: number;
  device_id: string;
  endpoint_ref: string;
  provider: string;
  account_ref: string;
}

export interface OAuthPageAssistGrant {
  connector_id: string;
  grant_generation: number;
  device_id: string;
  endpoint_ref: string;
}

export function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

export function boundedString(value: unknown, max = MAX_STRING): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= max;
}

export function finiteEpoch(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
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

export function normalizeOAuthApproval(value: unknown, nowMs = Date.now()): OAuthApprovalRecord | null {
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

export function normalizeConnector(value: unknown): OAuthConnectorRecord | null {
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
  const lastUsedAt = finiteEpoch(value.last_used_at_ms) ? value.last_used_at_ms as number : undefined;
  const tokenIssueCount = Number.isSafeInteger(value.token_issue_count) && (value.token_issue_count as number) >= 0
    ? value.token_issue_count as number
    : undefined;
  const revokedAt = finiteEpoch(value.revoked_at_ms) ? value.revoked_at_ms as number : undefined;
  const revokedBy = boundedString(value.revoked_by, 4096) ? value.revoked_by : undefined;
  const revocationReason = boundedString(value.revocation_reason, 256) ? value.revocation_reason : undefined;
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
    ...(lastUsedAt !== undefined ? { last_used_at_ms: lastUsedAt } : {}),
    ...(lastTokenIssuedAt !== undefined ? { last_token_issued_at_ms: lastTokenIssuedAt } : {}),
    ...(tokenIssueCount !== undefined ? { token_issue_count: tokenIssueCount } : {}),
    ...(revokedAt !== undefined ? { revoked_at_ms: revokedAt } : {}),
    ...(revokedBy !== undefined ? { revoked_by: revokedBy } : {}),
    ...(revocationReason !== undefined ? { revocation_reason: revocationReason } : {}),
  };
}

export function normalizeConnectorGrant(value: unknown): OAuthConnectorGrantRecord | null {
  if (!record(value)) return null;
  if (!boundedString(value.client_id, 4096)) return null;
  if (value.status !== "active" && value.status !== "revoked") return null;
  const webchatControl = normalizeWebChatControlGrants(value.webchat_control);
  if (!webchatControl) return null;
  const pageAssist = normalizePageAssistGrants(value.page_assist);
  if (!pageAssist) return null;
  const canApproveConnectors = typeof value.can_approve_connectors === "boolean"
    ? value.can_approve_connectors
    : false;
  const principalType = value.principal_type === "connector" || value.principal_type === "automation"
    ? value.principal_type
    : undefined;
  const resource = boundedString(value.resource, 4096) ? value.resource : undefined;
  const scope = typeof value.scope === "string" && value.scope.length > 0 && value.scope.length <= 512 ? value.scope : undefined;
  const approvedAt = finiteEpoch(value.approved_at_ms) ? value.approved_at_ms as number : undefined;
  const approvedBy = boundedString(value.approved_by, 4096) ? value.approved_by : undefined;
  const lastTokenIssuedAt = finiteEpoch(value.last_token_issued_at_ms) ? value.last_token_issued_at_ms as number : undefined;
  const lastUsedAt = finiteEpoch(value.last_used_at_ms) ? value.last_used_at_ms as number : lastTokenIssuedAt;
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
  const connectorId = typeof value.connector_id === "string" && /^conn_[A-Za-z0-9_-]{8,128}$/.test(value.connector_id)
    ? value.connector_id
    : undefined;
  const grantGeneration = Number.isSafeInteger(value.grant_generation) && (value.grant_generation as number) > 0
    ? value.grant_generation as number
    : undefined;
  const revokedAt = finiteEpoch(value.revoked_at_ms) ? value.revoked_at_ms as number : undefined;
  const revokedBy = typeof value.revoked_by === "string" && value.revoked_by.length <= 4096 ? value.revoked_by : undefined;
  const revocationScope = value.revocation_scope === "client" ? "client" as const : undefined;
  return {
    client_id: value.client_id,
    status: value.status,
    ...(revocationScope !== undefined ? { revocation_scope: revocationScope } : {}),
    can_approve_connectors: canApproveConnectors,
    webchat_control: webchatControl,
    page_assist: pageAssist,
    ...(connectorId !== undefined ? { connector_id: connectorId } : {}),
    ...(grantGeneration !== undefined ? { grant_generation: grantGeneration } : {}),
    ...(principalType !== undefined ? { principal_type: principalType } : {}),
    ...(resource !== undefined ? { resource } : {}),
    ...(scope !== undefined ? { scope } : {}),
    ...(deviceId !== undefined ? { device_id: deviceId } : {}),
    ...(deviceName !== undefined ? { device_name: deviceName } : {}),
    ...(approvedAt !== undefined ? { approved_at_ms: approvedAt } : {}),
    ...(approvedBy !== undefined ? { approved_by: approvedBy } : {}),
    ...(lastUsedAt !== undefined ? { last_used_at_ms: lastUsedAt } : {}),
    ...(lastTokenIssuedAt !== undefined ? { last_token_issued_at_ms: lastTokenIssuedAt } : {}),
    ...(tokenIssueCount !== undefined ? { token_issue_count: tokenIssueCount } : {}),
    ...(lastRotatedAt !== undefined ? { last_rotated_at_ms: lastRotatedAt } : {}),
    ...(lastRotatedBy !== undefined ? { last_rotated_by: lastRotatedBy } : {}),
    ...(revokedAt !== undefined ? { revoked_at_ms: revokedAt } : {}),
    ...(revokedBy !== undefined ? { revoked_by: revokedBy } : {}),
  };
}

export function normalizeWebChatControlGrants(value: unknown): OAuthWebChatControlGrant[] | null {
  // Existing grants predate Alpha 4. Missing means explicitly no WebChat Control.
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length > 32) return null;
  const grants: OAuthWebChatControlGrant[] = [];
  for (const item of value) {
    if (!record(item)) return null;
    const hasConnectorId = item.connector_id !== undefined;
    const hasGrantGeneration = item.grant_generation !== undefined;
    // Pre exact-Connector browser grants carried no Connector generation.
    // They cannot be attributed safely, so discard only that authority while
    // keeping the surrounding ordinary MCP grant usable for explicit re-grant.
    if (!hasConnectorId && !hasGrantGeneration) continue;
    if (hasConnectorId !== hasGrantGeneration) return null;
    if (typeof item.connector_id !== "string" || !/^conn_[A-Za-z0-9_-]{8,128}$/.test(item.connector_id)) return null;
    if (!Number.isSafeInteger(item.grant_generation) || (item.grant_generation as number) <= 0) return null;
    if (!boundedString(item.device_id, 64)) return null;
    if (!boundedString(item.endpoint_ref, 96) || !boundedString(item.account_ref, 96)) return null;
    if (!boundedString(item.provider, 32) || !/^[a-z0-9][a-z0-9._-]*$/.test(item.provider)) return null;
    grants.push({
      connector_id: item.connector_id,
      grant_generation: item.grant_generation as number,
      device_id: item.device_id,
      endpoint_ref: item.endpoint_ref,
      provider: item.provider,
      account_ref: item.account_ref,
    });
  }
  grants.sort((a, b) =>
    a.connector_id.localeCompare(b.connector_id)
    || a.grant_generation - b.grant_generation
    || a.device_id.localeCompare(b.device_id)
    || a.endpoint_ref.localeCompare(b.endpoint_ref)
    || a.provider.localeCompare(b.provider)
    || a.account_ref.localeCompare(b.account_ref));
  return grants;
}

export function normalizePageAssistGrants(value: unknown): OAuthPageAssistGrant[] | null {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length > 32) return null;
  const grants: OAuthPageAssistGrant[] = [];
  for (const item of value) {
    if (!record(item)) return null;
    const hasConnectorId = item.connector_id !== undefined;
    const hasGrantGeneration = item.grant_generation !== undefined;
    if (!hasConnectorId && !hasGrantGeneration) continue;
    if (hasConnectorId !== hasGrantGeneration) return null;
    if (Object.keys(item).some((key) => !["connector_id", "grant_generation", "device_id", "endpoint_ref"].includes(key))) return null;
    if (typeof item.connector_id !== "string" || !/^conn_[A-Za-z0-9_-]{8,128}$/.test(item.connector_id)) return null;
    if (!Number.isSafeInteger(item.grant_generation) || (item.grant_generation as number) <= 0) return null;
    if (!boundedString(item.device_id, 64) || !/^dev_[0-9A-HJKMNP-TV-Z]{26}$/i.test(item.device_id)) return null;
    if (!boundedString(item.endpoint_ref, 96)) return null;
    if (!grants.some((grant) =>
      grant.connector_id === item.connector_id
      && grant.grant_generation === item.grant_generation
      && grant.device_id === item.device_id
      && grant.endpoint_ref === item.endpoint_ref)) {
      grants.push({
        connector_id: item.connector_id,
        grant_generation: item.grant_generation as number,
        device_id: item.device_id,
        endpoint_ref: item.endpoint_ref,
      });
    }
  }
  grants.sort((a, b) =>
    a.connector_id.localeCompare(b.connector_id)
    || a.grant_generation - b.grant_generation
    || a.device_id.localeCompare(b.device_id)
    || a.endpoint_ref.localeCompare(b.endpoint_ref));
  return grants;
}
