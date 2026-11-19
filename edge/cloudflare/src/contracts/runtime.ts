import { EPOCH2_CONTRACT } from "./epoch2.js";

/**
 * Frozen runtime execution contract epoch 4 (neutral `herdr_exec` metadata),
 * retained exactly as shipped as the older rollback basis.
 */
export const EPOCH4_RUNTIME_EXECUTION_CONTRACT = {
  contract_epoch: 4,
  contract_hash: "sha256:1f4d272cedb3334b3e17e08080793f6ed81a03dccffba2f6434f149b10e2e135",
  tool_count: 18,
} as const;

/**
 * Previous workstation/runtime execution contract (epoch 6).
 *
 * This remains accepted for pre-upgrade workstations during the epoch-7
 * rollout.
 */
export const PREVIOUS_RUNTIME_EXECUTION_CONTRACT = {
  contract_epoch: 6,
  contract_hash: "sha256:9221eb8c46009ed68f447b00fd3581ad7c9bd02d69053bd799f541937047ba6e",
  tool_count: 18,
} as const;

/**
 * Current workstation/runtime execution contract (epoch 7).
 *
 * Epoch 7 keeps the epoch-6 catalog and adds three additive optional
 * inputs — `herdr_inspect` `{workspace, since_cursor, view}`,
 * `herdr_exec.max_output_bytes`, and `herdr_fs_read.reads[]` (which also
 * lifts `inputSchema.required` so batch-only calls validate)
 * (`contracts/runtime-exec-v7.json` is the programmatic descriptor; the
 * deterministic hash is pinned in Rust `relay::contract`). The Edge only needs
 * the identity here: it forwards calls under this contract and never re-serves
 * the runtime tool catalog, which is why this is a slim identity rather than a
 * duplicated 18-tool list.
 *
 * Keep this identity independent from the ChatGPT-visible public contract so
 * Edge-only tools and device routing can evolve without forcing every Rust
 * runtime to implement fleet-control semantics.
 */
export const RUNTIME_EXECUTION_CONTRACT = {
  contract_epoch: 7,
  contract_hash: "sha256:6fa3ce18830cacf928fd9b5a22bcbead949510dea33c3d2e799e2c9cbad075e1",
  tool_count: 18,
} as const;

/**
 * Published runtime execution contract epoch 5.
 *
 * Retained in the hello window so an epoch-5 workstation that passes the
 * `/health` contract precheck stays admitted at handshake. Note the precheck
 * itself only matches the published current/previous pair (epochs 7 and 6
 * after this bump), so a pre-upgrade v1.1.1-era Link that hard-gates on
 * epochs 5/4 at the precheck stage cannot reach hello — this entry covers
 * epoch-5 runtimes that arrive through other admission paths.
 */
export const EPOCH5_RUNTIME_EXECUTION_CONTRACT = {
  contract_epoch: 5,
  contract_hash: "sha256:fb1925844e873f12b609232890788c6297474433ac4bc7f003d73b1a3a60f030",
  tool_count: 18,
} as const;

/**
 * Workstation hello acceptance window: epoch 7, the epoch-6 previous
 * runtime, the published epoch-5 runtime, the epoch-4 rollback basis, and
 * the frozen epoch-2 catalog.
 */
export const COMPATIBLE_RUNTIME_CONTRACTS = [
  RUNTIME_EXECUTION_CONTRACT,
  PREVIOUS_RUNTIME_EXECUTION_CONTRACT,
  EPOCH5_RUNTIME_EXECUTION_CONTRACT,
  EPOCH4_RUNTIME_EXECUTION_CONTRACT,
  EPOCH2_CONTRACT,
] as const;

export function isCompatibleRuntimeContract(epoch: unknown, hash: unknown): boolean {
  return COMPATIBLE_RUNTIME_CONTRACTS.some(
    (contract) => contract.contract_epoch === epoch && contract.contract_hash === hash,
  );
}
