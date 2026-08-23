/**
 * mcp-placeholder.ts — MCP-facing router placeholder boundaries.
 *
 * The edge will eventually own the ChatGPT-facing MCP endpoints (plan §5.2:
 * initialize, tools/list, delivery). This module defines the boundaries with
 * clearly-labeled dev placeholders (never a production MCP claim):
 *
 *  - GET  /mcp              SSE transport boundary (placeholder)
 *  - POST /mcp              Streamable HTTP JSON-RPC boundary
 *      - initialize / tools/list -> placeholder 501
 *      - tools/call               -> OPTIONAL demo of the forwarding boundary:
 *                                    request -> DO -> link WSS -> response,
 *                                    or structured offline/reconnecting error.
 *  - /.well-known/mcp.json + OAuth discovery pairs -> placeholder 501 (Phase 4)
 *
 * NO live epoch-1 tool catalog is invented here; the contract manifest / hash
 * capture is Phase 1 work (see version.ts placeholder).
 */

import type { RelayErrorResult } from "./errors.js";
import type { InternalForwardRequest } from "./workstation-do.js";
import { CONTRACT_EPOCH, CONTRACT_HASH_PLACEHOLDER } from "./version.js";
import type { EdgeLimits } from "./limits.js";
import { classifyOp } from "./limits.js";
import { checkArgsBudget } from "./payload.js";
import { newRequestId } from "./pending.js";

export interface DemoCallBody {
  jsonrpc?: string;
  id?: string | number;
  method: string;
  params?: { name?: string; arguments?: unknown };
}

export interface ForwardDeps {
  limits: EdgeLimits;
  edgeEnv: string | undefined;
  /** Injected DO stub fetch — mirrors DurableObjectStub.fetch semantics. */
  forward(stub: unknown, body: string): Promise<Response>;
  getStub(workstationId: string): unknown;
  logger: { warn(event: string, fields?: Record<string, unknown>): void };
}

export interface PlaceholderResponse {
  body: Record<string, unknown>;
  status: number;
}

/** Structured 501 for any boundary that is not implemented yet. */
export function placeholder501(boundary: string, requestId: string, note: string): PlaceholderResponse {
  return {
    status: 501,
    body: {
      ok: false,
      code: "edge_mcp_placeholder",
      retryable: false,
      stage: "dev-scaffold",
      boundary,
      requestId,
      contractEpoch: CONTRACT_EPOCH,
      contractHash: CONTRACT_HASH_PLACEHOLDER,
      note,
    },
  };
}

/** Fail-closed response when a client calls /mcp before the link is there. */
export function notWired(method: string, requestId: string): PlaceholderResponse {
  return placeholder501(`mcp:${method}`, requestId, `The edge relays MCP framing only after Phases 1/3 wiring; ${method} is a boundary.`);
}

/** Handle GET /mcp (SSE transport boundary). */
export function getMcpPlaceholder(requestId: string): PlaceholderResponse {
  return placeholder501("mcp:sse", requestId, "SSE transport boundary: actual stream is wired in a later phase.");
}

/**
 * Demo tools/call path: build an internal request, forward through the DO to
 * the link WSS and map the response back. Returns a structured result that is
 * EXPLICITLY a dev relay demo — not a claim that the MCP contract works.
 */
export async function demoForwardCall(
  body: DemoCallBody,
  workstationId: string,
  deps: ForwardDeps,
): Promise<PlaceholderResponse> {
  const requestId = newRequestId();
  const op = typeof body.params?.name === "string" ? body.params.name : "";
  if (!op) return notWired("tools/call", requestId);

  const argsBudget = checkArgsBudget(body.params?.arguments ?? null, deps.limits.maxFrameBytes);
  if (!argsBudget.ok) {
    return {
      status: 413,
      body: {
        ok: false,
        code: "payload_too_large",
        retryable: false,
        requestId,
        bytes: argsBudget.bytes,
        maxBytes: argsBudget.maxBytes,
      },
    };
  }

  const internal: InternalForwardRequest = {
    kind: "request",
    requestId,
    op,
    opClass: classifyOp(op),
    args: body.params?.arguments ?? null,
    deadlineMs: Date.now() + deps.limits.requestTimeoutMs,
    contractEpoch: CONTRACT_EPOCH,
  };
  deps.logger.warn("mcp.tools_call.demo", {
    requestId,
    workstationId,
    op,
    opClass: internal.opClass,
    note: "dev relay demo; no live MCP contract assumed",
  });

  const stub = deps.getStub(workstationId);
  let resp: Response;
  try {
    resp = await deps.forward(stub, JSON.stringify(internal));
  } catch (e) {
    deps.logger.warn("mcp.tools_call.forward_error", { requestId, workstationId, op, error: String(e) });
    return {
      status: 503,
      body: { ok: false, code: "internal_error", retryable: false, requestId, workstationId },
    };
  }

  const payload = (await resp.json()) as { status: string; completion?: { status: "ok"; result?: unknown } | { status: "error"; error: RelayErrorResult }; error?: RelayErrorResult };
  if (payload.status === "ok" && payload.completion) {
    const completion = payload.completion;
    if (completion.status === "ok") {
      return { status: 200, body: { ok: true, demo: true, requestId, result: completion.result } };
    }
    return {
      status: 200,
      body: {
        ok: false,
        code: completion.error.code,
        retryable: completion.error.retryable,
        requestId,
        workstationId,
        message: completion.error.message,
        note: "dev relay demo; structured error is the offline/retryable surface",
      },
    };
  }
  return {
    status: 200,
    body: {
      ok: false,
      code: payload.error?.code ?? "internal_error",
      retryable: payload.error?.retryable ?? false,
      requestId,
      workstationId,
      message: payload.error?.message,
      note: "dev relay demo; structured error is the offline/retryable surface",
    },
  };
}