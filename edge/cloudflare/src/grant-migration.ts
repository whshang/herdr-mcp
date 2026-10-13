// #626 grant migration: read-only orphan listing and migrate dry-run planning.
// Pure functions: they never mutate their input. No write path exists here;
// `--confirm` is refused by the CLI. The caller grant check
// (verifiedControlGrantSnapshot) is not touched.
import type { OAuthConnectorGrantRecord } from "./oauth-records.js";

export type GrantKind = "page_assist" | "webchat_control";

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
  | { ok: false; code: string };

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

/** Dry run only. Fails closed on any missing or ambiguous selector. */
export function planMigration(
  records: Iterable<OAuthConnectorGrantRecord | null | undefined>,
  from: unknown,
  to: unknown,
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
    return { ok: false, code: "grant_record_not_active" };
  }
  const moves = listOrphans(target)
    .filter((row) => row.connector_id === from)
    .map((row) => ({ kind: row.kind, from: row, to: { ...row, connector_id: to, grant_generation: generation } }));
  if (moves.length === 0) return { ok: false, code: "migrate_no_matching_rows" };
  return { ok: true, from_connector_id: from, to_connector_id: to, grant_generation: generation, moves };
}
