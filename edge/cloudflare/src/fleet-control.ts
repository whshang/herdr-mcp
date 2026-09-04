import { sha256Hex } from "./device-crypto.js";
import { validateDeviceRecord, type DeviceRecord } from "./device-model.js";

const DEVICE_PREFIX = "device:";
const CHAIN_PREFIX = "fleet:chain:v1:";
const LANE_PREFIX = "fleet:lane:v1:";
const LANE_RESERVATION_PREFIX = "fleet:lane-reservation:v1:";
const IDEMPOTENCY_PREFIX = "fleet:idempotency:v1:";
const MAX_IDEMPOTENCY_KEY = 256;
const MAX_PRINCIPAL = 4096;
const MAX_STRING = 4096;
const MAX_SCOPE_ITEMS = 64;
const MAX_TAKEOVER_REASON = 512;
const DEFAULT_LEASE_TTL_MS = 5 * 60_000;
const MIN_LEASE_TTL_MS = 30_000;
const MAX_LEASE_TTL_MS = 30 * 60_000;
export const FLEET_IDEMPOTENCY_TTL_MS = 24 * 60 * 60_000;
export const FLEET_IDEMPOTENCY_MAX_RECORDS = 512;
// Alpha.1 has exactly two trusted credential principals. Reserve half of the
// bounded ledger for existing-resource control, split it evenly between the
// credentials, then keep 32 records per credential for critical recovery so
// routine control traffic cannot consume every exit/fencing opportunity.
export const FLEET_IDEMPOTENCY_ADMISSION_MAX_RECORDS = 256;
export const FLEET_IDEMPOTENCY_CONTROL_MAX_RECORDS = 256;
export const FLEET_IDEMPOTENCY_CONTROL_MAX_PER_PRINCIPAL = 128;
export const FLEET_IDEMPOTENCY_CONTROL_ROUTINE_MAX_PER_PRINCIPAL = 96;
export const FLEET_IDEMPOTENCY_CONTROL_CRITICAL_MAX_PER_PRINCIPAL = 32;

export const FLEET_CONTROL_METHODS = [
  "herdr_mcp.work_chain.create",
  "herdr_mcp.work_chain.inspect",
  "herdr_mcp.work_chain.checkpoint.update",
  "herdr_mcp.planner_lease.acquire",
  "herdr_mcp.planner_lease.inspect",
  "herdr_mcp.planner_lease.renew",
  "herdr_mcp.planner_lease.release",
  "herdr_mcp.planner_lease.takeover",
  "herdr_mcp.execution_lane.create",
  "herdr_mcp.execution_lane.inspect",
  "herdr_mcp.execution_lane.update",
] as const;

export type FleetControlMethod = (typeof FLEET_CONTROL_METHODS)[number];

const METHOD_FIELDS: Record<FleetControlMethod, { required: string[]; optional?: string[] }> = {
  "herdr_mcp.work_chain.create": { required: ["idempotency_key"] },
  "herdr_mcp.work_chain.inspect": { required: ["work_chain_id"] },
  "herdr_mcp.work_chain.checkpoint.update": {
    required: [
      "work_chain_id", "expected_chain_revision", "expected_lease_generation",
      "expected_checkpoint_revision", "idempotency_key", "summary",
      "checkpoint_json", "checkpoint_sha256", "portable_evidence_refs",
    ],
  },
  "herdr_mcp.planner_lease.acquire": { required: ["work_chain_id", "expected_chain_revision", "idempotency_key"], optional: ["ttl_ms"] },
  "herdr_mcp.planner_lease.inspect": { required: ["work_chain_id"] },
  "herdr_mcp.planner_lease.renew": { required: ["work_chain_id", "expected_chain_revision", "expected_lease_generation", "idempotency_key"], optional: ["ttl_ms"] },
  "herdr_mcp.planner_lease.release": { required: ["work_chain_id", "expected_chain_revision", "expected_lease_generation", "idempotency_key"] },
  "herdr_mcp.planner_lease.takeover": { required: ["work_chain_id", "expected_chain_revision", "expected_lease_generation", "idempotency_key", "reason"], optional: ["ttl_ms"] },
  "herdr_mcp.execution_lane.create": {
    required: ["work_chain_id", "expected_chain_revision", "expected_lease_generation", "idempotency_key", "device_id", "repo_id", "base_commit", "branch_ref"],
    optional: ["file_scope", "runtime_scope", "agent_ref", "status", "validation_summary"],
  },
  "herdr_mcp.execution_lane.inspect": { required: ["lane_id"] },
  "herdr_mcp.execution_lane.update": {
    required: ["work_chain_id", "expected_chain_revision", "expected_lease_generation", "expected_lane_generation", "lane_id", "idempotency_key"],
    optional: ["status", "validation_summary", "reassign", "device_id"],
  },
};

const FIELD_SCHEMAS: Record<string, Record<string, unknown>> = {
  work_chain_id: { type: "string", minLength: 1, maxLength: 128 },
  lane_id: { type: "string", minLength: 1, maxLength: 128 },
  idempotency_key: { type: "string", minLength: 1, maxLength: MAX_IDEMPOTENCY_KEY },
  expected_chain_revision: { type: "integer", minimum: 1 },
  expected_lease_generation: { type: "integer", minimum: 0 },
  expected_lane_generation: { type: "integer", minimum: 1 },
  expected_checkpoint_revision: { type: "integer", minimum: 0 },
  ttl_ms: { type: "integer", minimum: MIN_LEASE_TTL_MS, maximum: MAX_LEASE_TTL_MS },
  reason: { type: "string", minLength: 1, maxLength: MAX_TAKEOVER_REASON },
  device_id: { type: "string", minLength: 1, maxLength: 128 },
  repo_id: { type: "string", minLength: 1, maxLength: 512 },
  base_commit: { type: "string", pattern: "^(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})$" },
  branch_ref: { type: "string", minLength: 1, maxLength: 512, description: "canonical Git branch name; refs/heads/<name> is accepted and normalized to <name>" },
  file_scope: { type: "array", maxItems: MAX_SCOPE_ITEMS, items: { type: "string", maxLength: 1024 }, description: "repo-relative path prefixes; alpha.1 does not accept glob or shell syntax" },
  runtime_scope: { type: "array", maxItems: MAX_SCOPE_ITEMS, items: { type: "string", maxLength: 128, pattern: "^(?:service|runtime):[A-Za-z0-9][A-Za-z0-9._-]{0,119}$" } },
  agent_ref: { type: "string", minLength: 1, maxLength: 1024 },
  status: { type: "string", enum: ["planned", "active", "blocked", "completed", "cancelled"] },
  reassign: { type: "boolean" },
  validation_summary: { type: ["string", "null"], maxLength: 4096 },
  summary: { type: "string", minLength: 1, maxLength: 8192 },
  checkpoint_json: { type: "string", minLength: 2, maxLength: 65536 },
  checkpoint_sha256: { type: "string", pattern: "^[0-9a-fA-F]{64}$" },
  portable_evidence_refs: {
    type: "array",
    maxItems: 64,
    items: {
      type: "object",
      properties: {
        kind: { const: "git_source" },
        repo_id: { type: "string", minLength: 1, maxLength: 512 },
        commit_sha: { type: "string", pattern: "^(?:[0-9a-f]{40}|[0-9a-f]{64})$" },
        repo_relative_path: { type: "string", minLength: 1, maxLength: 1024 },
        line_start: { type: "integer", minimum: 1 },
        line_end: { type: "integer", minimum: 1 },
        evidence_sha256: { type: "string", pattern: "^[0-9a-f]{64}$" },
      },
      required: ["kind", "repo_id", "commit_sha", "repo_relative_path", "evidence_sha256"],
      additionalProperties: false,
    },
  },
};

export function discoverFleetControlMethods(query = ""): Array<Record<string, unknown>> {
  const needle = query.trim().toLowerCase();
  return FLEET_CONTROL_METHODS
    .filter((method) => !needle || method.toLowerCase().includes(needle) || "fleet control".includes(needle))
    .map((method) => {
      const fields = METHOD_FIELDS[method];
      const names = [...fields.required, ...(fields.optional ?? [])];
      return {
        method,
        source: "edge_fleet_control_v1",
        params: {
          type: "object",
          properties: Object.fromEntries(names.map((name) => [name, FIELD_SCHEMAS[name] ?? { type: "string" }])),
          required: fields.required,
          additionalProperties: false,
        },
      };
    });
}

export function invalidFleetControlParam(method: FleetControlMethod, params: Record<string, unknown>): string | null {
  const fields = METHOD_FIELDS[method];
  const allowed = new Set([...fields.required, ...(fields.optional ?? [])]);
  for (const key of Object.keys(params)) if (!allowed.has(key)) return key;
  return null;
}

export interface FleetControllerAuthority {
  principal: string;
  can_force_takeover: boolean;
}

export interface PlannerLease {
  holder_principal: string;
  generation: number;
  acquired_at_ms: number;
  renewed_at_ms: number;
  expires_at_ms: number;
}

export interface PlannerTakeoverAudit {
  previous_holder_principal: string;
  previous_generation: number;
  new_holder_principal: string;
  new_generation: number;
  reason: string;
  at_ms: number;
}

export interface PortableEvidenceRef {
  kind: "git_source";
  repo_id: string;
  commit_sha: string;
  repo_relative_path: string;
  line_start?: number;
  line_end?: number;
  evidence_sha256: string;
}

export interface CompactWorkMemoryCheckpoint {
  schema_version: 1;
  revision: number;
  summary: string;
  checkpoint_json: string;
  checkpoint_sha256: string;
  updated_at_ms: number;
}

export interface WorkChainRecord {
  schema_version: 1;
  work_chain_id: string;
  revision: number;
  status: "active" | "completed" | "cancelled";
  creator_principal: string;
  checkpoint_schema_version: 1;
  checkpoint_revision: number;
  compact_checkpoint: CompactWorkMemoryCheckpoint | null;
  portable_evidence_refs: PortableEvidenceRef[];
  planner_lease_generation: number;
  planner_lease: PlannerLease | null;
  last_planner_takeover: PlannerTakeoverAudit | null;
  created_at_ms: number;
  updated_at_ms: number;
}

export interface ExecutionLaneRecord {
  schema_version: 1;
  lane_id: string;
  work_chain_id: string;
  lane_generation: number;
  device_id: string;
  repo_id: string;
  base_commit: string;
  branch_ref: string;
  file_scope: string[];
  runtime_scope: string[];
  owner_principal: string;
  agent_ref: string | null;
  status: "planned" | "active" | "blocked" | "completed" | "cancelled";
  validation_summary: string | null;
  validation_refs: string[];
  created_at_ms: number;
  updated_at_ms: number;
}

interface IdempotencyRecord {
  schema_version: 1;
  operation: FleetControlMethod;
  request_hash: string;
  principal_hash?: string;
  quota_class?: IdempotencyQuotaClass;
  result: Record<string, unknown>;
  created_at_ms: number;
  expires_at_ms: number;
}

interface LaneReservationRecord {
  schema_version: 1;
  lane_id: string;
  work_chain_id: string;
  repo_id: string;
  branch_ref: string;
  created_at_ms: number;
}

export type FleetControlResult =
  | { ok: true; [key: string]: unknown }
  | { ok: false; code: string; retryable: false; [key: string]: unknown };

function error(code: string, extra: Record<string, unknown> = {}): FleetControlResult {
  return { ok: false, code, retryable: false, ...extra };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function boundedString(value: unknown, max = MAX_STRING): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= max;
}

function integer(value: unknown, min = 0): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= min;
}

function normalizeFileScope(value: unknown): string[] | null {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length > MAX_SCOPE_ITEMS) return null;
  const result: string[] = [];
  for (const item of value) {
    if (!boundedString(item, 1024)) return null;
    const trimmed = item.trim();
    const segments = trimmed.split("/");
    if (!trimmed || trimmed !== item || trimmed.startsWith("/") || trimmed.includes("~") || trimmed.includes("\\")
      || /^[A-Za-z][A-Za-z0-9+.-]*:/.test(trimmed) || /[\u0000-\u001f\u007f:$`"'(){};|&<>!*?\[\]#]/.test(trimmed)
      || segments.some((segment) => !segment || segment === "." || segment === "..")) return null;
    result.push(trimmed);
  }
  return result;
}

function normalizeRuntimeScope(value: unknown): string[] | null {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length > MAX_SCOPE_ITEMS) return null;
  const result: string[] = [];
  for (const item of value) {
    if (!boundedString(item, 128) || item !== item.trim() || !/^(?:service|runtime):[A-Za-z0-9][A-Za-z0-9._-]{0,119}$/.test(item)) return null;
    result.push(item);
  }
  return result;
}

function normalizeStoredEmptyRefs(value: unknown): string[] | null {
  if (value === undefined) return [];
  return Array.isArray(value) && value.length === 0 ? [] : null;
}

function normalizePortableEvidenceRefs(value: unknown): PortableEvidenceRef[] | null {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length > 64) return null;
  const result: PortableEvidenceRef[] = [];
  for (const item of value) {
    if (!isRecord(item) || Object.keys(item).some((key) => ![
      "kind", "repo_id", "commit_sha", "repo_relative_path", "line_start", "line_end", "evidence_sha256",
    ].includes(key))) return null;
    if (item.kind !== "git_source") return null;
    const repoId = normalizeRepoId(item.repo_id);
    if (!repoId || repoId !== item.repo_id) return null;
    if (typeof item.commit_sha !== "string" || !/^(?:[0-9a-f]{40}|[0-9a-f]{64})$/.test(item.commit_sha)) return null;
    const sourcePath = normalizeFileScope([item.repo_relative_path]);
    if (!sourcePath || sourcePath.length !== 1) return null;
    const lineStart = item.line_start === undefined ? undefined : integer(item.line_start, 1) ? item.line_start : null;
    const lineEnd = item.line_end === undefined ? undefined : integer(item.line_end, 1) ? item.line_end : null;
    if (lineStart === null || lineEnd === null || (lineEnd !== undefined && lineStart === undefined) || (lineStart !== undefined && lineEnd !== undefined && lineEnd < lineStart)) return null;
    if (typeof item.evidence_sha256 !== "string" || !/^[0-9a-f]{64}$/.test(item.evidence_sha256)) return null;
    result.push({
      kind: "git_source",
      repo_id: repoId,
      commit_sha: item.commit_sha,
      repo_relative_path: sourcePath[0],
      ...(lineStart === undefined ? {} : { line_start: lineStart }),
      ...(lineEnd === undefined ? {} : { line_end: lineEnd }),
      evidence_sha256: item.evidence_sha256,
    });
  }
  return result;
}

function normalizeCompactCheckpoint(value: unknown, revision: number): CompactWorkMemoryCheckpoint | null | undefined {
  if (value === undefined || value === null) return revision === 0 ? null : undefined;
  if (!isRecord(value) || value.schema_version !== 1 || value.revision !== revision || revision < 1) return undefined;
  if (!boundedString(value.summary, 8192) || value.summary !== value.summary.trim()) return undefined;
  if (!boundedString(value.checkpoint_json, 65536) || typeof value.checkpoint_sha256 !== "string" || !/^[0-9a-f]{64}$/.test(value.checkpoint_sha256)) return undefined;
  try {
    const parsed = JSON.parse(value.checkpoint_json);
    if (!isRecord(parsed)) return undefined;
  } catch {
    return undefined;
  }
  if (!integer(value.updated_at_ms, 0)) return undefined;
  return value as unknown as CompactWorkMemoryCheckpoint;
}

export function normalizeRepoId(value: unknown): string | null {
  if (!boundedString(value, 512)) return null;
  let repo = value.trim();
  repo = repo.replace(/^https?:\/\//i, "");
  repo = repo.replace(/^ssh:\/\/git@/i, "");
  const scp = /^git@([^:]+):(.+)$/.exec(repo);
  if (scp) repo = `${scp[1]}/${scp[2]}`;
  repo = repo.replace(/\.git$/i, "").replace(/^\/+|\/+$/g, "");
  if (repo.includes("\\") || repo.includes(":") || repo.startsWith("~") || /[\u0000-\u0020\u007f]/.test(repo)) return null;
  const parts = repo.split("/");
  if (parts.length < 3 || parts.some((part) => !part || part === "." || part === ".." || !/^[A-Za-z0-9._-]+$/.test(part))) return null;
  const [host, ...path] = parts;
  if (!host.includes(".")) return null;
  const canonicalHost = host.toLowerCase();
  const canonicalPath = canonicalHost === "github.com" ? path.map((part) => part.toLowerCase()) : path;
  return `${canonicalHost}/${canonicalPath.join("/")}`;
}

export function normalizeBranchRef(value: unknown): string | null {
  if (!boundedString(value, 512)) return null;
  let branch = value;
  if (branch.startsWith("refs/heads/")) branch = branch.slice("refs/heads/".length);
  if (!branch || branch === "@" || branch.startsWith("refs/") || branch.startsWith("/") || branch.startsWith("-")
    || branch.includes("//") || branch.includes("..") || branch.includes("@{") || branch.endsWith("/") || branch.endsWith(".")
    || /[\u0000-\u0020\u007f~^:?*\[\\]/.test(branch)) return null;
  const segments = branch.split("/");
  if (segments.some((segment) => !segment || segment.startsWith(".") || segment.endsWith(".lock"))) return null;
  return branch;
}

function validBranchRef(value: unknown): value is string {
  return typeof value === "string" && normalizeBranchRef(value) === value;
}

function validCommit(value: unknown): value is string {
  return typeof value === "string" && /^(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})$/.test(value);
}

function newOpaqueId(prefix: "wc" | "lane"): string {
  return `${prefix}_${crypto.randomUUID().replace(/-/g, "")}`;
}

function stableValue(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(stableValue);
  if (!isRecord(value)) return value;
  const out: Record<string, unknown> = {};
  for (const key of Object.keys(value).sort()) out[key] = stableValue(value[key]);
  return out;
}

async function requestHash(method: FleetControlMethod, params: Record<string, unknown>): Promise<string> {
  return sha256Hex(JSON.stringify({ method, params: stableValue(params) }));
}

async function idempotencyStorageKey(principal: string, key: string): Promise<string> {
  return IDEMPOTENCY_PREFIX + await sha256Hex(`${principal}\u0000${key}`);
}

async function laneReservationStorageKey(repoId: string, branchRef: string): Promise<string> {
  return LANE_RESERVATION_PREFIX + await sha256Hex(`${repoId}\u0000${branchRef}`);
}

function normalizeDeviceRecord(value: unknown): DeviceRecord | null {
  if (!isRecord(value)) return null;
  const candidate = value as unknown as DeviceRecord;
  return validateDeviceRecord(candidate) === null ? candidate : null;
}

function normalizeChain(value: unknown): WorkChainRecord | null {
  if (!isRecord(value) || value.schema_version !== 1) return null;
  if (!boundedString(value.work_chain_id, 128) || !integer(value.revision, 1)) return null;
  if (value.status !== "active" && value.status !== "completed" && value.status !== "cancelled") return null;
  if (!boundedString(value.creator_principal, MAX_PRINCIPAL)) return null;
  if (value.checkpoint_schema_version !== 1 || !integer(value.checkpoint_revision, 0)) return null;
  const portableEvidenceRefs = normalizePortableEvidenceRefs(value.portable_evidence_refs);
  if (!portableEvidenceRefs) return null;
  const compactCheckpoint = normalizeCompactCheckpoint(value.compact_checkpoint, value.checkpoint_revision);
  if (compactCheckpoint === undefined) return null;
  if (!integer(value.planner_lease_generation, 0) || !integer(value.created_at_ms, 0) || !integer(value.updated_at_ms, 0)) return null;
  const lease = value.planner_lease;
  if (lease !== null) {
    if (!isRecord(lease) || !boundedString(lease.holder_principal, MAX_PRINCIPAL)) return null;
    if (!integer(lease.generation, 1) || !integer(lease.acquired_at_ms, 0) || !integer(lease.renewed_at_ms, 0) || !integer(lease.expires_at_ms, 0)) return null;
  }
  const takeover = value.last_planner_takeover;
  if (takeover !== null && takeover !== undefined) {
    if (!isRecord(takeover)) return null;
    if (!boundedString(takeover.previous_holder_principal, MAX_PRINCIPAL) || !boundedString(takeover.new_holder_principal, MAX_PRINCIPAL)) return null;
    if (!integer(takeover.previous_generation, 1) || !integer(takeover.new_generation, 1) || takeover.new_generation <= takeover.previous_generation) return null;
    if (!boundedString(takeover.reason, MAX_TAKEOVER_REASON) || !integer(takeover.at_ms, 0)) return null;
  }
  return {
    ...value,
    compact_checkpoint: compactCheckpoint,
    portable_evidence_refs: portableEvidenceRefs,
    last_planner_takeover: takeover ?? null,
  } as unknown as WorkChainRecord;
}

function normalizeLane(value: unknown): ExecutionLaneRecord | null {
  if (!isRecord(value) || value.schema_version !== 1) return null;
  if (!boundedString(value.lane_id, 128) || !boundedString(value.work_chain_id, 128) || !integer(value.lane_generation, 1)) return null;
  const repoId = normalizeRepoId(value.repo_id);
  const fileScope = normalizeFileScope(value.file_scope);
  const runtimeScope = normalizeRuntimeScope(value.runtime_scope);
  const validationRefs = normalizeStoredEmptyRefs(value.validation_refs);
  if (!boundedString(value.device_id, 128) || !repoId || repoId !== value.repo_id || !validCommit(value.base_commit) || !validBranchRef(value.branch_ref)) return null;
  if (!fileScope || !runtimeScope || !validationRefs) return null;
  if (!boundedString(value.owner_principal, MAX_PRINCIPAL)) return null;
  if (!(value.agent_ref === null || boundedString(value.agent_ref, 1024))) return null;
  if (value.status !== "planned" && value.status !== "active" && value.status !== "blocked" && value.status !== "completed" && value.status !== "cancelled") return null;
  if (!(value.validation_summary === null || boundedString(value.validation_summary, 4096))) return null;
  if (!integer(value.created_at_ms, 0) || !integer(value.updated_at_ms, 0)) return null;
  return { ...value, file_scope: fileScope, runtime_scope: runtimeScope, validation_refs: validationRefs } as unknown as ExecutionLaneRecord;
}

function normalizeIdempotency(value: unknown): IdempotencyRecord | null {
  if (!isRecord(value) || value.schema_version !== 1) return null;
  if (typeof value.operation !== "string" || !isFleetControlMethod(value.operation) || !boundedString(value.request_hash, 128) || !isRecord(value.result)) return null;
  if (value.principal_hash !== undefined && (typeof value.principal_hash !== "string" || !/^[0-9a-f]{64}$/.test(value.principal_hash))) return null;
  if (value.quota_class !== undefined && value.quota_class !== "admission" && value.quota_class !== "control_routine" && value.quota_class !== "control_critical") return null;
  if (!integer(value.created_at_ms, 0) || !integer(value.expires_at_ms, 0) || value.expires_at_ms <= value.created_at_ms) return null;
  return value as unknown as IdempotencyRecord;
}

function normalizeLaneReservation(value: unknown): LaneReservationRecord | null {
  if (!isRecord(value) || value.schema_version !== 1) return null;
  if (!boundedString(value.lane_id, 128) || !boundedString(value.work_chain_id, 128)) return null;
  const repoId = normalizeRepoId(value.repo_id);
  if (!repoId || repoId !== value.repo_id || !validBranchRef(value.branch_ref) || !integer(value.created_at_ms, 0)) return null;
  return value as unknown as LaneReservationRecord;
}

function leaseTtlMs(value: unknown): number | null {
  if (value === undefined) return DEFAULT_LEASE_TTL_MS;
  if (!integer(value, MIN_LEASE_TTL_MS) || value > MAX_LEASE_TTL_MS) return null;
  return value;
}

function activeLease(chain: WorkChainRecord, nowMs: number): PlannerLease | null {
  return chain.planner_lease && chain.planner_lease.expires_at_ms > nowMs ? chain.planner_lease : null;
}

function isPlannerLease(value: FleetControlResult | PlannerLease): value is PlannerLease {
  return "holder_principal" in value && typeof value.holder_principal === "string";
}

function requireRevision(params: Record<string, unknown>, chain: WorkChainRecord): FleetControlResult | null {
  if (!integer(params.expected_chain_revision, 1)) return error("expected_chain_revision_required");
  if (params.expected_chain_revision !== chain.revision) return error("chain_revision_conflict", { expected: params.expected_chain_revision, actual: chain.revision });
  return null;
}

function requireLease(params: Record<string, unknown>, chain: WorkChainRecord, principal: string, nowMs: number): FleetControlResult | PlannerLease {
  if (!integer(params.expected_lease_generation, 1)) return error("expected_lease_generation_required");
  if (params.expected_lease_generation !== chain.planner_lease_generation) return error("stale_lease_generation", { expected: params.expected_lease_generation, actual: chain.planner_lease_generation });
  const lease = activeLease(chain, nowMs);
  if (!lease) return error("planner_lease_missing_or_expired");
  if (lease.holder_principal !== principal) return error("planner_lease_holder_mismatch");
  return lease;
}

function mutationKey(params: Record<string, unknown>): string | null {
  return boundedString(params.idempotency_key, MAX_IDEMPOTENCY_KEY) ? params.idempotency_key : null;
}

function chainId(params: Record<string, unknown>): string | null {
  return boundedString(params.work_chain_id, 128) ? params.work_chain_id : null;
}

function terminalLane(status: ExecutionLaneRecord["status"]): boolean {
  return status === "completed" || status === "cancelled";
}

function laneTransitionAllowed(from: ExecutionLaneRecord["status"], to: ExecutionLaneRecord["status"]): boolean {
  if (from === to) return true;
  if (from === "planned") return to === "active" || to === "cancelled";
  if (from === "active") return to === "blocked" || to === "completed" || to === "cancelled";
  if (from === "blocked") return to === "active" || to === "completed" || to === "cancelled";
  return false;
}

type IdempotencyQuotaClass = "admission" | "control_routine" | "control_critical";

interface IdempotencyUsage {
  total: number;
  admission: number;
  control: number;
  control_routine_for_principal: number;
  control_critical_for_principal: number;
  earliest_total_expiry_ms: number | null;
  earliest_admission_expiry_ms: number | null;
  earliest_control_expiry_ms: number | null;
  earliest_control_routine_principal_expiry_ms: number | null;
  earliest_control_critical_principal_expiry_ms: number | null;
}

function idempotencyQuotaClass(method: FleetControlMethod, params?: Record<string, unknown>): IdempotencyQuotaClass {
  if (method === "herdr_mcp.work_chain.create" || method === "herdr_mcp.execution_lane.create") return "admission";
  if (method === "herdr_mcp.planner_lease.renew" || method === "herdr_mcp.planner_lease.release" || method === "herdr_mcp.planner_lease.takeover") {
    return "control_critical";
  }
  if (method === "herdr_mcp.execution_lane.update" && (params === undefined || params.reassign === true || params.status === "completed" || params.status === "cancelled")) {
    return "control_critical";
  }
  return "control_routine";
}

function storedIdempotencyQuotaClass(record: IdempotencyRecord): IdempotencyQuotaClass {
  return record.quota_class ?? idempotencyQuotaClass(record.operation);
}

function earlier(current: number | null, candidate: number): number {
  return current === null ? candidate : Math.min(current, candidate);
}

async function pruneIdempotencyUsage(tx: DurableObjectTransaction, nowMs: number, principalHash: string): Promise<IdempotencyUsage> {
  const stored = await tx.list<unknown>({ prefix: IDEMPOTENCY_PREFIX });
  const usage: IdempotencyUsage = {
    total: 0,
    admission: 0,
    control: 0,
    control_routine_for_principal: 0,
    control_critical_for_principal: 0,
    earliest_total_expiry_ms: null,
    earliest_admission_expiry_ms: null,
    earliest_control_expiry_ms: null,
    earliest_control_routine_principal_expiry_ms: null,
    earliest_control_critical_principal_expiry_ms: null,
  };
  for (const [key, value] of stored) {
    const record = normalizeIdempotency(value);
    if (!record || record.expires_at_ms <= nowMs) {
      await tx.delete(key);
      continue;
    }
    usage.total += 1;
    usage.earliest_total_expiry_ms = earlier(usage.earliest_total_expiry_ms, record.expires_at_ms);
    const quotaClass = storedIdempotencyQuotaClass(record);
    if (quotaClass === "admission") {
      usage.admission += 1;
      usage.earliest_admission_expiry_ms = earlier(usage.earliest_admission_expiry_ms, record.expires_at_ms);
    } else {
      usage.control += 1;
      usage.earliest_control_expiry_ms = earlier(usage.earliest_control_expiry_ms, record.expires_at_ms);
      if (record.principal_hash === principalHash) {
        if (quotaClass === "control_routine") {
          usage.control_routine_for_principal += 1;
          usage.earliest_control_routine_principal_expiry_ms = earlier(usage.earliest_control_routine_principal_expiry_ms, record.expires_at_ms);
        } else {
          usage.control_critical_for_principal += 1;
          usage.earliest_control_critical_principal_expiry_ms = earlier(usage.earliest_control_critical_principal_expiry_ms, record.expires_at_ms);
        }
      }
    }
  }
  return usage;
}

function idempotencyCapacityError(quotaScope: string, liveRecords: number, limit: number, earliestExpiryMs: number | null, nowMs: number): FleetControlResult {
  return error("idempotency_capacity_exceeded", {
    quota_scope: quotaScope,
    live_records: liveRecords,
    limit,
    recover_after_ms: earliestExpiryMs === null ? FLEET_IDEMPOTENCY_TTL_MS : Math.max(1, earliestExpiryMs - nowMs),
  });
}

export async function executeFleetControl(storage: DurableObjectStorage, method: FleetControlMethod, params: Record<string, unknown>, authority: FleetControllerAuthority, nowMs = Date.now()): Promise<FleetControlResult> {
  const principal = authority?.principal;
  if (!boundedString(principal, MAX_PRINCIPAL) || typeof authority.can_force_takeover !== "boolean") return error("controller_principal_unavailable");
  const invalidParam = invalidFleetControlParam(method, params);
  if (invalidParam) return error("invalid_params", { field: invalidParam });
  if (method === "herdr_mcp.work_chain.inspect") {
    const id = chainId(params);
    if (!id) return error("invalid_params");
    const chain = normalizeChain(await storage.get(CHAIN_PREFIX + id));
    return chain
      ? { ok: true, chain: { ...chain, planner_lease: activeLease(chain, nowMs) } }
      : error("work_chain_not_found");
  }
  if (method === "herdr_mcp.planner_lease.inspect") {
    const id = chainId(params);
    if (!id) return error("invalid_params");
    const chain = normalizeChain(await storage.get(CHAIN_PREFIX + id));
    if (!chain) return error("work_chain_not_found");
    return { ok: true, work_chain_id: id, chain_revision: chain.revision, planner_lease_generation: chain.planner_lease_generation, planner_lease: activeLease(chain, nowMs) };
  }
  if (method === "herdr_mcp.execution_lane.inspect") {
    if (!boundedString(params.lane_id, 128)) return error("invalid_params");
    const lane = normalizeLane(await storage.get(LANE_PREFIX + params.lane_id));
    return lane ? { ok: true, lane } : error("execution_lane_not_found");
  }

  const idemKey = mutationKey(params);
  if (!idemKey) return error("idempotency_key_required");
  const hash = await requestHash(method, params);
  const idemStorageKey = await idempotencyStorageKey(principal, idemKey);
  const principalHash = await sha256Hex(principal);

  return storage.transaction(async (tx) => {
    const priorRaw = await tx.get<unknown>(idemStorageKey);
    const prior = normalizeIdempotency(priorRaw);
    if (prior && prior.expires_at_ms > nowMs) {
      if (prior.operation !== method || prior.request_hash !== hash) return error("idempotency_key_payload_mismatch");
      return { ...(prior.result as { ok: boolean; [key: string]: unknown }), replayed: true } as FleetControlResult;
    }
    if (priorRaw !== undefined) await tx.delete(idemStorageKey);
    const usage = await pruneIdempotencyUsage(tx, nowMs, principalHash);
    const quotaClass = idempotencyQuotaClass(method, params);
    if (quotaClass === "admission" && usage.admission >= FLEET_IDEMPOTENCY_ADMISSION_MAX_RECORDS) {
      return idempotencyCapacityError("admission", usage.admission, FLEET_IDEMPOTENCY_ADMISSION_MAX_RECORDS, usage.earliest_admission_expiry_ms, nowMs);
    }
    if (quotaClass === "control_routine" && usage.control_routine_for_principal >= FLEET_IDEMPOTENCY_CONTROL_ROUTINE_MAX_PER_PRINCIPAL) {
      return idempotencyCapacityError("control_routine_principal", usage.control_routine_for_principal, FLEET_IDEMPOTENCY_CONTROL_ROUTINE_MAX_PER_PRINCIPAL, usage.earliest_control_routine_principal_expiry_ms, nowMs);
    }
    if (quotaClass === "control_critical" && usage.control_critical_for_principal >= FLEET_IDEMPOTENCY_CONTROL_CRITICAL_MAX_PER_PRINCIPAL) {
      return idempotencyCapacityError("control_critical_principal", usage.control_critical_for_principal, FLEET_IDEMPOTENCY_CONTROL_CRITICAL_MAX_PER_PRINCIPAL, usage.earliest_control_critical_principal_expiry_ms, nowMs);
    }
    if (quotaClass !== "admission" && usage.control >= FLEET_IDEMPOTENCY_CONTROL_MAX_RECORDS) {
      return idempotencyCapacityError("control", usage.control, FLEET_IDEMPOTENCY_CONTROL_MAX_RECORDS, usage.earliest_control_expiry_ms, nowMs);
    }
    if (usage.total >= FLEET_IDEMPOTENCY_MAX_RECORDS) {
      return idempotencyCapacityError("global", usage.total, FLEET_IDEMPOTENCY_MAX_RECORDS, usage.earliest_total_expiry_ms, nowMs);
    }

    let result: FleetControlResult;
    if (method === "herdr_mcp.work_chain.create") {
      const id = newOpaqueId("wc");
      const chain: WorkChainRecord = { schema_version: 1, work_chain_id: id, revision: 1, status: "active", creator_principal: principal, checkpoint_schema_version: 1, checkpoint_revision: 0, compact_checkpoint: null, portable_evidence_refs: [], planner_lease_generation: 0, planner_lease: null, last_planner_takeover: null, created_at_ms: nowMs, updated_at_ms: nowMs };
      await tx.put(CHAIN_PREFIX + id, chain);
      result = { ok: true, chain };
    } else {
      const id = chainId(params);
      if (!id) return error("invalid_params");
      const chain = normalizeChain(await tx.get(CHAIN_PREFIX + id));
      if (!chain) return error("work_chain_not_found");
      const revisionError = requireRevision(params, chain);
      if (revisionError) return revisionError;

      if (method === "herdr_mcp.work_chain.checkpoint.update") {
        const leaseOrError = requireLease(params, chain, principal, nowMs);
        if (!isPlannerLease(leaseOrError)) return leaseOrError;
        if (!integer(params.expected_checkpoint_revision, 0)) return error("expected_checkpoint_revision_required");
        if (params.expected_checkpoint_revision !== chain.checkpoint_revision) {
          return error("checkpoint_revision_conflict", { expected: params.expected_checkpoint_revision, actual: chain.checkpoint_revision });
        }
        const summary = boundedString(params.summary, 8192) && params.summary === params.summary.trim() ? params.summary : null;
        const checkpointJson = boundedString(params.checkpoint_json, 65536) ? params.checkpoint_json : null;
        const checkpointSha256 = typeof params.checkpoint_sha256 === "string" && /^[0-9a-fA-F]{64}$/.test(params.checkpoint_sha256)
          ? params.checkpoint_sha256.toLowerCase()
          : null;
        if (!summary || !checkpointJson || !checkpointSha256) return error("invalid_checkpoint");
        try {
          if (!isRecord(JSON.parse(checkpointJson))) return error("invalid_checkpoint_json");
        } catch {
          return error("invalid_checkpoint_json");
        }
        if (await sha256Hex(checkpointJson) !== checkpointSha256) return error("checkpoint_hash_mismatch");
        if (!Array.isArray(params.portable_evidence_refs)) return error("invalid_portable_evidence_refs");
        const portableEvidenceRefs = normalizePortableEvidenceRefs(params.portable_evidence_refs);
        if (!portableEvidenceRefs) return error("invalid_portable_evidence_refs");
        const checkpointRevision = chain.checkpoint_revision + 1;
        const checkpoint: CompactWorkMemoryCheckpoint = {
          schema_version: 1,
          revision: checkpointRevision,
          summary,
          checkpoint_json: checkpointJson,
          checkpoint_sha256: checkpointSha256,
          updated_at_ms: nowMs,
        };
        const next: WorkChainRecord = {
          ...chain,
          revision: chain.revision + 1,
          checkpoint_revision: checkpointRevision,
          compact_checkpoint: checkpoint,
          portable_evidence_refs: portableEvidenceRefs,
          updated_at_ms: nowMs,
        };
        await tx.put(CHAIN_PREFIX + id, next);
        result = { ok: true, chain: next, checkpoint, portable_evidence_refs: portableEvidenceRefs };
      } else if (method === "herdr_mcp.planner_lease.acquire") {
        const ttl = leaseTtlMs(params.ttl_ms);
        if (ttl === null) return error("invalid_lease_ttl");
        const live = activeLease(chain, nowMs);
        if (live) return error("planner_lease_conflict", { holder_principal: live.holder_principal, generation: live.generation });
        const generation = chain.planner_lease_generation + 1;
        const lease: PlannerLease = { holder_principal: principal, generation, acquired_at_ms: nowMs, renewed_at_ms: nowMs, expires_at_ms: nowMs + ttl };
        const next: WorkChainRecord = { ...chain, revision: chain.revision + 1, planner_lease_generation: generation, planner_lease: lease, updated_at_ms: nowMs };
        await tx.put(CHAIN_PREFIX + id, next);
        result = { ok: true, chain: next, planner_lease: lease };
      } else if (method === "herdr_mcp.planner_lease.renew") {
        const ttl = leaseTtlMs(params.ttl_ms);
        if (ttl === null) return error("invalid_lease_ttl");
        const leaseOrError = requireLease(params, chain, principal, nowMs);
        if (!isPlannerLease(leaseOrError)) return leaseOrError;
        const lease: PlannerLease = { ...leaseOrError, renewed_at_ms: nowMs, expires_at_ms: nowMs + ttl };
        const next: WorkChainRecord = { ...chain, revision: chain.revision + 1, planner_lease: lease, updated_at_ms: nowMs };
        await tx.put(CHAIN_PREFIX + id, next);
        result = { ok: true, chain: next, planner_lease: lease };
      } else if (method === "herdr_mcp.planner_lease.release") {
        const leaseOrError = requireLease(params, chain, principal, nowMs);
        if (!isPlannerLease(leaseOrError)) return leaseOrError;
        const next: WorkChainRecord = { ...chain, revision: chain.revision + 1, planner_lease: null, updated_at_ms: nowMs };
        await tx.put(CHAIN_PREFIX + id, next);
        result = { ok: true, chain: next, released_generation: leaseOrError.generation };
      } else if (method === "herdr_mcp.planner_lease.takeover") {
        if (!integer(params.expected_lease_generation, 0) || params.expected_lease_generation !== chain.planner_lease_generation) return error("stale_lease_generation", { expected: params.expected_lease_generation ?? null, actual: chain.planner_lease_generation });
        const ttl = leaseTtlMs(params.ttl_ms);
        if (ttl === null) return error("invalid_lease_ttl");
        const reason = boundedString(params.reason, MAX_TAKEOVER_REASON) ? params.reason.trim() : "";
        if (!reason) return error("takeover_reason_required");
        const previous = activeLease(chain, nowMs);
        if (!previous) return error("planner_lease_missing_or_expired");
        if (previous.holder_principal === principal) return error("planner_lease_takeover_not_needed");
        if (!authority.can_force_takeover) return error("planner_lease_takeover_forbidden");
        const generation = chain.planner_lease_generation + 1;
        const lease: PlannerLease = { holder_principal: principal, generation, acquired_at_ms: nowMs, renewed_at_ms: nowMs, expires_at_ms: nowMs + ttl };
        const audit: PlannerTakeoverAudit = {
          previous_holder_principal: previous.holder_principal,
          previous_generation: previous.generation,
          new_holder_principal: principal,
          new_generation: generation,
          reason,
          at_ms: nowMs,
        };
        const next: WorkChainRecord = { ...chain, revision: chain.revision + 1, planner_lease_generation: generation, planner_lease: lease, last_planner_takeover: audit, updated_at_ms: nowMs };
        await tx.put(CHAIN_PREFIX + id, next);
        result = { ok: true, chain: next, planner_lease: lease, takeover: audit };
      } else if (method === "herdr_mcp.execution_lane.create") {
        const leaseOrError = requireLease(params, chain, principal, nowMs);
        if (!isPlannerLease(leaseOrError)) return leaseOrError;
        if (!boundedString(params.device_id, 128)) return error("device_id_required");
        const device = normalizeDeviceRecord(await tx.get(DEVICE_PREFIX + params.device_id));
        if (!device) return error("device_not_found");
        if (device.authorization !== "active") return error("device_not_authorized", { authorization: device.authorization });
        const repoId = normalizeRepoId(params.repo_id);
        const branchRef = normalizeBranchRef(params.branch_ref);
        if (!repoId || !validCommit(params.base_commit) || !branchRef) return error("invalid_lane_identity");
        const fileScope = normalizeFileScope(params.file_scope);
        if (!fileScope) return error("invalid_params", { field: "file_scope" });
        const runtimeScope = normalizeRuntimeScope(params.runtime_scope);
        if (!runtimeScope) return error("invalid_params", { field: "runtime_scope" });
        const status = params.status === undefined ? "planned" : params.status;
        if (status !== "planned" && status !== "active") return error("invalid_lane_status");
        const reservationKey = await laneReservationStorageKey(repoId, branchRef);
        const reservation = normalizeLaneReservation(await tx.get(reservationKey));
        if (reservation) {
          const existing = normalizeLane(await tx.get(LANE_PREFIX + reservation.lane_id));
          if (existing && !terminalLane(existing.status) && existing.repo_id === repoId && existing.branch_ref === branchRef) {
            return error("branch_lane_conflict", { conflicting_lane_id: existing.lane_id });
          }
          await tx.delete(reservationKey);
        }
        const laneId = newOpaqueId("lane");
        const lane: ExecutionLaneRecord = { schema_version: 1, lane_id: laneId, work_chain_id: id, lane_generation: 1, device_id: params.device_id, repo_id: repoId, base_commit: params.base_commit.toLowerCase(), branch_ref: branchRef, file_scope: fileScope, runtime_scope: runtimeScope, owner_principal: principal, agent_ref: boundedString(params.agent_ref, 1024) ? params.agent_ref : null, status, validation_summary: boundedString(params.validation_summary, 4096) ? params.validation_summary : null, validation_refs: [], created_at_ms: nowMs, updated_at_ms: nowMs };
        await tx.put(LANE_PREFIX + laneId, lane);
        await tx.put(reservationKey, { schema_version: 1, lane_id: laneId, work_chain_id: id, repo_id: repoId, branch_ref: branchRef, created_at_ms: nowMs } satisfies LaneReservationRecord);
        const next: WorkChainRecord = { ...chain, revision: chain.revision + 1, updated_at_ms: nowMs };
        await tx.put(CHAIN_PREFIX + id, next);
        result = { ok: true, chain: next, lane };
      } else if (method === "herdr_mcp.execution_lane.update") {
        const leaseOrError = requireLease(params, chain, principal, nowMs);
        if (!isPlannerLease(leaseOrError)) return leaseOrError;
        if (!boundedString(params.lane_id, 128)) return error("invalid_params");
        const lane = normalizeLane(await tx.get(LANE_PREFIX + params.lane_id));
        if (!lane || lane.work_chain_id !== id) return error("execution_lane_not_found");
        if (!integer(params.expected_lane_generation, 1) || params.expected_lane_generation !== lane.lane_generation) return error("lane_generation_conflict", { expected: params.expected_lane_generation ?? null, actual: lane.lane_generation });
        const nextStatus = params.status ?? lane.status;
        if (nextStatus !== "planned" && nextStatus !== "active" && nextStatus !== "blocked" && nextStatus !== "completed" && nextStatus !== "cancelled") return error("invalid_lane_status");
        if (!laneTransitionAllowed(lane.status, nextStatus)) return error("lane_transition_invalid", { from: lane.status, to: nextStatus });
        if (params.reassign !== undefined && typeof params.reassign !== "boolean") return error("invalid_params", { field: "reassign" });
        const reassign = params.reassign === true;
        const targetDeviceId = params.device_id === undefined ? lane.device_id : boundedString(params.device_id, 128) ? params.device_id : null;
        if (!targetDeviceId) return error("invalid_params", { field: "device_id" });
        const ownershipChanges = lane.owner_principal !== principal || targetDeviceId !== lane.device_id;
        if (ownershipChanges && !reassign) return error(lane.owner_principal !== principal ? "execution_lane_owner_mismatch" : "execution_lane_reassign_required");
        if (reassign && terminalLane(lane.status)) return error("execution_lane_terminal");
        if (reassign) {
          const targetDevice = normalizeDeviceRecord(await tx.get(DEVICE_PREFIX + targetDeviceId));
          if (!targetDevice) return error("device_not_found");
          if (targetDevice.authorization !== "active") return error("device_not_authorized", { authorization: targetDevice.authorization });
        }
        const validationSummary = params.validation_summary === undefined ? lane.validation_summary : params.validation_summary === null ? null : boundedString(params.validation_summary, 4096) ? params.validation_summary : undefined;
        if (validationSummary === undefined) return error("invalid_params");
        const nextLane: ExecutionLaneRecord = { ...lane, lane_generation: lane.lane_generation + 1, device_id: targetDeviceId, owner_principal: reassign ? principal : lane.owner_principal, status: nextStatus, validation_summary: validationSummary, updated_at_ms: nowMs };
        await tx.put(LANE_PREFIX + lane.lane_id, nextLane);
        if (!terminalLane(lane.status) && terminalLane(nextLane.status)) {
          await tx.delete(await laneReservationStorageKey(lane.repo_id, lane.branch_ref));
        }
        const next: WorkChainRecord = { ...chain, revision: chain.revision + 1, updated_at_ms: nowMs };
        await tx.put(CHAIN_PREFIX + id, next);
        result = { ok: true, chain: next, lane: nextLane };
      } else {
        return error("fleet_control_method_unsupported");
      }
    }
    const idem: IdempotencyRecord = {
      schema_version: 1,
      operation: method,
      request_hash: hash,
      principal_hash: principalHash,
      quota_class: quotaClass,
      result: result as Record<string, unknown>,
      created_at_ms: nowMs,
      expires_at_ms: nowMs + FLEET_IDEMPOTENCY_TTL_MS,
    };
    await tx.put(idemStorageKey, idem);
    return result;
  });
}

export function isFleetControlMethod(value: string): value is FleetControlMethod {
  return (FLEET_CONTROL_METHODS as readonly string[]).includes(value);
}
