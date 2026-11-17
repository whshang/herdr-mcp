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
export const PANE_REF_FIELDS = new Set(["pane_id", "pane", "target", "split_from", "parent_target"]);
/**
 * Scan-only fields contribute device evidence for routing and conflict
 * detection but are never unwrapped: `repair_of` carries a receipt id whose
 * bare form is not a workspace/pane id, so a ref here is routing evidence
 * only, and a malformed or foreign-device value still fails closed.
 */
const SCAN_ONLY_REF_FIELDS = new Set(["repair_of"]);
const REF_FIELDS = new Set([...WORKSPACE_REF_FIELDS, ...PANE_REF_FIELDS]);
const ALL_SCANNED_FIELDS = new Set([...REF_FIELDS, ...SCAN_ONLY_REF_FIELDS]);
/**
 * Nested containers whose own ref fields are scanned one level deep:
 * dispatch `spawn` objects carry `workspace_id` / `split_from` which may be
 * device-aware refs exactly like their top-level counterparts.
 */
const NESTED_REF_CONTAINERS = new Set(["spawn"]);

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

type RefHit = { deviceId: string; field: string; raw: string };

/**
 * Scan one flat object level for ref fields. Returns a failure marker on the
 * first malformed prefix or kind mismatch, otherwise the first decoded hit and
 * every decoded device id (for cross-device conflict detection).
 */
function scanRefFields(
  obj: Record<string, unknown>,
  fieldPrefix: string,
): { failure: RefHit | null; first: RefHit | null; devices: string[] } {
  let first: RefHit | null = null;
  const devices: string[] = [];
  for (const [key, value] of Object.entries(obj)) {
    if (typeof value !== "string") continue;
    if (!ALL_SCANNED_FIELDS.has(key)) continue;
    const field = `${fieldPrefix}${key}`;
    const decoded = decodeDeviceRef(value);
    if (!decoded) {
      if (value.startsWith(REF_PREFIX)) {
        return { failure: { deviceId: "__malformed__", field, raw: value }, first, devices };
      }
      continue;
    }
    const expected = fieldRefKind(key);
    const actual = refPayloadKind(decoded);
    // Wrong ref type for the target field must fail closed, not silently coerce
    if (expected && actual && expected !== actual) {
      return { failure: { deviceId: "__type_mismatch__", field, raw: value }, first, devices };
    }
    devices.push(decoded.d);
    if (!first) first = { deviceId: decoded.d, field, raw: value };
  }
  return { failure: null, first, devices };
}

/**
 * Iterate every scanned level of a call argument object: the top level, the
 * `params` object (JSON string or object), and one level of `spawn` inside
 * either. Yields `[object, fieldPrefix]` pairs for `scanRefFields`.
 */
function* scannedLevels(
  args: Record<string, unknown>,
): Generator<[Record<string, unknown>, string]> {
  yield [args, ""];
  const parsedParams: Record<string, unknown> | null = (() => {
    if (typeof args.params === "string") {
      try {
        const parsed = JSON.parse(args.params as string);
        return parsed !== null && typeof parsed === "object" && !Array.isArray(parsed)
          ? (parsed as Record<string, unknown>)
          : null;
      } catch {
        return null;
      }
    }
    if (args.params !== null && typeof args.params === "object" && !Array.isArray(args.params)) {
      return args.params as Record<string, unknown>;
    }
    return null;
  })();
  if (parsedParams) yield [parsedParams, "params."];
  for (const [obj, prefix] of [[args, ""] as const, [parsedParams, "params."] as const]) {
    if (!obj) continue;
    for (const container of NESTED_REF_CONTAINERS) {
      const nested = obj[container];
      if (nested !== null && typeof nested === "object" && !Array.isArray(nested)) {
        yield [nested as Record<string, unknown>, `${prefix}${container}.`];
      }
    }
  }
}

function extractFromObject(args: Record<string, unknown>): { deviceId: string; field: string; raw: string; kind: "ref" } | null {
  let refCandidate: { deviceId: string; field: string; raw: string } | null = null;
  const devices = new Set<string>();

  for (const [obj, prefix] of scannedLevels(args)) {
    const scan = scanRefFields(obj, prefix);
    if (scan.failure) {
      return { deviceId: scan.failure.deviceId, field: scan.failure.field, raw: scan.failure.raw, kind: "ref" };
    }
    for (const d of scan.devices) devices.add(d);
    if (!refCandidate && scan.first) {
      refCandidate = { deviceId: scan.first.deviceId, field: scan.first.field, raw: scan.first.raw };
    }
  }

  if (refCandidate) {
    // Conflicting refs fail closed as ambiguous before any routing
    if (devices.size > 1) {
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
 *
 * One flat object level plus `spawn` containers one level deep. Nested
 * containers are cloned before mutation so caller-owned objects are never
 * written. Mutates `obj`; returns true when anything changed.
 */
function unwrapRefFieldsInPlace(obj: Record<string, unknown>): boolean {
  let changed = false;
  for (const key of WORKSPACE_REF_FIELDS) {
    const value = obj[key];
    if (typeof value !== "string") continue;
    const decoded = decodeDeviceRef(value);
    if (!decoded) continue;
    if (decoded.w) {
      obj[key] = decoded.w;
      changed = true;
    }
    // pane payload in workspace field is type-mismatch – do not coerce
  }
  for (const key of PANE_REF_FIELDS) {
    const value = obj[key];
    if (typeof value !== "string") continue;
    const decoded = decodeDeviceRef(value);
    if (!decoded) continue;
    if (decoded.p) {
      obj[key] = decoded.p;
      changed = true;
    }
    // workspace payload in pane field is type-mismatch – do not coerce
  }
  for (const container of NESTED_REF_CONTAINERS) {
    const nested = obj[container];
    if (nested !== null && typeof nested === "object" && !Array.isArray(nested)) {
      const copy: Record<string, unknown> = { ...(nested as Record<string, unknown>) };
      obj[container] = copy;
      if (unwrapRefFieldsInPlace(copy)) changed = true;
    }
  }
  return changed;
}

export function unwrapDeviceRefs(args: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = { ...args };
  unwrapRefFieldsInPlace(out);
  // Handle herdr_call params (string or object) with same strictness
  if (typeof out.params === "string") {
    try {
      const parsed = JSON.parse(out.params as string) as Record<string, unknown>;
      if (unwrapRefFieldsInPlace(parsed)) out.params = JSON.stringify(parsed);
    } catch {}
  } else if (out.params !== null && typeof out.params === "object" && !Array.isArray(out.params)) {
    const parsed: Record<string, unknown> = { ...(out.params as Record<string, unknown>) };
    unwrapRefFieldsInPlace(parsed);
    out.params = parsed;
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
