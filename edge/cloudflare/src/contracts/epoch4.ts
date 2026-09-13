import { EPOCH3_CONTRACT } from "./epoch3.js";

/**
 * FUTURE (not yet active) public contract epoch 4.
 *
 * Epoch 4 is byte-for-byte the epoch-3 catalog (same 19 tools, same schemas and
 * descriptions, same `device` routing selector) with truthful MCP safety hints
 * added only where the complete reachable surface justifies them. Epoch 2 and
 * epoch 3 stay frozen and are never mutated in place, so their recorded hashes
 * below are unchanged.
 *
 * The annotation is a client-side safety hint only. It does not enlarge
 * authorization, does not remove any managed-root / dirty / busy / secret-path
 * gate, and does not make a tool "absolutely safe" — a host may still reject an
 * invocation before it reaches Herdr (e.g. an OpenAI-side false positive on an
 * otherwise read-only call). Mutation-capable or general-purpose tools are
 * deliberately left unannotated so the hint stays truthful:
 *   - `herdr_call`      — generic socket passthrough; the method decides the effect
 *   - `herdr_fs_patch` / `herdr_fs_edit` / `herdr_fs_write` — filesystem mutation
 *   - `herdr_exec_start` / `herdr_exec_kill` / `herdr_exec` — arbitrary shell
 *   - `herdr_prompt`    — dispatches work to a local Agent
 * `herdr_devices` is Edge-local read-only and is already annotated in epoch 3.
 *
 * ACTIVATION: this file is intentionally NOT referenced by `public.ts`. Wiring a
 * new epoch into the ChatGPT-visible surface is a deliberate deployment step
 * (bump the Edge version/contract identity, redeploy, re-verify), never an
 * ordinary build or runtime side effect. After deployment, refresh/recreate the
 * ChatGPT custom-app action snapshot before A/B testing; otherwise the host can
 * keep using its previously approved frozen tool metadata.
 */

/** Tools whose only reachable effects are reads of workstation/Edge state. */
export const EPOCH4_READ_ONLY_TOOL_NAMES = [
  "herdr_methods",
  "herdr_inspect",
  "herdr_skill",
  "herdr_since",
  "herdr_fs_read",
  "herdr_fs_list",
  "herdr_fs_grep",
  "herdr_fs_image",
  "herdr_git",
  "herdr_exec_read",
  "herdr_devices",
] as const;

const READ_ONLY_TOOL_NAMES_SET = new Set<string>(EPOCH4_READ_ONLY_TOOL_NAMES);

/**
 * Read-only tools whose reachable domain is limited to Herdr-managed local or
 * Worker state. `herdr_skill` is intentionally excluded because it may refresh
 * policy from a configured upstream source even though that refresh is read-only.
 */
export const EPOCH4_CLOSED_WORLD_READ_TOOL_NAMES = [
  "herdr_methods",
  "herdr_inspect",
  "herdr_since",
  "herdr_fs_read",
  "herdr_fs_list",
  "herdr_fs_grep",
  "herdr_fs_image",
  "herdr_git",
  "herdr_exec_read",
  "herdr_devices",
] as const;

const CLOSED_WORLD_READ_TOOL_NAMES_SET = new Set<string>(EPOCH4_CLOSED_WORLD_READ_TOOL_NAMES);

function withReadOnlyHint(tool: (typeof EPOCH3_CONTRACT.tools)[number]) {
  const annotations = {
    ...((tool as { annotations?: Record<string, unknown> }).annotations ?? {}),
    readOnlyHint: true,
    ...(CLOSED_WORLD_READ_TOOL_NAMES_SET.has(tool.name) ? { openWorldHint: false } : {}),
  };
  return { ...tool, annotations };
}

/** Epoch 3's catalog with `readOnlyHint: true` only on genuinely read-only tools. */
export const EPOCH4_CONTRACT = {
  contract_epoch: 4,
  contract_hash: "sha256:437ba1826dd612c8d265afb8d3203976df9db8b8370fa91ce62fa672c78a4c62",
  tool_count: 19,
  tools: EPOCH3_CONTRACT.tools.map((tool) =>
    READ_ONLY_TOOL_NAMES_SET.has(tool.name) ? withReadOnlyHint(tool) : tool,
  ),
} as const;
