/**
 * canonical-json.ts — deterministic JSON canonicalization.
 *
 * Used to produce a byte-stable encoding of structured values (contract
 * manifests, payloads) so that a SHA-256 contract hash is independent of
 * object key insertion order and of semantically order-insensitive sequences.
 *
 * Rules (documented as the canonical representation the contract hash covers):
 *
 *   1. Object keys are sorted lexicographically (by UTF-16 code unit, matching
 *      JS default string sort) and serialized in that order.
 *   2. The relative order of ARRAY elements is always preserved. Callers that
 *      want an order-insensitive set (e.g. the tool list) must pre-sort the
 *      array themselves with an explicit comparator and pass that in; this
 *      module never reorders arrays, because arrays such as JSON-Schema
 *      `required`, `enum`, `oneOf` and tuple schemas are semantically
 *      order-sensitive and must not be silently permuted.
 *   3. Strings are emitted with JSON.stringify escaping (UTF-8 bytes, no
 *      ASCII-minifying).
 *   4. Numbers are emitted via JSON.stringify. NaN / Infinity / -Infinity and
 *      bigint are rejected because they have no stable canonical JSON form.
 *   5. `undefined`, functions and symbols are rejected. Objects are allowed.
 *   6. `null` is supported.
 *   7. Cyclic structures are rejected with a clear error.
 *   8. Sparse arrays are rejected (a hole is neither `null` nor a value and
 *      has no canonical form).
 *   9. Top-level values may be object/array/primitive; a canonical contract
 *      manifest is expected to be an object, but the function accepts any
 *      supported JSON value.
 *
 * Output is deterministic: two structurally equal JSON values (ignoring object
 * key order, and with arrays already in the caller's desired order) always
 * canonicalize to identical bytes.
 */

/** Thrown for values that have no canonical JSON representation. */
export class CanonicalJsonError extends Error {
  constructor(message: string) {
    super(`canonical-json: ${message}`);
    this.name = "CanonicalJsonError";
  }
}

const SORT = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);

function isSparseArray(arr: unknown[]): boolean {
  for (let i = 0; i < arr.length; i++) {
    if (!(i in arr)) return true;
  }
  return false;
}

/**
 * Canonicalize a JSON-compatible value into a stable string.
 *
 * @param value  Any JSON-compatible value.
 * @param maxDepth  Optional maximum nesting depth to enforce (objects+arrays).
 *                  Defaults to no limit.
 * @throws CanonicalJsonError on unsupported/cyclic/sparse/non-finite values.
 */
export function canonicalJson(value: unknown, maxDepth = Number.MAX_SAFE_INTEGER): string {
  const seen = new Set<object>();
  const out: string[] = [];
  const walk = (v: unknown, depth: number): void => {
    if (depth > maxDepth) throw new CanonicalJsonError(`nesting exceeds max depth ${maxDepth}`);
    if (v === null) {
      out.push("null");
      return;
    }
    const t = typeof v;
    if (t === "string") {
      out.push(JSON.stringify(v));
      return;
    }
    if (t === "boolean") {
      out.push(v ? "true" : "false");
      return;
    }
    if (t === "number") {
      if (!Number.isFinite(v)) throw new CanonicalJsonError("non-finite number is not canonical");
      out.push(JSON.stringify(v));
      return;
    }
    if (t === "bigint") throw new CanonicalJsonError("bigint has no canonical JSON form");
    if (t === "undefined" || t === "symbol" || t === "function") {
      throw new CanonicalJsonError(`unsupported value type '${t}'`);
    }
    if (typeof v === "object") {
      if (seen.has(v)) throw new CanonicalJsonError("circular reference is not supported");
      seen.add(v);
      if (Array.isArray(v)) {
        if (isSparseArray(v)) throw new CanonicalJsonError("sparse arrays have no canonical form");
        out.push("[");
        for (let i = 0; i < v.length; i++) {
          if (i > 0) out.push(",");
          walk(v[i], depth + 1);
        }
        out.push("]");
      } else {
        const rec = v as Record<string, unknown>;
        const keys = Object.keys(rec).sort(SORT);
        out.push("{");
        for (let i = 0; i < keys.length; i++) {
          const key = keys[i];
          if (i > 0) out.push(",");
          out.push(JSON.stringify(key), ":");
          walk(rec[key], depth + 1);
        }
        out.push("}");
      }
      seen.delete(v);
      return;
    }
    throw new CanonicalJsonError(`unsupported value type '${t}'`);
  };
  walk(value, 0);
  return out.join("");
}

/**
 * Canonicalize a value into UTF-8 bytes (the exact input to the SHA-256 hash).
 * Equivalent to `Buffer.from(canonicalJson(value), "utf8")` but kept here so
 * callers don't reach into Buffer directly. Returned via TextEncoder to stay
 * environment-neutral (Node crypto hashes accept this ArrayBuffer view).
 */
export function canonicalBytes(value: unknown, maxDepth?: number): Uint8Array {
  const text = canonicalJson(value, maxDepth);
  return new TextEncoder().encode(text);
}
