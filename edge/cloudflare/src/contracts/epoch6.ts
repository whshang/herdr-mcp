import { EPOCH5_CONTRACT } from "./epoch5.js";

const CLEAN_TOOL_DESCRIPTIONS: Record<string, string> = {
  herdr_methods:
    "Return live Herdr socket API method names and parameter schemas from the installed runtime, optionally filtered by query.",
  herdr_inspect:
    "Return current Herdr workstation state, including workspaces, tabs, panes, agents, managed project roots, runtime/build identity, and execution-environment hints. Read-only.",
  herdr_skill:
    "Return the read-only Herdr operating policy plus live runtime, generation, and self-update status. refresh bypasses the policy cache; include_native_reference controls whether the release-matched native reference is appended.",
  herdr_call:
    "Invoke one Herdr socket API method with JSON-object params validated against the installed runtime schema. The selected method determines whether the call is read-only or mutating; mutation results include delivery evidence when available.",
  herdr_since:
    "Return incremental Herdr workspace, pane, tab, and agent events after a cursor, plus current matching agents/workspaces and the next cursor. Read-only.",
  herdr_fs_read:
    "Read a bounded text range from a file inside a managed Git project root. Secret-like paths are denied. Read-only.",
  herdr_fs_list:
    "List files and directories inside a managed Git project root, optionally recursively and with a glob filter. Secret-like entries and .git are omitted. Read-only.",
  herdr_fs_grep:
    "Search file contents inside a managed Git project root with literal or regular-expression matching and bounded result limits. Secret-like files are excluded. Read-only.",
  herdr_fs_patch:
    "Validate or apply a multi-hunk patch inside a managed Git project root. Applying may modify files; dirty-file and active-agent gates remain enforced unless explicitly acknowledged.",
  herdr_fs_image:
    "Read an image file inside a managed Git project root and return image content plus metadata. Read-only.",
  herdr_git:
    "Return deterministic Git status, diff, or log facts for a managed project root. Read-only.",
  herdr_exec_start:
    "Start one long-running process in a managed Git project root and return a session_id. Exactly one launch mode is accepted: legacy command, or program with optional literal args. This tool starts a local process and can execute arbitrary commands.",
  herdr_exec_read:
    "Read bounded stdout/stderr from a herdr_exec_start session at an offset and report whether the process is still running. Read-only.",
  herdr_exec_kill:
    "Terminate a herdr_exec_start session with SIGTERM followed by SIGKILL when necessary. This mutates process state.",
  herdr_exec:
    "Run a bounded command in a selected Herdr workspace/project root. Exactly one execution form is accepted: structured steps or freeform command. Ordinary roots use native execution; macOS Documents/Desktop/Downloads use the TCC-authorized visible utility pane. A timeout returns a resumable session_id while execution continues. Freeform command is a high-capability shell boundary and is not secret-path gated.",
  herdr_fs_edit:
    "Replace exactly one matching text fragment in a file inside a managed Git project root. This modifies the file and enforces managed-root, dirty-file, and active-agent gates.",
  herdr_fs_write:
    "Create a file or explicitly overwrite an existing file inside a managed Git project root. This modifies the file and enforces managed-root, dirty-file, and active-agent gates.",
  herdr_prompt:
    "Submit prompt text to an existing Herdr agent target. idempotency_key deduplicates submissions; optional wait can observe a bounded target-status transition. The call can cause the target agent to perform work and returns submission/delivery evidence.",
  herdr_devices:
    "List devices registered with this Herdr Worker and their current routability and runtime status. Edge-local and read-only.",
};

function cleanInputSchema(tool: {
  name: string;
  inputSchema: { properties: Record<string, Record<string, unknown>> } & Record<string, unknown>;
}) {
  const inputSchema = tool.inputSchema;
  if (tool.name === "herdr_call") {
    return {
      ...inputSchema,
      properties: {
        ...inputSchema.properties,
        method: {
          ...inputSchema.properties.method,
          description: "Herdr socket API method name.",
        },
      },
    };
  }
  if (tool.name === "herdr_exec_start") {
    return {
      ...inputSchema,
      properties: {
        ...inputSchema.properties,
        root: {
          ...inputSchema.properties.root,
          description: "Managed Git project root used as the process cwd; this tool has no workspace or project_root fields.",
        },
        command: {
          ...inputSchema.properties.command,
          description: "Legacy freeform shell command, mutually exclusive with program/args.",
        },
        program: {
          ...inputSchema.properties.program,
          description: "Executable name or path for the single long-running process.",
        },
        args: {
          ...inputSchema.properties.args,
          description: "Optional literal argv entries passed to program; defaults to an empty array and has no shell expansion.",
        },
      },
    };
  }
  if (tool.name === "herdr_exec") {
    return {
      ...inputSchema,
      properties: {
        ...inputSchema.properties,
        workspace: {
          ...inputSchema.properties.workspace,
          description: "Herdr workspace_id or unique workspace label that owns the execution target.",
        },
        project_root: {
          ...inputSchema.properties.project_root,
          description: "Project root inside the selected workspace; required when that workspace has multiple project roots.",
        },
        command: {
          ...inputSchema.properties.command,
          description: "Single freeform shell command, mutually exclusive with steps.",
        },
        steps: {
          ...inputSchema.properties.steps,
          description: "Sequential executable/argv steps. Steps run in order and stop after the first non-zero exit; argv entries have no inferred shell syntax.",
        },
      },
    };
  }
  if (tool.name === "herdr_prompt") {
    return {
      ...inputSchema,
      properties: {
        ...inputSchema.properties,
        target: {
          ...inputSchema.properties.target,
          description: "Existing Herdr agent name or pane_id.",
        },
        idempotency_key: {
          ...inputSchema.properties.idempotency_key,
          description: "Optional client key used to deduplicate the same prompt submission.",
        },
        wait: {
          ...inputSchema.properties.wait,
          description: "Optional bounded wait for one of the requested agent statuses after submission.",
        },
      },
    };
  }
  return inputSchema;
}

/**
 * Public contract epoch 6 removes planner workflow policy from model-visible tool
 * metadata while preserving the epoch-5 catalog, schemas, safety annotations,
 * routing selector, and native-default execution semantics.
 */
export const EPOCH6_CONTRACT = {
  contract_epoch: 6,
  contract_hash: "sha256:addcde324850f88cd2f87bf38de1edb7fc34e90e8cd178b37cc49694b56636cf",
  tool_count: 19,
  tools: EPOCH5_CONTRACT.tools.map((tool) => ({
    ...tool,
    description: CLEAN_TOOL_DESCRIPTIONS[tool.name] ?? tool.description,
    inputSchema: cleanInputSchema(tool),
  })),
} as const;
