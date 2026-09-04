/** Stateless MCP/JSON-RPC handler used by both development and production Edge. */

import { PUBLIC_CONTRACT } from "./contracts/public.js";
import { RUNTIME_EXECUTION_CONTRACT } from "./contracts/runtime.js";
import { MCP_SERVER_VERSION } from "./version.js";
import type { RelayErrorResult } from "./errors.js";
import { classifyOp, type EdgeLimits } from "./limits.js";
import { checkArgsBudget } from "./payload.js";
import { newRequestId } from "./pending.js";
import type { DeviceRouteResult } from "./device-directory.js";
import { normalizeDeviceId } from "./device-model.js";
import type { InternalForwardRequest } from "./workstation-do.js";
import {
  isChatgptOAuthClientId,
  isOpenAiMcpUserAgent,
} from "./mcp-chatgpt-transport.js";

export const MCP_SERVER_NAME = "herdr-mcp";
export const MCP_LEGACY_PROTOCOL = "2025-11-25";
/** ChatGPT/OpenAI connector probe version; advertised on discover only. */
export const OPENAI_PROBE_PROTOCOL = "2026-07-28" as const;
export const MCP_SUPPORTED_PROTOCOLS = [
  MCP_LEGACY_PROTOCOL,
  "2025-06-18",
  "2025-03-26",
  "2024-11-05",
  "2024-10-07",
] as const;

export type JsonRpcId = string | number | null;

export interface McpRequest {
  jsonrpc?: unknown;
  id?: unknown;
  method?: unknown;
  params?: unknown;
}

export interface McpResponse {
  status: number;
  body: Record<string, unknown> | null;
}

export interface McpClientContext {
  userAgent?: string | null;
  oauthClientId?: string | null;
}

export interface McpDeps {
  limits: EdgeLimits;
  forward(stub: unknown, body: string): Promise<Response>;
  getStub(workstationId: string): unknown;
  listDevices?(): Promise<unknown>;
  createPairing?(input: { ttl_seconds?: number; name?: string }): Promise<
    | { ok: true; pairing_id: string; code: string; expires_at_ms: number; pairing_address: string; worker_origin?: string }
    | { ok: false; code: string; status?: number }
  >;
  revokeDevice?(deviceId: string): Promise<
    | { ok: true; device_id: string; revoked_at_ms: number }
    | { ok: false; code: string; retryable?: boolean; status?: number }
  >;
  approveConnector?(input: { request_id: string; code: string }): Promise<
    | { ok: true; client_id: string; approved_at_ms: number | null }
    | { ok: false; code: string }
  >;
  revokeConnector?(clientId: string): Promise<
    | { ok: true }
    | { ok: false; code: string }
  >;
  listAutomations?(): Promise<
    | { ok: true; automations: unknown[] }
    | { ok: false; code: string }
  >;
  revokeAutomation?(clientId: string): Promise<
    | { ok: true }
    | { ok: false; code: string }
  >;
  resolveDevice?(selector: string | undefined, args?: Record<string, unknown>): Promise<DeviceRouteResult>;
  logger: { warn(event: string, fields?: Record<string, unknown>): void };
  now?: () => number;
  client?: McpClientContext;
}

interface ForwardEnvelope {
  status: "ok" | "error";
  completion?:
    | { status: "ok"; result?: unknown }
    | { status: "error"; error: RelayErrorResult; servedAtMs?: number };
  error?: RelayErrorResult;
}

const PUBLIC_TOOL_NAMES: ReadonlySet<string> = new Set<string>(PUBLIC_CONTRACT.tools.map((tool) => tool.name));
const GENERATION_SUPERSEDE_RETRY_BACKOFF_MS = [100, 400, 1_000, 1_500] as const;
const GENERATION_SUPERSEDE_CLIENT_RETRY_AFTER_MS = 1_000;

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function validId(value: unknown): value is JsonRpcId {
  return value === null || typeof value === "string" || (typeof value === "number" && Number.isFinite(value));
}

function rpcResult(id: JsonRpcId, result: unknown): McpResponse {
  return {
    status: 200,
    body: { jsonrpc: "2.0", id, result },
  };
}

function rpcError(id: JsonRpcId, code: number, message: string, data?: unknown): McpResponse {
  return {
    status: 200,
    body: {
      jsonrpc: "2.0",
      id,
      error: {
        code,
        message,
        ...(data === undefined ? {} : { data }),
      },
    },
  };
}

function structuredObject(value: unknown): Record<string, unknown> {
  return isRecord(value) ? value : { result: value ?? null };
}

function callToolResult(structured: Record<string, unknown>, isError = false): Record<string, unknown> {
  return {
    content: [{ type: "text", text: JSON.stringify(structured) }],
    structuredContent: structured,
    ...(isError ? { isError: true } : {}),
  };
}

/** Preserve a complete local MCP CallToolResult, including image/audio content. */
function isMcpCallToolResult(value: unknown): value is Record<string, unknown> {
  return isRecord(value) && Array.isArray(value.content);
}

function normalizeSuccessfulToolResult(value: unknown): Record<string, unknown> {
  return isMcpCallToolResult(value) ? value : callToolResult(structuredObject(value));
}

function relayErrorToolResult(error: RelayErrorResult, requestId: string, workstationId: string): Record<string, unknown> {
  const supersededDetails = error.code === "runtime_generation_superseded_before_dispatch" && isRecord(error.details)
    ? error.details
    : undefined;
  return callToolResult(
    {
      ok: false,
      code: error.code,
      retryable: error.retryable,
      delivery_state: error.delivery_state,
      retry_after_ms: error.retry_after_ms
        ?? (supersededDetails ? GENERATION_SUPERSEDE_CLIENT_RETRY_AFTER_MS : undefined),
      recovery: error.recovery,
      message: error.message ?? null,
      details: error.details ?? null,
      ...(supersededDetails
        ? {
            old_generation: supersededDetails.reserved_generation ?? null,
            current_generation: supersededDetails.current_generation ?? supersededDetails.active_generation ?? null,
            superseded_at_ms: error.atMs ?? null,
          }
        : {}),
      request_id: requestId,
      workstation_id: workstationId,
    },
    true,
  );
}

function generationSupersededRetry(error: RelayErrorResult | undefined): boolean {
  return error?.code === "runtime_generation_superseded_before_dispatch"
    && error.retryable === true
    && error.delivery_state === "not_delivered";
}

function generationSupersededRetryDelay(error: RelayErrorResult | undefined, attempt: number): number {
  const hinted = error?.retry_after_ms;
  if (typeof hinted === "number" && Number.isFinite(hinted) && hinted >= 0) {
    return Math.min(hinted, 2_000);
  }
  return GENERATION_SUPERSEDE_RETRY_BACKOFF_MS[attempt] ?? 0;
}

async function waitMs(delayMs: number): Promise<void> {
  if (delayMs <= 0) return;
  await new Promise<void>((resolve) => setTimeout(resolve, delayMs));
}

function forwardEnvelopeError(forwarded: ForwardEnvelope): RelayErrorResult | undefined {
  if (forwarded.status === "ok" && forwarded.completion?.status === "error") {
    return forwarded.completion.error;
  }
  if (forwarded.status === "error") return forwarded.error;
  return undefined;
}

export function publicContractTools(): readonly unknown[] {
  return PUBLIC_CONTRACT.tools;
}

export function publicContractIdentity(): Record<string, unknown> {
  return {
    contract_epoch: PUBLIC_CONTRACT.contract_epoch,
    contract_hash: PUBLIC_CONTRACT.contract_hash,
    tool_count: PUBLIC_CONTRACT.tool_count,
  };
}

function isOpenAiDiscoverClient(client?: McpClientContext): boolean {
  return isOpenAiMcpUserAgent(client?.userAgent) || isChatgptOAuthClientId(client?.oauthClientId);
}

function discoverSupportedVersions(client?: McpClientContext): string[] {
  const versions: string[] = [...MCP_SUPPORTED_PROTOCOLS];
  if (isOpenAiDiscoverClient(client) && !versions.includes(OPENAI_PROBE_PROTOCOL)) {
    versions.push(OPENAI_PROBE_PROTOCOL);
  }
  return versions;
}

function negotiateProtocolVersion(requested: unknown): string {
  if (typeof requested === "string" && (MCP_SUPPORTED_PROTOCOLS as readonly string[]).includes(requested)) {
    return requested;
  }
  return MCP_LEGACY_PROTOCOL;
}

export async function handleMcp(
  input: unknown,
  workstationId: string,
  deps: McpDeps,
): Promise<McpResponse> {
  if (!isRecord(input)) return rpcError(null, -32600, "Invalid Request");

  const request = input as McpRequest;
  const id: JsonRpcId = validId(request.id) ? request.id : null;
  if (request.jsonrpc !== "2.0" || typeof request.method !== "string") {
    return rpcError(id, -32600, "Invalid Request");
  }

  if (request.method === "notifications/initialized") {
    return { status: 204, body: null };
  }

  if (request.method === "initialize") {
    if (request.params !== undefined && !isRecord(request.params)) {
      return rpcError(id, -32602, "Invalid params");
    }
    const params = isRecord(request.params) ? request.params : {};
    return rpcResult(id, {
      protocolVersion: negotiateProtocolVersion(params.protocolVersion),
      capabilities: { tools: { listChanged: false } },
      serverInfo: { name: MCP_SERVER_NAME, version: MCP_SERVER_VERSION },
      instructions: `Herdr Edge public contract epoch ${PUBLIC_CONTRACT.contract_epoch}; workstation execution uses a separate authenticated runtime contract.`,
      _meta: { herdr: publicContractIdentity() },
    });
  }

  if (request.method === "server/discover") {
    return rpcResult(id, {
      resultType: "complete",
      supportedVersions: discoverSupportedVersions(deps.client),
      capabilities: { tools: { listChanged: false } },
      instructions: `Herdr Edge public MCP contract epoch ${PUBLIC_CONTRACT.contract_epoch}.`,
      ttlMs: 3_600_000,
      cacheScope: "private",
      _meta: {
        "io.modelcontextprotocol/serverInfo": {
          name: MCP_SERVER_NAME,
          version: MCP_SERVER_VERSION,
        },
        herdr: publicContractIdentity(),
      },
    });
  }

  if (request.method === "tools/list") {
    if (request.params !== undefined && !isRecord(request.params)) {
      return rpcError(id, -32602, "Invalid params");
    }
    return rpcResult(id, {
      tools: PUBLIC_CONTRACT.tools,
      _meta: { herdr: publicContractIdentity() },
    });
  }

  if (request.method === "tools/call") {
    if (!isRecord(request.params)) return rpcError(id, -32602, "Invalid params");
    const name = request.params.name;
    if (typeof name !== "string" || !PUBLIC_TOOL_NAMES.has(name)) {
      return rpcError(id, -32602, "Invalid params", { reason: `tool is not in public contract epoch ${PUBLIC_CONTRACT.contract_epoch}` });
    }
    const rawArgs = request.params.arguments;
    if (rawArgs !== undefined && rawArgs !== null && !isRecord(rawArgs)) {
      return rpcError(id, -32602, "Invalid params", { reason: "arguments must be an object or null" });
    }
    const args = rawArgs === null || rawArgs === undefined ? {} : rawArgs;
    const budget = checkArgsBudget(args, deps.limits.maxFrameBytes);
    if (!budget.ok) {
      return rpcError(id, -32602, "Invalid params", {
        reason: "arguments exceed edge payload budget",
        bytes: budget.bytes,
        maxBytes: budget.maxBytes,
      });
    }

    if (name === "herdr_devices") {
      if (!deps.listDevices) {
        return rpcResult(
          id,
          callToolResult({ ok: false, code: "device_registry_unavailable", retryable: false }, true),
        );
      }
      try {
        const devices = await deps.listDevices();
        return rpcResult(id, callToolResult({
          ok: true,
          devices,
          pairing_hint: "When the user asks to add a new computer, an explicitly approved WebChat can call herdr_call(method=\"herdr_mcp.device.pair\", params='{\"ttl_seconds\":600,\"name\":\"<optional>\"}'). `params` is a JSON string in the frozen public schema. This is a Worker fleet-admin action and does not route through a workstation. If this conversation lacks fleet-admin authority, create the pairing from any already-enrolled computer with `herdr-mcp worker pair`. A completely new first Worker must be bootstrapped before pairing. Present the pairing address, one-time code, exact expiry, and new-device command together.",
          revoke_hint: "When the user explicitly asks to permanently revoke an enrolled computer, an explicitly approved WebChat can select its immutable device_id and call herdr_call(method=\"herdr_mcp.device.revoke\", params='{\"device_id\":\"dev_...\",\"confirm\":true}'). This is a Worker fleet-admin action. Never revoke by display name.",
        }));
      } catch {
        return rpcResult(
          id,
          callToolResult({ ok: false, code: "device_registry_unavailable", retryable: true }, true),
        );
      }
    }

    const localMethod = name === "herdr_call" && typeof args.method === "string" ? args.method : null;

    // Edge-local pairing creation. The operation does not route through a
    // workstation, but the current MCP principal must already have explicit
    // Worker fleet-admin authority.
    if (localMethod === "herdr_mcp.device.pair") {
      if (args.device !== undefined) {
        return rpcResult(id, callToolResult({
          ok: false,
          code: "device_selector_not_allowed",
          message: "herdr_mcp.device.pair is Edge-local and does not accept a device selector; authorization comes from the current Worker fleet-admin principal rather than a routed workstation",
          retryable: false,
          delivery_state: "not_delivered",
          failure_layer: "edge_routing",
        }, true));
      }

      for (const key of Object.keys(args)) {
        if (key !== "method" && key !== "params") {
          return rpcResult(id, callToolResult({
            ok: false,
            code: "invalid_params",
            message: `unknown top-level argument '${key}'; herdr_call with method 'herdr_mcp.device.pair' only accepts 'method' and optional 'params'`,
            retryable: false,
            delivery_state: "not_delivered",
            failure_layer: "edge_routing",
          }, true));
        }
      }
      let methodParams: Record<string, unknown> = {};
      const rawParams = args.params;
      if (rawParams !== undefined && rawParams !== null) {
        if (typeof rawParams === "string") {
          const trimmed = rawParams.trim();
          if (trimmed.length > 0) {
            try {
              const parsed = JSON.parse(trimmed);
              if (isRecord(parsed)) {
                methodParams = parsed;
              } else {
                return rpcResult(
                  id,
                  callToolResult({ ok: false, code: "invalid_params", message: "params must be a JSON object" }, true),
                );
              }
            } catch {
              return rpcResult(
                id,
                callToolResult({ ok: false, code: "invalid_params", message: "params is not valid JSON" }, true),
              );
            }
          }
        } else if (isRecord(rawParams)) {
          methodParams = rawParams;
        } else {
          return rpcResult(
            id,
            callToolResult({ ok: false, code: "invalid_params", message: "params must be an object or a JSON object string" }, true),
          );
        }
      }

      const { extractDeviceIdFromArgs } = await import("./device-refs.js");
      if (extractDeviceIdFromArgs(args) || extractDeviceIdFromArgs(methodParams)) {
        return rpcResult(id, callToolResult({
          ok: false,
          code: "device_ref_not_allowed",
          message: "herdr_mcp.device.pair is Edge-local and does not accept workstation or pane refs",
          retryable: false,
          delivery_state: "not_delivered",
          failure_layer: "edge_routing",
        }, true));
      }

      for (const key of Object.keys(methodParams)) {
        if (key !== "ttl_seconds" && key !== "name") {
          return rpcResult(
            id,
            callToolResult({
              ok: false,
              code: "invalid_params",
              message: `unknown parameter '${key}'; allowed parameters for herdr_mcp.device.pair are 'ttl_seconds' and 'name'`,
              retryable: false,
            }, true),
          );
        }
      }

      let ttlSeconds: number | undefined;
      if (methodParams.ttl_seconds !== undefined) {
        if (
          typeof methodParams.ttl_seconds !== "number" ||
          !Number.isSafeInteger(methodParams.ttl_seconds) ||
          methodParams.ttl_seconds < 60 ||
          methodParams.ttl_seconds > 600
        ) {
          return rpcResult(
            id,
            callToolResult({
              ok: false,
              code: "invalid_pairing_ttl",
              message: "ttl_seconds must be a safe integer between 60 and 600 seconds",
              retryable: false,
            }, true),
          );
        }
        ttlSeconds = methodParams.ttl_seconds;
      }

      let pairingName: string | undefined;
      if (methodParams.name !== undefined) {
        if (
          typeof methodParams.name !== "string" ||
          methodParams.name.trim().length === 0 ||
          methodParams.name.length > 128
        ) {
          return rpcResult(
            id,
            callToolResult({
              ok: false,
              code: "invalid_device_name",
              message: "name must be a non-empty string up to 128 characters",
              retryable: false,
            }, true),
          );
        }
        pairingName = methodParams.name.trim();
      }

      if (!deps.createPairing) {
        return rpcResult(
          id,
          callToolResult({ ok: false, code: "pairing_unavailable", retryable: false }, true),
        );
      }
      try {
        const result = await deps.createPairing({ ttl_seconds: ttlSeconds, name: pairingName });
        if (!result.ok) {
          return rpcResult(
            id,
            callToolResult({ ok: false, code: result.code, retryable: false }, true),
          );
        }
        const expiresAt = new Date(result.expires_at_ms).toISOString();
        return rpcResult(
          id,
          callToolResult({
            ok: true,
            pairing_id: result.pairing_id,
            code: result.code,
            expires_at_ms: result.expires_at_ms,
            expires_at: expiresAt,
            ttl_seconds: ttlSeconds ?? 600,
            pairing_address: result.pairing_address,
            worker_origin: result.worker_origin,
            new_device_command: `herdr-mcp worker connect "${result.pairing_address}"`,
            instructions: `This one-time pairing expires at ${expiresAt}. Run on the new computer: herdr-mcp worker connect "${result.pairing_address}" and enter verification code ${result.code} only when the no-echo prompt asks for it.`,
          }),
        );
      } catch {
        return rpcResult(
          id,
          callToolResult({ ok: false, code: "pairing_create_failed", retryable: true }, true),
        );
      }
    }

    // Edge-local fleet-admin revoke: permanently revoke one enrolled immutable device
    // identity without requiring any workstation to be online. This stays under
    // the existing herdr_call public tool, so it does not change contract epoch
    // or tool count.
    if (localMethod === "herdr_mcp.device.revoke") {
      for (const key of Object.keys(args)) {
        if (key !== "method" && key !== "params") {
          return rpcResult(id, callToolResult({
            ok: false,
            code: "invalid_params",
            message: `unknown top-level argument '${key}'; herdr_call with method 'herdr_mcp.device.revoke' only accepts 'method' and 'params'`,
            retryable: false,
            delivery_state: "not_delivered",
            failure_layer: "edge_routing",
          }, true));
        }
      }

      let methodParams: Record<string, unknown> = {};
      const rawParams = args.params;
      if (typeof rawParams === "string") {
        const trimmed = rawParams.trim();
        if (trimmed.length > 0) {
          try {
            const parsed = JSON.parse(trimmed);
            if (!isRecord(parsed)) {
              return rpcResult(id, callToolResult({ ok: false, code: "invalid_params", message: "params must be a JSON object" }, true));
            }
            methodParams = parsed;
          } catch {
            return rpcResult(id, callToolResult({ ok: false, code: "invalid_params", message: "params is not valid JSON" }, true));
          }
        }
      } else if (isRecord(rawParams)) {
        methodParams = rawParams;
      } else {
        return rpcResult(id, callToolResult({ ok: false, code: "invalid_params", message: "params must be an object or a JSON object string" }, true));
      }

      for (const key of Object.keys(methodParams)) {
        if (key !== "device_id" && key !== "confirm") {
          return rpcResult(id, callToolResult({
            ok: false,
            code: "invalid_params",
            message: `unknown parameter '${key}'; allowed parameters for herdr_mcp.device.revoke are 'device_id' and 'confirm'`,
            retryable: false,
          }, true));
        }
      }

      if (typeof methodParams.device_id !== "string") {
        return rpcResult(id, callToolResult({
          ok: false,
          code: "invalid_device_id",
          message: "device_id must be one immutable enrolled device id",
          retryable: false,
        }, true));
      }
      const deviceId = normalizeDeviceId(methodParams.device_id);
      if (!deviceId) {
        return rpcResult(id, callToolResult({
          ok: false,
          code: "invalid_device_id",
          message: "device_id must be a canonical dev_<26-character ULID> identity; display names are not accepted",
          retryable: false,
        }, true));
      }
      if (methodParams.confirm !== true) {
        return rpcResult(id, callToolResult({
          ok: false,
          code: "confirmation_required",
          message: "permanent device revoke requires confirm=true",
          retryable: false,
          delivery_state: "not_delivered",
        }, true));
      }
      if (!deps.revokeDevice) {
        return rpcResult(id, callToolResult({ ok: false, code: "device_revoke_unavailable", retryable: false }, true));
      }
      try {
        const result = await deps.revokeDevice(deviceId);
        if (!result.ok) {
          return rpcResult(id, callToolResult({
            ok: false,
            code: result.code,
            retryable: result.retryable ?? false,
          }, true));
        }
        return rpcResult(id, callToolResult({
          ok: true,
          device_id: result.device_id,
          revoked: true,
          revoked_at_ms: result.revoked_at_ms,
          message: "Device authorization permanently revoked. Its old credential cannot reconnect; re-enrollment requires a new pairing and device identity.",
        }));
      } catch {
        return rpcResult(id, callToolResult({ ok: false, code: "device_revoke_failed", retryable: true }, true));
      }
    }

    if (localMethod === "herdr_mcp.connector.approve" || localMethod === "herdr_mcp.connector.revoke") {
      if (args.device !== undefined) {
        return rpcResult(id, callToolResult({
          ok: false,
          code: "device_selector_not_allowed",
          message: `${localMethod} is Edge-local and does not accept a device selector`,
          retryable: false,
          delivery_state: "not_delivered",
          failure_layer: "edge_routing",
        }, true));
      }
      for (const key of Object.keys(args)) {
        if (key !== "method" && key !== "params") {
          return rpcResult(id, callToolResult({
            ok: false,
            code: "invalid_params",
            message: `unknown top-level argument '${key}'; ${localMethod} only accepts 'method' and 'params'`,
            retryable: false,
          }, true));
        }
      }
      let methodParams: Record<string, unknown> = {};
      const rawParams = args.params;
      if (typeof rawParams === "string") {
        try {
          const parsed = rawParams.trim() ? JSON.parse(rawParams) : {};
          if (!isRecord(parsed)) throw new Error("not_object");
          methodParams = parsed;
        } catch {
          return rpcResult(id, callToolResult({ ok: false, code: "invalid_params", message: "params must be a JSON object" }, true));
        }
      } else if (isRecord(rawParams)) {
        methodParams = rawParams;
      } else if (rawParams !== undefined && rawParams !== null) {
        return rpcResult(id, callToolResult({ ok: false, code: "invalid_params", message: "params must be an object or JSON object string" }, true));
      }

      if (localMethod === "herdr_mcp.connector.approve") {
        for (const key of Object.keys(methodParams)) {
          if (key !== "request_id" && key !== "code") {
            return rpcResult(id, callToolResult({ ok: false, code: "invalid_params", message: `unknown parameter '${key}'` }, true));
          }
        }
        const requestId = typeof methodParams.request_id === "string" ? methodParams.request_id.trim() : "";
        const code = typeof methodParams.code === "string" ? methodParams.code.trim() : "";
        if (!requestId || requestId.length > 256 || !/^\d{6}$/.test(code)) {
          return rpcResult(id, callToolResult({ ok: false, code: "invalid_connector_approval", retryable: false }, true));
        }
        if (!deps.approveConnector) {
          return rpcResult(id, callToolResult({ ok: false, code: "connector_approval_unavailable", retryable: false }, true));
        }
        const result = await deps.approveConnector({ request_id: requestId, code });
        if (!result.ok) return rpcResult(id, callToolResult({ ok: false, code: result.code, retryable: false }, true));
        return rpcResult(id, callToolResult({
          ok: true,
          action: "connector_approve",
          client_id: result.client_id,
          approved_at_ms: result.approved_at_ms,
          message: "Connector grant approved. The waiting authorization page can now complete the PKCE redirect.",
        }));
      }

      for (const key of Object.keys(methodParams)) {
        if (key !== "client_id" && key !== "confirm") {
          return rpcResult(id, callToolResult({ ok: false, code: "invalid_params", message: `unknown parameter '${key}'` }, true));
        }
      }
      const clientId = typeof methodParams.client_id === "string" ? methodParams.client_id.trim() : "";
      if (!clientId || clientId.length > 4096) {
        return rpcResult(id, callToolResult({ ok: false, code: "invalid_client_id", retryable: false }, true));
      }
      if (methodParams.confirm !== true) {
        return rpcResult(id, callToolResult({ ok: false, code: "confirmation_required", message: "connector revoke requires confirm=true", retryable: false }, true));
      }
      if (!deps.revokeConnector) {
        return rpcResult(id, callToolResult({ ok: false, code: "connector_revoke_unavailable", retryable: false }, true));
      }
      const result = await deps.revokeConnector(clientId);
      if (!result.ok) return rpcResult(id, callToolResult({ ok: false, code: result.code, retryable: false }, true));
      return rpcResult(id, callToolResult({
        ok: true,
        action: "connector_revoke",
        client_id: clientId,
        revoked: true,
        message: "Connector grant revoked. Existing v0.4.6-issued access/refresh credentials are fenced by the grant tombstone.",
      }));
    }

    if (localMethod === "herdr_mcp.automation.list" || localMethod === "herdr_mcp.automation.revoke") {
      if (args.device !== undefined) {
        return rpcResult(id, callToolResult({
          ok: false,
          code: "device_selector_not_allowed",
          message: `${localMethod} is Edge-local and does not accept a device selector`,
          retryable: false,
          delivery_state: "not_delivered",
          failure_layer: "edge_routing",
        }, true));
      }
      for (const key of Object.keys(args)) {
        if (key !== "method" && key !== "params") {
          return rpcResult(id, callToolResult({ ok: false, code: "invalid_params", message: `unknown top-level argument '${key}'` }, true));
        }
      }
      let methodParams: Record<string, unknown> = {};
      const rawParams = args.params;
      if (typeof rawParams === "string") {
        try {
          const parsed = rawParams.trim() ? JSON.parse(rawParams) : {};
          if (!isRecord(parsed)) throw new Error("not_object");
          methodParams = parsed;
        } catch {
          return rpcResult(id, callToolResult({ ok: false, code: "invalid_params", message: "params must be a JSON object" }, true));
        }
      } else if (isRecord(rawParams)) {
        methodParams = rawParams;
      } else if (rawParams !== undefined && rawParams !== null) {
        return rpcResult(id, callToolResult({ ok: false, code: "invalid_params", message: "params must be an object or JSON object string" }, true));
      }

      if (localMethod === "herdr_mcp.automation.list") {
        if (Object.keys(methodParams).length !== 0) {
          return rpcResult(id, callToolResult({ ok: false, code: "invalid_params", message: "automation list accepts no parameters" }, true));
        }
        if (!deps.listAutomations) {
          return rpcResult(id, callToolResult({ ok: false, code: "automation_list_unavailable", retryable: false }, true));
        }
        const result = await deps.listAutomations();
        if (!result.ok) return rpcResult(id, callToolResult({ ok: false, code: result.code, retryable: false }, true));
        return rpcResult(id, callToolResult({
          ok: true,
          action: "automation_list",
          automations: result.automations,
          message: "Automation credentials are service principals for unattended MCP clients. Long-lived client secrets are never returned by inventory.",
        }));
      }

      for (const key of Object.keys(methodParams)) {
        if (key !== "client_id" && key !== "confirm") {
          return rpcResult(id, callToolResult({ ok: false, code: "invalid_params", message: `unknown parameter '${key}'` }, true));
        }
      }
      const clientId = typeof methodParams.client_id === "string" ? methodParams.client_id.trim() : "";
      if (!/^svc_[A-Za-z0-9_-]{8,128}$/.test(clientId)) {
        return rpcResult(id, callToolResult({ ok: false, code: "invalid_client_id", retryable: false }, true));
      }
      if (methodParams.confirm !== true) {
        return rpcResult(id, callToolResult({ ok: false, code: "confirmation_required", message: "automation revoke requires confirm=true", retryable: false }, true));
      }
      if (!deps.revokeAutomation) {
        return rpcResult(id, callToolResult({ ok: false, code: "automation_revoke_unavailable", retryable: false }, true));
      }
      const result = await deps.revokeAutomation(clientId);
      if (!result.ok) return rpcResult(id, callToolResult({ ok: false, code: result.code, retryable: false }, true));
      return rpcResult(id, callToolResult({
        ok: true,
        action: "automation_revoke",
        client_id: clientId,
        revoked: true,
        message: "Automation credential revoked. Existing access tokens are fenced immediately and this client can no longer mint new tokens.",
      }));
    }

    const selectorValue = args.device;
    if (selectorValue !== undefined && typeof selectorValue !== "string") {
      return rpcError(id, -32602, "Invalid params", { reason: "device must be a string" });
    }
    const explicitTextDevice = localMethod === "herdr_mcp.text.read" || localMethod === "herdr_mcp.text.write";
    if (explicitTextDevice) {
      if (typeof selectorValue !== "string" || selectorValue.trim().length === 0) {
        return rpcResult(id, callToolResult({
          ok: false,
          code: "device_required",
          retryable: false,
          delivery_state: "not_delivered",
          failure_layer: "edge_routing",
          next_action: "retry the text transfer with an explicit enrolled device selector",
        }, true));
      }
      const { extractDeviceIdFromArgs } = await import("./device-refs.js");
      if (extractDeviceIdFromArgs(args)) {
        return rpcResult(id, callToolResult({
          ok: false,
          code: "device_ref_not_allowed",
          retryable: false,
          delivery_state: "not_delivered",
          failure_layer: "edge_routing",
          next_action: "remove pane/workspace refs from text-transfer params and use only the explicit device selector",
        }, true));
      }
    }
    let route: DeviceRouteResult;
    try {
      route = deps.resolveDevice
        ? await deps.resolveDevice(selectorValue, args)
        : {
            ok: true,
            device_id: null,
            workstation_id: workstationId,
            routing_reason: "legacy_default_device",
          };
    } catch {
      return rpcResult(
        id,
        callToolResult({ ok: false, code: "device_registry_unavailable", retryable: true }, true),
      );
    }
    if (!route.ok) {
      return rpcResult(id, callToolResult({
        ok: false,
        code: route.code,
        retryable: false,
        delivery_state: "not_delivered",
        failure_layer: "edge_routing",
        requested_target: args.target ?? args.pane_id ?? args.pane ?? args.workspace_id ?? args.workspace ?? null,
        selected_device: route.selected_device ?? null,
        candidate_devices: route.candidate_devices ?? [],
        next_action: route.code === "device_ambiguous"
          ? "retry with an explicit device selector or a device-aware ref"
          : "inspect device routing state and retry only after selecting an enrolled routable device",
      }, true));
    }

    // Unwrap device-aware opaque refs before forwarding to the runtime;
    // the runtime contract remains epoch 2 without device metadata.
    const { unwrapDeviceRefs, wrapResultWithDevice } = await import("./device-refs.js");
    const runtimeArgs = unwrapDeviceRefs(args);
    // runtimeArgs already strips device and binding fields

    const now = deps.now?.() ?? Date.now();
    const requestId = newRequestId();
    const opClass = classifyOp(name);
    const idempotencyKey =
      typeof runtimeArgs.idempotency_key === "string" && runtimeArgs.idempotency_key.length > 0
        ? runtimeArgs.idempotency_key
        : undefined;
    const internal: InternalForwardRequest = {
      kind: "request",
      requestId,
      op: name,
      opClass,
      args: runtimeArgs,
      deadlineMs: now + deps.limits.requestTimeoutMs,
      contractEpoch: RUNTIME_EXECUTION_CONTRACT.contract_epoch,
      contractHash: RUNTIME_EXECUTION_CONTRACT.contract_hash,
      idempotencyKey,
    };

    deps.logger.warn("mcp.tools_call.forward", {
      requestId,
      workstationId: route.workstation_id,
      deviceId: route.device_id,
      routingReason: route.routing_reason,
      op: name,
      opClass,
    });

    let activeRequestId = requestId;
    let activeInternal = internal;
    let forwarded: ForwardEnvelope | undefined;
    for (let attempt = 0; attempt <= GENERATION_SUPERSEDE_RETRY_BACKOFF_MS.length; attempt += 1) {
      let response: Response;
      try {
        response = await deps.forward(deps.getStub(route.workstation_id), JSON.stringify(activeInternal));
      } catch (error) {
        return rpcResult(
          id,
          callToolResult(
            {
              ok: false,
              code: "edge_forward_failed",
              retryable: true,
              delivery_state: "not_delivered",
              message: String(error),
              request_id: activeRequestId,
              workstation_id: route.workstation_id,
            },
            true,
          ),
        );
      }

      try {
        forwarded = (await response.json()) as ForwardEnvelope;
      } catch {
        return rpcResult(
          id,
          callToolResult(
            {
              ok: false,
              code: "invalid_edge_response",
              retryable: false,
              delivery_state: "delivery_unknown",
              request_id: activeRequestId,
              workstation_id: route.workstation_id,
            },
            true,
          ),
        );
      }

      const retryError = forwardEnvelopeError(forwarded);
      if (attempt < GENERATION_SUPERSEDE_RETRY_BACKOFF_MS.length && generationSupersededRetry(retryError)) {
        const retryRequestId = newRequestId();
        const retryDelayMs = generationSupersededRetryDelay(retryError, attempt);
        deps.logger.warn("mcp.tools_call.generation_retry", {
          requestId: activeRequestId,
          retryRequestId,
          workstationId: route.workstation_id,
          deviceId: route.device_id,
          op: name,
          opClass,
          attempt: attempt + 1,
          retryDelayMs,
        });
        await waitMs(retryDelayMs);
        activeRequestId = retryRequestId;
        activeInternal = { ...activeInternal, requestId: retryRequestId };
        continue;
      }
      break;
    }

    if (!forwarded) {
      return rpcResult(
        id,
        callToolResult(
          {
            ok: false,
            code: "invalid_edge_response",
            retryable: false,
            delivery_state: "delivery_unknown",
            request_id: activeRequestId,
            workstation_id: route.workstation_id,
          },
          true,
        ),
      );
    }

    if (forwarded.status === "ok" && forwarded.completion?.status === "ok") {
      // For device-routed calls, wrap workspace/pane ids into device-aware opaque refs
      // so follow-up calls retain affinity without trusting arbitrary path strings.
      const wrapped = wrapResultWithDevice(forwarded.completion.result, route.device_id, route.device_name);
      return rpcResult(id, normalizeSuccessfulToolResult(wrapped));
    }
    if (forwarded.status === "ok" && forwarded.completion?.status === "error") {
      return rpcResult(id, relayErrorToolResult(forwarded.completion.error, activeRequestId, route.workstation_id));
    }
    if (forwarded.status === "error" && forwarded.error) {
      return rpcResult(id, relayErrorToolResult(forwarded.error, activeRequestId, route.workstation_id));
    }
    return rpcResult(
      id,
      callToolResult(
        {
          ok: false,
          code: "invalid_edge_response",
          retryable: false,
          delivery_state: "delivery_unknown",
          request_id: activeRequestId,
          workstation_id: route.workstation_id,
        },
        true,
      ),
    );
  }

  return rpcError(id, -32601, "Method not found");
}
