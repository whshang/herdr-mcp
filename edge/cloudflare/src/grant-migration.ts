// #626 grant migration: read-only orphan listing, migrate dry-run planning,
// and the #630 confirm transaction core helpers. Pure functions never mutate
// their input. The caller grant check (verifiedControlGrantSnapshot) is not
// touched by anything in this module.
import type { OAuthConnectorGrantRecord } from "./oauth-records.js";
import { sha256Hex } from "./device-crypto.js";

export type GrantKind = "page_assist" | "webchat_control";
export type MigrationPlan = Extract<PlanResult, { ok: true }>;
export type MigrationAuditEntry = {
  at_ms: number;
  by: string;
  action: "migrate";
  audit_id: string;
  from_connector_id: string;
  to_connector_id: string;
  rows: Array<{ device_id: string; endpoint_ref: string; provider?: string; account_ref?: string }>;
};
export const MIGRATE_AUDIT_RING = 64;

/** Canonical plan digest (#630 §7.3): SHA-256 over {from, to, generation, sorted moves}. */
export async function migrationPlanDigest(plan: MigrationPlan): Promise<string> {
  const moves = [...plan.moves].sort((a, b) =>
    (a.kind + a.from.device_id + a.from.endpoint_ref + (a.from.provider ?? "")).localeCompare(
      b.kind + b.from.device_id + b.from.endpoint_ref + (b.from.provider ?? "")));
  return sha256Hex(JSON.stringify({
    from_connector_id: plan.from_connector_id,
    to_connector_id: plan.to_connector_id,
    grant_generation: plan.grant_generation,
    moves,
  }));
}

/** Append the success audit entry to the RAW record (bounded ring, oldest dropped). */
export function appendMigrateAudit(
  raw: Record<string, unknown>,
  plan: MigrationPlan,
  by: string,
  nowMs: number,
  auditId: string,
): Record<string, unknown> {
  const entry: MigrationAuditEntry = {
    at_ms: nowMs,
    by,
    action: "migrate",
    audit_id: auditId,
    from_connector_id: plan.from_connector_id,
    to_connector_id: plan.to_connector_id,
    rows: plan.moves.map((m) => ({
      device_id: m.from.device_id,
      endpoint_ref: m.from.endpoint_ref,
      ...("provider" in m.from ? { provider: m.from.provider, account_ref: m.from.account_ref } : {}),
    })),
  };
  const prior = Array.isArray(raw.audit) ? raw.audit.filter((e): e is Record<string, unknown> => !!e && typeof e === "object") : [];
  const next = [...prior, entry];
  return { ...raw, audit: next.length > MIGRATE_AUDIT_RING ? next.slice(next.length - MIGRATE_AUDIT_RING) : next };
}

/**
 * Post-write verification (#630 §7.2): after applying the plan, no planned row
 * may remain an orphan under the source connector, and the plan must recompute
 * cleanly (empty) against the applied record.
 */
export function migratePostconditionSatisfied(applied: OAuthConnectorGrantRecord | null, plan: MigrationPlan): boolean {
  if (!applied) return false;
  const orphans = listOrphans(applied).filter((row) => row.connector_id === plan.from_connector_id);
  return orphans.length === 0;
}
export interface OrphanRow {
  kind: GrantKind;
  client_id: string;
  current_connector_id: string;
  connector_id: string;
  grant_generation: number;
  device_id: string;
  endpoint_ref: string;
  provider?: string;
  account_ref?: string;
}

export type PlanResult =
  | { ok: true; from_connector_id: string; to_connector_id: string; grant_generation: number; moves: Array<{ kind: GrantKind; from: OrphanRow; to: OrphanRow }> }
  | { ok: false; code: string; conflicts?: OrphanRow[] };

function destinationKey(row: { kind?: GrantKind; device_id: string; endpoint_ref: string; provider?: string; account_ref?: string }, kind: GrantKind): string {
  // Per review item 3: a target row with the same (device_id, endpoint_ref) conflicts,
  // regardless of provider/account (stricter = fail closed).
  return JSON.stringify([kind, row.device_id, row.endpoint_ref]);
}

/** Destination rows that collide with an existing target row or with each other. */
export function findTargetConflicts(record: OAuthConnectorGrantRecord, to: string, destinations: OrphanRow[]): OrphanRow[] {
  const existing = new Set<string>();
  for (const [kind, rows] of [["page_assist", record.page_assist ?? []], ["webchat_control", record.webchat_control ?? []]] as const) {
    for (const row of rows) if (row.connector_id === to) existing.add(destinationKey(row, kind));
  }
  const seen = new Set<string>();
  const conflicts: OrphanRow[] = [];
  for (const dest of destinations) {
    const key = destinationKey(dest, dest.kind);
    if (existing.has(key) || seen.has(key)) conflicts.push(dest);
    seen.add(key);
  }
  return conflicts;
}

/**
 * Whole-record write-back for a future --confirm: start from the RAW stored record so
 * fields this writer does not know (top-level and per-row) are preserved, and rewrite
 * only the planned rows. Pure: returns a new object, never mutates `raw`. Not called by
 * any write path yet (confirm is refused). Callers must go through applyPlanIfFresh.
 */
export function applyPlanToRawRecord(raw: Record<string, unknown>, plan: Extract<PlanResult, { ok: true }>): Record<string, unknown> {
  const rewrite = (kind: GrantKind, rows: unknown): unknown => {
    if (!Array.isArray(rows)) return rows;
    return rows.map((row) => {
      if (!row || typeof row !== "object") return row;
      const r = row as Record<string, unknown>;
      const hit = plan.moves.some((m) => m.kind === kind
        && r.connector_id === m.from.connector_id && r.grant_generation === m.from.grant_generation
        && r.device_id === m.from.device_id && r.endpoint_ref === m.from.endpoint_ref
        && (kind !== "webchat_control" || (r.provider === m.from.provider && r.account_ref === m.from.account_ref)));
      return hit ? { ...r, connector_id: plan.to_connector_id, grant_generation: plan.grant_generation } : { ...r };
    });
  };
  const out: Record<string, unknown> = structuredClone(raw);
  if ("page_assist" in raw) out.page_assist = rewrite("page_assist", structuredClone(raw.page_assist));
  if ("webchat_control" in raw) out.webchat_control = rewrite("webchat_control", structuredClone(raw.webchat_control));
  return out;
}

/**
 * Guarded apply for any future --confirm: recompute a fresh plan against the CURRENT
 * records (re-running target/conflict detection) and refuse unless it equals `plan`.
 * Pure; returns the rewritten raw record only when the plan is still exact.
 */
export function applyPlanIfFresh(
  currentRecords: Iterable<OAuthConnectorGrantRecord | null | undefined>,
  raw: Record<string, unknown>,
  plan: Extract<PlanResult, { ok: true }>,
  deviceId?: string,
): { ok: true; record: Record<string, unknown> } | { ok: false; code: string } {
  const fresh = planMigration(currentRecords, plan.from_connector_id, plan.to_connector_id, deviceId);
  if (!fresh.ok) {
    return { ok: false, code: fresh.code === "migrate_target_conflict" || fresh.code === "migrate_cross_device_rows" ? fresh.code : "migrate_plan_stale" };
  }
  if (JSON.stringify(fresh) !== JSON.stringify(plan)) return { ok: false, code: "migrate_plan_stale" };
  return { ok: true, record: applyPlanToRawRecord(raw, plan) };
}

const CONNECTOR_ID = /^conn_[A-Za-z0-9_-]{8,128}$/;

export function listOrphans(record: OAuthConnectorGrantRecord | null | undefined): OrphanRow[] {
  if (!record || record.status !== "active" || typeof record.connector_id !== "string" || !record.connector_id) return [];
  const current = record.connector_id;
  const out: OrphanRow[] = [];
  for (const [kind, rows] of [["page_assist", record.page_assist ?? []], ["webchat_control", record.webchat_control ?? []]] as const) {
    for (const row of rows) {
      if (row.connector_id === current) continue;
      out.push({
        kind,
        client_id: record.client_id,
        current_connector_id: current,
        connector_id: row.connector_id,
        grant_generation: row.grant_generation,
        device_id: row.device_id,
        endpoint_ref: row.endpoint_ref,
        ...("provider" in row ? { provider: row.provider, account_ref: row.account_ref } : {}),
      });
    }
  }
  return out;
}

export function listAllOrphans(records: Iterable<OAuthConnectorGrantRecord | null | undefined>): OrphanRow[] {
  const out: OrphanRow[] = [];
  for (const record of records) out.push(...listOrphans(record));
  return out;
}

/**
 * Dry run only. Fails closed on any missing or ambiguous selector.
 * Spec A device scope: when `deviceId` is given, moves are restricted to that
 * device BEFORE conflict detection and digest computation, so the returned
 * plan — and the digest the owner confirms — only ever covers the caller's own
 * rows. Any cross-device row in the underlying data makes the plan fail with
 * `migrate_cross_device_rows` when scoped; it is never silently dropped here
 * because the confirm path must reject, not partially apply.
 */
export function planMigration(
  records: Iterable<OAuthConnectorGrantRecord | null | undefined>,
  from: unknown,
  to: unknown,
  deviceId?: string,
): PlanResult {
  if (typeof from !== "string" || !from.trim()) return { ok: false, code: "migrate_from_required" };
  if (typeof to !== "string" || !to.trim()) return { ok: false, code: "migrate_to_required" };
  if (!CONNECTOR_ID.test(from) || !CONNECTOR_ID.test(to)) return { ok: false, code: "migrate_invalid_connector_id" };
  if (from === to) return { ok: false, code: "migrate_source_is_current" };
  const targets = [...records].filter((r): r is OAuthConnectorGrantRecord => !!r && r.connector_id === to);
  if (targets.length === 0) return { ok: false, code: "migrate_target_not_current" };
  if (targets.length > 1) return { ok: false, code: "migrate_target_ambiguous" };
  const target = targets[0];
  if (target.status !== "active") return { ok: false, code: "grant_record_not_active" };
  const generation = target.grant_generation;
  if (typeof generation !== "number" || !Number.isInteger(generation) || generation < 1) {
    return { ok: false, code: "grant_record_missing_field" };
  }
  const matching = listOrphans(target).filter((row) => row.connector_id === from);
  if (deviceId !== undefined) {
    const crossDevice = matching.some((row) => row.device_id !== deviceId);
    if (crossDevice) return { ok: false, code: "migrate_cross_device_rows" };
  }
  const moves = matching
    .map((row) => ({ kind: row.kind, from: row, to: { ...row, connector_id: to, grant_generation: generation } }));
  if (moves.length === 0) return { ok: false, code: "migrate_no_matching_rows" };
  // Target-side conflict: fail closed if a destination row already exists under the
  // target connector, or two source rows would land on the same destination.
  const conflicts = findTargetConflicts(target, to, moves.map((m) => m.to));
  if (conflicts.length > 0) return { ok: false, code: "migrate_target_conflict", conflicts };
  return { ok: true, from_connector_id: from, to_connector_id: to, grant_generation: generation, moves };
}
