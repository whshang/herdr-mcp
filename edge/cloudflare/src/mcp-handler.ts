/** Stateless MCP/JSON-RPC handler used by both development and production Edge. */

import { PUBLIC_CONTRACT } from "./contracts/public.js";
import { RUNTIME_EXECUTION_CONTRACT } from "./contracts/runtime.js";
import { MCP_SERVER_VERSION } from "./version.js";
import type { RelayErrorResult } from "./errors.js";
import { classifyOp, type EdgeLimits } from "./limits.js";
import { checkArgsBudget } from "./payload.js";
import { newRequestId } from "./pending.js";
import type { DeviceRouteResult } from "./device-directory.js";
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
  return callToolResult(
    {
      ok: false,
      code: error.code,
      retryable: error.retryable,
      delivery_state: error.delivery_state,
      retry_after_ms: error.retry_after_ms,
      recovery: error.recovery,
      message: error.message ?? null,
      request_id: requestId,
      workstation_id: workstationId,
    },
    true,
  );
}

function generationSupersededReadRetry(error: RelayErrorResult | undefined, opClass: string): boolean {
  return opClass === "read"
    && error?.code === "runtime_generation_superseded_before_dispatch"
    && error.retryable === true
    && error.delivery_state === "not_delivered";
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
          pairing_hint: "To connect a new computer, call herdr_call(method=\"herdr_mcp.device.pair\", params='{\"ttl_seconds\":600,\"name\":\"<optional>\"}'). params is a JSON string in the public schema. Do not provide a device selector.",
        }));
      } catch {
        return rpcResult(
          id,
          callToolResult({ ok: false, code: "device_registry_unavailable", retryable: true }, true),
        );
      }
    }

    const localMethod = name === "herdr_call" && typeof args.method === "string" ? args.method : null;

    // Edge-local pairing creation: allows an OAuth-authorized owner to initiate
    // a device pairing session directly at Edge without requiring an enrolled
    // or online workstation.
    if (localMethod === "herdr_mcp.device.pair") {
      if (args.device !== undefined) {
        return rpcResult(id, callToolResult({
          ok: false,
          code: "device_selector_not_allowed",
          message: "herdr_mcp.device.pair is Edge-local and does not accept a device selector; no existing workstation is required or used",
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
        return rpcResult(
          id,
          callToolResult({
            ok: true,
            pairing_id: result.pairing_id,
            code: result.code,
            expires_at_ms: result.expires_at_ms,
            pairing_address: result.pairing_address,
            worker_origin: result.worker_origin,
            instructions: `Run on the new computer: herdr-mcp worker connect "${result.pairing_address}" and enter verification code ${result.code} when prompted.`,
          }),
        );
      } catch {
        return rpcResult(
          id,
          callToolResult({ ok: false, code: "pairing_create_failed", retryable: true }, true),
        );
      }
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
    for (let attempt = 0; attempt < 2; attempt += 1) {
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
      if (attempt === 0 && generationSupersededReadRetry(retryError, opClass)) {
        const retryRequestId = newRequestId();
        deps.logger.warn("mcp.tools_call.generation_retry", {
          requestId: activeRequestId,
          retryRequestId,
          workstationId: route.workstation_id,
          deviceId: route.device_id,
          op: name,
        });
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
