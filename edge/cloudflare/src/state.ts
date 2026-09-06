/**
 * state.ts — persisted workstation session state (storage schema).
 *
 * What MUST survive DO hibernation (plan §11) is written to DO storage: hello
 * claims, presence, last_seen. This module is the (de)serialization boundary
 * and always returns sanitized summaries — never tokens, args or bodies.
 */

import type { EdgeLimits } from "./limits.js";
import { MAX_BOOT_ID_LEN, MAX_CAPABILITIES, MAX_CAPABILITY_LEN, MAX_HEADER_LEN, MAX_WS_ID_LEN } from "./limits.js";

export interface HelloClaims {
  workstationId: string;
  linkVersion: string;
  bootId: string;
  protocolVersion: string;
  connectedAtMs: number;
  runtimeVersion?: string;
  runtimeCommit?: string;
  runtimeGeneration?: string;
  herdProtocolVersion?: string;
  contractHash?: string;
  contractEpoch?: number;
  capabilities?: string[];
}

export type SessionStatus = "offline" | "connecting" | "online" | "draining";

export interface WorkstationSession {
  schemaVersion: 1;
  workstationId: string;
  status: SessionStatus;
  hello?: HelloClaims;
  connectedAtMs?: number;
  lastSeenAtMs?: number;
  disconnectedAtMs?: number;
  /** Most recent successful recovery after an observed disconnect. */
  lastRecoveredAtMs?: number;
  /** Wall time between the observed disconnect and the next validated hello. */
  lastReconnectDurationMs?: number;
  /** Count of observed disconnect -> validated hello recoveries. */
  reconnectCount?: number;
  drainNoticeAtMs?: number;
  /** Allowlist notifications from the link (no free-form strings). */
  runtimeStatus?: {
    runtimeVersion?: string;
    runtimeGeneration?: string;
    herdProtocolVersion?: string;
    health?: "ok" | "degraded";
  };
  upgradeStatus?: { phase?: string; generation?: string };
}

export interface RuntimeStatusGlimpse {
  runtime_version?: string;
  runtime_generation?: string | null;
  herdr_protocol?: string | null;
}

export const SESSION_SCHEMA_VERSION = 1 as const;
const SESSION_KIND = "herdr-edge/workstation-session";

export interface SessionEnvelope {
  kind: typeof SESSION_KIND;
  schemaVersion: 1;
  session: WorkstationSession;
}

function capString(value: string, max: number): string {
  return value.length > max ? value.slice(0, max) : value;
}

/** Build a session record from validated hello claims. Status starts online. */
export function sessionFromClaims(claims: HelloClaims): WorkstationSession {
  return {
    schemaVersion: 1,
    workstationId: claims.workstationId,
    status: "online",
    hello: {
      workstationId: claims.workstationId,
      linkVersion: claims.linkVersion,
      bootId: claims.bootId,
      protocolVersion: claims.protocolVersion,
      connectedAtMs: claims.connectedAtMs,
      ...(claims.runtimeVersion !== undefined ? { runtimeVersion: claims.runtimeVersion } : {}),
      ...(claims.runtimeCommit !== undefined ? { runtimeCommit: claims.runtimeCommit } : {}),
      ...(claims.runtimeGeneration !== undefined ? { runtimeGeneration: claims.runtimeGeneration } : {}),
      ...(claims.herdProtocolVersion !== undefined ? { herdProtocolVersion: claims.herdProtocolVersion } : {}),
      ...(claims.contractHash !== undefined ? { contractHash: claims.contractHash } : {}),
      ...(claims.contractEpoch !== undefined ? { contractEpoch: claims.contractEpoch } : {}),
      ...(claims.capabilities !== undefined && claims.capabilities.length > 0
        ? { capabilities: claims.capabilities.slice(0, MAX_CAPABILITIES).map((c) => capString(c, MAX_CAPABILITY_LEN)) }
        : {}),
    },
    connectedAtMs: claims.connectedAtMs,
    lastSeenAtMs: claims.connectedAtMs,
  };
}

export function serializeSession(session: WorkstationSession): string {
  const envelope: SessionEnvelope = { kind: SESSION_KIND, schemaVersion: 1, session };
  return JSON.stringify(envelope);
}

/**
 * Merge the cheap runtime identity carried by heartbeat/status frames into
 * persisted session state. Returns true only when the externally visible
 * runtime summary changed, so generation/version transitions bypass heartbeat
 * write throttling without forcing a Durable Object storage write every beat.
 */
export function applyRuntimeStatusGlimpse(
  session: WorkstationSession,
  runtime: RuntimeStatusGlimpse | undefined,
  healthy?: boolean,
): boolean {
  if (!runtime && healthy === undefined) return false;
  const previous = session.runtimeStatus;
  const next = {
    ...(previous ?? {}),
    ...(typeof runtime?.runtime_version === "string"
      ? { runtimeVersion: capString(runtime.runtime_version, MAX_HEADER_LEN) }
      : {}),
    ...(typeof runtime?.runtime_generation === "string"
      ? { runtimeGeneration: capString(runtime.runtime_generation, MAX_HEADER_LEN) }
      : {}),
    ...(typeof runtime?.herdr_protocol === "string"
      ? { herdProtocolVersion: capString(runtime.herdr_protocol, MAX_HEADER_LEN) }
      : {}),
    ...(healthy !== undefined ? { health: healthy ? "ok" as const : "degraded" as const } : {}),
  };
  const changed =
    previous?.runtimeVersion !== next.runtimeVersion ||
    previous?.runtimeGeneration !== next.runtimeGeneration ||
    previous?.herdProtocolVersion !== next.herdProtocolVersion ||
    previous?.health !== next.health;
  if (changed) session.runtimeStatus = next;
  return changed;
}

export type ParseSessionResult =
  | { ok: true; session: WorkstationSession }
  | { ok: false; reason: string };

/** Strict-enough deserializer: rejects unknown shape, caps string lengths. */
export function parseSession(raw: string): ParseSessionResult {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw) as unknown;
  } catch {
    return { ok: false, reason: "session blob is not JSON" };
  }
  if (parsed === null || typeof parsed !== "object") return { ok: false, reason: "session blob malformed" };
  const env = parsed as Record<string, unknown>;
  if (env.kind !== SESSION_KIND || env.schemaVersion !== 1) return { ok: false, reason: "unexpected session schema version" };
  const s = env.session as Record<string, unknown>;
  if (typeof s.workstationId !== "string" || s.workstationId.length === 0 || s.workstationId.length > MAX_WS_ID_LEN) {
    return { ok: false, reason: "session workstationId invalid" };
  }
  const status = typeof s.status === "string" ? capString(s.status as string, 12) : "offline";
  const helloRaw = s.hello as Record<string, unknown> | undefined;
  const hello: HelloClaims | undefined =
    helloRaw !== undefined && helloRaw !== null
      ? {
          workstationId: capString(String(helloRaw.workstationId ?? s.workstationId), MAX_WS_ID_LEN),
          linkVersion: capString(String(helloRaw.linkVersion ?? ""), MAX_HEADER_LEN),
          bootId: capString(String(helloRaw.bootId ?? ""), MAX_BOOT_ID_LEN),
          protocolVersion: capString(String(helloRaw.protocolVersion ?? "1"), 8),
          connectedAtMs:
            typeof helloRaw.connectedAtMs === "number" ? helloRaw.connectedAtMs : Date.now(),
          ...(typeof helloRaw.runtimeVersion === "string"
            ? { runtimeVersion: capString(helloRaw.runtimeVersion, MAX_HEADER_LEN) }
            : {}),
          ...(typeof helloRaw.runtimeCommit === "string"
            ? { runtimeCommit: capString(helloRaw.runtimeCommit, MAX_HEADER_LEN) }
            : {}),
          ...(typeof helloRaw.runtimeGeneration === "string"
            ? { runtimeGeneration: capString(helloRaw.runtimeGeneration, MAX_HEADER_LEN) }
            : {}),
          ...(typeof helloRaw.herdProtocolVersion === "string"
            ? { herdProtocolVersion: capString(helloRaw.herdProtocolVersion, MAX_HEADER_LEN) }
            : {}),
          ...(typeof helloRaw.contractHash === "string"
            ? { contractHash: capString(helloRaw.contractHash, 96) }
            : {}),
          ...(typeof helloRaw.contractEpoch === "number" && Number.isFinite(helloRaw.contractEpoch)
            ? { contractEpoch: helloRaw.contractEpoch }
            : {}),
          ...(Array.isArray(helloRaw.capabilities)
            ? {
                capabilities: helloRaw.capabilities
                  .filter((value): value is string => typeof value === "string")
                  .slice(0, MAX_CAPABILITIES)
                  .map((value) => capString(value, MAX_CAPABILITY_LEN)),
              }
            : {}),
        }
      : undefined;
  const runtimeStatusRaw = s.runtimeStatus as Record<string, unknown> | undefined;
  const runtimeStatus = runtimeStatusRaw && typeof runtimeStatusRaw === "object"
    ? {
        ...(typeof runtimeStatusRaw.runtimeVersion === "string"
          ? { runtimeVersion: capString(runtimeStatusRaw.runtimeVersion, MAX_HEADER_LEN) }
          : {}),
        ...(typeof runtimeStatusRaw.runtimeGeneration === "string"
          ? { runtimeGeneration: capString(runtimeStatusRaw.runtimeGeneration, MAX_HEADER_LEN) }
          : {}),
        ...(typeof runtimeStatusRaw.herdProtocolVersion === "string"
          ? { herdProtocolVersion: capString(runtimeStatusRaw.herdProtocolVersion, MAX_HEADER_LEN) }
          : {}),
        ...(["ok", "degraded"].includes(String(runtimeStatusRaw.health))
          ? { health: String(runtimeStatusRaw.health) as "ok" | "degraded" }
          : {}),
      }
    : undefined;
  const session: WorkstationSession = {
    schemaVersion: 1,
    workstationId: s.workstationId as string,
    status: (["offline", "connecting", "online", "draining"].includes(status)
      ? status
      : "offline") as SessionStatus,
    ...(hello !== undefined ? { hello } : {}),
    ...(typeof s.connectedAtMs === "number" ? { connectedAtMs: s.connectedAtMs } : {}),
    ...(typeof s.lastSeenAtMs === "number" ? { lastSeenAtMs: s.lastSeenAtMs } : {}),
    ...(typeof s.disconnectedAtMs === "number" ? { disconnectedAtMs: s.disconnectedAtMs } : {}),
    ...(typeof s.lastRecoveredAtMs === "number" ? { lastRecoveredAtMs: s.lastRecoveredAtMs } : {}),
    ...(typeof s.lastReconnectDurationMs === "number" ? { lastReconnectDurationMs: s.lastReconnectDurationMs } : {}),
    ...(typeof s.reconnectCount === "number" && Number.isSafeInteger(s.reconnectCount) && s.reconnectCount >= 0
      ? { reconnectCount: s.reconnectCount }
      : {}),
    ...(typeof s.drainNoticeAtMs === "number" ? { drainNoticeAtMs: s.drainNoticeAtMs } : {}),
    ...(runtimeStatus !== undefined ? { runtimeStatus } : {}),
  };
  return { ok: true, session };
}

/** Machine-readable /status payload (no secrets, no args). */
export function sessionSummary(
  session: WorkstationSession | undefined,
  opts: { now: number; linkStaleAfterMs: number; activeRequests: number; edgeVersion: string },
) {
  if (!session) {
    return {
      ok: true,
      workstationId: null,
      online: false,
      connected: false,
      status: "offline",
      activeRequests: 0,
      edgeVersion: opts.edgeVersion,
      atMs: opts.now,
    };
  }
  const lastSeenAtMs = session.lastSeenAtMs ?? session.connectedAtMs;
  const stale = lastSeenAtMs !== undefined && opts.now - lastSeenAtMs > opts.linkStaleAfterMs;
  const online = session.status === "online" && !stale;
  return {
    ok: true,
    workstationId: session.workstationId,
    online,
    connected: session.status === "online",
    status: session.status,
    linkVersion: session.hello?.linkVersion,
    bootId: session.hello?.bootId,
    protocolVersion: session.hello?.protocolVersion,
    runtimeVersion: session.runtimeStatus?.runtimeVersion ?? session.hello?.runtimeVersion,
    runtimeCommit: session.hello?.runtimeCommit,
    runtimeGeneration: session.runtimeStatus?.runtimeGeneration ?? session.hello?.runtimeGeneration,
    herdProtocolVersion: session.runtimeStatus?.herdProtocolVersion ?? session.hello?.herdProtocolVersion,
    runtimeHealth: session.runtimeStatus?.health,
    contractHash: session.hello?.contractHash,
    contractEpoch: session.hello?.contractEpoch,
    lastSeenAtMs,
    lastSeenAgoMs: lastSeenAtMs !== undefined ? opts.now - lastSeenAtMs : undefined,
    reconnectingSinceMs: session.status === "offline" ? session.disconnectedAtMs : undefined,
    lastRecoveredAtMs: session.lastRecoveredAtMs,
    lastReconnectDurationMs: session.lastReconnectDurationMs,
    reconnectCount: session.reconnectCount ?? 0,
    // The production Link recycles itself after five minutes continuously
    // offline. Crossing this threshold is useful evidence, but is deliberately
    // not called a launchd restart because Edge cannot prove which process
    // performed the recovery without a separate Link-process identity.
    lastReconnectCrossedRecycleThreshold:
      session.lastReconnectDurationMs !== undefined
        ? session.lastReconnectDurationMs >= 300_000
        : undefined,
    activeRequests: opts.activeRequests,
    edgeVersion: opts.edgeVersion,
    atMs: opts.now,
  };
}

export function makeEmptySession(workstationId: string, now: number): WorkstationSession {
  return { schemaVersion: 1, workstationId, status: "offline", disconnectedAtMs: now };
}

export function isStale(limits: EdgeLimits, session: WorkstationSession, now: number): boolean {
  const lastSeenAtMs = session.lastSeenAtMs ?? session.connectedAtMs;
  return lastSeenAtMs !== undefined && now - lastSeenAtMs > limits.linkStaleAfterMs;
}