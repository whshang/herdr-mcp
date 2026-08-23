/**
 * relay — provider-independent Relay Protocol v1 (self-upgrade Phase 1).
 *
 * Canonical wire protocol shared by the Cloudflare edge, the workstation
 * `herdr-link` sidecar, and any future VPS/Docker edge. This tree imports
 * nothing from `src/link/**` or `edge/cloudflare/**`; integration mapping is
 * documented in the Phase 1 report.
 *
 * Modules:
 *   protocol.ts       — constants, message kinds, discriminated union.
 *   validation.ts     — frame byte gate + strict shape/limit validation.
 *   canonical-json.ts — deterministic canonical JSON (hash input).
 *   errors.ts         — delivery-state & retryability taxonomy.
 *   contract.ts       — contract manifest, SHA-256 hash, diff/compat.
 */

export * from "./protocol.js";
export * from "./validation.js";
export * from "./canonical-json.js";
export * from "./errors.js";
export * from "./contract.js";