//! Discoverability schemas for the herdr-mcp local/private method surface.
//!
//! The canonical wire names stay owned by the parent `progressive_skills`
//! module; this module owns only the JSON schema projection, its order, and the
//! `query` substring filter.

use super::{
    AGENT_ATTENTION_ADVISE_METHOD, AGENT_CLOSEOUT_ADVISE_METHOD, BROWSER_COMPOSER_SET_APPS_METHOD,
    BROWSER_COMPOSER_SET_REASONING_METHOD, BROWSER_DISPATCH_STATUS_METHOD,
    BROWSER_DISPATCH_STOP_METHOD, BROWSER_DISPATCH_SUBMIT_METHOD, BROWSER_ENDPOINT_INSPECT_METHOD,
    BROWSER_ENDPOINT_LIST_METHOD, BROWSER_HANDOFF_PREPARE_METHOD, BROWSER_MESSAGE_APPEND_METHOD,
    BROWSER_RESOURCE_INSPECT_METHOD, BROWSER_RESOURCE_LIST_METHOD, BROWSER_RESOURCE_RESOLVE_METHOD,
    BROWSER_SESSION_ARCHIVE_METHOD, BROWSER_SESSION_ARCHIVE_STATUS_METHOD,
    BROWSER_SESSION_CREATE_METHOD, BROWSER_SESSION_INSPECT_METHOD, BROWSER_SESSION_OPEN_METHOD,
    BROWSER_SPACE_CREATE_METHOD, BROWSER_SPACE_INSPECT_METHOD, BROWSER_SPACE_OPEN_METHOD,
    CLEANUP_PREVIEW_METHOD, EXEC_WAIT_METHOD, GITHUB_STATUS_METHOD, LOCAL_DESCRIBE_METHOD,
    LOCAL_LIST_METHOD, LOCAL_LOAD_METHOD, PLANNING_ADVISE_METHOD, TEXT_READ_METHOD,
    TEXT_WRITE_METHOD, VALIDATION_ADVISE_METHOD, WORK_MEMORY_APPEND_EVIDENCE_METHOD,
    WORK_MEMORY_APPEND_TURN_METHOD, WORK_MEMORY_BIND_METHOD, WORK_MEMORY_CHECKPOINT_PUT_METHOD,
    WORK_MEMORY_RESUME_METHOD, WORK_MEMORY_SEARCH_METHOD,
};
use crate::prompt::{
    AGENT_TASK_ACK_METHOD, AGENT_TASK_DISPATCH_METHOD, AGENT_TASK_INBOX_METHOD,
    AGENT_TASK_STATUS_METHOD,
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
            "params": {
                "properties": {
                    "target": {"type": "string", "maxLength": 256},
                    "text": {"type": "string", "maxLength": 65536},
                    "parent_target": {"type": "string", "maxLength": 256},
                    "idempotency_key": {"type": "string", "maxLength": 256},
                    "wait": {
                        "type": "object",
                        "properties": {
                            "until": {"type": "array", "items": {"type": "string"}},
                            "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 60000}
                        }
                    }
                },
                "required": ["target", "text", "parent_target", "idempotency_key"],
                "empty": false,
            },
        }),
        json!({
            "method": AGENT_TASK_STATUS_METHOD,
            "source": "herdr_mcp_local",
            "access": "read_only",
            "params": {
                "properties": {
                    "task_id": {"type": "string", "maxLength": 128},
                },
                "required": ["task_id"],
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
                },
                "required": ["task_id"],
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
