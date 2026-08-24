import { computeContractHash, type ContractTool } from "../relay/contract.js";
import { LOCAL_MCP_CONTRACT_EPOCH, LocalMcpRuntimeTransport } from "./local-mcp-transport.js";
import type {
  LinkRuntimeTransport,
  RequestId,
  RuntimeIdentitySnapshot,
  RuntimeToolResult,
  ToolRequestFrame,
} from "./types.js";

export const RUNTIME_GENERATION_SCHEMA_VERSION = 1;
const GENERATION_RE = /^[A-Za-z0-9_.-]{1,64}$/;
const MAX_CATALOG_BYTES = 2 * 1024 * 1024;

export interface RuntimeGenerationSpec {
  generation: string;
  endpoint: string;
  expected_runtime_version?: string | null;
  runtime_commit?: string | null;
}

export type RuntimeGenerationPhase = "active" | "standby" | "draining" | "rejected";

export interface RuntimeGenerationValidation {
  ok: boolean;
  code: string;
  runtime_version: string | null;
  contract_hash: string | null;
  tool_count: number | null;
  checked_at_ms: number;
}

export interface RuntimeGenerationStatus {
  generation: string;
  endpoint: string;
  phase: RuntimeGenerationPhase;
  in_flight: number;
  validation: RuntimeGenerationValidation | null;
}

export interface RuntimeManagerStatus {
  active_generation: string;
  previous_generation: string | null;
  last_good_generation: string;
  transition_seq: number;
  last_transition: {
    from: string;
    to: string;
    outcome: "activated" | "rolled_back";
    reason: string | null;
    at_ms: number;
  } | null;
  generations: RuntimeGenerationStatus[];
}

export interface RuntimeGenerationManagerOptions {
  base: RuntimeGenerationSpec;
  bearerToken: string;
  contractHash: string;
  contractEpoch?: number;
  fetch?: typeof globalThis.fetch;
  defaultTimeoutMs?: number;
  maxTimeoutMs?: number;
  observationChecks?: number;
  observationIntervalMs?: number;
  sleep?: (ms: number) => Promise<void>;
  clock?: () => number;
}

interface GenerationRecord {
  spec: RuntimeGenerationSpec;
  transport: LocalMcpRuntimeTransport;
  phase: RuntimeGenerationPhase;
  inFlight: number;
  validation: RuntimeGenerationValidation | null;
}

function isLoopbackHostname(host: string): boolean {
  const normalized = host.toLowerCase().replace(/^\[|\]$/g, "");
  return normalized === "localhost" || normalized === "::1" || /^127(?:\.\d{1,3}){3}$/.test(normalized);
}

export function validateRuntimeGenerationSpec(spec: RuntimeGenerationSpec): void {
  if (!spec || typeof spec !== "object") throw new TypeError("runtime-generation: spec must be an object");
  if (!GENERATION_RE.test(spec.generation)) throw new TypeError("runtime-generation: invalid generation id");
  let url: URL;
  try { url = new URL(spec.endpoint); } catch { throw new TypeError("runtime-generation: endpoint must be a valid URL"); }
  if (url.protocol !== "http:" && url.protocol !== "https:") throw new TypeError("runtime-generation: endpoint must use http(s)");
  if (!isLoopbackHostname(url.hostname)) throw new TypeError("runtime-generation: endpoint must be loopback-only");
  if (spec.expected_runtime_version != null && (typeof spec.expected_runtime_version !== "string" || spec.expected_runtime_version.length === 0)) {
    throw new TypeError("runtime-generation: expected_runtime_version must be null or a non-empty string");
  }
  if (spec.runtime_commit != null && typeof spec.runtime_commit !== "string") {
    throw new TypeError("runtime-generation: runtime_commit must be null or a string");
  }
}

function boundedPositive(value: unknown, fallback: number, min: number, max: number): number {
  const n = Number(value);
  return Number.isFinite(n) && n >= min && n <= max ? Math.floor(n) : fallback;
}

function parseMcpJson(text: string): any {
  const trimmed = text.trim();
  if (!trimmed) return null;
  // MCP Streamable HTTP may return a complete SSE event beginning with
  // `event:`/`id:` rather than `data:`. Treat any response containing data:
  // lines as SSE and reconstruct the JSON payload from those lines only.
  const lines = trimmed.split(/\r?\n/);
  const dataLines = lines.filter((line) => line.startsWith("data: "));
  if (dataLines.length > 0) {
    const data = dataLines.map((line) => line.slice(6)).join("\n");
    return data ? JSON.parse(data) : null;
  }
  return JSON.parse(trimmed);
}

async function fetchCatalog(options: {
  endpoint: string;
  bearerToken: string;
  fetchFn: typeof globalThis.fetch;
  timeoutMs: number;
}): Promise<{ ok: true; tools: ContractTool[] } | { ok: false; code: string }> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), options.timeoutMs);
  try {
    let response: Response;
    try {
      response = await options.fetchFn(options.endpoint, {
        method: "POST",
        headers: {
          Authorization: `Bearer ${options.bearerToken}`,
          "Content-Type": "application/json",
          Accept: "application/json, text/event-stream",
          "User-Agent": "herdr-runtime-generation-probe/1",
        },
        body: JSON.stringify({ jsonrpc: "2.0", id: "generation-tools-list", method: "tools/list", params: {} }),
        signal: controller.signal,
      });
    } catch {
      return { ok: false, code: controller.signal.aborted ? "catalog_timeout" : "catalog_unreachable" };
    }
    if (!response.ok) return { ok: false, code: `catalog_http_${response.status}` };
    const length = Number(response.headers.get("content-length") || 0);
    if (length > MAX_CATALOG_BYTES) return { ok: false, code: "catalog_too_large" };
    const text = await response.text();
    if (Buffer.byteLength(text, "utf8") > MAX_CATALOG_BYTES) return { ok: false, code: "catalog_too_large" };
    let parsed: any;
    try { parsed = parseMcpJson(text); } catch { return { ok: false, code: "catalog_malformed" }; }
    const tools = parsed?.result?.tools;
    if (!Array.isArray(tools)) return { ok: false, code: "catalog_missing_tools" };
    return { ok: true, tools };
  } finally {
    clearTimeout(timer);
  }
}

export class RuntimeGenerationManager implements LinkRuntimeTransport {
  readonly name = "runtime-generation-manager";

  private readonly bearerToken: string;
  private readonly contractHash: string;
  private readonly contractEpoch: number;
  private readonly fetchFn: typeof globalThis.fetch;
  private readonly defaultTimeoutMs: number;
  private readonly maxTimeoutMs: number;
  private readonly observationChecks: number;
  private readonly observationIntervalMs: number;
  private readonly sleepFn: (ms: number) => Promise<void>;
  private readonly now: () => number;
  private readonly generations = new Map<string, GenerationRecord>();
  private readonly requestOwners = new Map<RequestId, string>();

  private activeGeneration: string;
  private previousGeneration: string | null = null;
  private lastGoodGeneration: string;
  private transitionSeq = 0;
  private lastTransition: RuntimeManagerStatus["last_transition"] = null;

  constructor(options: RuntimeGenerationManagerOptions) {
    validateRuntimeGenerationSpec(options.base);
    if (!options.bearerToken) throw new TypeError("runtime-generation: bearerToken is required");
    if (!options.contractHash) throw new TypeError("runtime-generation: contractHash is required");
    this.bearerToken = options.bearerToken;
    this.contractHash = options.contractHash;
    this.contractEpoch = options.contractEpoch ?? LOCAL_MCP_CONTRACT_EPOCH;
    if (this.contractEpoch !== LOCAL_MCP_CONTRACT_EPOCH) {
      throw new RangeError(`runtime-generation: contract epoch must remain ${LOCAL_MCP_CONTRACT_EPOCH} for this build`);
    }
    this.fetchFn = options.fetch ?? globalThis.fetch;
    this.defaultTimeoutMs = boundedPositive(options.defaultTimeoutMs, 30_000, 1_000, 60_000);
    this.maxTimeoutMs = boundedPositive(options.maxTimeoutMs, 60_000, this.defaultTimeoutMs, 120_000);
    this.observationChecks = boundedPositive(options.observationChecks, 3, 1, 20);
    this.observationIntervalMs = boundedPositive(options.observationIntervalMs, 500, 0, 10_000);
    this.sleepFn = options.sleep ?? ((ms) => new Promise((resolve) => setTimeout(resolve, ms)));
    this.now = options.clock ?? Date.now;

    const base = this.makeRecord(options.base, "active");
    this.generations.set(options.base.generation, base);
    this.activeGeneration = options.base.generation;
    this.lastGoodGeneration = options.base.generation;
  }

  private makeRecord(spec: RuntimeGenerationSpec, phase: RuntimeGenerationPhase): GenerationRecord {
    const transport = new LocalMcpRuntimeTransport({
      endpoint: spec.endpoint,
      bearerToken: this.bearerToken,
      contractHash: this.contractHash,
      contractEpoch: this.contractEpoch,
      runtimeVersion: spec.expected_runtime_version ?? undefined,
      runtimeCommit: spec.runtime_commit ?? null,
      runtimeGeneration: spec.generation,
      fetch: this.fetchFn,
      defaultTimeoutMs: this.defaultTimeoutMs,
      maxTimeoutMs: this.maxTimeoutMs,
    });
    return { spec: { ...spec }, transport, phase, inFlight: 0, validation: null };
  }

  get activeGenerationId(): string { return this.activeGeneration; }

  getGenerationSpec(generation: string): RuntimeGenerationSpec | null {
    const record = this.generations.get(generation);
    return record ? { ...record.spec } : null;
  }

  async registerGeneration(spec: RuntimeGenerationSpec): Promise<RuntimeGenerationValidation> {
    validateRuntimeGenerationSpec(spec);
    const existing = this.generations.get(spec.generation);
    if (existing?.phase === "active" && existing.spec.endpoint !== spec.endpoint) {
      return this.validationFailure("active_generation_endpoint_immutable");
    }
    if (existing?.inFlight && existing.spec.endpoint !== spec.endpoint) {
      return this.validationFailure("generation_has_in_flight_requests");
    }
    const record = existing && existing.spec.endpoint === spec.endpoint
      ? existing
      : this.makeRecord(spec, existing?.phase ?? "standby");
    record.spec = { ...spec };
    const validation = await this.validateRecord(record);
    record.validation = validation;
    if (!validation.ok && record.phase !== "active" && record.phase !== "draining") record.phase = "rejected";
    else if (validation.ok && record.phase === "rejected") record.phase = "standby";
    this.generations.set(spec.generation, record);
    return validation;
  }

  async validateGeneration(generation: string): Promise<RuntimeGenerationValidation> {
    const record = this.generations.get(generation);
    if (!record) return this.validationFailure("generation_not_found");
    const validation = await this.validateRecord(record);
    record.validation = validation;
    if (!validation.ok && record.phase !== "active" && record.phase !== "draining") record.phase = "rejected";
    return validation;
  }

  private validationFailure(code: string): RuntimeGenerationValidation {
    return { ok: false, code, runtime_version: null, contract_hash: null, tool_count: null, checked_at_ms: this.now() };
  }

  private async validateRecord(record: GenerationRecord): Promise<RuntimeGenerationValidation> {
    const health = await record.transport.getHealth();
    if (!health.healthy) return this.validationFailure(`health_${health.details || "failed"}`);
    const runtime = record.transport.getRuntimeInfo();
    if (record.spec.expected_runtime_version && runtime.runtime_version !== record.spec.expected_runtime_version) {
      return {
        ok: false,
        code: "runtime_version_mismatch",
        runtime_version: runtime.runtime_version,
        contract_hash: null,
        tool_count: null,
        checked_at_ms: this.now(),
      };
    }
    const catalog = await fetchCatalog({
      endpoint: record.spec.endpoint,
      bearerToken: this.bearerToken,
      fetchFn: this.fetchFn,
      timeoutMs: this.defaultTimeoutMs,
    });
    if (!catalog.ok) return this.validationFailure(catalog.code);
    let hash: string;
    try { hash = computeContractHash(catalog.tools); } catch { return this.validationFailure("catalog_contract_invalid"); }
    if (hash !== this.contractHash) {
      return {
        ok: false,
        code: "contract_mismatch",
        runtime_version: runtime.runtime_version,
        contract_hash: hash,
        tool_count: catalog.tools.length,
        checked_at_ms: this.now(),
      };
    }
    return {
      ok: true,
      code: "validated",
      runtime_version: runtime.runtime_version,
      contract_hash: hash,
      tool_count: catalog.tools.length,
      checked_at_ms: this.now(),
    };
  }

  async activateGeneration(generation: string, options: { checks?: number; intervalMs?: number } = {}): Promise<{
    ok: boolean;
    code: string;
    active_generation: string;
    previous_generation: string | null;
    rolled_back: boolean;
  }> {
    if (generation === this.activeGeneration) {
      return { ok: true, code: "already_active", active_generation: generation, previous_generation: this.previousGeneration, rolled_back: false };
    }
    const target = this.generations.get(generation);
    if (!target) return { ok: false, code: "generation_not_found", active_generation: this.activeGeneration, previous_generation: this.previousGeneration, rolled_back: false };
    const validation = await this.validateGeneration(generation);
    if (!validation.ok) return { ok: false, code: validation.code, active_generation: this.activeGeneration, previous_generation: this.previousGeneration, rolled_back: false };

    const previousId = this.activeGeneration;
    const previous = this.generations.get(previousId)!;
    previous.phase = previous.inFlight > 0 ? "draining" : "standby";
    target.phase = "active";
    this.activeGeneration = generation;
    this.previousGeneration = previousId;
    this.transitionSeq += 1;

    const checks = boundedPositive(options.checks, this.observationChecks, 1, 20);
    const intervalMs = boundedPositive(options.intervalMs, this.observationIntervalMs, 0, 10_000);
    for (let i = 0; i < checks; i += 1) {
      const health = await target.transport.getHealth();
      if (!health.healthy) {
        target.phase = target.inFlight > 0 ? "draining" : "rejected";
        previous.phase = "active";
        this.activeGeneration = previousId;
        this.previousGeneration = generation;
        this.lastTransition = { from: previousId, to: generation, outcome: "rolled_back", reason: `health_${health.details || "failed"}`, at_ms: this.now() };
        return { ok: false, code: "activation_rolled_back", active_generation: previousId, previous_generation: generation, rolled_back: true };
      }
      if (i + 1 < checks && intervalMs > 0) await this.sleepFn(intervalMs);
    }

    this.lastGoodGeneration = generation;
    this.lastTransition = { from: previousId, to: generation, outcome: "activated", reason: null, at_ms: this.now() };
    return { ok: true, code: "activated", active_generation: generation, previous_generation: previousId, rolled_back: false };
  }

  removeGeneration(generation: string): { ok: boolean; code: string } {
    const record = this.generations.get(generation);
    if (!record) return { ok: true, code: "already_absent" };
    if (generation === this.activeGeneration) return { ok: false, code: "cannot_remove_active_generation" };
    if (record.inFlight > 0) return { ok: false, code: "generation_draining" };
    this.generations.delete(generation);
    if (this.previousGeneration === generation) this.previousGeneration = null;
    return { ok: true, code: "removed" };
  }

  async getRuntimeInfo(): Promise<RuntimeIdentitySnapshot> {
    const record = this.generations.get(this.activeGeneration)!;
    const info = await record.transport.getRuntimeInfo();
    return { ...info, runtime_generation: this.activeGeneration };
  }

  async getHealth(): Promise<{ healthy: boolean; details?: string }> {
    return this.generations.get(this.activeGeneration)!.transport.getHealth();
  }

  async dispatchRequest(req: ToolRequestFrame): Promise<RuntimeToolResult> {
    const record = this.generations.get(this.activeGeneration)!;
    const generation = record.spec.generation;
    record.inFlight += 1;
    this.requestOwners.set(req.request_id, generation);
    try {
      return await record.transport.dispatchRequest(req);
    } finally {
      if (this.requestOwners.get(req.request_id) === generation) this.requestOwners.delete(req.request_id);
      record.inFlight = Math.max(0, record.inFlight - 1);
      if (record.inFlight === 0 && record.phase === "draining") record.phase = "standby";
    }
  }

  async cancelRequest(requestId: RequestId, reason: string): Promise<void> {
    const generation = this.requestOwners.get(requestId) ?? this.activeGeneration;
    const record = this.generations.get(generation);
    if (record) await record.transport.cancelRequest(requestId, reason);
  }

  getStatus(): RuntimeManagerStatus {
    return {
      active_generation: this.activeGeneration,
      previous_generation: this.previousGeneration,
      last_good_generation: this.lastGoodGeneration,
      transition_seq: this.transitionSeq,
      last_transition: this.lastTransition ? { ...this.lastTransition } : null,
      generations: [...this.generations.values()]
        .map((record) => ({
          generation: record.spec.generation,
          endpoint: record.spec.endpoint,
          phase: record.phase,
          in_flight: record.inFlight,
          validation: record.validation ? { ...record.validation } : null,
        }))
        .sort((a, b) => a.generation.localeCompare(b.generation)),
    };
  }
}
