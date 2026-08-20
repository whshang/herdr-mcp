/**
 * A-1: herdr socket API schema reflection + parameter validation.
 *
 * Caches `herdr api schema --json` (60s TTL) so herdr_call can validate params
 * against the LIVE schema of the installed herdr binary before sending — no
 * more blind passthrough. `$ref` values are resolved against
 * `schemas.request.$defs` (that is where the daemon publishes param shapes).
 */
import { execSync } from "node:child_process";

export interface ParamsSchema {
  /** Raw property map: name -> JSON schema fragment (may contain $ref/type/enum/anyOf). */
  properties: Record<string, unknown>;
  /** Names required by the schema (top-level `required` on the params def). */
  required: string[];
  /** True when the method's params def is `{type:"object"}` with no properties. */
  empty: boolean;
}

export interface ValidationIssue {
  name: string;
  message: string;
}

export interface ValidationResult {
  ok: boolean;
  errors: ValidationIssue[];
  warnings: ValidationIssue[];
}

const SCHEMA_TTL_MS = 60_000;
let schemaCache: { raw: string; loadedAt: number } | null = null;

interface SchemaDoc {
  schemas?: {
    request?: {
      oneOf?: unknown[];
      $defs?: Record<string, unknown>;
    };
  };
}

function loadSchema(force = false): SchemaDoc {
  const now = Date.now();
  if (!force && schemaCache && now - schemaCache.loadedAt < SCHEMA_TTL_MS) {
    return JSON.parse(schemaCache.raw) as SchemaDoc;
  }
  const raw = execSync("herdr api schema --json", { encoding: "utf-8", timeout: 5000 });
  schemaCache = { raw, loadedAt: now };
  return JSON.parse(raw) as SchemaDoc;
}

function requestDoc(doc: SchemaDoc): { oneOf: unknown[]; defs: Record<string, unknown> } {
  const req = doc?.schemas?.request ?? {};
  return {
    oneOf: Array.isArray(req.oneOf) ? req.oneOf : [],
    defs: (req.$defs ?? {}) as Record<string, unknown>,
  };
}

/** Resolve `#/schemas/request/$defs/Name` (or bare `Name`) against the defs map. */
export function resolveRef(ref: unknown, defs: Record<string, unknown>): Record<string, unknown> | null {
  if (typeof ref !== "string") return null;
  const name = ref.includes("/") ? ref.slice(ref.lastIndexOf("/") + 1) : ref;
  const def = defs[name];
  return def && typeof def === "object" ? (def as Record<string, unknown>) : null;
}

/** Find the params schema for a method by scanning request.oneOf method consts. */
export function getMethodParamsSchema(method: string): ParamsSchema | null {
  const doc = loadSchema();
  const { oneOf, defs } = requestDoc(doc);
  for (const item of oneOf) {
    const rec = (item ?? {}) as Record<string, unknown>;
    const props = (rec["properties"] ?? {}) as Record<string, unknown>;
    const mprop = (props["method"] ?? {}) as Record<string, unknown>;
    if (mprop["const"] !== method) continue;
    const pprops = (props["params"] ?? {}) as Record<string, unknown>;
    // Resolve the params $ref (most methods) or inline shape.
    const paramsDef = resolveRef(pprops["$ref"], defs) ?? pprops;
    const propMap = (paramsDef["properties"] ?? {}) as Record<string, unknown>;
    const required = Array.isArray(paramsDef["required"]) ? (paramsDef["required"] as string[]) : [];
    const empty = Object.keys(propMap).length === 0 && (paramsDef["type"] === "object" || paramsDef["type"] === undefined);
    return { properties: propMap, required, empty };
  }
  return null;
}
/** List every method name with its params schema INLINED (resolve $ref to
 * properties + required) so clients see param names without guessing. */
export function listMethods(query = ""): { method: string; params: { properties: Record<string, unknown>; required: string[]; empty: boolean } }[] {
  const doc = loadSchema();
  const { oneOf } = requestDoc(doc);
  const out: { method: string; params: { properties: Record<string, unknown>; required: string[]; empty: boolean } }[] = [];
  for (const item of oneOf) {
    const rec = (item ?? {}) as Record<string, unknown>;
    const props = (rec["properties"] ?? {}) as Record<string, unknown>;
    const mprop = (props["method"] ?? {}) as Record<string, unknown>;
    const method = typeof mprop["const"] === "string" ? mprop["const"] : null;
    if (!method || (query && !method.toLowerCase().includes(query.toLowerCase()))) continue;
    out.push({ method, params: getMethodParamsSchema(method) ?? { properties: {}, required: [], empty: true } });
  }
  return out;
}

function primitiveType(prop: unknown, defs: Record<string, unknown>): { type: string | string[] | null; enum?: unknown[] } | null {
  const rec = (prop ?? {}) as Record<string, unknown>;
  if (typeof rec["type"] === "string" || Array.isArray(rec["type"])) {
    return { type: rec["type"] as string | string[], enum: Array.isArray(rec["enum"]) ? (rec["enum"] as unknown[]) : undefined };
  }
  if (Array.isArray(rec["enum"])) return { type: "string", enum: rec["enum"] as unknown[] };
  const refd = rec["$ref"] !== undefined ? resolveRef(rec["$ref"], defs) : null;
  if (refd) {
    if (Array.isArray(refd["enum"])) return { type: "string", enum: refd["enum"] as unknown[] };
    if (typeof refd["type"] === "string" || Array.isArray(refd["type"])) return { type: refd["type"] as string | string[], enum: Array.isArray(refd["enum"]) ? (refd["enum"] as unknown[]) : undefined };
  }
  if (Array.isArray(rec["anyOf"])) return null; // composite — skip type check
  return null;
}

function matchesType(value: unknown, types: string | string[]): boolean {
  const list = Array.isArray(types) ? types : [types];
  for (const t of list) {
    switch (t) {
      case "string": if (typeof value === "string") return true; break;
      case "number": case "integer": if (typeof value === "number") return true; break;
      case "boolean": if (typeof value === "boolean") return true; break;
      case "object": if (value !== null && typeof value === "object") return true; break;
      case "array": if (Array.isArray(value)) return true; break;
      case "null": if (value === null) return true; break;
    }
  }
  return false;
}

/**
 * Validate `params` for `method` against the live schema. Unknown params are
 * WARNINGS (schema may lag the daemon); missing required / wrong type / wrong
 * enum are ERRORS. Returns ok:false without touching the socket when errors.
 */
export function validateMethodParams(method: string, params: Record<string, unknown>): ValidationResult {
  const errors: ValidationIssue[] = [];
  const warnings: ValidationIssue[] = [];
  const ps = getMethodParamsSchema(method);
  if (!ps) {
    // Method unknown to schema — cannot validate; treat as pass-through warning.
    warnings.push({ name: "method", message: `method "${method}" not found in herdr schema — passed through unvalidated` });
    return { ok: true, errors, warnings };
  }
  if (ps.empty) {
    const extra = Object.keys(params ?? {});
    if (extra.length > 0) warnings.push({ name: "params", message: `method takes no params; got: ${extra.join(", ")}` });
    return { ok: true, errors, warnings };
  }
  const doc = loadSchema();
  const { defs } = requestDoc(doc);
  const given = params ?? {};

  for (const name of ps.required) {
    if (!(name in given) || given[name] === undefined) {
      errors.push({ name, message: `missing required param "${name}"` });
    }
  }
  for (const [name, value] of Object.entries(given)) {
    const prop = ps.properties[name];
    if (!prop) {
      warnings.push({ name, message: `unknown param "${name}" (not in schema — daemon may still accept it)` });
      continue;
    }
    const pt = primitiveType(prop, defs);
    if (!pt || value === undefined) continue;
    if (pt.type !== null && value !== null && !matchesType(value, pt.type)) {
      errors.push({ name, message: `param "${name}" should be ${Array.isArray(pt.type) ? pt.type.join("|") : pt.type}, got ${typeof value}` });
      continue;
    }
    if (Array.isArray(pt.enum) && typeof value === "string" && !pt.enum.includes(value)) {
      errors.push({ name, message: `param "${name}" must be one of [${pt.enum.join(", ")}], got "${value}"` });
    }
  }
  return { ok: errors.length === 0, errors, warnings };
}
