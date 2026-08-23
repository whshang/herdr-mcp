/**
 * Exact metadata overrides required for the frozen ChatGPT contract epoch 1.
 * Generated from the captured live 0.3.23 baseline; execution behavior is NOT downgraded.
 * Do not edit casually: changes alter the public contract hash.
 */
export const EPOCH1_TOOL_OVERRIDES = {
  "herdr_methods": {
    "description": "Discover herdr socket API methods and parameter schemas. LIVE reflection from the installed herdr binary (herdr api schema, 60s cached). Use before herdr_call when you don't know the exact method or argument names."
  },
  "herdr_inspect": {
    "description": "Check herdr connection and list workspaces (with cwd), tabs, panes, and agents in one call. Also returns workstation_info: default_cwd hints, server/build, readonly/write_roots, and a short exec_environment summary (PATH binaries relevant to local coding). Agents come from the shared SnapshotCache (live events + 30s snapshot fallback) and carry started_at + last_activity_at. If session.snapshot blips (TaskGroup), falls back to workspace.list / pane.list / agent.list and sets warnings[] — do not treat that as a repo blocker; keep using herdr_fs_* / herdr_git / herdr_exec. YOU (web) are the planner/orchestrator. Prefer herdr_fs_* / herdr_exec / herdr_git before any herdr_prompt. Agent lists soft-hide expensive kinds (Claude/OMP/Codex); only allowlisted workers (pi, cline, opencode, anti) and auditors (droid, grok) appear — override with HERDR_MCP_AGENT_ALLOW. herdr_prompt by known name/pane_id is NOT blocked. Prefer explicit pane_id/workspace_id from this view."
  },
  "herdr_call": {
    "description": "Generic passthrough to the herdr socket API, VALIDATED against the live schema (schema reflected from the installed herdr binary, 60s cache). Params are checked before sending: missing required / wrong type / wrong enum -> invalid_params error (no socket call); unknown params -> warnings. Call herdr_methods first when the schema is unknown; prefer explicit pane_id/workspace_id over bare names. For agent.prompt prefer herdr_prompt (fire-and-forget + idempotency_key + delivery evidence); do not pass wait on mutations unless you intentionally want submit+wait. Never blind-retry a mutating call after failure — delivery may be uncertain; verify with herdr_inspect / herdr_since first. Status-wait timeouts are failure agent_status_wait_timeout (not herdr_transport).",
    "methodDescription": "herdr socket method, e.g. pane.split, agent.start"
  },
  "herdr_since": {
    "description": "Incremental digest since a cursor — cheap conversation-resume primitive (❺). MCP clients only run when the user sends a message, so polling is not an option; pass the cursor from your last call to get only NEW events. Returns: events[] (pane/workspace/tab changes with cursor+at), current agents[] (status/started_at/last_activity_at/cwd), workspaces[], and a new cursor. First call (cursor=0) returns the recent tail. The server keeps a live events.subscribe stream (A-2) so this is a single round-trip instead of inspect+read+explain. Events/agents carry explicit pane_id/workspace_id — prefer those IDs over labels when addressing targets later."
  },
  "herdr_prompt": {
    "description": "Send a prompt to a herdr agent via socket agent.prompt (NEVER pane.send_text). Prefer herdr_fs_* / herdr_exec when the work is deterministic file/shell IO (no local API burn). Target a cheap/fast worker (pi, flash, …) with a self-contained task; do NOT prompt Claude/OMP/main to plan or to command other panes — the web client owns orchestration. DEFAULT: fire-and-forget (omit wait); confirm with herdr_since / herdr_inspect. Strongly prefer idempotency_key (replays return stored result; never auto-retried). Returns delivery evidence: submitted, before/after, state_observation ({changed:true|false|\"unknown\", fresh}), plus legacy state_changed. Blocked target -> status 'agent_blocked', submitted:false. Optional wait {until, timeout_ms} is submit+wait; a status-wait timeout is failure_phase post_submission_status_wait (not a socket transport failure) — verify before re-sending. Worker invariant: project root == pane cwd == foreground cwd.",
    "targetDescription": "Agent name or pane_id to prompt"
  }
} as const;
