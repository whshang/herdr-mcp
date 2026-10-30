//! Discoverability schemas for the herdr-mcp local/private method surface.
//!
//! The canonical wire names stay owned by the parent `progressive_skills`
//! module; this module owns only the JSON schema projection, its order, and the
//! `query` substring filter.

use super::{
    AGENT_ATTENTION_ADVISE_METHOD, AGENT_CLOSEOUT_ADVISE_METHOD, BILIBILI_VIDEO_TRANSCRIPT_METHOD,
    BROWSER_COMPOSER_SET_APPS_METHOD, BROWSER_COMPOSER_SET_REASONING_METHOD,
    BROWSER_DISPATCH_STATUS_METHOD, BROWSER_DISPATCH_STOP_METHOD, BROWSER_DISPATCH_SUBMIT_METHOD,
    BROWSER_ENDPOINT_INSPECT_METHOD, BROWSER_ENDPOINT_LIST_METHOD, BROWSER_HANDOFF_PREPARE_METHOD,
    BROWSER_MESSAGE_APPEND_METHOD, BROWSER_PAGE_ACTION_METHOD, BROWSER_PAGE_LIFECYCLE_METHOD,
    BROWSER_RESOURCE_INSPECT_METHOD, BROWSER_RESOURCE_LIST_METHOD, BROWSER_RESOURCE_RESOLVE_METHOD,
    BROWSER_SESSION_ARCHIVE_METHOD, BROWSER_SESSION_ARCHIVE_STATUS_METHOD,
    BROWSER_SESSION_CREATE_METHOD, BROWSER_SESSION_INSPECT_METHOD, BROWSER_SESSION_OPEN_METHOD,
    BROWSER_SPACE_CREATE_METHOD, BROWSER_SPACE_INSPECT_METHOD, BROWSER_SPACE_OPEN_METHOD,
    CLEANUP_PREVIEW_METHOD, DOUBAO_IMAGE_GENERATE_METHOD, DOUBAO_IMAGE_STATUS_METHOD,
    EXEC_WAIT_METHOD, GITHUB_STATUS_METHOD, LOCAL_DESCRIBE_METHOD, LOCAL_LIST_METHOD,
    LOCAL_LOAD_METHOD, PLANNING_ADVISE_METHOD, TEXT_READ_METHOD, TEXT_WRITE_METHOD,
    VALIDATION_ADVISE_METHOD, WORK_MEMORY_APPEND_EVIDENCE_METHOD, WORK_MEMORY_APPEND_TURN_METHOD,
    WORK_MEMORY_BIND_METHOD, WORK_MEMORY_CHECKPOINT_PUT_METHOD, WORK_MEMORY_RESUME_METHOD,
    WORK_MEMORY_SEARCH_METHOD, WORK_PICKUP_METHOD, X_SEARCH_POSTS_METHOD, X_THREAD_READ_METHOD,
};
use crate::prompt::{
    AGENT_TASK_ACK_METHOD, AGENT_TASK_DISPATCH_METHOD, AGENT_TASK_INBOX_METHOD,
    AGENT_TASK_REPORT_METHOD, AGENT_TASK_STATUS_METHOD,
};
use serde_json::{Value, json};

pub fn local_method_schemas(query: &str) -> Vec<Value> {
    let schemas = vec![
        json!({
            "method": LOCAL_LIST_METHOD,
            "source": "herdr_mcp_local",
            "params": {
                "properties": {"project_root": {"type": "string"}},
                "required": [],
                "empty": true,
            },
        }),
        json!({
            "method": LOCAL_DESCRIBE_METHOD,
            "source": "herdr_mcp_local",
            "params": {
                "properties": {
                    "id": {"type": "string"},
                    "project_root": {"type": "string"},
                },
                "required": ["id"],
                "empty": false,
            },
        }),
        json!({
            "method": LOCAL_LOAD_METHOD,
            "source": "herdr_mcp_local",
            "params": {
                "properties": {
                    "ids": {"type": "array", "items": {"type": "string"}},
                    "expected_digests": {"type": "object"},
                    "omit_unchanged": {
                        "type": "boolean",
                        "description": "with a matching expected digest, return the entry with unchanged=true instead of content",
                    },
                    "project_root": {"type": "string"},
                },
                "required": ["ids"],
                "empty": false,
            },
        }),
        json!({
            "method": PLANNING_ADVISE_METHOD,
            "source": "herdr_mcp_local",
            "access": "read_only",
            "advisory_only": true,
            "params": {
                "properties": {
                    "deterministic_tool": {"type": "string"},
                    "task_text": {"type": "string", "maxLength": 8192},
                    "project_root": {"type": "string"},
                    "explicit_target": {"type": "string"},
                    "requires_code_edit": {"type": "boolean"},
                    "requires_shell": {"type": "boolean"},
                    "requires_vision": {"type": "boolean"},
                    "minimum_reasoning_tier": {"type": "integer", "minimum": 0, "maximum": 255},
                    "destructive_production_mutation": {"type": "boolean"},
                    "delegates_other_workers": {"type": "boolean"},
                    "supervisor": {
                        "type": "boolean",
                        "description": "true only when the planner designates this task's agent as the one supervisor allowed to re-dispatch workers; workers never re-dispatch",
                    },
                    "independent_units": {"type": "integer", "minimum": 1, "maximum": 64},
                    "ownership_isolated": {"type": "boolean"},
                    "shared_runtime_state": {"type": "boolean"},
                },
                "required": [],
                "empty": true,
            },
        }),
        json!({
            "method": AGENT_CLOSEOUT_ADVISE_METHOD,
            "source": "herdr_mcp_local",
            "access": "read_only",
            "params": {
                "properties": {
                    "target": {"type": "string", "maxLength": 256},
                    "recent_text": {"type": "string", "maxLength": 12000},
                    "agent_status": {"type": "string", "maxLength": 64},
                    "process_running": {"type": "boolean"},
                    "worktree_dirty": {"type": "boolean"},
                    "open_pr": {"type": "boolean"},
                    "running_exec": {"type": "boolean"},
                    "task_owned_resources": {"type": "integer", "minimum": 0, "maximum": 1024},
                },
                "required": ["target", "recent_text"],
                "empty": false,
            },
        }),
        json!({
            "method": AGENT_ATTENTION_ADVISE_METHOD,
            "source": "herdr_mcp_local",
            "access": "read_only",
            "params": {
                "properties": {
                    "children": {"type": "array", "minItems": 1, "maxItems": 16},
                },
                "required": ["children"],
                "empty": false,
            },
        }),
        json!({
            "method": AGENT_TASK_DISPATCH_METHOD,
            "source": "herdr_mcp_local",
            "access": "mutation",
            "description": "Dispatch one task card to an existing agent (target) or to a NEW agent in one call (spawn): create the pane, start the agent, wait for interactive readiness and prompt it without separate pane.split/agent.start calls. spawn and target are mutually exclusive and spawn requires idempotency_key; a replayed key never creates a second pane, tab or agent.",
            "params": {
                "properties": {
                    "target": {"type": "string", "maxLength": 256, "description": "Exact agent/pane or explicit @idle, @kind, @kind:idle selector. Roles choose the first idle, delegation-safe pane in live snapshot order; never broadcast (@all is rejected). Replays retain the original resolved_target and candidate evidence. Omit when spawn is present."},
                    "spawn": {
                        "type": "object",
                        "description": "One-shot spawn-and-dispatch. Validated before any side effect; the whole chain is pane create -> agent.start -> bounded readiness -> normal dispatch, all sharing ONE caller-visible deadline. status/failed_step values are validate, pane_create, agent_start, agent_ready and prompt. A chain that runs out of budget before the prompt returns a resumable checkpoint (resumable:true with phase agent_start or agent_ready and the persisted pane facts); replaying the SAME idempotency_key continues from that pane/agent and prompts once, never creating a second pane, tab or agent. Other failures keep delivery_state not_delivered when the prompt was never sent and report the created pane/agent for reclaim.",
                        "properties": {
                            "kind": {"type": "string", "maxLength": 64, "description": "Required agent kind passed to agent.start (e.g. pi, codex, claude, grok). An unsupported kind fails at failed_step agent_start with the native code."},
                            "name": {"type": "string", "maxLength": 32, "description": "Optional agent name matching [a-z][a-z0-9_-]{0,31}; defaults to kind plus a short stable suffix derived from the idempotency_key."},
                            "workspace_id": {"type": "string", "maxLength": 256, "description": "Target workspace id or unique label. Required unless cwd is inside exactly one live workspace."},
                            "cwd": {"type": "string", "maxLength": 1024, "description": "Absolute cwd inside a managed project root from the live topology; defaults to the workspace checkout path. Validated lexically, never by canonicalizing or running git."},
                            "tab_label": {"type": "string", "maxLength": 64, "description": "Label for the new tab (default path only); defaults to the agent name."},
                            "split_from": {"type": "string", "maxLength": 256, "description": "Optional parent pane id for pane.split. Without it the pane is created as a new tab so the agent gets the full tab width. A horizontal split still respects the readable-width floor and fails with pane_split_too_narrow."},
                            "direction": {"type": "string", "enum": ["right", "down"], "description": "Split direction; requires split_from and defaults to right."},
                            "startup_timeout_ms": {"type": "integer", "minimum": 1, "maximum": 100000, "description": "Budget for the WHOLE one-call chain (pane create + agent.start + readiness + prompt submission), not just readiness. Defaults to 25000 so it fits a WebChat herdr_call without timeout_ms (Edge cuts at 30s; the link never exceeds 60s); the local CLI waits chain + 10s and never exceeds 120s. Every step gets only what is left, and a chain that cannot reach the prompt pauses in a resumable checkpoint instead of holding an in-flight key."}
                        },
                        "required": ["kind"],
                    },
                    "workspace_id": {"type": "string", "maxLength": 256, "description": "Optional workspace scope for role resolution. Not applicable with spawn (use spawn.workspace_id)."},
                    "max_concurrent": {"type": "integer", "minimum": 1, "maximum": 16, "description": "Role selectors only: cap non-terminal tasks for this exact requested_target in the bound parent session, else parent_target, else caller-less global scope. Limit rejection is not_delivered."},
                    "text": {"type": "string", "maxLength": 65536},
                    "parent_target": {"type": "string", "maxLength": 256},
                    "idempotency_key": {"type": "string", "maxLength": 256},
                    "stall_after_ms": {"type": "integer", "minimum": 60000, "maximum": 7200000, "description": "Liveness stall threshold; defaults to 20 minutes. Stalled is non-terminal and never redispatched automatically."},
                    "inject_task_identity": {
                        "type": "boolean",
                        "default": false,
                        "description": "Append a bounded task_id/dispatch_id identity line and completion hint after fingerprinting. Replays never resubmit. Both ids are separate fields; callers must not assume their values are equal.",
                    },
                    "parent_session_ref": {
                        "type": "string",
                        "maxLength": 96,
                        "description": "Exact registered browser session ref. Takes precedence over current_user_message. WebChat requires an already-existing caller-session mapping equal to this ref; never creates a mapping. An unmapped caller gets explicit_parent_session_caller_session_unmapped. Any failed identity, generation, grant or mapping check leaves the task unstamped with no fallback; first-time binding uses the source-turn path without an explicit ref.",
                    },
                    "current_user_message": {
                        "type": "string",
                        "maxLength": 262144,
                        "description": "Ignored for binding when parent_session_ref is supplied. WebChat callers: the exact current user turn text (trimmed, non-empty). When it matches the observed source turn of your ChatGPT conversation, the task is stamped with that conversation's session as parent_session_ref so its session-scoped inbox and wake can see it; binding failure never fails the dispatch and is reported as parent_session {bound:false, reason}.",
                    },
                    "wait": {
                        "type": "object",
                        "description": "A task wait timeout is a successful checkpoint: ok:true, wait.completed:false, wait.outcome:checkpoint, with the current lifecycle/liveness snapshot. Delivery failures remain failures. Re-check status/inbox rather than redispatching.",
                        "properties": {
                            "until": {"type": "array", "items": {"type": "string"}},
                            "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 60000}
                        }
                    }
                },
                "required": ["text", "idempotency_key"],
                "empty": false,
            },
        }),
        json!({
            "method": crate::prompt::decision_gates::CREATE,
            "source": "herdr_mcp_local", "access": "mutation",
            "description": "Create one blocking decision gate on a visible non-terminal task. Bounds are UTF-8 bytes; control characters and duplicate options are rejected. Open gates block ack and completion, including worker_done.",
            "params": {"properties": {
                "task_id": {"type": "string", "minLength": 1, "maxLength": 128},
                "question": {"type": "string", "minLength": 1, "maxLength": 1024},
                "options": {"type": "array", "minItems": 2, "maxItems": 8, "uniqueItems": true, "items": {"type": "string", "minLength": 1, "maxLength": 128}}
            }, "required": ["task_id", "question", "options"], "empty": false}
        }),
        json!({
            "method": crate::prompt::decision_gates::STATUS,
            "source": "herdr_mcp_local", "access": "read_only",
            "params": {"properties": {"gate_id": {"type": "string", "minLength": 1, "maxLength": 128}}, "required": ["gate_id"], "empty": false}
        }),
        json!({
            "method": crate::prompt::decision_gates::CANCEL,
            "source": "herdr_mcp_local", "access": "mutation",
            "description": "Cancel an open gate only from its creating parent scope. Bot approvals never count as owner approval.",
            "params": {"properties": {"gate_id": {"type": "string", "minLength": 1, "maxLength": 128}}, "required": ["gate_id"], "empty": false}
        }),
        json!({
            "method": crate::prompt::decision_gates::RESOLVE,
            "source": "herdr_mcp_local", "access": "mutation",
            "description": "Always rejected from MCP, WebChat, browser and agent paths with gate_resolution_requires_human. Only independent local CLI agent gate resolve with a controlling interactive TTY and exact retyped option may resolve. Best-effort local attestation, not cryptographic proof.",
            "params": {"properties": {
                "gate_id": {"type": "string", "minLength": 1, "maxLength": 128},
                "resolution": {"type": "string", "minLength": 1, "maxLength": 128}
            }, "required": ["gate_id", "resolution"], "empty": false}
        }),
        json!({
            "method": AGENT_TASK_REPORT_METHOD,
            "source": "herdr_mcp_local",
            "access": "mutation",
            "description": "Trusted local IPC only; never available to WebChat callers. Matching task_id and dispatch_id are required. worker_done is durable and accepted exactly once; a late report preserves settled lifecycle and never re-notifies the parent. Heartbeats only refresh timestamps of non-terminal tasks. Summary is capped at 2048 UTF-8 bytes after control characters are stripped.",
            "params": {
                "properties": {
                    "task_id": {"type": "string", "minLength": 1, "maxLength": 128},
                    "dispatch_id": {"type": "string", "minLength": 1, "maxLength": 128},
                    "kind": {"type": "string", "enum": ["heartbeat", "worker_done"]},
                    "summary": {"type": "string", "maxLength": 2048}
                },
                "required": ["task_id", "dispatch_id", "kind"],
                "empty": false
            }
        }),
        json!({
            "method": AGENT_TASK_STATUS_METHOD,
            "description": "Durable lifecycle and derived liveness (active or stalled). Stalled is non-terminal; no automatic redispatch.",
            "source": "herdr_mcp_local",
            "access": "read_only",
            "params": {
                "properties": {
                    "task_id": {"type": "string", "maxLength": 128},
                    "idempotency_key": {"type": "string", "maxLength": 256},
                    "target": {"type": "string", "maxLength": 256},
                },
                "required": [],
                "empty": false,
            },
        }),
        json!({
            "method": AGENT_TASK_INBOX_METHOD,
            "source": "herdr_mcp_local",
            "access": "read_only",
            "params": {
                "properties": {
                    "workspace_id": {"type": "string", "maxLength": 128},
                    "parent_target": {"type": "string", "maxLength": 256},
                    "parent_session_ref": {"type": "string", "maxLength": 256},
                    "include_acknowledged": {"type": "boolean"},
                    "advisory": {"type": "boolean"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 512},
                    "wait_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 60000,
                        "description": "Optional bounded long-poll budget. Returns as soon as an unacknowledged terminal task is in scope; deterministic task facts only, no advisory or semantic work."
                    },
                    "view": {
                        "type": "string",
                        "enum": ["full", "compact"],
                        "description": "Optional task projection; compact keeps ids, target, pane, lifecycle, liveness, terminal/acknowledged flags, timestamps, the awaiting_children hold fact and the bounded final_excerpt when captured. final_excerpt is omitted, not degraded, when the child pane is narrower than 40 columns: a TUI agent hard-wraps its own layout there, so the only readable options are reading the pane or giving the agent a wider pane (a new tab, a reused wide pane, or a down split)."
                    },
                },
                "required": [],
                "empty": true,
            },
        }),
        json!({
            "method": AGENT_TASK_ACK_METHOD,
            "source": "herdr_mcp_local",
            "access": "mutation",
            "params": {
                "properties": {
                    "task_id": {"type": "string", "maxLength": 128},
                    "task_ids": {
                        "type": "array",
                        "items": {"type": "string", "maxLength": 128},
                        "minItems": 1,
                        "maxItems": 32,
                        "uniqueItems": true,
                        "description": "Optional batch alternative to task_id; out-of-scope or unknown ids are reported as not_found."
                    },
                },
                "required": [],
                "empty": false,
            },
        }),
        json!({
            "method": VALIDATION_ADVISE_METHOD,
            "source": "herdr_mcp_local",
            "access": "read_only",
            "params": {
                "properties": {
                    "project_root": {"type": "string", "maxLength": 1024},
                    "summary": {"type": "string", "maxLength": 4096},
                    "changed_files": {"type": "array", "maxItems": 64},
                    "changed_symbols": {"type": "array", "maxItems": 64},
                    "candidate_checks": {"type": "array", "maxItems": 32},
                },
                "required": ["changed_files"],
                "empty": false,
            },
        }),
        json!({
            "method": GITHUB_STATUS_METHOD,
            "source": "herdr_mcp_local",
            "params": {
                "properties": {
                    "project_root": {"type": "string"},
                    "repository": {"type": "string"},
                    "pr_number": {"type": "integer", "minimum": 1},
                    "previous_fingerprint": {"type": "string"},
                    "wait_ms": {"type": "integer", "minimum": 1, "maximum": 20000},
                },
                "required": ["project_root"],
                "empty": false,
            },
        }),
        json!({
            "method": CLEANUP_PREVIEW_METHOD,
            "source": "herdr_mcp_local",
            "params": {
                "properties": {
                    "project_root": {"type": "string"},
                    "target_ref": {"type": "string"},
                    "advisory": {"type": "boolean"},
                },
                "required": ["project_root"],
                "empty": false,
            },
        }),
        json!({
            "method": EXEC_WAIT_METHOD,
            "source": "herdr_mcp_local",
            "params": {
                "properties": {
                    "session_id": {"type": "string"},
                    "stream": {"type": "string", "enum": ["stdout", "stderr", "both"]},
                    "offset": {"type": "integer", "minimum": 0, "maximum": 9007199254740991_i64},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 262144},
                    "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 20000},
                },
                "required": ["session_id"],
                "empty": false,
            },
        }),
        json!({
            "method": TEXT_READ_METHOD,
            "source": "herdr_mcp_local",
            "params": {
                "properties": {
                    "path": {"type": "string"},
                    "max_bytes": {"type": "integer", "minimum": 1, "maximum": 262144},
                },
                "required": ["path"],
                "empty": false,
            },
        }),
        json!({
            "method": TEXT_WRITE_METHOD,
            "source": "herdr_mcp_local",
            "params": {
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"},
                    "sha256": {"type": "string"},
                    "overwrite": {"type": "boolean"},
                    "backup": {"type": "boolean"},
                },
                "required": ["path", "content", "sha256"],
                "empty": false,
            },
        }),
        json!({
            "method": WORK_PICKUP_METHOD,
            "source": "herdr_mcp_local",
            "access": "read_only",
            "params": {
                "properties": {
                    "worktree": {"type": "string", "maxLength": 4096},
                    "project_root": {"type": "string", "maxLength": 4096},
                    "project_ref": {"type": "string", "maxLength": 512},
                    "cwd": {"type": "string", "maxLength": 4096},
                    "pane": {"type": "string", "maxLength": 256},
                },
                "required": [],
                "description": "Pass worktree or project_root (or cwd/pane to resolve the caller's project); with no selector, lists projects that have handoffs.",
            },
        }),
        json!({
            "method": WORK_MEMORY_BIND_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "params": {
                "properties": {
                    "continuity_id": {"type": "string", "maxLength": 160},
                    "project_ref": {"type": "string", "maxLength": 512},
                    "repo_id": {"type": "string", "maxLength": 512},
                    "work_chain_id": {"type": "string", "maxLength": 128},
                    "provider": {"type": "string", "maxLength": 32},
                    "account_ref": {"type": ["string", "null"], "maxLength": 256},
                    "space_ref": {"type": ["string", "null"], "maxLength": 512},
                    "session_ref": {"type": "string", "maxLength": 512},
                    "bound_at": {"type": "integer", "minimum": 0},
                },
                "required": ["continuity_id", "project_ref", "repo_id", "work_chain_id", "provider", "session_ref", "bound_at"],
                "empty": false,
            },
        }),
        json!({
            "method": WORK_MEMORY_APPEND_TURN_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "params": {
                "properties": {
                    "continuity_id": {"type": "string", "maxLength": 160},
                    "provider": {"type": "string", "maxLength": 32},
                    "account_ref": {"type": ["string", "null"], "maxLength": 256},
                    "space_ref": {"type": ["string", "null"], "maxLength": 512},
                    "session_ref": {"type": "string", "maxLength": 512},
                    "provider_message_ref": {"type": "string", "maxLength": 512},
                    "role": {"type": "string", "maxLength": 32},
                    "text": {"type": "string", "maxLength": 262144},
                    "fingerprint": {"type": ["string", "null"], "maxLength": 256},
                    "observed_at": {"type": "integer", "minimum": 0},
                },
                "required": ["continuity_id", "provider", "session_ref", "provider_message_ref", "role", "text", "observed_at"],
                "empty": false,
            },
        }),
        json!({
            "method": WORK_MEMORY_APPEND_EVIDENCE_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "params": {
                "properties": {
                    "continuity_id": {"type": "string", "maxLength": 160},
                    "kind": {"type": "string", "maxLength": 32},
                    "content": {"type": "string", "maxLength": 262144},
                    "provider": {"type": ["string", "null"], "maxLength": 32},
                    "account_ref": {"type": ["string", "null"], "maxLength": 256},
                    "space_ref": {"type": ["string", "null"], "maxLength": 512},
                    "session_ref": {"type": ["string", "null"], "maxLength": 512},
                    "portable_source": {
                        "type": ["object", "null"],
                        "properties": {
                            "repo_id": {"type": "string", "maxLength": 512},
                            "commit_sha": {"type": "string", "maxLength": 64},
                            "repo_relative_path": {"type": "string", "maxLength": 1024},
                            "line_start": {"type": ["integer", "null"]},
                            "line_end": {"type": ["integer", "null"]},
                        },
                        "required": ["repo_id", "commit_sha", "repo_relative_path"],
                        "additionalProperties": false,
                    },
                    "created_at": {"type": "integer", "minimum": 0},
                },
                "required": ["continuity_id", "kind", "content", "created_at"],
                "empty": false,
            },
        }),
        json!({
            "method": WORK_MEMORY_CHECKPOINT_PUT_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "params": {
                "properties": {
                    "continuity_id": {"type": "string", "maxLength": 160},
                    "expected_checkpoint_revision": {"type": "integer", "minimum": 0},
                    "summary": {"type": "string", "maxLength": 8192},
                    "checkpoint_json": {"type": "string", "maxLength": 65536},
                    "through_message_id": {"type": ["string", "null"], "maxLength": 512},
                    "through_evidence_id": {"type": ["string", "null"], "maxLength": 128},
                    "created_at": {"type": "integer", "minimum": 0},
                },
                "required": ["continuity_id", "expected_checkpoint_revision", "summary", "checkpoint_json", "created_at"],
                "anyOf": [
                    {
                        "properties": {
                            "through_message_id": {"type": "string", "minLength": 1, "maxLength": 512},
                        },
                        "required": ["through_message_id"],
                    },
                    {
                        "properties": {
                            "through_evidence_id": {"type": "string", "minLength": 1, "maxLength": 128},
                        },
                        "required": ["through_evidence_id"],
                    },
                ],
                "empty": false,
            },
        }),
        json!({
            "method": WORK_MEMORY_RESUME_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "params": {
                "properties": {
                    "project_ref": {"type": "string", "maxLength": 512},
                    "repo_id": {"type": "string", "maxLength": 512},
                    "work_chain_id": {"type": "string", "maxLength": 128},
                    "max_turns": {"type": "integer", "minimum": 1, "maximum": 64},
                },
                "required": ["project_ref", "repo_id", "work_chain_id"],
                "empty": false,
            },
        }),
        json!({
            "method": WORK_MEMORY_SEARCH_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 2,
            "params": {
                "properties": {
                    "project_ref": {"type": "string", "maxLength": 512},
                    "repo_id": {"type": "string", "maxLength": 512},
                    "work_chain_id": {"type": "string", "maxLength": 128},
                    "query": {"type": "string", "maxLength": 512},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 20},
                    "cursor": {"type": "string", "maxLength": 4096},
                },
                "required": [],
                "oneOf": [
                    {"required": ["project_ref", "repo_id", "work_chain_id", "query"]},
                    {"required": ["cursor"]},
                ],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_ENDPOINT_LIST_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "read_only",
            "params": {
                "properties": {
                    "limit": {"type": "integer", "minimum": 1, "maximum": 64},
                },
                "required": [],
                "empty": true,
            },
        }),
        json!({
            "method": BROWSER_ENDPOINT_INSPECT_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "read_only",
            "params": {
                "properties": {
                    "endpoint_ref": {"type": "string", "maxLength": 96},
                },
                "required": ["endpoint_ref"],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_RESOURCE_LIST_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "read_only",
            "params": {
                "properties": {
                    "endpoint_ref": {"type": ["string", "null"], "maxLength": 96},
                    "provider": {"type": ["string", "null"], "maxLength": 32},
                    "kind": {"type": ["string", "null"], "enum": ["account", "space", "session", null]},
                    "parent_ref": {"type": ["string", "null"], "maxLength": 96},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 64},
                },
                "required": [],
                "empty": true,
            },
        }),
        json!({
            "method": BROWSER_RESOURCE_INSPECT_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "read_only",
            "params": {
                "properties": {
                    "resource_ref": {"type": "string", "maxLength": 96},
                },
                "required": ["resource_ref"],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_RESOURCE_RESOLVE_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "read_only",
            "params": {
                "properties": {
                    "endpoint_ref": {"type": "string", "maxLength": 96},
                    "provider": {"type": "string", "maxLength": 32},
                    "kind": {"type": "string", "enum": ["account", "space", "session"]},
                    "parent_ref": {"type": ["string", "null"], "maxLength": 96},
                    "display_label": {"type": ["string", "null"], "maxLength": 256},
                    "expected_observation_generation": {"type": ["integer", "null"], "minimum": 1},
                },
                "required": ["endpoint_ref", "provider", "kind"],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_SPACE_CREATE_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "mutation",
            "params": {
                "properties": {
                    "endpoint_ref": {"type": "string", "maxLength": 96},
                    "provider": {"type": "string", "maxLength": 32},
                    "account_ref": {"type": "string", "maxLength": 96},
                    "display_label": {"type": "string", "maxLength": 256},
                    "expected_generation": {"type": "integer", "minimum": 1},
                    "idempotency_key": {"type": "string", "maxLength": 256},
                },
                "required": ["endpoint_ref", "provider", "account_ref", "display_label", "expected_generation", "idempotency_key"],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_SPACE_OPEN_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "mutation",
            "params": {
                "properties": {
                    "space_ref": {"type": "string", "maxLength": 96},
                    "expected_generation": {"type": "integer", "minimum": 1},
                    "idempotency_key": {"type": "string", "maxLength": 256},
                },
                "required": ["space_ref", "expected_generation", "idempotency_key"],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_SPACE_INSPECT_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "read_only",
            "params": {
                "properties": {
                    "space_ref": {"type": ["string", "null"], "maxLength": 96},
                    "endpoint_ref": {"type": ["string", "null"], "maxLength": 96},
                    "provider": {"type": ["string", "null"], "maxLength": 32},
                    "account_ref": {"type": ["string", "null"], "maxLength": 96},
                    "display_label": {"type": ["string", "null"], "maxLength": 256},
                },
                "required": [],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_SESSION_CREATE_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "mutation",
            "params": {
                "properties": {
                    "source_url": {"type": ["string", "null"], "maxLength": 2048},
                    "source_session_ref": {
                        "type": "string",
                        "maxLength": 96,
                        "description": "Optional exact observed source session for source_url ambiguity across accounts; URL and resource generation must match."
                    },
                    "endpoint_ref": {"type": "string", "maxLength": 96},
                    "provider": {"type": "string", "maxLength": 32},
                    "account_ref": {"type": "string", "maxLength": 96},
                    "space_ref": {"type": ["string", "null"], "maxLength": 96},
                    "display_label": {"type": "string", "maxLength": 256},
                    "message": {"type": "string", "maxLength": 262144},
                    "reasoning_effort": {"type": ["string", "null"], "enum": ["economy", "balanced", "thorough", null]},
                    "required_apps": {"type": ["array", "null"], "maxItems": 32, "items": {"type": "string", "maxLength": 64}},
                    "expected_generation": {"type": "integer", "minimum": 1},
                    "idempotency_key": {"type": "string", "maxLength": 256},
                    "work_chain_id": {"type": ["string", "null"], "maxLength": 128},
                    "lane_id": {"type": ["string", "null"], "maxLength": 160},
                },
                "required": ["message", "idempotency_key"],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_SESSION_OPEN_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "mutation",
            "params": {
                "properties": {
                    "session_ref": {"type": "string", "maxLength": 96},
                    "expected_generation": {"type": "integer", "minimum": 1},
                    "idempotency_key": {"type": "string", "maxLength": 256},
                },
                "required": ["session_ref", "expected_generation", "idempotency_key"],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_SESSION_ARCHIVE_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "mutation",
            "params": {
                "properties": {
                    "session_ref": {"type": ["string", "null"], "maxLength": 96},
                    "current_user_message": {"type": ["string", "null"], "maxLength": 262144},
                    "expected_generation": {"type": ["integer", "null"], "minimum": 1},
                    "idempotency_key": {"type": "string", "maxLength": 256},
                },
                "required": ["idempotency_key"],
                "oneOf": [
                    {"required": ["session_ref", "expected_generation"]},
                    {"required": ["current_user_message"]},
                ],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_SESSION_ARCHIVE_STATUS_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "read_only",
            "params": {
                "properties": {
                    "session_ref": {"type": "string", "maxLength": 96},
                    "expected_generation": {"type": "integer", "minimum": 1},
                },
                "required": ["session_ref", "expected_generation"],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_SESSION_INSPECT_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "read_only",
            "params": {
                "properties": {
                    "session_ref": {"type": "string", "maxLength": 96},
                },
                "required": ["session_ref"],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_MESSAGE_APPEND_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "mutation",
            "params": {
                "properties": {
                    "session_ref": {"type": "string", "maxLength": 96},
                    "message": {"type": "string", "maxLength": 262144},
                    "expected_generation": {"type": "integer", "minimum": 1},
                    "idempotency_key": {"type": "string", "maxLength": 256},
                },
                "required": ["session_ref", "message", "expected_generation", "idempotency_key"],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_COMPOSER_SET_REASONING_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "mutation",
            "params": {
                "properties": {
                    "session_ref": {"type": "string", "maxLength": 96},
                    "reasoning_effort": {"type": "string", "enum": ["economy", "balanced", "thorough"]},
                    "expected_generation": {"type": "integer", "minimum": 1},
                    "idempotency_key": {"type": "string", "maxLength": 256},
                },
                "required": ["session_ref", "reasoning_effort", "expected_generation", "idempotency_key"],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_COMPOSER_SET_APPS_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "mutation",
            "params": {
                "properties": {
                    "session_ref": {"type": "string", "maxLength": 96},
                    "required_apps": {"type": "array", "maxItems": 32, "items": {"type": "string", "maxLength": 64}},
                    "expected_generation": {"type": "integer", "minimum": 1},
                    "idempotency_key": {"type": "string", "maxLength": 256},
                },
                "required": ["session_ref", "required_apps", "expected_generation", "idempotency_key"],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_DISPATCH_SUBMIT_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "mutation",
            "params": {
                "properties": {
                    "session_ref": {"type": "string", "maxLength": 96},
                    "message": {"type": "string", "maxLength": 262144},
                    "reasoning_effort": {"type": ["string", "null"], "enum": ["economy", "balanced", "thorough", null]},
                    "required_apps": {"type": ["array", "null"], "maxItems": 32, "items": {"type": "string", "maxLength": 64}},
                    "expected_generation": {"type": "integer", "minimum": 1},
                    "idempotency_key": {"type": "string", "maxLength": 256},
                    "work_chain_id": {"type": ["string", "null"], "maxLength": 128},
                    "lane_id": {"type": ["string", "null"], "maxLength": 160},
                },
                "required": ["session_ref", "message", "expected_generation", "idempotency_key"],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_DISPATCH_STATUS_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "read_only",
            "params": {
                "properties": {
                    "dispatch_id": {"type": "string", "maxLength": 96},
                },
                "required": ["dispatch_id"],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_DISPATCH_STOP_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "mutation",
            "params": {
                "properties": {
                    "dispatch_id": {"type": "string", "maxLength": 96},
                    "expected_generation": {"type": "integer", "minimum": 1},
                    "idempotency_key": {"type": "string", "maxLength": 256},
                },
                "required": ["dispatch_id", "expected_generation", "idempotency_key"],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_PAGE_LIFECYCLE_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "mutation",
            "params": {
                "properties": {
                    "endpoint_ref": {"type": "string", "maxLength": 96},
                    "action": {"type": "string", "enum": ["open", "claim", "release", "finalize"]},
                    "target_origin": {"type": ["string", "null"], "maxLength": 4096},
                    "url": {"type": ["string", "null"], "maxLength": 4096},
                    "page_ref": {"type": ["string", "null"], "maxLength": 67},
                    "idempotency_key": {"type": ["string", "null"], "maxLength": 256},
                },
                "required": ["endpoint_ref", "action"],
                "oneOf": [
                    {
                        "properties": {"action": {"enum": ["open", "claim"]}},
                        "required": ["target_origin", "url", "idempotency_key"],
                    },
                    {
                        "properties": {"action": {"enum": ["release", "finalize"]}},
                        "required": ["page_ref"],
                    },
                ],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_PAGE_ACTION_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "mutation",
            "params": {
                "properties": {
                    "endpoint_ref": {"type": "string", "maxLength": 96},
                    "page_ref": {"type": "string", "maxLength": 67},
                    "action": {"type": "string",  "enum": ["observe", "click", "hover", "fill", "expect", "screenshot", "scroll", "extract", "extract_table", "press", "navigate", "reload", "back", "forward"]},
                    "max_chars": {"type": ["integer", "null"], "minimum": 1, "maximum": 100000},
                    "element_offset": {"type": ["integer", "null"], "minimum": 0, "maximum": 4096, "description": "observe only: skip this many eligible, visible controls before building refs. A response still holds at most 128 refs and reports next_element_offset (null at the end). Each observation mints a new generation, so page, re-observe, then act."},
                    "generation": {"type": ["string", "null"], "maxLength": 256},
                    "ref": {"type": ["string", "null"], "maxLength": 256},
                    "value": {"type": ["string", "null"], "maxLength": 10000},
                    "condition": {"type": ["string", "null"], "enum": ["document_ready", "url_equals", "text_present", "text_absent", "element_present", "element_absent", "input_value_equals", "input_value_contains", "element_checked", "element_selected", "element_class_contains", "element_visible", "element_enabled", null]},
                    "timeout_ms": {"type": ["integer", "null"], "minimum": 0, "maximum": 5000},
                    "idempotency_key": {"type": ["string", "null"], "maxLength": 256},
                    "direction": {"type": ["string", "null"], "enum": ["up", "down", null]},
                    "amount": {"type": ["integer", "null"], "minimum": 1, "maximum": 1500},
                    "max_rows": {"type": ["integer", "null"], "minimum": 1, "maximum": 100},
                    "expected_url": {"type": "string", "maxLength": 4096, "description": "Click/back/forward only: explicit absolute same-origin destination to verify after a navigation, including a history move. Never auto-retry an unverified navigation."},
                    "url": {"type": "string", "maxLength": 4096, "description": "navigate only: explicit absolute same-origin destination URL on an owned BrowserPage. Reuses the mutation ledger and advances the page generation."},
                    "page_generation": {"type": ["integer", "null"], "minimum": 1, "description": "navigate/reload/back/forward only: current page_generation from the page view; the page record advances it after a verified navigation."},
                    "key": {"type": ["string", "null"], "enum": ["Enter", "Escape", "Tab", "Backspace", "Delete", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End", "PageUp", "PageDown", "Space", null]},
                    "objective": {"type": ["string", "null"], "maxLength": 1024},
                },
                "required": ["endpoint_ref", "page_ref", "action"],
                "oneOf": [
                    {
                        "properties": {"action": {"enum": ["observe"]}},
                    },
                    {
                        "properties": {"action": {"enum": ["click"]}},
                        "required": ["generation", "ref", "idempotency_key"],
                    },
                    {
                        "properties": {"action": {"enum": ["hover"]}},
                        "required": ["generation", "ref", "idempotency_key"],
                    },
                    {
                        "properties": {"action": {"enum": ["fill"]}},
                        "required": ["generation", "ref", "value", "idempotency_key"],
                    },
                    {
                        "properties": {"action": {"enum": ["expect"]}},
                        "required": ["condition"],
                        "oneOf": [
                            {"properties": {"condition": {"enum": ["document_ready"]}}},
                            {"properties": {"condition": {"enum": ["url_equals", "text_present", "text_absent", "element_present", "element_absent"]}}, "required": ["value"]},
                            {"properties": {"condition": {"enum": ["input_value_equals", "input_value_contains", "element_class_contains"]}}, "required": ["generation", "ref", "value"]},
                            {"properties": {"condition": {"enum": ["element_checked", "element_selected", "element_visible", "element_enabled"]}}, "required": ["generation", "ref"]}
                        ],
                    },
                    {
                        "properties": {"action": {"enum": ["screenshot"]}},
                    },
                    { "properties": {"action": {"enum": ["scroll"]}}, "required": ["generation", "direction", "idempotency_key"] },
                    { "properties": {"action": {"enum": ["extract", "extract_table"]}}, "required": ["generation", "ref"] },
                    { "properties": {"action": {"enum": ["press"]}}, "required": ["generation", "key", "idempotency_key"]
                    },
                    {"properties": {"action": {"enum": ["navigate"]}}, "required": ["page_generation", "url", "idempotency_key"]},
                    {"properties": {"action": {"enum": ["reload"]}}, "required": ["page_generation", "idempotency_key"]},
                    {"properties": {"action": {"enum": ["back", "forward"]}}, "required": ["page_generation", "expected_url", "idempotency_key"]},
                ],
                "empty": false,
            },
        }),
        json!({
            "method": BROWSER_HANDOFF_PREPARE_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "read_only",
            "params": {
                "properties": {
                    "continuity_id": {"type": "string", "maxLength": 160},
                    "source_url": {"type": "string", "maxLength": 2048},
                    "objective": {"type": ["string", "null"], "maxLength": 1024},
                    "work_chain_id": {"type": ["string", "null"], "maxLength": 128},
                    "handoff_id": {"type": ["string", "null"], "maxLength": 96}
                },
                "required": ["continuity_id", "source_url"],
                "empty": false
            }
        }),
        json!({
            "method": crate::codex_history::LIST_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "read_only",
            "params": {
                "properties": {
                    "project_root": {"type": "string", "maxLength": 4096},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100}
                },
                "required": ["project_root"],
                "empty": false
            }
        }),
        json!({
            "method": crate::codex_history::READ_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "read_only",
            "params": {
                "properties": {
                    "project_root": {"type": "string", "maxLength": 4096},
                    "session_id": {"type": "string", "maxLength": 128, "description": "Optional. Omit to select the preferred session for the exact project_root."},
                    "max_messages": {"type": "integer", "minimum": 1, "maximum": 200},
                    "max_chars": {"type": "integer", "minimum": 1, "maximum": 20000}
                },
                "required": ["project_root"],
                "empty": false
            }
        }),
        json!({
            "method": crate::codex_history::RESUME_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "mutation",
            "params": {
                "properties": {
                    "project_root": {"type": "string", "maxLength": 4096},
                    "session_id": {"type": "string", "maxLength": 128, "description": "Optional. Omit to select the preferred session for the exact project_root."},
                    "pane_id": {"type": "string", "maxLength": 256},
                    "name": {"type": "string", "maxLength": 64},
                    "timeout_ms": {"type": "integer", "minimum": 5000, "maximum": 55000}
                },
                "required": ["project_root", "pane_id", "name"],
                "empty": false
            }
        }),
        json!({
            "method": BILIBILI_VIDEO_TRANSCRIPT_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "read_only",
            "params": {
                "properties": {
                    "endpoint_ref": {"type": "string", "maxLength": 96},
                    "page_ref": {"type": "string", "maxLength": 67},
                    "offset": {"type": ["integer", "null"], "minimum": 0, "maximum": 1000000},
                    "limit": {"type": ["integer", "null"], "minimum": 1, "maximum": 100},
                    "language": {"type": ["string", "null"], "maxLength": 32}
                },
                "required": ["endpoint_ref", "page_ref"],
                "empty": false
            }
        }),
        json!({
            "method": X_SEARCH_POSTS_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "read_only",
            "params": {
                "properties": {
                    "endpoint_ref": {"type": "string", "maxLength": 96},
                    "page_ref": {"type": "string", "maxLength": 67},
                    "query": {"type": "string", "maxLength": 512},
                    "limit": {"type": ["integer", "null"], "minimum": 1, "maximum": 50},
                    "cursor": {"type": ["string", "null"], "maxLength": 4096}
                },
                "required": ["endpoint_ref", "page_ref", "query"],
                "empty": false
            }
        }),
        json!({
            "method": X_THREAD_READ_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "read_only",
            "params": {
                "properties": {
                    "endpoint_ref": {"type": "string", "maxLength": 96},
                    "page_ref": {"type": "string", "maxLength": 67},
                    "post_id": {"type": "string", "pattern": "^[0-9]{1,32}$"},
                    "limit": {"type": ["integer", "null"], "minimum": 1, "maximum": 50},
                    "cursor": {"type": ["string", "null"], "maxLength": 4096}
                },
                "required": ["endpoint_ref", "page_ref", "post_id"],
                "empty": false
            }
        }),
        json!({
            "method": DOUBAO_IMAGE_GENERATE_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "mutation",
            "params": {
                "properties": {
                    "endpoint_ref": {"type": "string", "maxLength": 96},
                    "page_ref": {"type": "string", "maxLength": 67},
                    "prompt": {"type": "string", "maxLength": 4000},
                    "idempotency_key": {"type": "string", "maxLength": 256}
                },
                "required": ["endpoint_ref", "page_ref", "prompt", "idempotency_key"],
                "empty": false
            }
        }),
        json!({
            "method": DOUBAO_IMAGE_STATUS_METHOD,
            "source": "herdr_mcp_local",
            "schema_version": 1,
            "access": "read_only",
            "params": {
                "properties": {
                    "endpoint_ref": {"type": "string", "maxLength": 96},
                    "op_id": {"type": "string", "maxLength": 96}
                },
                "required": ["endpoint_ref", "op_id"],
                "empty": false
            }
        }),
    ];
    let query = query.trim().to_ascii_lowercase();
    if query.is_empty() {
        return schemas;
    }
    schemas
        .into_iter()
        .filter(|schema| {
            schema
                .get("method")
                .and_then(Value::as_str)
                .is_some_and(|method| method.to_ascii_lowercase().contains(&query))
        })
        .collect()
}
