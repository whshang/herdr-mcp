import { EPOCH1_CONTRACT } from "./epoch1.js";
import { EPOCH2_CONTRACT } from "./epoch2.js";

/**
 * Current workstation/runtime execution contract.
 *
 * Keep this identity independent from the ChatGPT-visible public contract so
 * Edge-only tools and device routing can evolve without forcing every Rust
 * runtime to implement fleet-control semantics.
 */
export const RUNTIME_EXECUTION_CONTRACT = EPOCH2_CONTRACT;

/** Previous runtime contract accepted only for the existing bounded rollback window. */
export const COMPATIBLE_RUNTIME_CONTRACTS = [EPOCH2_CONTRACT, EPOCH1_CONTRACT] as const;

export function isCompatibleRuntimeContract(epoch: unknown, hash: unknown): boolean {
  return COMPATIBLE_RUNTIME_CONTRACTS.some(
    (contract) => contract.contract_epoch === epoch && contract.contract_hash === hash,
  );
}
