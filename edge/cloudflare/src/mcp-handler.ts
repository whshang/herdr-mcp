/** Stateless MCP/JSON-RPC handler used by both development and production Edge. */

import { PUBLIC_CONTRACT } from "./contracts/public.js";
import { MCP_SERVER_VERSION } from "./version.js";
import type { RelayErrorResult } from "./errors.js";
import { classifyOp, type EdgeLimits } from "./limits.js";
import { checkArgsBudget } from "./payload.js";
import { newRequestId } from "./pending.js";
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
      message: error.message ?? null,
      request_id: requestId,
      workstation_id: workstationId,
    },
    true,
  );
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
      instructions: "Stable edge MCP contract epoch 2; workstation execution is relayed over authenticated Herdr Link.",
      _meta: { herdr: publicContractIdentity() },
    });
  }

  if (request.method === "server/discover") {
    return rpcResult(id, {
      resultType: "complete",
      supportedVersions: discoverSupportedVersions(deps.client),
      capabilities: { tools: { listChanged: false } },
      instructions: "Herdr Edge with frozen MCP contract epoch 2.",
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
      return rpcError(id, -32602, "Invalid params", { reason: "tool is not in public contract epoch 2" });
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

    const now = deps.now?.() ?? Date.now();
    const requestId = newRequestId();
    const idempotencyKey =
      typeof args.idempotency_key === "string" && args.idempotency_key.length > 0
        ? args.idempotency_key
        : undefined;
    const internal: InternalForwardRequest = {
      kind: "request",
      requestId,
      op: name,
      opClass: classifyOp(name),
      args,
      deadlineMs: now + deps.limits.requestTimeoutMs,
      contractEpoch: PUBLIC_CONTRACT.contract_epoch,
      contractHash: PUBLIC_CONTRACT.contract_hash,
      idempotencyKey,
    };

    deps.logger.warn("mcp.tools_call.forward", {
      requestId,
      workstationId,
      op: name,
      opClass: internal.opClass,
    });

    let response: Response;
    try {
      response = await deps.forward(deps.getStub(workstationId), JSON.stringify(internal));
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
            request_id: requestId,
            workstation_id: workstationId,
          },
          true,
        ),
      );
    }

    let forwarded: ForwardEnvelope;
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
            request_id: requestId,
            workstation_id: workstationId,
          },
          true,
        ),
      );
    }

    if (forwarded.status === "ok" && forwarded.completion?.status === "ok") {
      return rpcResult(id, normalizeSuccessfulToolResult(forwarded.completion.result));
    }
    if (forwarded.status === "ok" && forwarded.completion?.status === "error") {
      return rpcResult(id, relayErrorToolResult(forwarded.completion.error, requestId, workstationId));
    }
    if (forwarded.status === "error" && forwarded.error) {
      return rpcResult(id, relayErrorToolResult(forwarded.error, requestId, workstationId));
    }
    return rpcResult(
      id,
      callToolResult(
        {
          ok: false,
          code: "invalid_edge_response",
          retryable: false,
          delivery_state: "delivery_unknown",
          request_id: requestId,
          workstation_id: workstationId,
        },
        true,
      ),
    );
  }

  return rpcError(id, -32601, "Method not found");
}
