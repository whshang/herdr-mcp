import { normalizeDeviceId } from "./device-model.js";

const REF_PREFIX = "herdr_ref_";
const REF_VERSION = 1;

/** Workspace ids like w1, wAB, pane ids like w1:p1; keep legacy pattern permissive. */
const LEGACY_WORKSPACE_RE = /^w[A-Za-z0-9_-]+$/;
const LEGACY_PANE_RE = /^w[A-Za-z0-9_-]+:p[0-9A-Za-z_-]+$/;

function base64UrlEncode(bytes: Uint8Array): string {
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
}

function base64UrlDecode(str: string): Uint8Array {
  const b64 = str.replace(/-/g, "+").replace(/_/g, "/");
  const pad = b64.length % 4 === 0 ? "" : "=".repeat(4 - (b64.length % 4));
  const binary = atob(b64 + pad);
  const out = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

function textEncode(s: string): Uint8Array { return new TextEncoder().encode(s); }
function textDecode(b: Uint8Array): string { return new TextDecoder().decode(b); }

export interface DeviceRefPayload {
  v: number;
  d: string; // device_id
  w?: string; // workspace_id
  p?: string; // pane_id (full w1:p1)
}

export function isLegacyWorkspaceId(value: string): boolean {
  return LEGACY_WORKSPACE_RE.test(value);
}
export function isLegacyPaneId(value: string): boolean {
  return LEGACY_PANE_RE.test(value);
}

/**
 * Encode a device-aware opaque reference. The payload is base64url JSON;
 * an optional HMAC is appended when a secret is available (authenticated).
 * For the P0-D slice we provide an unauthenticated but validated opaque:
 * the server validates device_id existence before routing, so path/name
 * strings cannot impersonate a device. A future hardening can add HMAC
 * without changing the decode contract (legacy refs remain valid).
 *
 * Strict payload: exactly one of workspaceId or paneId must be present;
 * workspaceId must match LEGACY_WORKSPACE_RE, paneId must match LEGACY_PANE_RE.
 * Device-only refs are rejected – they carry no routable subject.
 */
export function encodeDeviceRef(
  deviceId: string,
  workspaceId?: string,
  paneId?: string,
): string {
  const normalized = normalizeDeviceId(deviceId);
  if (!normalized) throw new Error("invalid device_id for ref");
  const hasW = typeof workspaceId === "string" && workspaceId.length > 0;
  const hasP = typeof paneId === "string" && paneId.length > 0;
  if ((hasW && hasP) || (!hasW && !hasP)) {
    throw new Error("ref must contain exactly one of workspaceId or paneId");
  }
  if (hasW && !isLegacyWorkspaceId(workspaceId!)) throw new Error("invalid workspace_id for ref");
  if (hasP && !isLegacyPaneId(paneId!)) throw new Error("invalid pane_id for ref");
  const payload: DeviceRefPayload = { v: REF_VERSION, d: normalized };
  if (workspaceId) payload.w = workspaceId;
  if (paneId) payload.p = paneId;
  const json = JSON.stringify(payload);
  const b64 = base64UrlEncode(textEncode(json));
  return `${REF_PREFIX}${b64}`;
}

export function decodeDeviceRef(ref: string): DeviceRefPayload | null {
  if (typeof ref !== "string" || !ref.startsWith(REF_PREFIX)) return null;
  const b64 = ref.slice(REF_PREFIX.length);
  // Reject obviously non-base64url or truncated payloads quickly.
  if (!b64 || b64.length < 8) return null;
  try {
    const bytes = base64UrlDecode(b64);
    const json = textDecode(bytes);
    const payload = JSON.parse(json) as DeviceRefPayload;
    if (payload?.v !== REF_VERSION) return null;
    const normalized = normalizeDeviceId(payload.d);
    if (!normalized || normalized !== payload.d) return null;
    const hasW = payload.w !== undefined;
    const hasP = payload.p !== undefined;
    // Strict: exactly one subject, validated grammars, reject /tmp and dual payloads
    if ((hasW && hasP) || (!hasW && !hasP)) return null;
    if (hasW) {
      if (typeof payload.w !== "string" || !isLegacyWorkspaceId(payload.w)) return null;
    }
    if (hasP) {
      if (typeof payload.p !== "string" || !isLegacyPaneId(payload.p)) return null;
    }
    return { v: payload.v, d: normalized, w: payload.w, p: payload.p };
  } catch {
    return null;
  }
}

export function isDeviceAwareRef(value: unknown): boolean {
  return typeof value === "string" && decodeDeviceRef(value) !== null;
}

/** Workspace fields must carry workspace refs; pane fields must carry pane refs. */
export const WORKSPACE_REF_FIELDS = new Set(["workspace", "workspace_id"]);
export const PANE_REF_FIELDS = new Set(["pane_id", "pane", "target"]);
const REF_FIELDS = new Set([...WORKSPACE_REF_FIELDS, ...PANE_REF_FIELDS]);

export type RefKind = "workspace" | "pane";
export function refPayloadKind(payload: DeviceRefPayload): RefKind | null {
  if (payload.w && !payload.p) return "workspace";
  if (payload.p && !payload.w) return "pane";
  return null;
}

function fieldRefKind(field: string): RefKind | null {
  if (WORKSPACE_REF_FIELDS.has(field)) return "workspace";
  if (PANE_REF_FIELDS.has(field)) return "pane";
  // params.* inherits the inner key kind
  if (field.startsWith("params.")) {
    const inner = field.slice("params.".length);
    if (WORKSPACE_REF_FIELDS.has(inner)) return "workspace";
    if (PANE_REF_FIELDS.has(inner)) return "pane";
  }
  return null;
}

function extractFromObject(args: Record<string, unknown>): { deviceId: string; field: string; raw: string; kind: "ref" } | null {
  let refCandidate: { deviceId: string; field: string; raw: string } | null = null;

  for (const [key, value] of Object.entries(args)) {
    if (typeof value !== "string") continue;
    if (!REF_FIELDS.has(key)) continue;
    const decoded = decodeDeviceRef(value);
    if (!decoded) {
      if (value.startsWith(REF_PREFIX)) return { deviceId: "__malformed__", field: key, raw: value, kind: "ref" };
      continue;
    }
    const expected = fieldRefKind(key);
    const actual = refPayloadKind(decoded);
    // Wrong ref type for the target field must fail closed, not silently coerce
    if (expected && actual && expected !== actual) {
      return { deviceId: "__type_mismatch__", field: key, raw: value, kind: "ref" };
    }
    if (!refCandidate) refCandidate = { deviceId: decoded.d, field: key, raw: value };
  }

  // Also check herdr_call params JSON string for embedded pane/workspace
  if (typeof args.params === "string") {
    try {
      const parsed = JSON.parse(args.params as string) as Record<string, unknown>;
      for (const [k, v] of Object.entries(parsed)) {
        if (typeof v !== "string") continue;
        if (!REF_FIELDS.has(k)) continue;
        const decoded = decodeDeviceRef(v);
        if (!decoded) {
          if (v.startsWith(REF_PREFIX)) return { deviceId: "__malformed__", field: `params.${k}`, raw: v, kind: "ref" };
          continue;
        }
        const expected = fieldRefKind(k);
        const actual = refPayloadKind(decoded);
        if (expected && actual && expected !== actual) {
          return { deviceId: "__type_mismatch__", field: `params.${k}`, raw: v, kind: "ref" };
        }
        if (!refCandidate) refCandidate = { deviceId: decoded.d, field: `params.${k}`, raw: v };
      }
    } catch { /* ignore non-JSON */ }
  }

  if (refCandidate) {
    // Detect conflicting device_ids among all ref fields
    const devices = new Set<string>();
    for (const [k, v] of Object.entries(args)) {
      if (!REF_FIELDS.has(k) || typeof v !== "string") continue;
      const d = decodeDeviceRef(v);
      if (d) devices.add(d.d);
    }
    // Include params-embedded refs
    if (typeof args.params === "string") {
      try {
        const parsed = JSON.parse(args.params as string) as Record<string, unknown>;
        for (const [k, v] of Object.entries(parsed)) {
          if (!REF_FIELDS.has(k) || typeof v !== "string") continue;
          const d = decodeDeviceRef(v);
          if (d) devices.add(d.d);
        }
      } catch {}
    }
    if (devices.size > 1) {
      // Conflicting refs fail closed as ambiguous before any routing
      return { deviceId: "__conflict__", field: refCandidate.field, raw: refCandidate.raw, kind: "ref" };
    }
    return { deviceId: refCandidate.deviceId, field: refCandidate.field, raw: refCandidate.raw, kind: "ref" };
  }
  return null;
}

export function extractDeviceIdFromArgs(args: Record<string, unknown>): { deviceId: string; field: string; raw: string; kind: "ref" } | null {
  if (!args || typeof args !== "object") return null;
  return extractFromObject(args);
}

/**
 * Unwrap device-aware refs to their underlying workspace/pane ids before forwarding
 * to the runtime. Only strips the opaque wrapper; legacy plain ids pass through.
 * Strict: workspace fields unwrap only w-payloads, pane fields only p-payloads.
 * Mismatched types are left intact here – the caller must have already failed
 * closed via extractDeviceIdFromArgs type-mismatch detection; we avoid silent coerce.
 */
export function unwrapDeviceRefs(args: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = { ...args };
  for (const key of WORKSPACE_REF_FIELDS) {
    const value = out[key];
    if (typeof value !== "string") continue;
    const decoded = decodeDeviceRef(value);
    if (!decoded) continue;
    if (decoded.w) out[key] = decoded.w;
    // pane payload in workspace field is type-mismatch – do not coerce
  }
  for (const key of PANE_REF_FIELDS) {
    const value = out[key];
    if (typeof value !== "string") continue;
    const decoded = decodeDeviceRef(value);
    if (!decoded) continue;
    if (decoded.p) out[key] = decoded.p;
    // workspace payload in pane field is type-mismatch – do not coerce
  }
  // Handle herdr_call params string with same strictness
  if (typeof out.params === "string") {
    try {
      const parsed = JSON.parse(out.params as string) as Record<string, unknown>;
      let changed = false;
      for (const k of WORKSPACE_REF_FIELDS) {
        const v = parsed[k];
        if (typeof v !== "string") continue;
        const d = decodeDeviceRef(v);
        if (!d || !d.w) continue;
        parsed[k] = d.w;
        changed = true;
      }
      for (const k of PANE_REF_FIELDS) {
        const v = parsed[k];
        if (typeof v !== "string") continue;
        const d = decodeDeviceRef(v);
        if (!d || !d.p) continue;
        parsed[k] = d.p;
        changed = true;
      }
      if (changed) out.params = JSON.stringify(parsed);
    } catch {}
  }
  // Strip legacy untrusted binding keys if present (caller-controlled, not trusted for routing)
  for (const k of ["binding_device_id", "__herdr_binding_device_id", "herdr_binding"]) {
    if (k in out) delete out[k];
  }
  // Never forward Edge-only device selector (handled separately)
  if ("device" in out) delete out["device"];
  return out;
}

/**
 * Wrap runtime result workspace/pane ids into device-aware opaque refs when the
 * call was routed to a specific device. This ensures follow-up calls retain
 * device affinity without trusting arbitrary path strings.
 * Handles both plain result objects and full MCP CallToolResult shapes.
 * Existing structured id fields remain device-aware exactly as before. 0.4.4
 * mirrors routing metadata into JSON text blocks additively: bare ids stay bare
 * there and sibling pane_ref/workspace_ref fields carry device affinity.
 */
export function wrapResultWithDevice(result: unknown, deviceId: string | null, deviceName?: string): unknown {
  if (!deviceId || !result || typeof result !== "object") return result;
  const normalized = normalizeDeviceId(deviceId);
  if (!normalized) return result;

  function wrapId(value: string): string {
    if (decodeDeviceRef(value)) return value; // already wrapped
    if (isLegacyPaneId(value)) return encodeDeviceRef(normalized!, undefined, value);
    if (isLegacyWorkspaceId(value)) return encodeDeviceRef(normalized!, value, undefined);
    return value;
  }

  function wrapStructuredContainer(container: Record<string, unknown>): void {
    if (Array.isArray(container.workspaces)) {
      const workspaces = container.workspaces as Array<Record<string, unknown>>;
      for (const entry of workspaces) {
        const id = (entry.workspace_id ?? entry.id) as string | undefined;
        if (typeof id === "string") entry.workspace_id = wrapId(id);
      }
    }
    if (Array.isArray(container.panes)) {
      const panes = container.panes as Array<Record<string, unknown>>;
      for (const entry of panes) {
        if (typeof entry.pane_id === "string") entry.pane_id = wrapId(entry.pane_id);
        if (typeof entry.workspace_id === "string") entry.workspace_id = wrapId(entry.workspace_id);
      }
    }
    if (Array.isArray(container.agents)) {
      const agents = container.agents as Array<Record<string, unknown>>;
      for (const entry of agents) {
        if (typeof entry.pane_id === "string") entry.pane_id = wrapId(entry.pane_id);
        if (typeof entry.workspace_id === "string") entry.workspace_id = wrapId(entry.workspace_id);
        if (typeof entry.pane === "string") entry.pane = wrapId(entry.pane);
        if (typeof entry.workspace === "string") entry.workspace = wrapId(entry.workspace);
      }
    }
    if (Array.isArray(container.tabs)) {
      const tabs = container.tabs as Array<Record<string, unknown>>;
      for (const entry of tabs) {
        if (typeof entry.workspace_id === "string") entry.workspace_id = wrapId(entry.workspace_id);
      }
    }
  }

  function addTextRefs(container: Record<string, unknown>): void {
    if (Array.isArray(container.workspaces)) {
      const workspaces = container.workspaces as Array<Record<string, unknown>>;
      for (const entry of workspaces) {
        const id = (entry.workspace_id ?? entry.id) as string | undefined;
        if (typeof id === "string") entry.workspace_ref = wrapId(id);
      }
    }
    if (Array.isArray(container.panes)) {
      const panes = container.panes as Array<Record<string, unknown>>;
      for (const entry of panes) {
        if (typeof entry.pane_id === "string") entry.pane_ref = wrapId(entry.pane_id);
        if (typeof entry.workspace_id === "string") entry.workspace_ref = wrapId(entry.workspace_id);
      }
    }
    if (Array.isArray(container.agents)) {
      const agents = container.agents as Array<Record<string, unknown>>;
      for (const entry of agents) {
        const paneId = (entry.pane_id ?? entry.pane) as string | undefined;
        const workspaceId = (entry.workspace_id ?? entry.workspace) as string | undefined;
        if (typeof paneId === "string") entry.pane_ref = wrapId(paneId);
        if (typeof workspaceId === "string") entry.workspace_ref = wrapId(workspaceId);
      }
    }
    if (Array.isArray(container.tabs)) {
      const tabs = container.tabs as Array<Record<string, unknown>>;
      for (const entry of tabs) {
        if (typeof entry.workspace_id === "string") entry.workspace_ref = wrapId(entry.workspace_id);
      }
    }
  }

  // Deep clone via JSON for now; result is expected to be JSON-serializable
  const clone = JSON.parse(JSON.stringify(result)) as Record<string, unknown>;

  function addProvenance(container: Record<string, unknown>): void {
    container.device_id = normalized;
    if (deviceName) container.device_name = deviceName;
  }

  // Preserve the pre-0.4.4 in-place device-aware ids and add explicit sibling
  // refs/provenance to structured and text-visible JSON results. Non-JSON text
  // and binary blocks remain untouched.
  if (Array.isArray(clone.content)) {
    if (isRecord(clone.structuredContent)) {
      const structured = clone.structuredContent as Record<string, unknown>;
      wrapStructuredContainer(structured);
      addProvenance(structured);
    }
    wrapStructuredContainer(clone);
    addProvenance(clone);
    for (const block of clone.content as Array<Record<string, unknown>>) {
      if (block.type !== "text" || typeof block.text !== "string") continue;
      try {
        const parsed = JSON.parse(block.text) as unknown;
        if (!isRecord(parsed)) continue;
        addTextRefs(parsed);
        if (isRecord(parsed.result)) addTextRefs(parsed.result);
        addProvenance(parsed);
        block.text = JSON.stringify(parsed);
      } catch { /* preserve non-JSON text */ }
    }
    return clone;
  }
  if (isRecord(clone.structuredContent)) {
    wrapStructuredContainer(clone.structuredContent as Record<string, unknown>);
    addProvenance(clone.structuredContent as Record<string, unknown>);
  }
  wrapStructuredContainer(clone);
  addProvenance(clone);
  return clone;
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}
