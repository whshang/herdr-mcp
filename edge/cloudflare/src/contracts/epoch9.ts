import { EPOCH8_CONTRACT } from "./epoch8.js";

/**
 * Public epoch 9 mirrors the runtime-exec v7 additions: it advertises the
 * three additive optional inputs on the same tools without changing the 19
 * tool names, existing parameters, or safety annotations.
 *
 * - `herdr_inspect` gains `workspace`, `since_cursor` and `view` so a caller
 *   can scope the live view and take a compact projection.
 * - `herdr_exec` gains `max_output_bytes` to bound the synchronous output
 *   window; the untruncated tail stays resumable via `herdr_exec_read`.
 * - `herdr_fs_read` gains `reads[]` for batch reads and drops the `required`
 *   list so a `reads`-only call validates; single-file `path` callers are
 *   unchanged.
 */
const ADDITIVE_PROPERTIES: Record<string, Record<string, unknown>> = {
  herdr_inspect: {
    workspace: {
      type: "string",
      description: "Workspace ID or label; filters workspaces/tabs/panes/agents/shared projects to that workspace.",
    },
    since_cursor: {
      type: "integer",
      minimum: 0,
      description: "Optional change cursor; embeds incremental workspace/agent events since that cursor under view.since.",
    },
    view: {
      type: "string",
      enum: ["full", "compact"],
      description: "Optional projection; compact drops verbose sections while keeping identities and statuses.",
    },
  },
  herdr_exec: {
    max_output_bytes: {
      type: "integer",
      minimum: 1,
      maximum: 4194304,
      description: "Byte cap for the synchronous output window (default 65536, cap 4194304); bytes_total reports the untruncated size and herdr_exec_read resumes the tail.",
    },
  },
  herdr_fs_read: {
    reads: {
      type: "array",
      minItems: 1,
      maxItems: 32,
      description: "Optional batch reads; each entry uses the same path/start_line/end_line/max_bytes parameters and gates as the single-file mode.",
      items: {
        type: "object",
        additionalProperties: false,
        required: ["path"],
        properties: {
          path: { type: "string", description: "Absolute file path inside a managed project root" },
          start_line: { type: "integer", minimum: 1, maximum: 9007199254740991 },
          end_line: { type: "integer", minimum: 1, maximum: 9007199254740991 },
          max_bytes: { type: "integer", minimum: 1, maximum: 262144 },
        },
      },
    },
  },
};

export const EPOCH9_CONTRACT = {
  contract_epoch: 9,
  contract_hash: "sha256:846d27bb0eb9fc4c8c2a83b9feb0c7f8aa2933a111377169a815ba840c6f6777",
  tool_count: 19,
  tools: EPOCH8_CONTRACT.tools.map((tool) => {
    const additions = ADDITIVE_PROPERTIES[tool.name];
    if (!additions) return tool;
    const inputSchema = { ...(tool.inputSchema as Record<string, unknown>) };
    inputSchema.properties = {
      ...(inputSchema.properties as Record<string, unknown>),
      ...additions,
    };
    if (tool.name === "herdr_fs_read") {
      // Batch mode is an alternative to the single-file `path` input.
      delete inputSchema.required;
    }
    return { ...tool, inputSchema };
  }),
} as const;
