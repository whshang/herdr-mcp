import { PUBLIC_CONTRACT } from "./contracts/public.js";
import type { EdgeLogger } from "./logger.js";

const MINUTE_MS = 60_000;
const MAX_OPERATION_BUCKETS = 48;
const PUBLIC_TOOL_NAMES = new Set<string>(PUBLIC_CONTRACT.tools.map((tool) => tool.name));
const SAFE_RPC_METHODS = new Set(["initialize", "notifications/initialized", "ping", "tools/list"]);
const SAFE_CALL_PREFIXES = new Set([
  "agent",
  "continuity",
  "pane",
  "session",
  "tab",
  "worktree",
  "workspace",
]);
const SAFE_LOCAL_PREFIXES = new Set([
  "automation",
  "browser_composer",
  "browser_dispatch",
  "browser_endpoint",
  "browser_message",
  "browser_resource",
  "browser_session",
  "browser_space",
  "cleanup",
  "connector",
  "device",
  "execution_lane",
  "fleet",
  "page_assist",
  "planner_control",
  "planner_lease",
  "planning",
  "skill",
  "text",
  "work_chain",
]);

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function classifyHerdrCallMethod(value: unknown): string {
  if (typeof value !== "string") return "call:missing";
  if (value.startsWith("herdr_mcp.")) {
    const segment = value.slice("herdr_mcp.".length).split(".", 1)[0] ?? "";
    return SAFE_LOCAL_PREFIXES.has(segment) ? `call:herdr_mcp.${segment}` : "call:herdr_mcp.other";
  }
  const segment = value.split(".", 1)[0] ?? "";
  return SAFE_CALL_PREFIXES.has(segment) ? `call:${segment}` : "call:other";
}

export function classifyMcpAttributionOperation(payload: unknown): string {
  if (!isRecord(payload)) return "rpc:invalid";
  const method = payload.method;
  if (method !== "tools/call") {
    return typeof method === "string" && SAFE_RPC_METHODS.has(method) ? `rpc:${method}` : "rpc:other";
  }
  if (!isRecord(payload.params)) return "tool:invalid";
  const name = payload.params.name;
  if (typeof name !== "string" || !PUBLIC_TOOL_NAMES.has(name)) return "tool:unknown";
  if (name !== "herdr_call") return `tool:${name}`;
  const args = payload.params.arguments;
  return isRecord(args) ? classifyHerdrCallMethod(args.method) : "call:missing";
}

/**
 * Best-effort per-isolate attribution only. The active minute may disappear if
 * Cloudflare recycles the isolate before the next request rolls the bucket.
 * Durable Object metrics remain the billing/request-count source of truth; this
 * aggregate exists only to explain which bounded MCP operations drove a peak.
 *
 * AMPLIFICATION BOUNDARY: `recordWorkstationForward` counts ONLY the Edge
 * `forward` callback hops issued for one OpenAI-visible MCP request (normally 1;
 * more when a bounded generation/transient retry runs). It is deliberately NOT a
 * total Cloudflare DO/subrequest count — device-registry, OAuth-store, planner,
 * fleet and device-resolution DO fetches are not represented. Exact total DO
 * fan-out would require invasive cross-helper plumbing and new instrumentation
 * seams, so this metric stays intentionally narrow and clearly named instead.
 * It is computed in-process from data the router already has and adds no
 * requests.
 */
export class McpMinuteAttribution {
  private minuteStartMs: number | null = null;
  private total = 0;
  private readonly counts = new Map<string, number>();
  private workstationForwardCount = 0;
  private readonly workstationForwards = new Map<string, number>();

  constructor(private readonly logger: EdgeLogger) {}

  /** Record one OpenAI-visible MCP operation and return its classification token. */
  record(payload: unknown, nowMs = Date.now()): string {
    this.rollMinute(nowMs);
    const operation = classifyMcpAttributionOperation(payload);
    const bucket = this.counts.has(operation) || this.counts.size < MAX_OPERATION_BUCKETS ? operation : "other";
    this.counts.set(bucket, (this.counts.get(bucket) ?? 0) + 1);
    this.total += 1;
    return operation;
  }

  /**
   * Attribute downstream workstation `forward` hops to the OpenAI-visible
   * operation that caused them. `operation` should be the token returned by
   * `record` for the same request. This is not a total DO-fetch counter; see the
   * class comment for the exact boundary.
   */
  recordWorkstationForward(operation: string, count = 1, nowMs = Date.now()): void {
    if (count <= 0) return;
    this.rollMinute(nowMs);
    this.workstationForwardCount += count;
    this.workstationForwards.set(operation, (this.workstationForwards.get(operation) ?? 0) + count);
  }

  private rollMinute(nowMs: number): void {
    const minuteStartMs = Math.floor(nowMs / MINUTE_MS) * MINUTE_MS;
    if (this.minuteStartMs !== null && minuteStartMs !== this.minuteStartMs) this.flush();
    if (this.minuteStartMs !== minuteStartMs) {
      this.minuteStartMs = minuteStartMs;
      this.total = 0;
      this.counts.clear();
      this.workstationForwardCount = 0;
      this.workstationForwards.clear();
    }
  }

  flush(): void {
    if (this.minuteStartMs === null || (this.total === 0 && this.workstationForwardCount === 0)) return;
    const operations = [...this.counts.entries()]
      .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
      .map(([operation, count]) => `${operation}=${count}`)
      .join(",");
    this.logger.info("mcp.minute_attribution", {
      minute_start_ms: this.minuteStartMs,
      total: this.total,
      operations,
      ...(this.workstationForwardCount > 0
        ? {
            workstation_forward_count: this.workstationForwardCount,
            workstation_forwards: [...this.workstationForwards.entries()]
              .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
              .map(([operation, count]) => `${operation}=${count}`)
              .join(","),
          }
        : {}),
    });
  }
}
