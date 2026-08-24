import { EPOCH1_CONTRACT } from "./epoch1.js";
import { EPOCH2_CONTRACT } from "./epoch2.js";

/** Current ChatGPT-visible public contract. Epoch 1 remains a rollback/link-compat baseline only. */
export const PUBLIC_CONTRACT = EPOCH2_CONTRACT;

/**
 * Workstation links may reconnect with the previous frozen contract during a
 * supervised rollout/rollback. Public tools/list is still always epoch 2.
 */
export const COMPATIBLE_LINK_CONTRACTS = [EPOCH2_CONTRACT, EPOCH1_CONTRACT] as const;

export function isCompatibleLinkContract(epoch: unknown, hash: unknown): boolean {
  return COMPATIBLE_LINK_CONTRACTS.some(
    (contract) => contract.contract_epoch === epoch && contract.contract_hash === hash,
  );
}
