import { EPOCH6_CONTRACT } from "./epoch6.js";

const TOOL_TITLES: Record<string, string> = {
  herdr_methods: "Discover Herdr Actions",
  herdr_inspect: "View Workstation",
  herdr_skill: "Read Herdr Guide",
  herdr_call: "Run Herdr Action",
  herdr_since: "Catch Up on Changes",
  herdr_fs_read: "Read Project File",
  herdr_fs_list: "Browse Project Files",
  herdr_fs_grep: "Search Project Files",
  herdr_fs_patch: "Apply Project Patch",
  herdr_fs_image: "View Project Image",
  herdr_git: "Check Git",
  herdr_exec_start: "Start Long-Running Task",
  herdr_exec_read: "Read Task Output",
  herdr_exec_kill: "Stop Running Task",
  herdr_exec: "Run Project Command",
  herdr_fs_edit: "Edit Project File",
  herdr_fs_write: "Write Project File",
  herdr_prompt: "Send Task to Coding Agent",
  herdr_devices: "List Workstations",
};

const TOOL_DESCRIPTIONS: Record<string, string> = {
  herdr_methods:
    "Use this when the exact Herdr action or input shape is unknown. Returns supported action names and parameter schemas without changing workstation state.",
  herdr_inspect:
    "Use this to see what is currently open or running on a workstation before continuing work. Returns workspaces, panes, agents, project locations, and runtime identity without changing state.",
  herdr_skill:
    "Use this to read Herdr reference material and current runtime or update context. Refreshing may retrieve the configured reference source; it does not perform project or workspace actions.",
  herdr_call:
    "Use this for a supported Herdr action that has no dedicated public tool. The action name and JSON inputs are validated before dispatch; side effects depend on the selected action.",
  herdr_since:
    "Use this to catch up after a previous Herdr observation. Returns workspace, pane, tab, and agent changes after a cursor together with current matching state.",
  herdr_fs_read:
    "Use this to read a specific text file in a Herdr-managed project. Reads are bounded by line and byte limits, and sensitive file paths are rejected.",
  herdr_fs_list:
    "Use this to browse files and folders in a project when names and metadata are enough. Sensitive entries are omitted from the result.",
  herdr_fs_grep:
    "Use this to find text across project files by literal or regular-expression pattern. Results are bounded and sensitive files are excluded.",
  herdr_fs_patch:
    "Use this to validate or apply a coherent patch across project files. Applying a patch can create, edit, move, or delete files and is subject to project activity and dirty-state checks.",
  herdr_fs_image:
    "Use this to inspect an image stored in a project. Returns the image and file metadata without changing it.",
  herdr_git:
    "Use this to inspect Git status, diff, or recent history for a project without changing repository state.",
  herdr_exec_start:
    "Use this for a command expected to keep running beyond a short request, such as a build, test, server, or transfer. Starts one process in the chosen project and returns a session ID for follow-up.",
  herdr_exec_read:
    "Use this to read new output and completion state from a previously started long-running process. It does not modify the process.",
  herdr_exec_kill:
    "Use this to stop a previously started long-running process by session ID. Stopping a process changes workstation state and may interrupt unfinished work.",
  herdr_exec:
    "Use this for a bounded command or short sequence in a selected project. It supports direct executable/argument steps and shell-command text; a timeout returns a session ID for the same continuing run.",
  herdr_fs_edit:
    "Use this for one precise text replacement inside an existing project file. The file changes only when the old text matches exactly once and project activity and dirty-state checks allow the edit.",
  herdr_fs_write:
    "Use this to create a project file or replace an existing file when overwrite is explicitly acknowledged. It changes file contents and respects project activity and dirty-state checks.",
  herdr_prompt:
    "Use this to hand a self-contained task to an existing local coding agent. Returns submission and delivery evidence; an idempotency key can deduplicate the same submission.",
  herdr_devices:
    "Use this to see which enrolled workstations are available and which runtime version each is reporting. This does not change workstation enrollment or state.",
};

type ToolAnnotations = {
  readOnlyHint: boolean;
  destructiveHint: boolean;
  openWorldHint: boolean;
};

const TOOL_ANNOTATIONS: Record<string, ToolAnnotations> = {
  herdr_methods: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
  herdr_inspect: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
  herdr_skill: { readOnlyHint: true, destructiveHint: false, openWorldHint: true },
  herdr_call: { readOnlyHint: false, destructiveHint: true, openWorldHint: true },
  herdr_since: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
  herdr_fs_read: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
  herdr_fs_list: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
  herdr_fs_grep: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
  herdr_fs_patch: { readOnlyHint: false, destructiveHint: true, openWorldHint: false },
  herdr_fs_image: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
  herdr_git: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
  herdr_exec_start: { readOnlyHint: false, destructiveHint: true, openWorldHint: true },
  herdr_exec_read: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
  herdr_exec_kill: { readOnlyHint: false, destructiveHint: true, openWorldHint: false },
  herdr_exec: { readOnlyHint: false, destructiveHint: true, openWorldHint: true },
  herdr_fs_edit: { readOnlyHint: false, destructiveHint: true, openWorldHint: false },
  herdr_fs_write: { readOnlyHint: false, destructiveHint: true, openWorldHint: false },
  herdr_prompt: { readOnlyHint: false, destructiveHint: true, openWorldHint: true },
  herdr_devices: { readOnlyHint: true, destructiveHint: false, openWorldHint: false },
};

function withDescription(
  property: Record<string, unknown> | undefined,
  description: string,
) {
  return { ...(property ?? {}), description };
}

function neutralInputSchema(tool: {
  name: string;
  inputSchema: { properties: Record<string, Record<string, unknown>> } & Record<string, unknown>;
}) {
  const inputSchema = tool.inputSchema;
  const properties = { ...inputSchema.properties };

  if (properties.device) {
    properties.device = withDescription(
      properties.device,
      "Workstation device_id or unique device name. Omit when the target workstation is already unambiguous.",
    );
  }
  if (properties.confirm_busy) {
    properties.confirm_busy = withDescription(
      properties.confirm_busy,
      "Set true to acknowledge concurrent agent activity in the target project.",
    );
  }
  if (properties.confirm_dirty) {
    properties.confirm_dirty = withDescription(
      properties.confirm_dirty,
      "Set true to acknowledge existing uncommitted changes in the target file or patch scope.",
    );
  }

  if (tool.name === "herdr_skill") {
    properties.refresh = withDescription(
      properties.refresh,
      "Whether to refresh the configured reference source before returning.",
    );
    properties.include_native_reference = withDescription(
      properties.include_native_reference,
      "Whether to append release-matched native Herdr reference material.",
    );
  }
  if (tool.name === "herdr_call") {
    properties.method = withDescription(properties.method, "Supported Herdr action name.");
    properties.params = withDescription(
      properties.params,
      "JSON object string containing the selected action's arguments; use {} when the action has no arguments.",
    );
  }
  if (tool.name === "herdr_exec_start") {
    properties.root = withDescription(properties.root, "Absolute project root used as the process working directory.");
    properties.command = withDescription(
      properties.command,
      "Shell command text, mutually exclusive with program and args.",
    );
    properties.program = withDescription(properties.program, "Executable name or path for the process.");
    properties.args = withDescription(
      properties.args,
      "Optional literal arguments passed to program; defaults to an empty array and has no shell expansion.",
    );
  }
  if (tool.name === "herdr_exec") {
    properties.workspace = withDescription(
      properties.workspace,
      "Workspace ID or unique label containing the target project.",
    );
    properties.project_root = withDescription(
      properties.project_root,
      "Absolute project root in the selected workspace; required when the workspace contains more than one project.",
    );
    properties.command = withDescription(
      properties.command,
      "Shell command text, mutually exclusive with steps.",
    );
    properties.steps = withDescription(
      properties.steps,
      "Ordered executable and literal-argument steps. Each step runs after the previous one succeeds.",
    );
  }
  if (tool.name === "herdr_fs_write") {
    properties.overwrite = withDescription(
      properties.overwrite,
      "Set true to allow replacement of an existing file.",
    );
  }
  if (tool.name === "herdr_prompt") {
    properties.target = withDescription(properties.target, "Existing Herdr agent name or pane ID.");
    properties.text = withDescription(properties.text, "Task text sent to the selected agent.");
    properties.idempotency_key = withDescription(
      properties.idempotency_key,
      "Optional client key that deduplicates the same task submission.",
    );
    properties.wait = withDescription(
      properties.wait,
      "Optional bounded wait for a requested agent status after submission.",
    );
  }

  return { ...inputSchema, properties };
}

/**
 * Public contract epoch 7 keeps epoch 6 capabilities and schema structure while
 * presenting user-intent metadata, explicit human-readable titles, and complete
 * safety annotations. Cross-tool workflow policy remains in herdr_skill.
 */
export const EPOCH7_CONTRACT = {
  contract_epoch: 7,
  contract_hash: "sha256:971ee73a86eef74b1f046b1097806e8d71a4e63b0b57d6f6d5ea35c4e54c44d3",
  tool_count: 19,
  tools: EPOCH6_CONTRACT.tools.map((tool) => ({
    ...tool,
    title: TOOL_TITLES[tool.name],
    description: TOOL_DESCRIPTIONS[tool.name] ?? tool.description,
    annotations: TOOL_ANNOTATIONS[tool.name],
    inputSchema: neutralInputSchema(tool),
  })),
} as const;
