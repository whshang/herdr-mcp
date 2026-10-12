import { EPOCH3_CONTRACT } from "./epoch3.js";
import { EPOCH7_CONTRACT } from "./epoch7.js";

/**
 * Conservative fallback ChatGPT-visible device-aware public contract.
 *
 * The published tool schemas describe the runtime-visible arguments only.
 * `herdr_call` additionally accepts two Edge-only relay fields. Both are
 * consumed at the Edge and ignored by the runtime, which reads only `method`
 * and `params` from a `herdr_call` envelope:
 *
 *  - `device` — the device routing selector (documented by the epoch-3 schema);
 *  - `idempotency_key` — a deliberate Edge relay-level idempotency key. It must
 *    be a non-empty string of at most 128 characters with no leading/trailing
 *    whitespace and no control characters; anything else is rejected at the
 *    Edge with `invalid_params` naming the field. The bound matches
 *    `MAX_IDEMPOTENCY_KEY_LEN` in the relay protocol: over-long,
 *    whitespace-wrapped and control-character keys were already forwarded as
 *    the frame's `tool_request.idempotency_key` and rejected there as
 *    `invalid_id`, so the Edge now reports them earlier and explicitly, and the
 *    empty string — previously omitted from that frame field — is rejected too.
 *    The WorkstationDO replay ledger scopes it to the authenticated principal /
 *    connector grant generation, the routed device and a canonical request
 *    hash over the inner method, canonical args and endpoint_ref, and refuses
 *    to replay a page-scoped completion once the caller no longer holds the
 *    endpoint grant. It is NOT the runtime `params.idempotency_key` used by
 *    individual methods, and it is not part of any epoch schema, so the
 *    published contract hashes stay stable. The Edge excludes it from the
 *    request hash it computes; when present it can remain in the forwarded
 *    `arguments` envelope, where the runtime `herdr_call` handler ignores it and
 *    reads only `method` and `params`.
 *
 * Every other top-level `herdr_call` argument is rejected at the Edge.
 */
export const PUBLIC_CONTRACT = EPOCH3_CONTRACT;

export function resolvePublicContract(edgeEnv?: string) {
  return edgeEnv === "dev" || edgeEnv === "prod" ? EPOCH7_CONTRACT : PUBLIC_CONTRACT;
}
