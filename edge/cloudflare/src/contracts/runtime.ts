import { EPOCH2_CONTRACT } from "./epoch2.js";

/**
 * Current workstation/runtime execution contract (epoch 3).
 *
 * Epoch 3 is the frozen epoch-2 catalog with only `herdr_exec` execution
 * metadata shaped to the native-default semantics (`contracts/runtime-exec-v3.json`
 * is the programmatic descriptor; the deterministic hash is pinned in Rust
 * `relay::contract`). The Edge only needs the identity here: it forwards calls
 * under this contract and never re-serves the runtime tool catalog, which is why
 * this is a slim identity rather than a duplicated 18-tool list.
 *
 * Keep this identity independent from the ChatGPT-visible public contract so
 * Edge-only tools and device routing can evolve without forcing every Rust
 * runtime to implement fleet-control semantics.
 */
export const RUNTIME_EXECUTION_CONTRACT = {
  contract_epoch: 3,
  contract_hash: "sha256:05350993b3e964ab28c8b586c3fdbffa5fa615025bc7f3e93eb6aa960c901fc5",
  tool_count: 18,
} as const;

/**
 * Workstation hello acceptance window: the current runtime contract or its
 * immediately previous rollback baseline. Rollback activates the previous
 * binary, which still advertises epoch 2.
 */
export const COMPATIBLE_RUNTIME_CONTRACTS = [RUNTIME_EXECUTION_CONTRACT, EPOCH2_CONTRACT] as const;

export function isCompatibleRuntimeContract(epoch: unknown, hash: unknown): boolean {
  return COMPATIBLE_RUNTIME_CONTRACTS.some(
    (contract) => contract.contract_epoch === epoch && contract.contract_hash === hash,
  );
}
