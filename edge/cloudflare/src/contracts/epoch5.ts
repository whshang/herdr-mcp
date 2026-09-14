import { EPOCH4_CONTRACT } from "./epoch4.js";

/**
 * First-party deployed public contract epoch 5.
 *
 * Epoch 5 keeps the epoch-4 19-tool catalog, `device` routing selector,
 * read-only annotations, and structured `herdr_exec.steps` schema unchanged.
 * The only model-visible delta is the `herdr_exec` description, which now
 * truthfully describes native-default synchronous execution: ordinary roots run
 * as a durable native session (start + bounded wait), macOS protected roots keep
 * the visible utility pane, and a timeout returns a resumable `session_id`
 * instead of implying the command stays in a pane. Epoch 2, epoch 3, and epoch 4
 * stay frozen and are never mutated in place, so their recorded hashes remain
 * unchanged.
 *
 * ACTIVATION: `public.ts` selects this contract for explicit first-party
 * `EDGE_ENV=dev` and `EDGE_ENV=prod`. Missing and unknown environments keep the
 * conservative epoch-3 fallback.
 */

export const EPOCH5_EXEC_DESCRIPTION =
  "Run commands on the workstation inside the target workspace's selected project root and return the bounded result synchronously. herdr_exec requires workspace and optionally accepts project_root to select a project within that workspace; it does not take root. root belongs to herdr_exec_start, which starts one long-running process without workspace/project_root. Ordinary roots run as a durable native session (start + bounded wait) and do not open a visible pane; macOS user-protected roots (Documents/Desktop/Downloads) use the persistent visible utility pane so execution stays under the TCC-authorized Herdr terminal host. Prefer transparent structured steps for sequential program/argv execution when shell syntax is not required; steps run in order and stop on the first non-zero exit. Use command only when pipes, redirects, expansion, or other shell syntax are actually needed. Exactly one of command or steps is accepted. The same managed-root, busy-project, timeout, and delivery-evidence rules apply to both modes. On timeout the command keeps running as the returned session_id; resume it with herdr_exec_read and never re-send. Freeform command remains a high-capability shell boundary and is not secret-path gated; prefer fs/git tools for ordinary file and Git operations.";

export const EPOCH5_CONTRACT = {
  contract_epoch: 5,
  contract_hash: "sha256:560dc151053f2a54fc1271c299fb6ed4464d5b9651a6c172c5eb6c9d63396e39",
  tool_count: 19,
  tools: EPOCH4_CONTRACT.tools.map((tool) =>
    tool.name === "herdr_exec" ? { ...tool, description: EPOCH5_EXEC_DESCRIPTION } : tool,
  ),
} as const;
