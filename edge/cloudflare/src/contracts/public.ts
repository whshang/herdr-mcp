import { EPOCH3_CONTRACT } from "./epoch3.js";
import { EPOCH7_CONTRACT } from "./epoch7.js";

/**
 * Conservative fallback ChatGPT-visible device-aware public contract.
 *
 * The published tool schemas describe the runtime-visible arguments only.
 * `herdr_call` additionally accepts two Edge-only relay fields that are never
 * forwarded to the runtime as call arguments:
 *
 *  - `device` — the device routing selector (documented by the epoch-3 schema);
 *  - `idempotency_key` — a deliberate Edge relay-level idempotency key. The
 *    WorkstationDO replay ledger scopes it to the authenticated principal /
 *    connector grant generation, the routed device and a canonical request
 *    hash over the inner method, canonical args and endpoint_ref, and refuses
 *    to replay a page-scoped completion once the caller no longer holds the
 *    endpoint grant. It is NOT the runtime `params.idempotency_key` used by
 *    individual methods, and it is not part of any epoch schema, so the
 *    published contract hashes stay stable.
 *
 * Every other top-level `herdr_call` argument is rejected at the Edge.
 */
export const PUBLIC_CONTRACT = EPOCH3_CONTRACT;

export function resolvePublicContract(edgeEnv?: string) {
  return edgeEnv === "dev" || edgeEnv === "prod" ? EPOCH7_CONTRACT : PUBLIC_CONTRACT;
}
