/**
 * mcp-dev.ts — dependency-free, stateless MCP/JSON-RPC handler for the
 * Cloudflare development edge.
 *
 * Phase 3 intentionally supports only POST-side development semantics:
 * initialize, server/discover, tools/list, tools/call and
 * notifications/initialized. GET/SSE and OAuth remain later phases.
 */

import { EPOCH1_CONTRACT } from "./contracts/epoch1.js";
import type { RelayErrorResult } from "./errors.js";
import { classifyOp, type EdgeLimits } from "./limits.js";
import { checkArgsBudget } from "./payload.js";
import { newRequestId } from "./pending.js";
import type { InternalForwardRequest } from "./workstation-do.js";

export const MCP_DEV_SERVER_NAME = "herdr-mcp";
export const MCP_DEV_SERVER_VERSION = "0.3.23-edge-dev";
export const MCP_LEGACY_PROTOCOL = "2025-11-25";
export const MCP_SUPPORTED_PROTOCOLS = [MCP_LEGACY_PROTOCOL, "2025-06-18"] as const;

export type JsonRpcId = string | number | null;

export interface McpDevRequest {
  jsonrpc?: unknown;
  id?: unknown;
  method?: unknown;
  params?: unknown;
}

export interface McpDevResponse {
  status: number;
  body: Record<string, unknown> | null;
}

export interface McpDevDeps {
  limits: EdgeLimits;
  forward(stub: unknown, body: string): Promise<Response>;
  getStub(workstationId: string): unknown;
  logger: { warn(event: string, fields?: Record<string, unknown>): void };
  now?: () => number;
}

interface ForwardEnvelope {
  status: "ok" | "error";
  completion?:
    | { status: "ok"; result?: unknown }
    | { status: "error"; error: RelayErrorResult; servedAtMs?: number };
  error?: RelayErrorResult;
}

const FROZEN_TOOL_NAMES: ReadonlySet<string> = new Set<string>(EPOCH1_CONTRACT.tools.map((tool) => tool.name));

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function validId(value: unknown): value is JsonRpcId {
  return value === null || typeof value === "string" || (typeof value === "number" && Number.isFinite(value));
}

function rpcResult(id: JsonRpcId, result: unknown): McpDevResponse {
  return {
    status: 200,
    body: { jsonrpc: "2.0", id, result },
  };
}

function rpcError(id: JsonRpcId, code: number, message: string, data?: unknown): McpDevResponse {
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

export function frozenEpoch1Tools(): readonly unknown[] {
  return EPOCH1_CONTRACT.tools;
}

export function frozenEpoch1Identity(): Record<string, unknown> {
  return {
    contract_epoch: EPOCH1_CONTRACT.contract_epoch,
    contract_hash: EPOCH1_CONTRACT.contract_hash,
    tool_count: EPOCH1_CONTRACT.tool_count,
  };
}

export async function handleMcpDev(
  input: unknown,
  workstationId: string,
  deps: McpDevDeps,
): Promise<McpDevResponse> {
  if (!isRecord(input)) return rpcError(null, -32600, "Invalid Request");

  const request = input as McpDevRequest;
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
    return rpcResult(id, {
      protocolVersion: MCP_LEGACY_PROTOCOL,
      capabilities: { tools: { listChanged: false } },
      serverInfo: { name: MCP_DEV_SERVER_NAME, version: MCP_DEV_SERVER_VERSION },
      instructions: "Stable edge MCP contract epoch 1; workstation execution is relayed over authenticated Herdr Link.",
      _meta: { herdr: frozenEpoch1Identity() },
    });
  }

  if (request.method === "server/discover") {
    return rpcResult(id, {
      resultType: "complete",
      supportedVersions: [...MCP_SUPPORTED_PROTOCOLS],
      capabilities: { tools: { listChanged: false } },
      instructions: "Development edge with frozen MCP contract epoch 1.",
      ttlMs: 3_600_000,
      cacheScope: "private",
      _meta: {
        "io.modelcontextprotocol/serverInfo": {
          name: MCP_DEV_SERVER_NAME,
          version: MCP_DEV_SERVER_VERSION,
        },
        herdr: frozenEpoch1Identity(),
      },
    });
  }

  if (request.method === "tools/list") {
    if (request.params !== undefined && !isRecord(request.params)) {
      return rpcError(id, -32602, "Invalid params");
    }
    return rpcResult(id, {
      tools: EPOCH1_CONTRACT.tools,
      _meta: { herdr: frozenEpoch1Identity() },
    });
  }

  if (request.method === "tools/call") {
    if (!isRecord(request.params)) return rpcError(id, -32602, "Invalid params");
    const name = request.params.name;
    if (typeof name !== "string" || !FROZEN_TOOL_NAMES.has(name)) {
      return rpcError(id, -32602, "Invalid params", { reason: "tool is not in frozen contract epoch 1" });
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
      contractEpoch: EPOCH1_CONTRACT.contract_epoch,
      contractHash: EPOCH1_CONTRACT.contract_hash,
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
