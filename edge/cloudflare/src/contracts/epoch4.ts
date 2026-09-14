import { EPOCH3_CONTRACT } from "./epoch3.js";

/**
 * First-party deployed public contract epoch 4.
 *
 * Epoch 4 keeps the same 19-tool catalog and `device` routing selector while
 * adding two deliberate model-visible improvements: truthful MCP safety hints
 * for genuinely read-only tools, and a transparent structured `steps` form for
 * `herdr_exec`. Epoch 2 and epoch 3 stay frozen and are never mutated in place,
 * so their recorded hashes remain unchanged.
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
 * ACTIVATION: `public.ts` selects this contract for explicit first-party
 * `EDGE_ENV=dev` and `EDGE_ENV=prod`. Missing and unknown environments keep the
 * conservative epoch-3 fallback so ordinary/self-hosted deployments do not
 * change contract epoch implicitly. After deployment, the host may need to
 * refresh its previously approved frozen tool metadata before it exposes the
 * new structured `herdr_exec.steps` schema.
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

function withStructuredExec(tool: (typeof EPOCH3_CONTRACT.tools)[number]) {
  if (tool.name !== "herdr_exec") return tool;
  const inputSchema = tool.inputSchema as typeof tool.inputSchema & {
    properties: Record<string, unknown>;
    required?: readonly string[];
  };
  const command = inputSchema.properties.command as Record<string, unknown>;
  return {
    ...tool,
    description:
      "Run commands on the workstation inside the target workspace's persistent visible utility pane. Prefer transparent structured steps for sequential program/argv execution when shell syntax is not required; steps run in order and stop on the first non-zero exit. Use command only when pipes, redirects, expansion, or other shell syntax are actually needed. Exactly one of command or steps is accepted. The same managed-root, busy-project, timeout, and delivery-evidence rules apply to both modes. Freeform command remains a high-capability shell boundary and is not secret-path gated; prefer fs/git tools for ordinary file and Git operations.",
    inputSchema: {
      ...inputSchema,
      properties: {
        ...inputSchema.properties,
        command: {
          ...command,
          description:
            "Single freeform shell command. Use only when shell syntax is required; otherwise prefer steps.",
        },
        steps: {
          type: "array",
          minItems: 1,
          maxItems: 16,
          description:
            "Sequential transparent process steps. Each step is one executable plus argv; no pipe, redirect, shell expansion, or command substitution semantics are inferred from args.",
          items: {
            type: "object",
            additionalProperties: false,
            properties: {
              program: {
                type: "string",
                minLength: 1,
                description: "Executable name or path for this step.",
              },
              args: {
                type: "array",
                maxItems: 128,
                items: { type: "string" },
                description: "Literal argv entries passed to the executable.",
              },
            },
            required: ["program"],
          },
        },
      },
      required: ["workspace"],
      oneOf: [
        { required: ["command"], not: { required: ["steps"] } },
        { required: ["steps"], not: { required: ["command"] } },
      ],
    },
  } as unknown as (typeof EPOCH3_CONTRACT.tools)[number];
}

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
  contract_hash: "sha256:5367650249eb2385098f66f0cdb1a1d643c74164bb5513e2fb4318f8a3171b51",
  tool_count: 19,
  tools: EPOCH3_CONTRACT.tools.map((tool) => {
    const shaped = withStructuredExec(tool);
    return READ_ONLY_TOOL_NAMES_SET.has(shaped.name) ? withReadOnlyHint(shaped) : shaped;
  }),
} as const;
