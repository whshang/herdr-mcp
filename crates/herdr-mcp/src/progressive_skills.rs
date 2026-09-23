use crate::agent_visibility::AgentVisibility;
use crate::capability_inventory::{AgentCapabilityRecord, CapabilityInventoryStore};
use crate::capability_resolver::{WorkerCapability, project_capabilities_with_inventory};
use crate::local_skills::{self, LocalSkillFile, parse_frontmatter, read_file_bounded};
use crate::paths::RuntimePaths;
use crate::prompt::{
    AGENT_TASK_ACK_METHOD, AGENT_TASK_DISPATCH_METHOD, AGENT_TASK_INBOX_METHOD,
    AGENT_TASK_STATUS_METHOD,
};
use crate::semantic::{
    DEFAULT_DECISION_THRESHOLD, SemanticAnswer, SemanticQuestion, SemanticRequest, SemanticService,
};
use crate::skill_dispatch::{DispatchAdvice, TaskProfile, advise_dispatch};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

// Canonical wire names for herdr-mcp private/local methods; this block is the
// single owner of each name. The MCP router (`crate::mcp`), the local-agent CLI
// (`crate::local_agent_cli`) and the discoverability schema below reference
// these constants instead of maintaining independent copies of the wire names.
pub const LOCAL_LIST_METHOD: &str = "herdr_mcp.skill.list";
pub const LOCAL_DESCRIBE_METHOD: &str = "herdr_mcp.skill.describe";
pub const LOCAL_LOAD_METHOD: &str = "herdr_mcp.skill.load";
pub const PLANNING_ADVISE_METHOD: &str = "herdr_mcp.planning.advise";
pub const AGENT_CLOSEOUT_ADVISE_METHOD: &str = "herdr_mcp.agent.closeout.advise";
pub const AGENT_ATTENTION_ADVISE_METHOD: &str = "herdr_mcp.agent.attention.advise";
pub const VALIDATION_ADVISE_METHOD: &str = "herdr_mcp.validation.advise";
pub const GITHUB_STATUS_METHOD: &str = "herdr_mcp.github.status";
pub const CLEANUP_PREVIEW_METHOD: &str = "herdr_mcp.cleanup.preview";
pub const EXEC_WAIT_METHOD: &str = "herdr_mcp.exec.wait";
pub const TEXT_READ_METHOD: &str = "herdr_mcp.text.read";
pub const TEXT_WRITE_METHOD: &str = "herdr_mcp.text.write";
pub const WORK_MEMORY_BIND_METHOD: &str = "work_memory.bind";
pub const WORK_MEMORY_APPEND_TURN_METHOD: &str = "work_memory.append_turn";
pub const WORK_MEMORY_APPEND_EVIDENCE_METHOD: &str = "work_memory.append_evidence";
pub const WORK_MEMORY_CHECKPOINT_PUT_METHOD: &str = "work_memory.checkpoint.put";
pub const WORK_MEMORY_RESUME_METHOD: &str = "work_memory.resume";
pub const WORK_MEMORY_SEARCH_METHOD: &str = "work_memory.search";
pub const BROWSER_ENDPOINT_LIST_METHOD: &str = "herdr_mcp.browser_endpoint.list";
pub const BROWSER_ENDPOINT_INSPECT_METHOD: &str = "herdr_mcp.browser_endpoint.inspect";
pub const BROWSER_RESOURCE_LIST_METHOD: &str = "herdr_mcp.browser_resource.list";
pub const BROWSER_RESOURCE_INSPECT_METHOD: &str = "herdr_mcp.browser_resource.inspect";
pub const BROWSER_RESOURCE_RESOLVE_METHOD: &str = "herdr_mcp.browser_resource.resolve";
pub const BROWSER_HANDOFF_PREPARE_METHOD: &str = "herdr_mcp.browser_handoff.prepare";
pub const BROWSER_SOURCE_RESOLVE_METHOD: &str = "herdr_mcp.browser_source.resolve";
pub const BROWSER_SPACE_CREATE_METHOD: &str = "herdr_mcp.browser_space.create";
pub const BROWSER_SPACE_OPEN_METHOD: &str = "herdr_mcp.browser_space.open";
pub const BROWSER_SPACE_INSPECT_METHOD: &str = "herdr_mcp.browser_space.inspect";
pub const BROWSER_SESSION_CREATE_METHOD: &str = "herdr_mcp.browser_session.create";
pub const BROWSER_SESSION_OPEN_METHOD: &str = "herdr_mcp.browser_session.open";
pub const BROWSER_SESSION_ARCHIVE_METHOD: &str = "herdr_mcp.browser_session.archive";
pub const BROWSER_SESSION_ARCHIVE_STATUS_METHOD: &str = "herdr_mcp.browser_session.archive_status";
pub const BROWSER_SESSION_INSPECT_METHOD: &str = "herdr_mcp.browser_session.inspect";
pub const BROWSER_MESSAGE_APPEND_METHOD: &str = "herdr_mcp.browser_message.append";
pub const BROWSER_COMPOSER_SET_REASONING_METHOD: &str = "herdr_mcp.browser_composer.set_reasoning";
pub const BROWSER_COMPOSER_SET_APPS_METHOD: &str = "herdr_mcp.browser_composer.set_apps";
pub const BROWSER_DISPATCH_SUBMIT_METHOD: &str = "herdr_mcp.browser_dispatch.submit";
pub const BROWSER_DISPATCH_STATUS_METHOD: &str = "herdr_mcp.browser_dispatch.status";
pub const BROWSER_DISPATCH_STOP_METHOD: &str = "herdr_mcp.browser_dispatch.stop";

/// Task requirements the semantic layer may fill only when the planner left
/// them unspecified. Semantic inference can add an advisory requirement, but
/// cannot override an explicit planner value or become execution authority.
const SEMANTIC_INFERABLE_KEYS: &[&str] = &[
    "requires_code_edit",
    "requires_shell",
    "requires_vision",
    "destructive_production_mutation",
    "delegates_other_workers",
    "shared_runtime_state",
];

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

const BUILTIN_SOURCE_IDENTITY: &str = "herdr-mcp:builtin";
const GLOBAL_POLICY_URI: &str = "skill://herdr-mcp/AGENTS.md";
const GLOBAL_AGENTS: &str = include_str!("../../../assets/herdr/AGENTS.md");
const MAX_PLANNING_WORKERS: usize = 12;

const WORKSTATION_CONTROL: &str =
    include_str!("../../../assets/herdr/skills/workstation-control/SKILL.md");
const FILES_SEARCH: &str = include_str!("../../../assets/herdr/skills/files-search/SKILL.md");
const FILES_MUTATION: &str = include_str!("../../../assets/herdr/skills/files-mutation/SKILL.md");
const GIT_REPOSITORY: &str = include_str!("../../../assets/herdr/skills/git-repository/SKILL.md");
const EXECUTION: &str = include_str!("../../../assets/herdr/skills/execution/SKILL.md");
const AGENT_DISPATCH: &str = include_str!("../../../assets/herdr/skills/agent-dispatch/SKILL.md");
const DEVELOPMENT_ORCHESTRATION: &str =
    include_str!("../../../assets/herdr/skills/development-orchestration/SKILL.md");
const ENGINEERING_ROBUSTNESS: &str =
    include_str!("../../../assets/herdr/skills/engineering-robustness/SKILL.md");
const REQUIREMENTS_GRILLING: &str =
    include_str!("../../../assets/herdr/skills/requirements-grilling/SKILL.md");

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Digest(String);

impl Digest {
    pub fn from_content(content: &str) -> Self {
        let value = Sha256::digest(content.as_bytes());
        Self(format!("sha256:{value:x}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Digest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct SkillIdentity {
    pub source_identity: String,
    pub uri: String,
    pub digest: Digest,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SkillDescriptor {
    pub id: String,
    pub name: String,
    pub description: String,
    pub identity: SkillIdentity,
    pub size: usize,
    pub triggers: Vec<String>,
    pub requires_capabilities: Vec<String>,
    pub related_skills: Vec<String>,
    pub risk_domains: Vec<String>,
    pub owned_tools: Vec<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LoadEvidence {
    pub id: String,
    pub identity: SkillIdentity,
    pub bytes: usize,
    pub cache_hit: bool,
    pub loaded_at: String,
}

#[derive(Debug, Clone, Copy)]
struct BuiltinSkillSpec {
    id: &'static str,
    description: &'static str,
    content: &'static str,
    triggers: &'static [&'static str],
    requires_capabilities: &'static [&'static str],
    related_skills: &'static [&'static str],
    risk_domains: &'static [&'static str],
    owned_tools: &'static [&'static str],
}

const BUILTIN_SKILLS: [BuiltinSkillSpec; 9] = [
    BuiltinSkillSpec {
        id: "workstation-control",
        description: "Control live Herdr workspaces, panes, agents, incremental state, and native methods.",
        content: WORKSTATION_CONTROL,
        triggers: &[
            "workspace",
            "pane",
            "agent state",
            "native method",
            "reconnect",
            "continue prior work",
            "resume conversation",
            "handoff",
            "continuity",
        ],
        requires_capabilities: &["herdr socket"],
        related_skills: &["agent-dispatch", "development-orchestration"],
        risk_domains: &["control-target"],
        owned_tools: &[
            "herdr_methods",
            "herdr_inspect",
            "herdr_call",
            "herdr_since",
        ],
    },
    BuiltinSkillSpec {
        id: "files-search",
        description: "READ ONLY: read, list, search, and inspect images inside managed project roots. Use herdr_fs_read for source reads; this skill owns no file-creation tool.",
        content: FILES_SEARCH,
        triggers: &["read file", "list files", "search", "grep", "image"],
        requires_capabilities: &["managed project root"],
        related_skills: &["files-mutation", "git-repository"],
        risk_domains: &[],
        owned_tools: &[
            "herdr_fs_read",
            "herdr_fs_list",
            "herdr_fs_grep",
            "herdr_fs_image",
        ],
    },
    BuiltinSkillSpec {
        id: "files-mutation",
        description: "File mutation only: herdr_fs_write = CREATE / FULL REWRITE; herdr_fs_edit = exact replacement; herdr_fs_patch = PATCH EXISTING FILES or coherent multi-file changes.",
        content: FILES_MUTATION,
        triggers: &["edit", "write", "patch", "modify files"],
        requires_capabilities: &["managed project root", "mutation gate"],
        related_skills: &[
            "files-search",
            "git-repository",
            "development-orchestration",
        ],
        risk_domains: &["filesystem-mutation"],
        owned_tools: &["herdr_fs_edit", "herdr_fs_write", "herdr_fs_patch"],
    },
    BuiltinSkillSpec {
        id: "git-repository",
        description: "Read deterministic Git facts and manage branch/worktree lifecycle evidence.",
        content: GIT_REPOSITORY,
        triggers: &[
            "git", "diff", "status", "branch", "worktree", "rebase", "merge",
        ],
        requires_capabilities: &["git repository"],
        related_skills: &["development-orchestration"],
        risk_domains: &["repository-mutation"],
        owned_tools: &["herdr_git"],
    },
    BuiltinSkillSpec {
        id: "execution",
        description: "Run bounded commands and start/resume/stop durable execution sessions.",
        content: EXECUTION,
        triggers: &[
            "command",
            "test",
            "build",
            "process",
            "long task",
            "session",
        ],
        requires_capabilities: &["shell execution"],
        related_skills: &["development-orchestration"],
        risk_domains: &["process-mutation"],
        owned_tools: &[
            "herdr_exec",
            "herdr_exec_start",
            "herdr_exec_read",
            "herdr_exec_kill",
        ],
    },
    BuiltinSkillSpec {
        id: "agent-dispatch",
        description: "Select and submit compatible local-agent work from live capability facts with explicit ownership and verification.",
        content: AGENT_DISPATCH,
        triggers: &[
            "delegate",
            "coding agent",
            "review",
            "parallel implementation",
            "audit",
        ],
        requires_capabilities: &["live agent state"],
        related_skills: &["workstation-control", "development-orchestration"],
        risk_domains: &["agent-mutation"],
        owned_tools: &["herdr_prompt"],
    },
    BuiltinSkillSpec {
        id: "development-orchestration",
        description: "Compose serial and parallel development lanes with explicit ownership and validation.",
        content: DEVELOPMENT_ORCHESTRATION,
        triggers: &[
            "multi-line development",
            "parallel development",
            "worktree lane",
            "orchestration",
        ],
        requires_capabilities: &[],
        related_skills: &[
            "workstation-control",
            "files-mutation",
            "git-repository",
            "execution",
            "agent-dispatch",
            "engineering-robustness",
        ],
        risk_domains: &["cross-lane-mutation"],
        owned_tools: &[],
    },
    BuiltinSkillSpec {
        id: "engineering-robustness",
        description: "Design and verify maintainable AI-generated code with regression-first bug fixes, silent-wrongness tests, layered delivery evidence, and minimal product state.",
        content: ENGINEERING_ROBUSTNESS,
        triggers: &[
            "bug fix",
            "regression",
            "robustness",
            "reliability",
            "self-test",
            "refactor",
            "release",
            "race",
            "stale state",
        ],
        requires_capabilities: &[],
        related_skills: &["development-orchestration", "execution", "git-repository"],
        risk_domains: &[],
        owned_tools: &[],
    },
    BuiltinSkillSpec {
        id: "requirements-grilling",
        description: "Interrogate material unresolved requirements one decision at a time, after grounding device/workspace/history and independently retrieving discoverable facts.",
        content: REQUIREMENTS_GRILLING,
        triggers: &[
            "unclear requirements",
            "ambiguous scope",
            "design decision",
            "requirements interview",
            "requirements grill",
            "stress-test plan",
        ],
        requires_capabilities: &[],
        related_skills: &["workstation-control", "development-orchestration"],
        risk_domains: &[],
        owned_tools: &[],
    },
];

#[derive(Debug, Clone)]
struct CachedSkill {
    content: Arc<str>,
}

#[derive(Debug, Default)]
pub struct ProgressiveSkillService {
    cache: Mutex<HashMap<SkillIdentity, CachedSkill>>,
}

impl ProgressiveSkillService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enabled_from_env() -> bool {
        std::env::var("HERDR_MCP_PROGRESSIVE_SKILLS")
            .ok()
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "on" | "progressive"
                )
            })
    }

    pub fn catalog(&self) -> Vec<SkillDescriptor> {
        BUILTIN_SKILLS.iter().map(descriptor).collect()
    }

    /// Resolve the effective skill catalog with precedence builtin > project
    /// `.agents/skills` > user `~/.agents/skills`. Same-name lower-precedence
    /// local skills never override higher-precedence ones. An optional
    /// `project_root` enables deterministic project-skill resolution.
    pub fn effective_catalog(&self, project_root: Option<&Path>) -> Vec<SkillDescriptor> {
        self.effective_entries(project_root)
            .into_iter()
            .map(|entry| entry.descriptor)
            .collect()
    }

    /// Resolve precedence-merged builtin + local entries (metadata only).
    fn effective_entries(&self, project_root: Option<&Path>) -> Vec<LocalEntry> {
        let mut entries = Vec::with_capacity(BUILTIN_SKILLS.len() + 8);
        let mut seen = BTreeSet::new();
        for spec in BUILTIN_SKILLS.iter() {
            seen.insert(spec.id.to_owned());
            entries.push(LocalEntry::from_builtin(spec));
        }

        let home = local_skills::home_dir().unwrap_or_default();
        let discovery = local_skills::LocalSkillRegistry::discover(project_root, &home);
        for file in discovery.project.into_iter().chain(discovery.user) {
            if !seen.insert(file.id.clone()) {
                continue;
            }
            if let Some(entry) = Self::entry_from_file(&file) {
                entries.push(entry);
            }
        }
        entries
    }

    fn entry_from_file(file: &LocalSkillFile) -> Option<LocalEntry> {
        let content = read_file_bounded(&file.path)?;
        let fm = parse_frontmatter(&content);
        // Digest the exact raw bytes that load() will verify, mirroring the
        // builtin trim-for-metadata convention but kept internally consistent.
        let body = content.trim();
        let digest = Digest::from_content(body);
        let size = body.len();
        let name = fm.name.clone().unwrap_or_else(|| file.id.clone());
        let description = fm.description.clone().unwrap_or_default();
        let version = fm.version;
        let descriptor = SkillDescriptor {
            id: file.id.clone(),
            name,
            description,
            identity: SkillIdentity {
                source_identity: file.scope_identity.clone(),
                uri: format!("skill://local/{}", file.id),
                digest,
                version,
            },
            size,
            triggers: Vec::new(),
            requires_capabilities: Vec::new(),
            related_skills: Vec::new(),
            risk_domains: Vec::new(),
            owned_tools: Vec::new(),
        };
        // load_verified() digests the exact trimmed bytes it receives, so the
        // cached body is the same trimmed slice used for the identity digest.
        let content = Arc::from(body);
        Some(LocalEntry {
            descriptor,
            content,
        })
    }

    pub fn bootstrap(&self, snapshot: &Value) -> Value {
        let inventory = RuntimePaths::discover()
            .ok()
            .and_then(|paths| CapabilityInventoryStore::load_existing(&paths.config_dir).ok())
            .unwrap_or_default();
        self.bootstrap_with_inventory(snapshot, &inventory)
    }

    fn bootstrap_with_inventory(
        &self,
        snapshot: &Value,
        inventory: &[AgentCapabilityRecord],
    ) -> Value {
        let global_content = GLOBAL_AGENTS.trim();
        let global_digest = Digest::from_content(global_content);
        let catalog = self.catalog();
        let content = format!(
            "{}\n\n## Progressive load contract\n\nUse the compact `catalog` field to select policy modules. Load all required domains in one call:\n\n```text\nherdr_call(method=\"{}\", params={{\"ids\":[\"files-search\",\"git-repository\"]}})\n```\n\nFor non-trivial task planning, `herdr_call(method=\"{}\", ...)` returns evidence-backed direct/delegation/parallelism advice without choosing or starting an Agent. Loaded text is sticky in the current context while source identity and digest are unchanged. A new user turn does not reload it. Refresh live worker/pane/runtime facts through inspect/since.",
            global_content, LOCAL_LOAD_METHOD, PLANNING_ADVISE_METHOD
        );
        json!({
            "ok": true,
            "mode": "progressive",
            "content": content,
            "global_policy": {
                "logical_name": "AGENTS.md",
                "source_identity": BUILTIN_SOURCE_IDENTITY,
                "uri": GLOBAL_POLICY_URI,
                "digest": global_digest.as_str(),
                "bytes": global_content.len(),
            },
            "catalog": catalog.iter().map(bootstrap_descriptor_json).collect::<Vec<_>>(),
            "load": {
                "method": LOCAL_LOAD_METHOD,
                "params": {
                    "ids": "required array<string>, batched, first-request order preserved",
                    "expected_digests": "optional object keyed by skill id; mismatch fails closed",
                    "project_root": "optional string; enables deterministic project-local .agents/skills resolution"
                },
                "sticky": "conversation/task-context until source identity or digest changes, new capability domain, handoff, or explicit refresh",
                "authorization": "none"
            },
            "planning_advice": {
                "method": PLANNING_ADVISE_METHOD,
                "decision_owner": "web_planner",
                "effect": "read_only_advice",
                "params": {
                    "deterministic_tool": "optional non-empty string",
                    "task_text": "optional bounded task text; when a semantic provider is available it supplies advisory task-profile and Skill/method relevance without overriding explicit fields",
                    "project_root": "optional non-empty string",
                    "explicit_target": "optional non-empty agent id/kind/pane id",
                    "requires_code_edit": "optional boolean",
                    "requires_shell": "optional boolean",
                    "requires_vision": "optional boolean",
                    "minimum_reasoning_tier": "optional integer 0..255",
                    "destructive_production_mutation": "optional boolean",
                    "delegates_other_workers": "optional boolean",
                    "independent_units": "optional integer 1..64; omitted means task independence is unknown",
                    "ownership_isolated": "optional boolean",
                    "shared_runtime_state": "optional boolean"
                }
            },
            "orchestration_consumption": parent_orchestration_consumption(),
            "github_status": {
                "method": GITHUB_STATUS_METHOD,
                "effect": "read_only_fresh_status",
                "source": "local authenticated gh API; bypasses connector cache",
                "params": {
                    "project_root": "required managed git project/worktree root",
                    "pr_number": "optional positive integer; omit for repository Auto-merge state only",
                    "previous_fingerprint": "optional fingerprint from the prior call; unchanged state returns a compact changed=false response",
                    "wait_ms": "optional 1..20000; requires previous_fingerprint and combines planner waiting plus one fresh GitHub status probe into a single bounded call"
                }
            },
            "cleanup_preview": {
                "method": CLEANUP_PREVIEW_METHOD,
                "effect": "read_only_fail_closed_reclaim_preview",
                "source": "local Git + live Herdr resource snapshot + local authenticated GitHub API",
                "params": {
                    "project_root": "required managed git project/worktree root",
                    "target_ref": "optional integration target; defaults to origin/main"
                },
                "safety": "never fetches, prunes, closes, deletes, or mutates; safe_to_delete requires fresh target/GitHub evidence, clean/reachable Git state, no open PR reference, and no live Herdr resource blockers"
            },
            "request_budget": {
                "goal": "minimize Edge/Worker round trips without weakening mutation safety or verification",
                "default_strategy": "coalesce logical work into the fewest high-value supported calls",
                "planning_gate": "before the first remote call, derive the next dependency-aware call wave from facts already known",
                "call_admission": [
                    "obtain evidence that can change the next decision",
                    "execute work whose arguments and safety boundary are already known",
                    "verify a falsifiable acceptance boundary"
                ],
                "default_shape": "baseline -> independent read wave -> execution bundle -> verification wave -> event/delta follow-up only when change is expected",
                "rules": [
                    "treat herdr_inspect as an aggregate baseline for runtime, workspace, pane, Agent, project-root, and dirty-state facts; do not immediately rebuild those same views with separate list/status calls",
                    "load herdr_skill only when detailed operating policy or Agent control is needed; unless native Herdr CLI semantics matter, request include_native_reference=false",
                    "group independent reads into one dependency-aware wave instead of serial call/replan loops",
                    "when deterministic shell/Git arguments are already known and share one safety boundary, execute them in one bounded herdr_exec and perform intermediate local checks inside that call instead of returning to the model after every command",
                    "load multiple required Skill ids in one herdr_mcp.skill.load call and keep unchanged Skill content sticky",
                    "reuse github.status previous_fingerprint and exec_read next_offset; unchanged state and already-read output are not fetched again",
                    "for active GitHub CI monitoring, prefer github.status with previous_fingerprint plus wait_ms=20000 over a separate planner sleep followed by a status call or gh run watch; after the bounded local wait it performs one fresh probe and unchanged state returns compact changed=false",
                    "prefer summary private methods such as cleanup.preview over rebuilding the same view with many MCP calls",
                    "start long work once and read only deltas when completion or actionable progress could plausibly have changed; do not poll idle state or emit planner heartbeats",
                    "re-plan only when a result changes later arguments or safety, a human action is required, or mutation delivery is uncertain",
                    "use protocol/server-side batching only when live capabilities advertise it; never simulate unsafe mutation batching"
                ],
                "capability_source": "live runtime context is authoritative for JSON-RPC batch, multi-operation arguments, and concurrency"
            },
            "capability_snapshot": capability_summary_with_inventory(snapshot, inventory),
            "planning_context": planning_context_with_inventory(snapshot, inventory),
            "bytes": content.len(),
        })
    }

    pub fn local_call(&self, method: &str, params: &Value, snapshot: &Value) -> Option<Value> {
        if !method.starts_with("herdr_mcp.") {
            return None;
        }
        Some(match method {
            LOCAL_LIST_METHOD => self.list_method(params),
            LOCAL_DESCRIBE_METHOD => self.describe_method(params),
            LOCAL_LOAD_METHOD => self.load_method(params),
            PLANNING_ADVISE_METHOD => self.planning_advise_method(params, snapshot),
            AGENT_CLOSEOUT_ADVISE_METHOD => self.agent_closeout_advise_method(params),
            AGENT_ATTENTION_ADVISE_METHOD => self.agent_attention_advise_method(params),
            VALIDATION_ADVISE_METHOD => self.validation_advise_method(params),
            GITHUB_STATUS_METHOD => crate::github_status::status(params, snapshot),
            CLEANUP_PREVIEW_METHOD => crate::cleanup_preview::preview(params, snapshot),
            TEXT_READ_METHOD => crate::text_transfer::read(params),
            TEXT_WRITE_METHOD => crate::text_transfer::write(params),
            _ => json!({
                "ok": false,
                "code": "unknown_local_method",
                "method": method,
                "message": "unknown herdr-mcp local method; request was not forwarded to the Herdr socket",
            }),
        })
    }

    fn planning_advise_method(&self, params: &Value, snapshot: &Value) -> Value {
        let inventory = RuntimePaths::discover()
            .ok()
            .and_then(|paths| CapabilityInventoryStore::load_existing(&paths.config_dir).ok())
            .unwrap_or_default();
        self.planning_advise_method_with_inventory(params, snapshot, &inventory)
    }

    fn planning_advise_method_with_inventory(
        &self,
        params: &Value,
        snapshot: &Value,
        inventory: &[AgentCapabilityRecord],
    ) -> Value {
        self.planning_advise_method_with_inventory_and_semantic(params, snapshot, inventory, None)
    }

    fn planning_advise_method_with_inventory_and_semantic(
        &self,
        params: &Value,
        snapshot: &Value,
        inventory: &[AgentCapabilityRecord],
        semantic_service: Option<&SemanticService>,
    ) -> Value {
        const KEYS: &[&str] = &[
            "deterministic_tool",
            "task_text",
            "project_root",
            "explicit_target",
            "requires_code_edit",
            "requires_shell",
            "requires_vision",
            "minimum_reasoning_tier",
            "destructive_production_mutation",
            "delegates_other_workers",
            "independent_units",
            "ownership_isolated",
            "shared_runtime_state",
        ];
        if let Err(error) = validate_object_keys(params, KEYS) {
            return error;
        }
        let task_text = match optional_bounded_nonempty_string(params, "task_text", 8192) {
            Ok(value) => value,
            Err(error) => return error,
        };
        let mut task = match task_profile_from_params(params) {
            Ok(task) => task,
            Err(error) => return error,
        };
        let visibility = AgentVisibility::from_env();
        let capabilities = project_capabilities_with_inventory(snapshot, &visibility, inventory);
        let initial_advice = advise_dispatch(&task, &capabilities);
        let initial_startable = startable_candidates_json(inventory, &visibility, snapshot, &task);
        let agent_route_criteria =
            agent_route_criteria(&initial_advice, &capabilities.workers, &initial_startable);
        let mut semantic = if task.deterministic_tool.is_none() {
            task_text.as_deref().map(|task_text| {
                if let Some(service) = semantic_service {
                    self.semantic_planning_advice_with_service(
                        task_text,
                        params,
                        &mut task,
                        &agent_route_criteria,
                        service,
                    )
                } else {
                    self.semantic_planning_advice(
                        task_text,
                        params,
                        &mut task,
                        &agent_route_criteria,
                    )
                }
            })
        } else {
            None
        };
        let advice = advise_dispatch(&task, &capabilities);
        let startable_candidates =
            startable_candidates_json(inventory, &visibility, snapshot, &task);
        let agent_lifecycle = agent_lifecycle_json(&advice, &startable_candidates);
        if let Some(semantic) = semantic.as_mut() {
            retain_compatible_agent_routes(semantic, &advice, &startable_candidates);
        }
        let planner_supplied_requirements = SEMANTIC_INFERABLE_KEYS
            .iter()
            .copied()
            .filter(|key| params.get(*key).is_some_and(|value| !value.is_null()))
            .map(|key| json!(key))
            .collect::<Vec<_>>();
        let semantic_inferred_requirements = semantic
            .as_ref()
            .and_then(|semantic| semantic.get("applied_fields"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let determinism = json!({
            "advice_basis": if semantic_inferred_requirements.is_empty() {
                "planner_supplied_requirements"
            } else {
                "planner_requirements_plus_semantic_inferred"
            },
            "planner_supplied_requirements": planner_supplied_requirements,
            "semantic_inferred_requirements": semantic_inferred_requirements,
            "zero_candidates_blocks_execution": false,
            "authority": "deterministic gates remain authoritative",
            "note": "planning advice, semantic inference, agent ranking, and zero compatible candidates never block direct execution or create a human boundary"
        });
        json!({
            "ok": true,
            "decision_owner": "web_planner",
            "planning_policy": {
                "read_only": true,
                "advisory_only": true,
            },
            "advice": dispatch_advice_json(&advice),
            "semantic": semantic.unwrap_or_else(|| json!({
                "attempted": false,
                "used": false,
                "advisory_only": true,
                "reason": if task_text.is_none() {
                    "task_text_absent"
                } else {
                    "deterministic_tool_explicit"
                },
                "capability": SemanticService::from_config().capability_json(),
            })),
            "context_resolution": {
                "level": "required_before_prior_or_ambiguous_project_discussion",
                "order": ["device", "project_workspace", "continuity_history", "live_git_runtime", "requirements_planning"],
                "detail_skill": "workstation-control"
            },
            "orchestration_policy": {
                "detail_skill": "development-orchestration",
                "levels": {
                    "minimum_entities": "required",
                    "parallelism": "advisory",
                    "progress_control": "required_when_delegated_or_long_running",
                    "cross_audit": "required_for_multi_lane_mutation; conditional_for_single_lane",
                    "verification": "required_for_mutation",
                    "reclamation": "required_for_planner_created_resources"
                },
                "parallelism": {
                    "worth_considering": advice.parallelism.worth_considering,
                    "max_useful_lanes": advice.parallelism.max_useful_lanes
                },
                "consumption": parent_orchestration_consumption()
            },
            "requirements_resolution": {
                "level": "conditional_on_material_ambiguity",
                "detail_skill": "requirements-grilling",
                "question_mode": "one_at_a_time"
            },
            "startable_candidates": startable_candidates,
            "agent_lifecycle": agent_lifecycle,
            "determinism": determinism,
            "resource_context": resource_context_json(snapshot),
            "refresh": {
                "live": "herdr_inspect/herdr_since",
                "capabilities": "herdr-mcp scan --probe",
            },
        })
    }

    fn agent_closeout_advise_method(&self, params: &Value) -> Value {
        const KEYS: &[&str] = &[
            "target",
            "recent_text",
            "agent_status",
            "process_running",
            "worktree_dirty",
            "open_pr",
            "running_exec",
            "task_owned_resources",
        ];
        if let Err(error) = validate_object_keys(params, KEYS) {
            return error;
        }
        let target = match optional_bounded_nonempty_string(params, "target", 256) {
            Ok(Some(value)) => value,
            Ok(None) => return invalid_params("target must be a non-empty string"),
            Err(error) => return error,
        };
        let recent_text = match optional_bounded_nonempty_string(params, "recent_text", 12_000) {
            Ok(Some(value)) => value,
            Ok(None) => return invalid_params("recent_text must be a non-empty string"),
            Err(error) => return error,
        };
        let agent_status = match optional_bounded_nonempty_string(params, "agent_status", 64) {
            Ok(value) => value,
            Err(error) => return error,
        };
        for key in [
            "process_running",
            "worktree_dirty",
            "open_pr",
            "running_exec",
        ] {
            if params
                .get(key)
                .is_some_and(|value| !value.is_boolean() && !value.is_null())
            {
                return invalid_params(&format!("{key} must be a boolean when provided"));
            }
        }
        if params.get("task_owned_resources").is_some_and(|value| {
            !value.is_null() && value.as_u64().is_none_or(|count| count > 1024)
        }) {
            return invalid_params(
                "task_owned_resources must be an integer from 0 to 1024 when provided",
            );
        }

        let service = SemanticService::from_config();
        let capability = service.capability_json();
        if !service.configured() {
            return json!({
                "ok": true,
                "attempted": true,
                "used": false,
                "reason": "not_configured",
                "advisory_only": true,
                "capability": capability,
            });
        }

        let request = SemanticRequest::new(json!({
            "target": target,
            "recent_text": recent_text,
            "metadata": {
                "agent_status": agent_status,
                "process_running": params.get("process_running").and_then(Value::as_bool),
                "worktree_dirty": params.get("worktree_dirty").and_then(Value::as_bool),
                "open_pr": params.get("open_pr").and_then(Value::as_bool),
                "running_exec": params.get("running_exec").and_then(Value::as_bool),
                "task_owned_resources": params.get("task_owned_resources").and_then(Value::as_u64),
            },
        }))
        .ask(
            "closeout_state",
            SemanticQuestion::choice(
                "Which advisory state best describes the agent's latest bounded output?",
                BTreeMap::from([
                    (
                        "working".to_owned(),
                        Some("The agent is still actively progressing its assigned work".to_owned()),
                    ),
                    (
                        "claims_complete".to_owned(),
                        Some("The agent says its assigned work is complete".to_owned()),
                    ),
                    (
                        "waiting_user".to_owned(),
                        Some("The agent is waiting for a user decision, approval, or input".to_owned()),
                    ),
                    (
                        "blocked_external".to_owned(),
                        Some("The agent is blocked on an external system, job, or dependency".to_owned()),
                    ),
                    (
                        "unclear".to_owned(),
                        Some("The bounded evidence does not establish a clear state".to_owned()),
                    ),
                ]),
            ),
        )
        .ask(
            "needs_human",
            SemanticQuestion::noul(
                "Does the bounded recent output indicate that a human decision or action is needed before useful progress can continue?",
                "A human decision or action is needed",
                "No human decision or action is needed",
            ),
        )
        .ask(
            "task_completed",
            SemanticQuestion::noul(
                "Does the bounded recent output indicate that the delegated task objective itself is complete, beyond this turn merely settling?",
                "The delegated task objective is complete",
                "The delegated task objective is not yet complete",
            ),
        )
        .ask(
            "needs_followup",
            SemanticQuestion::noul(
                "Does the parent need to perform follow-up work after this settled turn to complete the delegated objective?",
                "Parent follow-up work is needed",
                "No parent follow-up work is needed",
            ),
        )
        .ask(
            "needs_handoff",
            SemanticQuestion::noul(
                "Does the bounded recent output indicate that the work should be handed off to another agent, tool, or WebChat session?",
                "A handoff is needed",
                "No handoff is needed",
            ),
        );

        let response = match service.evaluate(&request) {
            Ok(response) => response,
            Err(error) => {
                return json!({
                    "ok": true,
                    "attempted": true,
                    "used": false,
                    "reason": error.code(),
                    "advisory_only": true,
                    "capability": capability,
                });
            }
        };
        let Some((state, probabilities, confidence)) = response
            .answer("closeout_state")
            .and_then(SemanticAnswer::choice_value)
        else {
            return json!({
                "ok": true,
                "attempted": true,
                "used": false,
                "reason": "bad_response",
                "advisory_only": true,
                "capability": capability,
            });
        };
        if !matches!(
            state,
            "working" | "claims_complete" | "waiting_user" | "blocked_external" | "unclear"
        ) {
            return json!({
                "ok": true,
                "attempted": true,
                "used": false,
                "reason": "bad_response",
                "advisory_only": true,
                "capability": capability,
            });
        }

        json!({
            "ok": true,
            "attempted": true,
            "used": true,
            "advisory_only": true,
            "assessment": {
                "state": state,
                "probabilities": probabilities,
                "confidence": confidence,
                "needs_human": response
                    .answer("needs_human")
                    .and_then(SemanticAnswer::noul_probability),
                "task_completed": response
                    .answer("task_completed")
                    .and_then(SemanticAnswer::noul_probability),
                "needs_followup": response
                    .answer("needs_followup")
                    .and_then(SemanticAnswer::noul_probability),
                "needs_handoff": response
                    .answer("needs_handoff")
                    .and_then(SemanticAnswer::noul_probability),
            },
            "provider": response.provider,
            "model": response.model,
            "capability": capability,
            "authority": "semantic classification only; deterministic reclaim gates remain authoritative",
        })
    }

    fn agent_attention_advise_method(&self, params: &Value) -> Value {
        self.agent_attention_advise_with_timeout(params, None)
    }

    fn agent_attention_advise_with_timeout(
        &self,
        params: &Value,
        timeout: Option<Duration>,
    ) -> Value {
        if let Err(error) = validate_object_keys(params, &["children"]) {
            return error;
        }
        let Some(children) = params.get("children").and_then(Value::as_array) else {
            return invalid_params("children must be an array");
        };
        if children.is_empty() || children.len() > 16 {
            return invalid_params("children must contain 1 to 16 entries");
        }
        let allowed = [
            "task_id",
            "dispatch_id",
            "agent_id",
            "terminal_state",
            "status",
            "recent_text",
            "age_ms",
            "has_running_exec",
            "dirty_worktree",
            "open_pr",
        ];
        let mut frozen = Vec::with_capacity(children.len());
        for (index, child) in children.iter().enumerate() {
            let Some(object) = child.as_object() else {
                return invalid_params(&format!("children[{index}] must be an object"));
            };
            if let Err(error) = validate_object_keys(child, &allowed) {
                return error;
            }
            let bounded = |key: &str, max: usize| -> Result<Option<String>, Value> {
                match object.get(key) {
                    None | Some(Value::Null) => Ok(None),
                    Some(Value::String(value))
                        if !value.trim().is_empty() && value.len() <= max =>
                    {
                        Ok(Some(value.clone()))
                    }
                    Some(Value::String(_)) => Err(invalid_params(&format!(
                        "children[{index}].{key} must be non-empty and at most {max} bytes"
                    ))),
                    Some(_) => Err(invalid_params(&format!(
                        "children[{index}].{key} must be a string when provided"
                    ))),
                }
            };
            let task_id = match bounded("task_id", 160) {
                Ok(v) => v,
                Err(e) => return e,
            };
            let dispatch_id = match bounded("dispatch_id", 160) {
                Ok(v) => v,
                Err(e) => return e,
            };
            let agent_id = match bounded("agent_id", 256) {
                Ok(v) => v,
                Err(e) => return e,
            };
            let terminal_state = match bounded("terminal_state", 64) {
                Ok(v) => v,
                Err(e) => return e,
            };
            let status = match bounded("status", 64) {
                Ok(v) => v,
                Err(e) => return e,
            };
            let recent_text = match bounded("recent_text", 8000) {
                Ok(v) => v,
                Err(e) => return e,
            };
            let age_ms = match object.get("age_ms") {
                None | Some(Value::Null) => None,
                Some(value) => match value.as_u64() {
                    Some(value) if value <= 604_800_000 => Some(value),
                    _ => {
                        return invalid_params(&format!(
                            "children[{index}].age_ms must be an integer from 0 to 604800000"
                        ));
                    }
                },
            };
            for key in ["has_running_exec", "dirty_worktree", "open_pr"] {
                if object
                    .get(key)
                    .is_some_and(|value| !value.is_null() && !value.is_boolean())
                {
                    return invalid_params(&format!(
                        "children[{index}].{key} must be a boolean when provided"
                    ));
                }
            }
            let mut frozen_child = Map::new();
            frozen_child.insert("id".to_owned(), json!(format!("child_{index}")));
            for (key, value) in [
                ("task_id", task_id),
                ("dispatch_id", dispatch_id),
                ("agent_id", agent_id),
                ("terminal_state", terminal_state),
                ("status", status),
                ("recent_text", recent_text),
            ] {
                if let Some(value) = value {
                    frozen_child.insert(key.to_owned(), json!(value));
                }
            }
            if let Some(age_ms) = age_ms {
                frozen_child.insert("age_ms".to_owned(), json!(age_ms));
            }
            for key in ["has_running_exec", "dirty_worktree", "open_pr"] {
                if let Some(value) = object.get(key).and_then(Value::as_bool) {
                    frozen_child.insert(key.to_owned(), json!(value));
                }
            }
            frozen.push(Value::Object(frozen_child));
        }

        let semantic_state = json!({"children": frozen});
        let state_bytes = serde_json::to_vec(&semantic_state)
            .map(|value| value.len())
            .unwrap_or(0);
        let question_count = children.len() + usize::from(children.len() >= 2);
        let budget_ms =
            timeout.map(|timeout| u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX));
        let metrics = |elapsed_ms: u64| {
            json!({
                "child_count": children.len(),
                "state_bytes": state_bytes,
                "question_count": question_count,
                "elapsed_ms": elapsed_ms,
                "budget_ms": budget_ms,
            })
        };

        let service = SemanticService::from_config();
        let capability = service.capability_json();
        if !service.configured() {
            return json!({
                "ok": true, "attempted": true, "used": false,
                "reason": "not_configured", "advisory_only": true,
                "children": frozen, "capability": capability,
                "metrics": metrics(0),
            });
        }
        let categories = BTreeMap::from([
            ("continue_unobserved".to_owned(), Some("The child appears to be progressing normally and does not need immediate parent attention".to_owned())),
            ("verify_completion".to_owned(), Some("The child claims or appears complete and should be verified next".to_owned())),
            ("needs_human".to_owned(), Some("The child appears to require a human decision or action".to_owned())),
            ("blocked_external".to_owned(), Some("The child appears blocked on an external dependency or service".to_owned())),
            ("investigate_drift".to_owned(), Some("The child appears active but may be drifting from its assigned objective".to_owned())),
            ("unclear".to_owned(), Some("The bounded evidence does not support a clearer classification".to_owned())),
        ]);
        let mut request = SemanticRequest::new(semantic_state);
        for index in 0..children.len() {
            request = request.ask(
                format!("child_{index}_state"),
                SemanticQuestion::choice(
                    "Which advisory attention state best describes this child?",
                    categories.clone(),
                ),
            );
        }
        let child_criteria = (0..children.len())
            .map(|index| {
                (
                    format!("child_{index}"),
                    Some(format!("Frozen child summary at index {index}")),
                )
            })
            .collect::<BTreeMap<_, _>>();
        if child_criteria.len() >= 2 {
            request = request.ask(
                "next_child",
                SemanticQuestion::choice(
                    "Which child deserves the parent's next bounded inspection?",
                    child_criteria,
                ),
            );
        }
        let started = Instant::now();
        let response = match timeout
            .map(|timeout| service.evaluate_with_timeout(&request, timeout))
            .unwrap_or_else(|| service.evaluate(&request))
        {
            Ok(response) => response,
            Err(error) => {
                let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                return json!({
                    "ok": true, "attempted": true, "used": false,
                    "reason": error.code(), "advisory_only": true,
                    "children": frozen, "capability": capability,
                    "metrics": metrics(elapsed_ms),
                });
            }
        };
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut assessments = Vec::new();
        for index in 0..children.len() {
            let key = format!("child_{index}_state");
            let Some((state, probabilities, confidence)) =
                response.answer(&key).and_then(SemanticAnswer::choice_value)
            else {
                return json!({
                    "ok": true, "attempted": true, "used": false,
                    "reason": "bad_response", "advisory_only": true,
                    "children": frozen, "capability": capability,
                    "metrics": metrics(elapsed_ms),
                });
            };
            if !categories.contains_key(state) {
                return json!({
                    "ok": true, "attempted": true, "used": false,
                    "reason": "bad_response", "advisory_only": true,
                    "children": frozen, "capability": capability,
                    "metrics": metrics(elapsed_ms),
                });
            }
            assessments.push(json!({
                "id": format!("child_{index}"),
                "state": state,
                "probabilities": probabilities,
                "confidence": confidence,
            }));
        }
        if children.len() >= 2 {
            let Some((selected, _, _)) = response
                .answer("next_child")
                .and_then(SemanticAnswer::choice_value)
            else {
                return json!({
                    "ok": true, "attempted": true, "used": false,
                    "reason": "bad_response", "advisory_only": true,
                    "children": frozen, "capability": capability,
                    "metrics": metrics(elapsed_ms),
                });
            };
            let selected_index = selected
                .strip_prefix("child_")
                .and_then(|value| value.parse::<usize>().ok());
            if selected_index.is_none_or(|index| index >= children.len()) {
                return json!({
                    "ok": true, "attempted": true, "used": false,
                    "reason": "bad_response", "advisory_only": true,
                    "children": frozen, "capability": capability,
                    "metrics": metrics(elapsed_ms),
                });
            }
        }
        let ranking = response
            .answer("next_child")
            .and_then(SemanticAnswer::choice_value)
            .map(|(_, probabilities, _)| {
                let mut ranked = probabilities
                    .iter()
                    .filter_map(|(id, probability)| {
                        id.strip_prefix("child_")
                            .and_then(|value| value.parse::<usize>().ok())
                            .filter(|index| *index < children.len())
                            .map(|_| (id.clone(), *probability))
                    })
                    .collect::<Vec<_>>();
                ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                ranked
                    .into_iter()
                    .map(|(id, probability)| json!({"id": id, "probability": probability}))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        json!({
            "ok": true, "attempted": true, "used": true, "advisory_only": true,
            "children": frozen, "assessments": assessments, "attention_ranking": ranking,
            "provider": response.provider, "model": response.model, "capability": capability,
            "metrics": metrics(elapsed_ms),
            "authority": "semantic attention triage only; deterministic task, process, terminal, replay, and reclaim facts remain authoritative",
        })
    }

    fn validation_advise_method(&self, params: &Value) -> Value {
        const KEYS: &[&str] = &[
            "project_root",
            "summary",
            "changed_files",
            "changed_symbols",
            "candidate_checks",
        ];
        if let Err(error) = validate_object_keys(params, KEYS) {
            return error;
        }
        let parse_strings =
            |key: &str, max_items: usize, max_len: usize| -> Result<Vec<String>, Value> {
                let Some(values) = params.get(key) else {
                    return Ok(Vec::new());
                };
                if values.is_null() {
                    return Ok(Vec::new());
                }
                let Some(values) = values.as_array() else {
                    return Err(invalid_params(&format!("{key} must be an array")));
                };
                if values.len() > max_items {
                    return Err(invalid_params(&format!(
                        "{key} must contain at most {max_items} entries"
                    )));
                }
                values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| match value.as_str() {
                        Some(value) if !value.trim().is_empty() && value.len() <= max_len => {
                            Ok(value.to_owned())
                        }
                        _ => Err(invalid_params(&format!(
                            "{key}[{index}] must be a non-empty string at most {max_len} bytes"
                        ))),
                    })
                    .collect()
            };
        let changed_files = match parse_strings("changed_files", 64, 1024) {
            Ok(values) if !values.is_empty() => values,
            Ok(_) => return invalid_params("changed_files must contain at least one entry"),
            Err(error) => return error,
        };
        let changed_symbols = match parse_strings("changed_symbols", 64, 256) {
            Ok(values) => values,
            Err(error) => return error,
        };
        let summary = match optional_bounded_nonempty_string(params, "summary", 4096) {
            Ok(value) => value,
            Err(error) => return error,
        };
        let project_root = match optional_bounded_nonempty_string(params, "project_root", 1024) {
            Ok(value) => value,
            Err(error) => return error,
        };
        let supplied_checks = match parse_strings("candidate_checks", 32, 256) {
            Ok(values) => values,
            Err(error) => return error,
        };
        let mut check_ids = BTreeSet::new();
        if supplied_checks.is_empty() {
            for path in &changed_files {
                if path.starts_with("crates/herdr-mcp/") {
                    check_ids.insert("rust_gate".to_owned());
                }
                if path.starts_with("extension/") {
                    check_ids.insert("extension_targeted".to_owned());
                    check_ids.insert("extension_smoke".to_owned());
                    if path == "extension/background.js"
                        || path.contains("binding")
                        || path.contains("browser-state")
                    {
                        check_ids.insert("extension_background_bind".to_owned());
                    }
                }
                if path.starts_with("edge/")
                    || path.starts_with("src/")
                    || path.contains("contract")
                {
                    check_ids.insert("node_build".to_owned());
                    check_ids.insert("edge_tests".to_owned());
                }
                if path.starts_with("docs/") || path == "README.md" {
                    check_ids.insert("docs_gate".to_owned());
                    check_ids.insert("hygiene_gate".to_owned());
                }
            }
        } else {
            check_ids.extend(supplied_checks);
        }
        let checks = check_ids.into_iter().collect::<Vec<_>>();
        let service = SemanticService::from_config();
        let capability = service.capability_json();
        let deterministic = json!({
            "changed_files": changed_files,
            "changed_symbols": changed_symbols,
            "candidate_checks": checks,
            "checks_preserved": true,
        });
        if !service.configured() {
            return json!({
                "ok": true, "attempted": true, "used": false,
                "reason": "not_configured", "advisory_only": true,
                "deterministic": deterministic, "capability": capability,
            });
        }
        let criteria = checks
            .iter()
            .map(|id| {
                (
                    id.clone(),
                    Some(format!("Frozen deterministic validation candidate {id}")),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut request = SemanticRequest::new(json!({
            "project_root": project_root, "summary": summary,
            "changed_files": changed_files, "changed_symbols": changed_symbols,
            "candidate_checks": checks,
        }))
        .ask(
            "regression_surface",
            SemanticQuestion::choice(
                "Which bounded regression surface best describes these changes?",
                BTreeMap::from([
                    (
                        "rust".to_owned(),
                        Some("Rust runtime or native control plane".to_owned()),
                    ),
                    (
                        "extension".to_owned(),
                        Some("Browser extension behavior".to_owned()),
                    ),
                    (
                        "edge".to_owned(),
                        Some("Edge or cross-language contract behavior".to_owned()),
                    ),
                    (
                        "docs".to_owned(),
                        Some("Documentation or generated site only".to_owned()),
                    ),
                    (
                        "cross_boundary".to_owned(),
                        Some("Multiple ownership boundaries are affected".to_owned()),
                    ),
                    (
                        "unknown".to_owned(),
                        Some("The bounded evidence is insufficient".to_owned()),
                    ),
                ]),
            ),
        )
        .ask(
            "cross_boundary_risk",
            SemanticQuestion::noul(
                "Do these frozen changes span multiple validation ownership boundaries?",
                "Multiple ownership boundaries are affected",
                "The change is contained within one ownership boundary",
            ),
        );
        if criteria.len() >= 2 {
            request = request.ask(
                "first_check",
                SemanticQuestion::choice(
                    "Which frozen validation candidate is most useful to run first?",
                    criteria.clone(),
                ),
            );
        }
        let response = match service.evaluate(&request) {
            Ok(response) => response,
            Err(error) => {
                return json!({
                    "ok": true, "attempted": true, "used": false,
                    "reason": error.code(), "advisory_only": true,
                    "deterministic": deterministic, "capability": capability,
                });
            }
        };
        let Some((surface, surface_probabilities, surface_confidence)) = response
            .answer("regression_surface")
            .and_then(SemanticAnswer::choice_value)
        else {
            return json!({
                "ok": true, "attempted": true, "used": false,
                "reason": "bad_response", "advisory_only": true,
                "deterministic": deterministic, "capability": capability,
            });
        };
        if criteria.len() >= 2 {
            let Some((selected, _, _)) = response
                .answer("first_check")
                .and_then(SemanticAnswer::choice_value)
            else {
                return json!({
                    "ok": true, "attempted": true, "used": false,
                    "reason": "bad_response", "advisory_only": true,
                    "deterministic": deterministic, "capability": capability,
                });
            };
            if !criteria.contains_key(selected) {
                return json!({
                    "ok": true, "attempted": true, "used": false,
                    "reason": "bad_response", "advisory_only": true,
                    "deterministic": deterministic, "capability": capability,
                });
            }
        }
        let ranking = response
            .answer("first_check")
            .and_then(SemanticAnswer::choice_value)
            .map(|(_, probabilities, _)| {
                let allowed = criteria.keys().cloned().collect::<BTreeSet<_>>();
                let mut ranked = probabilities
                    .iter()
                    .filter(|(id, _)| allowed.contains(*id))
                    .map(|(id, probability)| (id.clone(), *probability))
                    .collect::<Vec<_>>();
                ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                ranked
                    .into_iter()
                    .map(|(id, probability)| json!({"id": id, "probability": probability}))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        json!({
            "ok": true, "attempted": true, "used": true, "advisory_only": true,
            "deterministic": deterministic,
            "recommended_first_checks": ranking,
            "likely_regression_surface": {
                "value": surface,
                "probabilities": surface_probabilities,
                "confidence": surface_confidence,
            },
            "cross_boundary_risk": response
                .answer("cross_boundary_risk")
                .and_then(SemanticAnswer::noul_probability),
            "provider": response.provider, "model": response.model, "capability": capability,
            "authority": "semantic ordering only; deterministic required checks and gate outcomes remain authoritative",
        })
    }

    fn semantic_planning_advice(
        &self,
        task_text: &str,
        params: &Value,
        task: &mut TaskProfile,
        agent_route_criteria: &BTreeMap<String, Option<String>>,
    ) -> Value {
        let service = SemanticService::from_config();
        self.semantic_planning_advice_with_service(
            task_text,
            params,
            task,
            agent_route_criteria,
            &service,
        )
    }

    fn semantic_planning_advice_with_service(
        &self,
        task_text: &str,
        params: &Value,
        task: &mut TaskProfile,
        agent_route_criteria: &BTreeMap<String, Option<String>>,
        service: &SemanticService,
    ) -> Value {
        const MAX_SKILLS: usize = 48;
        const MAX_METHODS: usize = 48;
        const TOP_ROUTES: usize = 6;

        let capability = service.capability_json();
        if !service.configured() {
            return json!({
                "attempted": true,
                "used": false,
                "advisory_only": true,
                "reason": "not_configured",
                "capability": capability,
            });
        }

        let project_root = task.project_root.as_deref().map(Path::new);
        let skills = self
            .effective_catalog(project_root)
            .into_iter()
            .take(MAX_SKILLS)
            .collect::<Vec<_>>();
        let methods = local_method_schemas("")
            .into_iter()
            .filter(|schema| {
                schema.get("method").and_then(Value::as_str) != Some(PLANNING_ADVISE_METHOD)
            })
            .take(MAX_METHODS)
            .collect::<Vec<_>>();

        let mut request = SemanticRequest::new(json!({"task": task_text}))
            .ask(
                "requires_code_edit",
                SemanticQuestion::noul(
                    "Does completing the task require editing source code or tracked configuration?",
                    "A source or tracked configuration edit is required",
                    "No source or tracked configuration edit is required",
                ),
            )
            .ask(
                "requires_shell",
                SemanticQuestion::noul(
                    "Does completing the task require shell commands, builds, tests, or local processes?",
                    "Shell or local process execution is required",
                    "No shell or local process execution is required",
                ),
            )
            .ask(
                "requires_vision",
                SemanticQuestion::noul(
                    "Does completing the task require visual inspection of an image, rendered page, browser UI, or screenshot?",
                    "Visual inspection is required",
                    "Text and structured evidence are sufficient",
                ),
            )
            .ask(
                "destructive_production_mutation",
                SemanticQuestion::noul(
                    "Does the task explicitly require an irreversible or destructive production mutation?",
                    "An irreversible or destructive production mutation is required",
                    "No such production mutation is explicitly required",
                ),
            )
            .ask(
                "delegates_other_workers",
                SemanticQuestion::noul(
                    "Would a delegated worker itself need to dispatch or manage other workers?",
                    "The delegated worker would need to manage workers",
                    "The delegated worker can complete its assigned work directly",
                ),
            )
            .ask(
                "shared_runtime_state",
                SemanticQuestion::noul(
                    "Would parallel lanes contend for the same mutable runtime, repository, browser session, or other shared state?",
                    "Parallel lanes would share mutable state",
                    "No shared mutable runtime or state is implied",
                ),
            )
            .ask(
                "independent_units",
                SemanticQuestion::choice(
                    "How many independently completable units are clearly present in the task?",
                    BTreeMap::from([
                        ("one".to_owned(), Some("One coherent unit of work".to_owned())),
                        ("two".to_owned(), Some("Two independent units".to_owned())),
                        ("three".to_owned(), Some("Three independent units".to_owned())),
                        (
                            "four_plus".to_owned(),
                            Some("Four or more independent units".to_owned()),
                        ),
                    ]),
                ),
            )
            .ask(
                "reasoning_tier",
                SemanticQuestion::score(
                    "How much reasoning depth does the task appear to require?",
                    vec![
                        "Routine deterministic execution or lookup".to_owned(),
                        "Moderate analysis with a few dependent decisions".to_owned(),
                        "Deep multi-step reasoning, architecture, or difficult diagnosis".to_owned(),
                    ],
                ),
            );

        let skill_criteria = skills
            .iter()
            .map(|skill| {
                (
                    skill.id.clone(),
                    Some(if skill.description.is_empty() {
                        skill.name.clone()
                    } else {
                        skill.description.clone()
                    }),
                )
            })
            .collect::<BTreeMap<_, _>>();
        if skill_criteria.len() >= 2 {
            request = request.ask(
                "skill_route",
                SemanticQuestion::choice(
                    "Which Skill is most directly useful for completing the task?",
                    skill_criteria,
                ),
            );
        }

        let method_criteria = methods
            .iter()
            .filter_map(|schema| {
                let method = schema.get("method").and_then(Value::as_str)?;
                let properties = schema
                    .pointer("/params/properties")
                    .and_then(Value::as_object)
                    .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                let description = if properties.is_empty() {
                    "No parameters".to_owned()
                } else {
                    format!("Parameters: {}", properties.join(", "))
                };
                Some((method.to_owned(), Some(description)))
            })
            .collect::<BTreeMap<_, _>>();
        if method_criteria.len() >= 2 {
            request = request.ask(
                "method_route",
                SemanticQuestion::choice(
                    "Which native/private method is most directly useful for the next task step?",
                    method_criteria,
                ),
            );
        }
        if agent_route_criteria.len() >= 2 {
            request = request.ask(
                "agent_route",
                SemanticQuestion::choice(
                    "Among only these deterministically compatible candidates, which agent route best fits the task?",
                    agent_route_criteria.clone(),
                ),
            );
        }

        let response = match service.evaluate(&request) {
            Ok(response) => response,
            Err(error) => {
                return json!({
                    "attempted": true,
                    "used": false,
                    "advisory_only": true,
                    "reason": error.code(),
                    "capability": capability,
                });
            }
        };

        let mut applied_fields = Vec::new();
        let mut profile = Map::new();
        let mut profile_results = Map::new();
        let mut apply_positive = |answer_id: &str, param_key: &str, target: &mut bool| {
            let answer = response.answer(answer_id);
            let probability = answer.and_then(SemanticAnswer::noul_probability);
            let result = answer.and_then(SemanticAnswer::noul_result);
            profile.insert(param_key.to_owned(), json!(probability));
            profile_results.insert(param_key.to_owned(), json!(result));
            if params.get(param_key).is_none() && result == Some("true") {
                *target = true;
                applied_fields.push(param_key.to_owned());
            }
        };
        apply_positive(
            "requires_code_edit",
            "requires_code_edit",
            &mut task.requires_code_edit,
        );
        apply_positive("requires_shell", "requires_shell", &mut task.requires_shell);
        apply_positive(
            "requires_vision",
            "requires_vision",
            &mut task.requires_vision,
        );
        apply_positive(
            "destructive_production_mutation",
            "destructive_production_mutation",
            &mut task.destructive_production_mutation,
        );
        apply_positive(
            "delegates_other_workers",
            "delegates_other_workers",
            &mut task.delegates_other_workers,
        );
        apply_positive(
            "shared_runtime_state",
            "shared_runtime_state",
            &mut task.shared_runtime_state,
        );

        let independent = response
            .answer("independent_units")
            .and_then(SemanticAnswer::choice_value);
        profile.insert(
            "independent_units".to_owned(),
            independent
                .map(|(choice, probabilities, confidence)| {
                    json!({
                        "choice": choice,
                        "probabilities": probabilities,
                        "confidence": confidence,
                        "advisory_only": true,
                    })
                })
                .unwrap_or(Value::Null),
        );
        let reasoning = response
            .answer("reasoning_tier")
            .and_then(SemanticAnswer::score_value);
        profile.insert(
            "reasoning_tier".to_owned(),
            reasoning
                .map(|(score, probabilities, confidence)| {
                    json!({
                        "score": score,
                        "probabilities": probabilities,
                        "confidence": confidence,
                        "advisory_only": true,
                    })
                })
                .unwrap_or(Value::Null),
        );

        let ranked = |answer_id: &str| {
            let mut entries = response
                .answer(answer_id)
                .and_then(SemanticAnswer::choice_value)
                .map(|(_, probabilities, _)| {
                    probabilities
                        .iter()
                        .map(|(id, relevance)| (id.clone(), *relevance))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            entries.sort_by(|left, right| {
                right
                    .1
                    .partial_cmp(&left.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| left.0.cmp(&right.0))
            });
            entries
                .into_iter()
                .take(TOP_ROUTES)
                .map(|(id, relevance)| json!({"id": id, "relevance": relevance}))
                .collect::<Vec<_>>()
        };
        let ranked_agents = || {
            let Some((choice, probabilities, _)) = response
                .answer("agent_route")
                .and_then(SemanticAnswer::choice_value)
            else {
                return Vec::new();
            };
            if !agent_route_criteria.contains_key(choice)
                || probabilities
                    .keys()
                    .any(|id| !agent_route_criteria.contains_key(id))
            {
                return Vec::new();
            }
            let mut entries = probabilities
                .iter()
                .map(|(id, relevance)| (id.clone(), *relevance))
                .collect::<Vec<_>>();
            entries.sort_by(|left, right| {
                right
                    .1
                    .partial_cmp(&left.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| left.0.cmp(&right.0))
            });
            entries
                .into_iter()
                .take(TOP_ROUTES)
                .map(|(id, relevance)| json!({"id": id, "relevance": relevance}))
                .collect::<Vec<_>>()
        };

        json!({
            "attempted": true,
            "used": true,
            "advisory_only": true,
            "provider": response.provider,
            "model": response.model,
            "threshold": DEFAULT_DECISION_THRESHOLD,
            "profile": Value::Object(profile),
            "profile_results": Value::Object(profile_results),
            "applied_fields": applied_fields,
            "routing": {
                "skills": ranked("skill_route"),
                "methods": ranked("method_route"),
                "agents": ranked_agents(),
                "skills_considered": skills.len(),
                "methods_considered": methods.len(),
                "agents_considered": agent_route_criteria.len(),
            },
            "capability": capability,
            "authority": "advisory; deterministic gates remain authoritative",
        })
    }

    fn list_method(&self, params: &Value) -> Value {
        if let Err(error) = validate_object_keys(params, &["project_root"]) {
            return error;
        }
        let project_root = match optional_project_root(params) {
            Ok(value) => value,
            Err(error) => return error,
        };
        let catalog = self.effective_catalog(project_root.as_deref());
        json!({
            "ok": true,
            "skills": catalog.iter().map(descriptor_json).collect::<Vec<_>>(),
            "count": catalog.len(),
            "loaded": false,
        })
    }

    fn describe_method(&self, params: &Value) -> Value {
        if let Err(error) = validate_object_keys(params, &["id", "project_root"]) {
            return error;
        }
        let Some(id) = params.get("id").and_then(Value::as_str) else {
            return invalid_params("id must be a non-empty string");
        };
        if id.trim().is_empty() {
            return invalid_params("id must be a non-empty string");
        }
        let project_root = match optional_project_root(params) {
            Ok(value) => value,
            Err(error) => return error,
        };
        match self
            .effective_catalog(project_root.as_deref())
            .into_iter()
            .find(|item| item.id == id)
        {
            Some(item) => json!({"ok": true, "skill": descriptor_json(&item), "loaded": false}),
            None => json!({"ok": false, "code": "unknown_skill", "id": id}),
        }
    }

    fn load_method(&self, params: &Value) -> Value {
        if let Err(error) =
            validate_object_keys(params, &["ids", "expected_digests", "project_root"])
        {
            return error;
        }
        let Some(ids) = params.get("ids").and_then(Value::as_array) else {
            return invalid_params("ids must be a non-empty array of skill ids");
        };
        if ids.is_empty() || ids.len() > 16 {
            return invalid_params("ids must contain between 1 and 16 skill ids");
        }
        let expected = match params.get("expected_digests") {
            None | Some(Value::Null) => None,
            Some(Value::Object(value)) => Some(value),
            Some(_) => return invalid_params("expected_digests must be an object when provided"),
        };
        let project_root = match optional_project_root(params) {
            Ok(value) => value,
            Err(error) => return error,
        };

        let mut requested = Vec::new();
        let mut seen = BTreeSet::new();
        for value in ids {
            let Some(id) = value.as_str() else {
                return invalid_params("every ids entry must be a string");
            };
            if id.trim().is_empty() {
                return invalid_params("every ids entry must be non-empty");
            }
            if seen.insert(id.to_owned()) {
                requested.push(id.to_owned());
            }
        }

        let entries = self.effective_entries(project_root.as_deref());
        let mut loaded = Vec::with_capacity(requested.len());
        for id in requested {
            let Some(entry) = entries.iter().find(|entry| entry.descriptor.id == id) else {
                return json!({"ok": false, "code": "unknown_skill", "id": id});
            };
            let descriptor = &entry.descriptor;
            if let Some(expected_digest) = expected
                .and_then(|map| map.get(&id))
                .and_then(Value::as_str)
                && expected_digest != descriptor.identity.digest.as_str()
            {
                return json!({
                    "ok": false,
                    "code": "skill_digest_mismatch",
                    "id": id,
                    "expected_digest": expected_digest,
                    "actual_digest": descriptor.identity.digest.as_str(),
                });
            }
            let content = entry.content.clone();
            let (content, cache_hit) = match self.load_verified(&descriptor.identity, &content) {
                Ok(value) => value,
                Err(message) => {
                    return json!({
                        "ok": false,
                        "code": "skill_digest_mismatch",
                        "id": id,
                        "message": message,
                    });
                }
            };
            let evidence = LoadEvidence {
                id: descriptor.id.clone(),
                identity: descriptor.identity.clone(),
                bytes: content.len(),
                cache_hit,
                loaded_at: now_rfc3339(),
            };
            let mut item = load_evidence_json(&evidence);
            item.as_object_mut()
                .expect("load evidence must be an object")
                .insert("content".to_owned(), json!(content.as_ref()));
            loaded.push(item);
        }
        json!({
            "ok": true,
            "skills": loaded,
            "count": loaded.len(),
            "authorization": "none",
            "loaded_at": now_rfc3339(),
        })
    }

    fn load_verified(
        &self,
        identity: &SkillIdentity,
        content: &str,
    ) -> Result<(Arc<str>, bool), String> {
        let actual = Digest::from_content(content);
        if actual != identity.digest {
            return Err(format!(
                "digest mismatch for {}: expected {} actual {}",
                identity.uri, identity.digest, actual
            ));
        }
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| "skill cache lock poisoned".to_owned())?;
        if let Some(cached) = cache.get(identity) {
            return Ok((Arc::clone(&cached.content), true));
        }
        let content: Arc<str> = Arc::from(content);
        cache.insert(
            identity.clone(),
            CachedSkill {
                content: Arc::clone(&content),
            },
        );
        Ok((content, false))
    }

    #[cfg(test)]
    fn cache_len(&self) -> usize {
        self.cache.lock().map(|cache| cache.len()).unwrap_or(0)
    }
}

/// `builtin_content` is the frozen spec body; `file` entries carry the body
/// captured at discovery so discovery stays metadata-only while `load` is a
/// cheap in-memory read of the SAME bounded file bytes (digest-verified).
#[derive(Debug, Clone)]
struct LocalEntry {
    descriptor: SkillDescriptor,
    content: Arc<str>,
}

impl LocalEntry {
    fn from_builtin(spec: &BuiltinSkillSpec) -> Self {
        Self {
            descriptor: descriptor(spec),
            content: Arc::from(spec.content.trim()),
        }
    }
}

fn descriptor(spec: &BuiltinSkillSpec) -> SkillDescriptor {
    let content = spec.content.trim();
    SkillDescriptor {
        id: spec.id.to_owned(),
        name: spec.id.to_owned(),
        description: spec.description.to_owned(),
        identity: SkillIdentity {
            source_identity: BUILTIN_SOURCE_IDENTITY.to_owned(),
            uri: format!("skill://herdr-mcp/{}", spec.id),
            digest: Digest::from_content(content),
            version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        },
        size: content.len(),
        triggers: strings(spec.triggers),
        requires_capabilities: strings(spec.requires_capabilities),
        related_skills: strings(spec.related_skills),
        risk_domains: strings(spec.risk_domains),
        owned_tools: strings(spec.owned_tools),
    }
}

fn optional_project_root(params: &Value) -> Result<Option<PathBuf>, Value> {
    match params.get("project_root") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => {
            Ok(Some(PathBuf::from(value.trim())))
        }
        Some(Value::String(_)) => Ok(None),
        Some(_) => Err(invalid_params(
            "project_root must be a string when provided",
        )),
    }
}

fn descriptor_json(item: &SkillDescriptor) -> Value {
    json!({
        "id": item.id,
        "name": item.name,
        "description": item.description,
        "source_identity": item.identity.source_identity,
        "uri": item.identity.uri,
        "digest": item.identity.digest.as_str(),
        "version": item.identity.version,
        "size": item.size,
        "triggers": item.triggers,
        "requires_capabilities": item.requires_capabilities,
        "related_skills": item.related_skills,
        "risk_domains": item.risk_domains,
        "owned_tools": item.owned_tools,
    })
}

fn bootstrap_descriptor_json(item: &SkillDescriptor) -> Value {
    json!({
        "id": item.id,
        "description": item.description,
        "source_identity": item.identity.source_identity,
        "uri": item.identity.uri,
        "digest": item.identity.digest.as_str(),
        "bytes": item.size,
        "owned_tools": item.owned_tools,
    })
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn load_evidence_json(evidence: &LoadEvidence) -> Value {
    json!({
        "id": evidence.id,
        "source_identity": evidence.identity.source_identity,
        "uri": evidence.identity.uri,
        "digest": evidence.identity.digest.as_str(),
        "version": evidence.identity.version,
        "bytes": evidence.bytes,
        "cache_hit": evidence.cache_hit,
        "loaded_at": evidence.loaded_at,
    })
}

fn capability_summary_with_inventory(
    snapshot: &Value,
    inventory: &[AgentCapabilityRecord],
) -> Value {
    let visibility = AgentVisibility::from_env();
    let snapshot = project_capabilities_with_inventory(snapshot, &visibility, inventory);
    let mut status_counts = Map::new();
    for worker in &snapshot.workers {
        let count = status_counts
            .get(&worker.current_status)
            .and_then(Value::as_u64)
            .unwrap_or(0);
        status_counts.insert(worker.current_status.clone(), json!(count + 1));
    }
    let verified = json!({
        "provider_known": snapshot.workers.iter().filter(|worker| worker.provider.is_some()).count(),
        "model_known": snapshot.workers.iter().filter(|worker| worker.model.is_some()).count(),
        "code_edit": snapshot.workers.iter().filter(|worker| worker.supports_code_edit == Some(true)).count(),
        "shell": snapshot.workers.iter().filter(|worker| worker.supports_shell == Some(true)).count(),
        "vision": snapshot.workers.iter().filter(|worker| worker.supports_vision == Some(true)).count(),
        "headless": snapshot.workers.iter().filter(|worker| worker.can_run_headless == Some(true)).count(),
    });
    json!({
        "source": snapshot.source,
        "revision": snapshot.revision,
        "worker_count": snapshot.workers.len(),
        "hidden_workers": snapshot.hidden_workers,
        "status_counts": status_counts,
        "verified_capabilities": verified,
        "detail_refresh": "herdr_inspect/herdr_since",
        "capability_refresh": "herdr-mcp scan --probe",
        "unverified_traits": "unknown; never inferred",
    })
}

fn planning_context_with_inventory(
    raw_snapshot: &Value,
    inventory: &[AgentCapabilityRecord],
) -> Value {
    let visibility = AgentVisibility::from_env();
    let snapshot = project_capabilities_with_inventory(raw_snapshot, &visibility, inventory);
    let shown = snapshot
        .workers
        .iter()
        .take(MAX_PLANNING_WORKERS)
        .map(planning_worker_json)
        .collect::<Vec<_>>();

    json!({
        "decision_owner": "web_planner",
        "delegation": "optional",
        "parallelism": "advisory",
        "workers": {
            "total": snapshot.workers.len(),
            "shown": shown.len(),
            "truncated": snapshot.workers.len() > shown.len(),
            "candidates": shown,
            "unknown_traits": "remain unknown; never infer role, quality, cost, or latency from agent name/kind",
        },
        "resources": resource_context_json(raw_snapshot),
        "refresh": {
            "live": "herdr_inspect/herdr_since",
            "capabilities": "herdr-mcp scan --probe",
        },
    })
}

fn planning_worker_json(worker: &WorkerCapability) -> Value {
    json!({
        "agent_id": worker.agent_id,
        "control_target": worker.pane_id.as_deref().unwrap_or(worker.agent_id.as_str()),
        "kind": worker.kind,
        "provider": worker.provider,
        "provider_source": worker.provider_source,
        "model": worker.model,
        "model_source": worker.model_source,
        "status": worker.current_status,
        "project": worker.current_project,
        "verified": {
            "code_edit": worker.supports_code_edit,
            "shell": worker.supports_shell,
            "vision": worker.supports_vision,
            "headless": worker.can_run_headless,
        },
        "observed_traits": {
            "reasoning_tier": worker.reasoning_tier,
            "latency_tier": worker.latency_tier,
            "cost_tier": worker.cost_tier,
            "context_tier": worker.context_tier,
        },
    })
}

fn task_profile_from_params(params: &Value) -> Result<TaskProfile, Value> {
    Ok(TaskProfile {
        deterministic_tool: optional_nonempty_string(params, "deterministic_tool")?,
        project_root: optional_nonempty_string(params, "project_root")?,
        explicit_target: optional_nonempty_string(params, "explicit_target")?,
        requires_code_edit: optional_bool(params, "requires_code_edit")?,
        requires_shell: optional_bool(params, "requires_shell")?,
        requires_vision: optional_bool(params, "requires_vision")?,
        minimum_reasoning_tier: optional_u8(params, "minimum_reasoning_tier")?,
        destructive_production_mutation: optional_bool(params, "destructive_production_mutation")?,
        delegates_other_workers: optional_bool(params, "delegates_other_workers")?,
        independent_units: optional_independent_units(params)?,
        ownership_isolated: optional_bool(params, "ownership_isolated")?,
        shared_runtime_state: optional_bool(params, "shared_runtime_state")?,
    })
}

fn optional_nonempty_string(params: &Value, key: &str) -> Result<Option<String>, Value> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.trim().to_owned())),
        Some(Value::String(_)) => Err(invalid_params(&format!(
            "{key} must be non-empty when provided"
        ))),
        Some(_) => Err(invalid_params(&format!(
            "{key} must be a string when provided"
        ))),
    }
}

fn optional_bounded_nonempty_string(
    params: &Value,
    key: &str,
    max_bytes: usize,
) -> Result<Option<String>, Value> {
    let value = optional_nonempty_string(params, key)?;
    if value
        .as_deref()
        .is_some_and(|value| value.len() > max_bytes || value.chars().any(char::is_control))
    {
        return Err(invalid_params(&format!(
            "{key} must be at most {max_bytes} bytes and contain no control characters"
        )));
    }
    Ok(value)
}

fn optional_bool(params: &Value, key: &str) -> Result<bool, Value> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(invalid_params(&format!(
            "{key} must be a boolean when provided"
        ))),
    }
}

fn optional_u8(params: &Value, key: &str) -> Result<Option<u8>, Value> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|value| u8::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| invalid_params(&format!("{key} must be an integer from 0 to 255"))),
    }
}

fn optional_independent_units(params: &Value) -> Result<Option<usize>, Value> {
    match params.get("independent_units") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|value| (1..=64).contains(value))
            .map(|value| Some(value as usize))
            .ok_or_else(|| invalid_params("independent_units must be an integer from 1 to 64")),
    }
}

fn dispatch_advice_json(advice: &DispatchAdvice) -> Value {
    let candidates = advice
        .candidates
        .iter()
        .take(MAX_PLANNING_WORKERS)
        .map(|candidate| {
            json!({
                "agent_id": candidate.agent_id,
                "control_target": candidate.control_target,
                "kind": candidate.kind,
                "provider": candidate.provider,
                "provider_source": candidate.provider_source,
                "model": candidate.model,
                "model_source": candidate.model_source,
                "status": candidate.current_status,
                "project": candidate.current_project,
                "workspace_id": candidate.workspace_id,
                "observed_traits": {
                    "reasoning_tier": candidate.reasoning_tier,
                    "latency_tier": candidate.latency_tier,
                    "cost_tier": candidate.cost_tier,
                },
            })
        })
        .collect::<Vec<_>>();
    let rejected = advice
        .rejected
        .iter()
        .take(MAX_PLANNING_WORKERS)
        .map(|rejection| {
            json!({
                "agent_id": rejection.agent_id,
                "reason": rejection.reason,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "direct_tool": advice.direct_tool,
        "explicit_target": advice.explicit_target,
        "delegation_allowed": advice.delegation_allowed,
        "reason": advice.reason,
        "candidate_count": advice.candidates.len(),
        "candidates_shown": candidates.len(),
        "candidates_truncated": advice.candidates.len() > candidates.len(),
        "candidates": candidates,
        "rejection_count": advice.rejected.len(),
        "rejections_shown": rejected.len(),
        "rejections_truncated": advice.rejected.len() > rejected.len(),
        "rejected": rejected,
        "parallelism": {
            "worth_considering": advice.parallelism.worth_considering,
            "max_useful_lanes": advice.parallelism.max_useful_lanes,
            "reason": advice.parallelism.reason,
        },
    })
}

fn agent_route_criteria(
    advice: &DispatchAdvice,
    workers: &[WorkerCapability],
    startable_candidates: &Value,
) -> BTreeMap<String, Option<String>> {
    let mut criteria = BTreeMap::new();
    for candidate in advice.candidates.iter().take(MAX_PLANNING_WORKERS) {
        let worker = workers
            .iter()
            .find(|worker| worker.agent_id == candidate.agent_id);
        let id = format!("live:{}", candidate.agent_id);
        let metadata = json!({
            "kind": candidate.kind,
            "provider": candidate.provider,
            "model": candidate.model,
            "verified": {
                "code_edit": worker.and_then(|worker| worker.supports_code_edit),
                "shell": worker.and_then(|worker| worker.supports_shell),
                "vision": worker.and_then(|worker| worker.supports_vision),
                "headless": worker.and_then(|worker| worker.can_run_headless),
            },
            "observed_traits": {
                "reasoning_tier": candidate.reasoning_tier,
                "latency_tier": candidate.latency_tier,
                "cost_tier": candidate.cost_tier,
                "context_tier": worker.and_then(|worker| worker.context_tier),
            },
            "status": candidate.current_status,
        });
        criteria.insert(id, Some(metadata.to_string()));
    }
    for candidate in startable_candidates
        .get("candidates")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_PLANNING_WORKERS)
    {
        let Some(kind) = candidate.get("kind").and_then(Value::as_str) else {
            continue;
        };
        criteria.insert(
            format!("start:{kind}"),
            Some(
                json!({
                    "kind": kind,
                    "provider": candidate.get("provider"),
                    "model": candidate.get("model"),
                    "verified": candidate.get("verified"),
                    "observed_traits": candidate.get("observed_traits"),
                    "status": "not_running",
                })
                .to_string(),
            ),
        );
    }
    criteria
}

fn retain_compatible_agent_routes(
    semantic: &mut Value,
    advice: &DispatchAdvice,
    startable_candidates: &Value,
) {
    let mut compatible = advice
        .candidates
        .iter()
        .map(|candidate| format!("live:{}", candidate.agent_id))
        .collect::<BTreeSet<_>>();
    compatible.extend(
        startable_candidates
            .get("candidates")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|candidate| candidate.get("kind").and_then(Value::as_str))
            .map(|kind| format!("start:{kind}")),
    );
    let Some(agents) = semantic
        .pointer_mut("/routing/agents")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    agents.retain(|route| {
        route
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| compatible.contains(id))
    });
}

fn startable_candidates_json(
    inventory: &[AgentCapabilityRecord],
    visibility: &AgentVisibility,
    snapshot: &Value,
    task: &TaskProfile,
) -> Value {
    let live_kinds = snapshot
        .get("agents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|agent| {
            agent
                .get("agent")
                .or_else(|| agent.get("kind"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let available = inventory
        .iter()
        .filter(|record| {
            record.available_for_start.as_ref().map(|value| value.value) == Some(true)
                && visibility.is_visible(Some(record.agent.as_str()), Some(record.agent.as_str()))
                && !live_kinds.contains(&record.agent)
        })
        .collect::<Vec<_>>();
    let mut candidates = Vec::new();
    let mut rejected = Vec::new();
    let mut evidence_gap = Vec::new();
    for record in available {
        if let Some(reason) = inactive_candidate_reject_reason(record, task) {
            if reason.ends_with("_not_verified") {
                evidence_gap.push(json!({
                    "kind": record.agent,
                    "reason": reason,
                }));
            }
            rejected.push(json!({"kind": record.agent, "reason": reason}));
        } else {
            candidates.push(record);
        }
    }
    let shown = candidates
        .iter()
        .take(MAX_PLANNING_WORKERS)
        .map(|record| {
            json!({
                "kind": record.agent,
                "status": "not_running",
                "start_kind": record.agent,
                "binary_version": record.binary_version.as_ref().map(|value| value.value.as_str()),
                "provider": record.provider.as_ref().map(|value| value.value.as_str()),
                "model": record.model.as_ref().map(|value| value.value.as_str()),
                "verified": {
                    "code_edit": record.supports_code_edit.as_ref().map(|value| value.value),
                    "shell": record.supports_shell.as_ref().map(|value| value.value),
                    "vision": record.supports_vision.as_ref().map(|value| value.value),
                    "headless": record.can_run_headless.as_ref().map(|value| value.value),
                },
                "observed_traits": {
                    "reasoning_tier": record.reasoning_tier.as_ref().map(|value| value.value),
                    "latency_tier": record.latency_tier.as_ref().map(|value| value.value),
                    "cost_tier": record.cost_tier.as_ref().map(|value| value.value),
                    "context_tier": record.context_tier.as_ref().map(|value| value.value),
                },
                "evidence": {
                    "available_for_start": true,
                    "source": record.available_for_start.as_ref().map(|value| value.source.as_str()),
                    "observed_at_ms": record.observed_at_ms,
                },
            })
        })
        .collect::<Vec<_>>();
    let rejected_total = rejected.len();
    let rejected_shown = rejected
        .into_iter()
        .take(MAX_PLANNING_WORKERS)
        .collect::<Vec<_>>();
    let evidence_gap_shown = evidence_gap
        .iter()
        .take(MAX_PLANNING_WORKERS)
        .cloned()
        .collect::<Vec<_>>();
    json!({
        "available_total": candidates.len() + rejected_total,
        "compatible_total": candidates.len(),
        "shown": shown.len(),
        "truncated": candidates.len() > shown.len(),
        "candidates": shown,
        "rejected_total": rejected_total,
        "rejected_shown": rejected_shown.len(),
        "rejected_truncated": rejected_total > rejected_shown.len(),
        "rejected": rejected_shown,
        "evidence_gap": {
            "present": !evidence_gap.is_empty(),
            "count": evidence_gap.len(),
            "shown": evidence_gap_shown.len(),
            "truncated": evidence_gap.len() > evidence_gap_shown.len(),
            "items": evidence_gap_shown,
            "action": if evidence_gap.is_empty() { Value::Null } else { json!("refresh_capability_evidence") },
            "command": if evidence_gap.is_empty() { Value::Null } else { json!("herdr-mcp scan --probe") },
            "meaning": "unverified capability evidence is not proof that an installed/startable Agent is unavailable"
        },
        "meaning": "installed/startable evidence only; planner decides whether creating a new Agent lane is worth the resource cost",
    })
}

fn agent_lifecycle_json(advice: &DispatchAdvice, startable_candidates: &Value) -> Value {
    if advice.direct_tool.is_some() {
        return json!({
            "state": "direct_tool_available",
            "can_proceed": true,
            "requires_human": false,
            "action": "use_direct_tool",
            "authority": "deterministic"
        });
    }
    if advice.delegation_allowed && !advice.candidates.is_empty() {
        return json!({
            "state": "live_agent_available",
            "can_proceed": true,
            "requires_human": false,
            "action": "dispatch_existing_agent",
            "authority": "deterministic",
            "ownership": "do_not_reclaim_foreign_or_user_owned_panes"
        });
    }
    if startable_candidates
        .get("compatible_total")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
    {
        return json!({
            "state": "startable_agent_available",
            "can_proceed": true,
            "requires_human": false,
            "action": "start_agent_then_dispatch",
            "authority": "deterministic",
            "sequence": ["pane.split", "agent.start", "agent.prompt_or_task_dispatch"],
            "pane_policy": "reuse only a verified task-owned free shell pane; otherwise create a planner-owned pane",
            "ownership": "agent:null is not ownership evidence",
            "reclaim": "planner_created_pane_only_after_terminal_and_captured_evidence"
        });
    }
    if startable_candidates
        .pointer("/evidence_gap/present")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return json!({
            "state": "capability_evidence_missing",
            "can_proceed": true,
            "requires_human": false,
            "action": "refresh_capability_evidence",
            "command": "herdr-mcp scan --probe",
            "then": "re-run planning advice; continue direct work instead of treating the Agent surface as unavailable",
            "authority": "deterministic"
        });
    }
    json!({
        "state": "no_compatible_agent",
        "can_proceed": true,
        "requires_human": false,
        "action": "continue_without_agent",
        "authority": "deterministic",
        "meaning": "delegation is optional; absence of a live/startable Agent is not a tool-channel or human boundary"
    })
}

fn inactive_candidate_reject_reason(
    record: &AgentCapabilityRecord,
    task: &TaskProfile,
) -> Option<&'static str> {
    if task.deterministic_tool.is_some() {
        return Some("deterministic_native_tool_available");
    }
    if task.destructive_production_mutation {
        return Some("destructive_production_mutation_not_auto_delegated");
    }
    if task.delegates_other_workers {
        return Some("middle_manager_delegation_forbidden");
    }
    if let Some(target) = task.explicit_target.as_deref()
        && record.agent != target
    {
        return Some("explicit_target_mismatch");
    }
    if task.requires_code_edit
        && record.supports_code_edit.as_ref().map(|value| value.value) != Some(true)
    {
        return Some("code_edit_capability_not_verified");
    }
    if task.requires_shell && record.supports_shell.as_ref().map(|value| value.value) != Some(true)
    {
        return Some("shell_capability_not_verified");
    }
    if task.requires_vision
        && record.supports_vision.as_ref().map(|value| value.value) != Some(true)
    {
        return Some("vision_capability_not_verified");
    }
    if let Some(minimum) = task.minimum_reasoning_tier {
        match record.reasoning_tier.as_ref().map(|value| value.value) {
            Some(actual) if actual >= minimum => {}
            Some(_) => return Some("reasoning_tier_below_requirement"),
            None => return Some("reasoning_tier_not_verified"),
        }
    }
    None
}

fn parent_orchestration_consumption() -> Value {
    json!({
        "authority": {
            "decisions": "semantic advisory only",
            "facts": "deterministic runtime and repository evidence",
            "mutations": "deterministic tools and lifecycle gates"
        },
        "latency_policy": "at most one semantic evaluation per orchestration boundary; never add a semantic RTT to each low-level fs, git, exec, or inspect call",
        "durable_task_source": "prefer herdr_mcp.agent.task.inbox with advisory=true when advertised: deterministic task facts remain authoritative and one bounded existing attention decision is returned for the frozen inbox batch; when unavailable, keep the existing inspect/since plus deterministic task/process evidence path and do not create a second task ledger",
        "boundaries": {
            "plan": {
                "method": PLANNING_ADVISE_METHOD,
                "when": "non-trivial routing or delegation choice",
                "fallback": "existing deterministic direct-tool and capability-filtered planning"
            },
            "attention": {
                "method": AGENT_ATTENTION_ADVISE_METHOD,
                "preferred_entry": "herdr_mcp.agent.task.inbox(advisory=true)",
                "when": "a durable inbox batch contains new unacknowledged terminal facts; active siblings may join the same frozen attention evaluation",
                "states": {
                    "continue_unobserved": "continue independent parent work; do not wait or poll solely for progress",
                    "verify_completion": "collect deterministic diff/status/change evidence, then enter validation",
                    "needs_human": "surface the deterministic human boundary; do not invent input or continue mutation",
                    "blocked_external": "preserve deterministic blocker evidence and continue only independent work",
                    "investigate_drift": "use read-only inspect/read evidence before any correction",
                    "unclear": "use the existing deterministic observation path"
                },
                "fallback": "existing deterministic observation path"
            },
            "validation": {
                "method": VALIDATION_ADVISE_METHOD,
                "when": "deterministic changed-file/symbol evidence has frozen the candidate checks",
                "effect": "reorder frozen checks only; run the most informative relevant check first, then all required deterministic gates",
                "fallback": "preserve and run the complete deterministic candidate set"
            },
            "closeout": {
                "method": AGENT_CLOSEOUT_ADVISE_METHOD,
                "when": "bounded recent child output is still useful after deterministic validation evidence",
                "effect": "classify working/claims-complete/waiting-user/external-block only; never create terminal or reclaim truth",
                "fallback": "use deterministic task/process/validation evidence"
            },
            "cleanup": {
                "method": CLEANUP_PREVIEW_METHOD,
                "params": {"advisory": true},
                "when": "task-owned resources are eligible for closeout review",
                "effect": "rank cleanup inspection only; safe_to_delete and reasons remain deterministic",
                "fallback": "cleanup.preview without semantic advice remains fully usable"
            }
        },
        "semantic_unavailable": "same workflow and required gates remain valid; only advisory ordering/classification is absent"
    })
}

fn resource_context_json(raw_snapshot: &Value) -> Value {
    let panes = raw_snapshot
        .get("panes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let workspaces = raw_snapshot
        .get("workspaces")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let agents = raw_snapshot
        .get("agents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut utility_by_workspace = HashMap::<String, usize>::new();
    for pane in &panes {
        if pane.get("label").and_then(Value::as_str) != Some("herdr-mcp:utility") {
            continue;
        }
        let workspace = pane
            .get("workspace_id")
            .or_else(|| pane.get("workspace"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        *utility_by_workspace.entry(workspace).or_default() += 1;
    }
    let utility_panes = utility_by_workspace.values().copied().sum::<usize>();
    let duplicate_utility_panes = utility_by_workspace
        .values()
        .map(|count| count.saturating_sub(1))
        .sum::<usize>();
    let working_agents = agents
        .iter()
        .filter(|agent| {
            agent
                .get("agent_status")
                .or_else(|| agent.get("status"))
                .and_then(Value::as_str)
                == Some("working")
        })
        .count();
    let reusable_agents = agents
        .iter()
        .filter(|agent| {
            matches!(
                agent
                    .get("agent_status")
                    .or_else(|| agent.get("status"))
                    .and_then(Value::as_str),
                Some("idle" | "done")
            )
        })
        .count();
    let worktree_paths = workspaces
        .iter()
        .filter_map(|workspace| {
            workspace
                .get("worktree")
                .and_then(|worktree| worktree.get("checkout_path"))
                .and_then(Value::as_str)
        })
        .collect::<BTreeSet<_>>();
    json!({
        "workspace_count": workspaces.len(),
        "pane_count": panes.len(),
        "known_worktree_count": worktree_paths.len(),
        "utility_panes": utility_panes,
        "duplicate_utility_panes": duplicate_utility_panes,
        "working_agents": working_agents,
        "reusable_idle_or_done_agents": reusable_agents,
        "reuse_preferred": true,
        "new_lane_requires_task_value": true,
        "cleanup": "planner-owned; no autonomous cleanup daemon",
    })
}

pub(crate) fn agent_attention_advice(params: &Value) -> Value {
    ProgressiveSkillService::new()
        .agent_attention_advise_with_timeout(params, Some(Duration::from_millis(1_500)))
}

fn validate_object_keys(params: &Value, allowed: &[&str]) -> Result<(), Value> {
    let Some(object) = params.as_object() else {
        return Err(invalid_params("params must be an object"));
    };
    let allowed = allowed.iter().copied().collect::<BTreeSet<_>>();
    let unknown = object
        .keys()
        .filter(|key| !allowed.contains(key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(json!({
            "ok": false,
            "code": "invalid_params",
            "message": "unknown local method params",
            "unknown": unknown,
        }))
    }
}

fn invalid_params(message: &str) -> Value {
    json!({"ok": false, "code": "invalid_params", "message": message})
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Value {
        json!({
            "agents": [
                {"agent": "pi", "name": "worker", "agent_status": "idle", "cwd": "/repo", "pane_id": "w1:p1", "workspace_id": "w1", "state_change_seq": 7},
                {"agent": "claude", "agent_status": "idle", "cwd": "/repo", "pane_id": "w1:p2", "workspace_id": "w1", "state_change_seq": 8}
            ]
        })
    }

    fn planning_snapshot() -> Value {
        json!({
            "agents": [
                {"agent": "pi", "name": "worker", "agent_status": "idle", "cwd": "/repo", "pane_id": "w1:p1", "workspace_id": "w1", "state_change_seq": 7},
                {"agent": "reviewer", "name": "reviewer-live", "agent_status": "working", "cwd": "/repo", "pane_id": "w1:p2", "workspace_id": "w1", "state_change_seq": 8}
            ],
            "panes": [
                {"pane_id": "w1:p1", "workspace_id": "w1", "label": "worker"},
                {"pane_id": "w1:p2", "workspace_id": "w1", "label": "herdr-mcp:utility"},
                {"pane_id": "w1:p3", "workspace_id": "w1", "label": "herdr-mcp:utility"}
            ],
            "workspaces": [
                {"workspace_id": "w1", "worktree": {"checkout_path": "/repo"}}
            ]
        })
    }

    fn bool_evidence(value: bool) -> crate::capability_inventory::Evidence<bool> {
        crate::capability_inventory::Evidence {
            value,
            source: "test-scan".to_owned(),
            authority: "test".to_owned(),
            observed_at_ms: 1,
            detail: None,
        }
    }

    fn inventory_record(
        agent: &str,
        startable: bool,
        code_edit: Option<bool>,
        shell: Option<bool>,
    ) -> AgentCapabilityRecord {
        AgentCapabilityRecord {
            schema_version: crate::capability_inventory::INVENTORY_SCHEMA_VERSION,
            agent: agent.to_owned(),
            manifest_version: Some("test".to_owned()),
            manifest_source: Some("test".to_owned()),
            manifest_source_kind: Some("test".to_owned()),
            binary_path: Some(format!("/bin/{agent}")),
            herdr_startable: Some(bool_evidence(startable)),
            executable_available: Some(bool_evidence(startable)),
            available_for_start: Some(bool_evidence(startable)),
            binary_version: None,
            provider: None,
            model: None,
            profile: None,
            supports_code_edit: code_edit.map(bool_evidence),
            supports_shell: shell.map(bool_evidence),
            supports_vision: None,
            reasoning_tier: None,
            latency_tier: None,
            cost_tier: None,
            context_tier: None,
            interactive_only: None,
            can_run_headless: Some(bool_evidence(true)),
            probe_level: crate::capability_inventory::ProbeLevel::Deep,
            probe_adapter_version: 1,
            fingerprint: format!("test:{agent}"),
            observed_at_ms: 1,
        }
    }

    /// Run a closure with HOME pointed at an empty temp dir so local-skill
    /// discovery is hermetic (builtin catalog only) and never reads the real
    /// developer `~/.agents/skills`.
    fn with_isolated_home<T>(run: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env::lock();
        let root = std::env::temp_dir().join(format!(
            "herdr-progressive-skill-home-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let previous = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &root);
        }
        let result = run();
        unsafe {
            match previous {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(root);
        result
    }

    // Write `$HOME/.agents/skills/<name>/SKILL.md` (or a project root variant).
    fn write_user_skill(home: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let dir = home.join(".agents/skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SKILL.md");
        std::fs::write(&path, body).unwrap();
        path
    }

    fn write_project_skill(
        project: &std::path::Path,
        name: &str,
        body: &str,
    ) -> std::path::PathBuf {
        let dir = project.join(".agents/skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("SKILL.md");
        std::fs::write(&path, body).unwrap();
        path
    }

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "herdr-local-skill-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn semantic_test_server(body: &'static str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 64 * 1024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        format!("http://127.0.0.1:{}/v1", address.port())
    }

    fn write_semantic_test_config(config_dir: &std::path::Path, url: &str) {
        let path = config_dir.join("config.json");
        let route_name = config_dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("semantic-test");
        std::fs::write(
            &path,
            format!(
                r#"{{"semantic":{{"routes":[{{"name":"{route_name}","protocol":"decision","url":"{url}","model":"jev-test","api_key":"test"}}]}}}}"#
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn catalog_is_stable_and_covers_all_non_skill_tools_once() {
        let service = ProgressiveSkillService::new();
        let catalog = service.catalog();
        assert_eq!(catalog.len(), 9);
        assert_eq!(catalog[0].id, "workstation-control");
        assert_eq!(catalog[6].id, "development-orchestration");
        assert_eq!(catalog[7].id, "engineering-robustness");
        assert_eq!(catalog[8].id, "requirements-grilling");
        let tools = catalog
            .iter()
            .flat_map(|item| item.owned_tools.iter().cloned())
            .collect::<Vec<_>>();
        assert_eq!(tools.len(), 17);
        assert_eq!(tools.iter().collect::<BTreeSet<_>>().len(), 17);
        assert!(!tools.iter().any(|tool| tool == "herdr_skill"));
    }

    #[test]
    fn identities_include_source_uri_and_digest() {
        let service = ProgressiveSkillService::new();
        for item in service.catalog() {
            assert_eq!(item.identity.source_identity, BUILTIN_SOURCE_IDENTITY);
            assert!(item.identity.uri.starts_with("skill://herdr-mcp/"));
            assert!(item.identity.digest.as_str().starts_with("sha256:"));
        }
    }

    #[test]
    fn workstation_control_exposes_fail_closed_continuity_recovery() {
        let service = ProgressiveSkillService::new();
        let descriptor = service
            .catalog()
            .into_iter()
            .find(|item| item.id == "workstation-control")
            .expect("workstation-control must be in the builtin catalog");
        assert!(
            descriptor
                .triggers
                .iter()
                .any(|trigger| trigger == "continuity")
        );
        let loaded = service
            .local_call(
                LOCAL_LOAD_METHOD,
                &json!({"ids": ["workstation-control"]}),
                &snapshot(),
            )
            .unwrap();
        assert_eq!(loaded["ok"], true);
        let content = loaded["skills"][0]["content"].as_str().unwrap();
        assert!(content.contains("continuity.search"));
        assert!(content.contains("confirmation_required"));
        assert!(content.contains("never choose by recency or textual similarity"));
        assert!(content.contains("A pane and an Agent are separate resources"));
        assert!(content.contains("agent:null"));
        assert!(content.contains("pane.split"));
        assert!(content.contains("agent.start"));
    }

    #[test]
    fn agent_dispatch_is_discoverable_without_host_policy_fallback() {
        let service = ProgressiveSkillService::new();
        let descriptor = service
            .catalog()
            .into_iter()
            .find(|item| item.id == "agent-dispatch")
            .expect("agent-dispatch must be in the builtin catalog");
        assert!(descriptor.description.contains("live capability facts"));
        for forbidden in ["pre-delivery", "host rejection", "safety rejection"] {
            assert!(
                !descriptor
                    .triggers
                    .iter()
                    .any(|trigger| trigger == forbidden)
            );
        }
        let loaded = service
            .local_call(
                LOCAL_LOAD_METHOD,
                &json!({"ids": ["agent-dispatch"]}),
                &snapshot(),
            )
            .unwrap();
        assert_eq!(loaded["ok"], true);
        let content = loaded["skills"][0]["content"].as_str().unwrap();
        assert!(content.contains("External host outcome"));
        assert!(content.contains("no Herdr execution identity or result fields"));
        assert!(content.contains("Host policy is external to Agent Dispatch"));
        assert!(content.contains("pre-dispatch condition"));
        assert!(content.contains("delivery_state=not_delivered"));
        assert!(content.contains("requires_human=false"));
        assert!(content.contains("startable_candidates.evidence_gap.present=true"));
        assert!(content.contains("never proves planner ownership"));
        for forbidden in [
            "one bounded, identical retry",
            "existing compatible local Agent",
            "fallback chain stops",
        ] {
            assert!(!content.contains(forbidden));
        }
    }

    #[test]
    fn engineering_robustness_reference_is_discoverable_and_loadable() {
        let service = ProgressiveSkillService::new();
        let descriptor = service
            .catalog()
            .into_iter()
            .find(|item| item.id == "engineering-robustness")
            .expect("engineering robustness reference must be in the builtin catalog");
        assert!(
            descriptor
                .triggers
                .iter()
                .any(|trigger| trigger == "bug fix")
        );
        assert!(
            descriptor
                .triggers
                .iter()
                .any(|trigger| trigger == "release")
        );
        let loaded = service
            .local_call(
                LOCAL_LOAD_METHOD,
                &json!({"ids": ["engineering-robustness"]}),
                &snapshot(),
            )
            .unwrap();
        assert_eq!(loaded["ok"], true);
        let content = loaded["skills"][0]["content"].as_str().unwrap();
        assert!(content.contains("silent wrongness"));
        assert!(content.contains("Turn every real bug into a durable asset"));
        assert!(content.contains("Verify state planes separately"));
        assert!(content.contains("minimum-entity rule"));
    }

    #[test]
    fn discovery_does_not_load_and_batched_load_hits_immutable_cache() {
        with_isolated_home(|| {
            let service = ProgressiveSkillService::new();
            let listed = service
                .local_call(LOCAL_LIST_METHOD, &json!({}), &snapshot())
                .unwrap();
            assert_eq!(listed["count"], 9);
            assert_eq!(service.cache_len(), 0);
            let first = service
                .local_call(
                    LOCAL_LOAD_METHOD,
                    &json!({"ids": ["files-search", "git-repository"]}),
                    &snapshot(),
                )
                .unwrap();
            assert_eq!(first["ok"], true);
            assert_eq!(first["skills"][0]["id"], "files-search");
            assert_eq!(first["skills"][1]["id"], "git-repository");
            assert_eq!(first["skills"][0]["cache_hit"], false);
            assert_eq!(first["skills"][1]["cache_hit"], false);
            let second = service
                .local_call(
                    LOCAL_LOAD_METHOD,
                    &json!({"ids": ["files-search", "git-repository"]}),
                    &snapshot(),
                )
                .unwrap();
            assert_eq!(second["skills"][0]["cache_hit"], true);
            assert_eq!(second["skills"][1]["cache_hit"], true);
            assert_eq!(second["authorization"], "none");
        })
    }

    #[test]
    fn a_new_capability_domain_only_populates_one_additional_cache_entry() {
        let service = ProgressiveSkillService::new();
        let first = service
            .local_call(
                LOCAL_LOAD_METHOD,
                &json!({"ids": ["files-search"]}),
                &snapshot(),
            )
            .unwrap();
        assert_eq!(first["skills"][0]["cache_hit"], false);
        assert_eq!(service.cache_len(), 1);

        let second = service
            .local_call(
                LOCAL_LOAD_METHOD,
                &json!({"ids": ["execution"]}),
                &snapshot(),
            )
            .unwrap();
        assert_eq!(second["skills"][0]["id"], "execution");
        assert_eq!(second["skills"][0]["cache_hit"], false);
        assert_eq!(service.cache_len(), 2);

        let repeated = service
            .local_call(
                LOCAL_LOAD_METHOD,
                &json!({"ids": ["files-search"]}),
                &snapshot(),
            )
            .unwrap();
        assert_eq!(repeated["skills"][0]["cache_hit"], true);
        assert_eq!(service.cache_len(), 2);
    }

    #[test]
    fn live_capability_refresh_does_not_reload_or_change_skill_text_identity() {
        let service = ProgressiveSkillService::new();
        let first = service
            .local_call(
                LOCAL_LOAD_METHOD,
                &json!({"ids": ["agent-dispatch"]}),
                &snapshot(),
            )
            .unwrap();
        let digest = first["skills"][0]["digest"].clone();
        assert_eq!(service.cache_len(), 1);

        let changed_snapshot = json!({
            "agents": [{
                "agent": "pi",
                "name": "worker",
                "agent_status": "working",
                "cwd": "/repo",
                "pane_id": "w2:p9",
                "workspace_id": "w2",
                "state_change_seq": 99
            }]
        });
        let bootstrap = service.bootstrap_with_inventory(&changed_snapshot, &[]);
        assert_eq!(bootstrap["capability_snapshot"]["revision"], 99);
        assert_eq!(service.cache_len(), 1);

        let repeated = service
            .local_call(
                LOCAL_LOAD_METHOD,
                &json!({"ids": ["agent-dispatch"]}),
                &changed_snapshot,
            )
            .unwrap();
        assert_eq!(repeated["skills"][0]["digest"], digest);
        assert_eq!(repeated["skills"][0]["cache_hit"], true);
        assert_eq!(service.cache_len(), 1);
    }

    #[test]
    fn digest_mismatch_fails_closed() {
        let service = ProgressiveSkillService::new();
        let result = service
            .local_call(
                LOCAL_LOAD_METHOD,
                &json!({
                    "ids": ["files-search"],
                    "expected_digests": {"files-search": "sha256:wrong"}
                }),
                &snapshot(),
            )
            .unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "skill_digest_mismatch");
        assert_eq!(service.cache_len(), 0);
    }

    #[test]
    fn same_uri_and_digest_from_different_sources_do_not_collide() {
        let service = ProgressiveSkillService::new();
        let content = "same";
        let first = SkillIdentity {
            source_identity: "source-a".to_owned(),
            uri: "skill://same/name".to_owned(),
            digest: Digest::from_content(content),
            version: None,
        };
        let second = SkillIdentity {
            source_identity: "source-b".to_owned(),
            ..first.clone()
        };
        assert!(!service.load_verified(&first, content).unwrap().1);
        assert!(!service.load_verified(&second, content).unwrap().1);
        assert_eq!(service.cache_len(), 2);
    }

    #[test]
    fn changed_content_changes_digest() {
        assert_ne!(Digest::from_content("one"), Digest::from_content("two"));
    }

    #[test]
    fn bootstrap_exposes_agents_catalog_and_load_schema_without_skill_bodies() {
        let service = ProgressiveSkillService::new();
        let result = service.bootstrap_with_inventory(&snapshot(), &[]);
        assert_eq!(result["ok"], true);
        assert_eq!(result["mode"], "progressive");
        assert_eq!(result["catalog"].as_array().unwrap().len(), 9);
        assert_eq!(result["load"]["method"], LOCAL_LOAD_METHOD);
        assert_eq!(result["planning_advice"]["method"], PLANNING_ADVISE_METHOD);
        assert_eq!(result["planning_advice"]["decision_owner"], "web_planner");
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("# Herdr Global AGENTS.md"));
        assert!(content.contains("compact `catalog` field"));
        assert!(content.contains("A new user turn does not reload it"));
        assert!(content.contains("engineering-robustness"));
        assert!(!content.contains("# Engineering Robustness Reference"));
        assert!(!content.contains("# Files Mutation"));
        assert!(!content.contains("# Agent Dispatch"));
        assert_eq!(result["capability_snapshot"]["worker_count"], 2);
        assert_eq!(
            result["capability_snapshot"]["detail_refresh"],
            "herdr_inspect/herdr_since"
        );
        assert_eq!(
            result["capability_snapshot"]["capability_refresh"],
            "herdr-mcp scan --probe"
        );
        assert_eq!(
            result["capability_snapshot"]["verified_capabilities"]["code_edit"],
            0
        );
        assert_eq!(result["planning_context"]["decision_owner"], "web_planner");
        assert_eq!(result["planning_context"]["delegation"], "optional");
        assert_eq!(result["planning_context"]["parallelism"], "advisory");
        assert_eq!(result["planning_context"]["workers"]["total"], 2);
        assert_eq!(
            result["planning_context"]["workers"]["candidates"][0]["control_target"],
            "w1:p1"
        );
    }

    #[test]
    fn planning_advice_exposes_live_and_inactive_candidates_without_selecting_one() {
        let service = ProgressiveSkillService::new();
        let inventory = vec![
            inventory_record("pi", true, Some(true), Some(true)),
            inventory_record("builder", true, Some(true), Some(true)),
            inventory_record("unknown-worker", true, None, None),
        ];
        let result = service.planning_advise_method_with_inventory(
            &json!({
                "project_root": "/repo",
                "requires_code_edit": true,
                "requires_shell": true,
                "independent_units": 2,
                "ownership_isolated": true
            }),
            &planning_snapshot(),
            &inventory,
        );
        assert_eq!(result["ok"], true);
        assert_eq!(result["decision_owner"], "web_planner");
        assert_eq!(result["advice"]["delegation_allowed"], true);
        assert_eq!(result["advice"]["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(result["advice"]["candidates"][0]["agent_id"], "worker");
        assert_eq!(result["advice"]["candidates"][0]["control_target"], "w1:p1");
        assert_eq!(result["advice"]["candidates"][0]["status"], "idle");
        assert_eq!(result["advice"]["parallelism"]["worth_considering"], true);
        assert_eq!(result["advice"]["parallelism"]["max_useful_lanes"], 2);
        assert_eq!(result["context_resolution"]["order"][0], "device");
        assert_eq!(
            result["orchestration_policy"]["levels"]["minimum_entities"],
            "required"
        );
        assert_eq!(
            result["orchestration_policy"]["levels"]["parallelism"],
            "advisory"
        );
        assert_eq!(
            result["orchestration_policy"]["parallelism"]["max_useful_lanes"],
            2
        );
        assert_eq!(
            result["orchestration_policy"]["levels"]["reclamation"],
            "required_for_planner_created_resources"
        );
        assert_eq!(
            result["orchestration_policy"]["consumption"]["boundaries"]["attention"]["method"],
            AGENT_ATTENTION_ADVISE_METHOD
        );
        assert_eq!(
            result["orchestration_policy"]["consumption"]["boundaries"]["validation"]["effect"],
            "reorder frozen checks only; run the most informative relevant check first, then all required deterministic gates"
        );
        assert_eq!(
            result["orchestration_policy"]["consumption"]["boundaries"]["cleanup"]["params"]["advisory"],
            true
        );
        assert_eq!(
            result["requirements_resolution"]["question_mode"],
            "one_at_a_time"
        );
        assert_eq!(result["startable_candidates"]["available_total"], 2);
        assert_eq!(result["startable_candidates"]["compatible_total"], 1);
        assert_eq!(result["startable_candidates"]["rejected_total"], 1);
        assert_eq!(
            result["startable_candidates"]["candidates"][0]["kind"],
            "builder"
        );
        assert!(
            result["startable_candidates"]["rejected"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["kind"] == "unknown-worker"
                    && item["reason"] == "code_edit_capability_not_verified")
        );
        assert_eq!(result["agent_lifecycle"]["state"], "live_agent_available");
        assert_eq!(
            result["agent_lifecycle"]["action"],
            "dispatch_existing_agent"
        );
        assert_eq!(result["agent_lifecycle"]["requires_human"], false);
        assert_eq!(result["resource_context"]["duplicate_utility_panes"], 1);
        assert_eq!(result["resource_context"]["working_agents"], 1);
        assert_eq!(
            result["resource_context"]["reusable_idle_or_done_agents"],
            1
        );
        assert!(result["advice"].get("selected_target").is_none());
    }

    #[test]
    fn planning_no_live_worker_with_startable_agent_is_not_a_stop_condition() {
        let service = ProgressiveSkillService::new();
        let inventory = vec![inventory_record("pi", true, Some(true), Some(true))];
        let snapshot = json!({
            "agents": [],
            "panes": [
                {"pane_id": "w1:p1", "workspace_id": "w1", "label": "user-shell"},
                {"pane_id": "w1:p2", "workspace_id": "w1", "label": "herdr-mcp:utility"}
            ],
            "workspaces": [
                {"workspace_id": "w1", "worktree": {"checkout_path": "/repo"}}
            ]
        });
        let result = service.planning_advise_method_with_inventory(
            &json!({
                "project_root": "/repo",
                "requires_code_edit": true,
                "requires_shell": true,
                "independent_units": 1,
                "ownership_isolated": true
            }),
            &snapshot,
            &inventory,
        );
        assert_eq!(result["advice"]["delegation_allowed"], false);
        assert_eq!(result["advice"]["reason"], "no_compatible_live_worker");
        assert_eq!(result["startable_candidates"]["compatible_total"], 1);
        assert_eq!(
            result["startable_candidates"]["candidates"][0]["kind"],
            "pi"
        );
        assert_eq!(
            result["agent_lifecycle"]["state"],
            "startable_agent_available"
        );
        assert_eq!(
            result["agent_lifecycle"]["action"],
            "start_agent_then_dispatch"
        );
        assert_eq!(result["agent_lifecycle"]["can_proceed"], true);
        assert_eq!(result["agent_lifecycle"]["requires_human"], false);
        assert_eq!(
            result["agent_lifecycle"]["sequence"],
            json!(["pane.split", "agent.start", "agent.prompt_or_task_dispatch"])
        );
        assert_eq!(
            result["agent_lifecycle"]["ownership"],
            "agent:null is not ownership evidence"
        );
    }

    #[test]
    fn planning_missing_capability_evidence_is_not_agent_unavailable() {
        let service = ProgressiveSkillService::new();
        let inventory = vec![inventory_record("pi", true, None, None)];
        let result = service.planning_advise_method_with_inventory(
            &json!({
                "project_root": "/repo",
                "requires_code_edit": true,
                "requires_shell": true,
                "independent_units": 1,
                "ownership_isolated": true
            }),
            &json!({
                "agents": [],
                "panes": [],
                "workspaces": [{"workspace_id": "w1", "worktree": {"checkout_path": "/repo"}}]
            }),
            &inventory,
        );
        assert_eq!(result["startable_candidates"]["compatible_total"], 0);
        assert_eq!(
            result["startable_candidates"]["evidence_gap"]["present"],
            true
        );
        assert_eq!(
            result["startable_candidates"]["evidence_gap"]["action"],
            "refresh_capability_evidence"
        );
        assert_eq!(
            result["agent_lifecycle"]["state"],
            "capability_evidence_missing"
        );
        assert_eq!(
            result["agent_lifecycle"]["action"],
            "refresh_capability_evidence"
        );
        assert_eq!(result["agent_lifecycle"]["requires_human"], false);
    }

    #[test]
    fn planning_agent_route_is_advisory_and_falls_back_to_deterministic_candidates() {
        let service = ProgressiveSkillService::new();
        let inventory = vec![
            inventory_record("pi", true, Some(true), Some(true)),
            inventory_record("builder", true, Some(true), Some(true)),
        ];
        let params = json!({
            "task_text": "Implement and verify the requested Rust change.",
            "project_root": "/repo",
            "requires_code_edit": true,
            "requires_shell": true,
            "requires_vision": false,
            "destructive_production_mutation": false,
            "delegates_other_workers": false,
            "shared_runtime_state": false,
            "independent_units": 2,
            "ownership_isolated": true
        });

        let no_semantic = SemanticService::test_empty();
        let no_config = service.planning_advise_method_with_inventory_and_semantic(
            &params,
            &planning_snapshot(),
            &inventory,
            Some(&no_semantic),
        );
        assert_eq!(no_config["semantic"]["used"], false);
        assert_eq!(no_config["semantic"]["reason"], "not_configured");
        assert_eq!(no_config["advice"]["candidates"][0]["agent_id"], "worker");
        assert_eq!(
            no_config["startable_candidates"]["candidates"][0]["kind"],
            "builder"
        );
        let deterministic_advice = no_config["advice"].clone();
        let deterministic_startable = no_config["startable_candidates"].clone();
        let deterministic_lifecycle = no_config["agent_lifecycle"].clone();

        use std::io::{ErrorKind, Read, Write};
        use std::net::TcpListener;
        fn repeat_semantic_server(body: &'static str) -> u16 {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while std::time::Instant::now() < deadline {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let mut request = [0_u8; 64 * 1024];
                            let _ = stream.read(&mut request);
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = stream.write_all(response.as_bytes());
                        }
                        Err(error) if error.kind() == ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });
            port
        }
        let port = repeat_semantic_server(
            r#"{"model":"jev-test","answers":{"requires_code_edit":{"type":"noul","noul":0.9},"requires_shell":{"type":"noul","noul":0.9},"requires_vision":{"type":"noul","noul":0.1},"destructive_production_mutation":{"type":"noul","noul":0.1},"delegates_other_workers":{"type":"noul","noul":0.1},"shared_runtime_state":{"type":"noul","noul":0.1},"independent_units":{"type":"choice","choice":"two","probabilities":{"two":0.9},"confidence":0.9},"reasoning_tier":{"type":"score","score":1.0,"probabilities":{"1":0.9},"confidence":0.9},"skill_route":{"type":"choice","choice":"agent-dispatch","probabilities":{"agent-dispatch":0.9},"confidence":0.9},"method_route":{"type":"choice","choice":"herdr_mcp.agent.closeout.advise","probabilities":{"herdr_mcp.agent.closeout.advise":0.9},"confidence":0.9},"agent_route":{"type":"choice","choice":"live:worker","probabilities":{"live:worker":0.9,"start:builder":0.1},"confidence":0.9}}}"#,
        );
        let configured_semantic = SemanticService::test_decision_route(
            "planning-agent-route",
            &format!("http://127.0.0.1:{port}/v1"),
        )
        .unwrap();
        let configured = service.planning_advise_method_with_inventory_and_semantic(
            &params,
            &planning_snapshot(),
            &inventory,
            Some(&configured_semantic),
        );
        assert_eq!(configured["semantic"]["used"], true);
        assert_eq!(
            configured["semantic"]["routing"]["agents"][0]["id"],
            "live:worker"
        );
        assert_eq!(configured["advice"], deterministic_advice);
        assert_eq!(configured["startable_candidates"], deterministic_startable);
        assert_eq!(configured["agent_lifecycle"], deterministic_lifecycle);

        let invalid_port = repeat_semantic_server(
            r#"{"model":"jev-test","answers":{"requires_code_edit":{"type":"noul","noul":0.9},"requires_shell":{"type":"noul","noul":0.9},"requires_vision":{"type":"noul","noul":0.1},"destructive_production_mutation":{"type":"noul","noul":0.1},"delegates_other_workers":{"type":"noul","noul":0.1},"shared_runtime_state":{"type":"noul","noul":0.1},"independent_units":{"type":"choice","choice":"two","probabilities":{"two":0.9},"confidence":0.9},"reasoning_tier":{"type":"score","score":1.0,"probabilities":{"1":0.9},"confidence":0.9},"skill_route":{"type":"choice","choice":"agent-dispatch","probabilities":{"agent-dispatch":0.9},"confidence":0.9},"method_route":{"type":"choice","choice":"herdr_mcp.agent.closeout.advise","probabilities":{"herdr_mcp.agent.closeout.advise":0.9},"confidence":0.9},"agent_route":{"type":"choice","choice":"start:forbidden","probabilities":{"live:worker":0.9,"start:builder":0.1},"confidence":0.9}}}"#,
        );
        let invalid_semantic = SemanticService::test_decision_route(
            "planning-agent-route-invalid",
            &format!("http://127.0.0.1:{invalid_port}/v1"),
        )
        .unwrap();
        let invalid_route = service.planning_advise_method_with_inventory_and_semantic(
            &params,
            &planning_snapshot(),
            &inventory,
            Some(&invalid_semantic),
        );
        assert_eq!(
            invalid_route["semantic"]["used"], true,
            "semantic={}",
            invalid_route["semantic"]
        );
        assert!(
            invalid_route["semantic"]["routing"]["agents"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(invalid_route["advice"], deterministic_advice);
        assert_eq!(
            invalid_route["startable_candidates"],
            deterministic_startable
        );
        assert_eq!(invalid_route["agent_lifecycle"], deterministic_lifecycle);

        let offline_semantic =
            SemanticService::test_decision_route("offline", "http://127.0.0.1:1/v1").unwrap();
        let provider_error = service.planning_advise_method_with_inventory_and_semantic(
            &params,
            &planning_snapshot(),
            &inventory,
            Some(&offline_semantic),
        );
        assert_eq!(provider_error["semantic"]["used"], false);
        assert_ne!(provider_error["semantic"]["reason"], "not_configured");
        assert_eq!(provider_error["advice"], deterministic_advice);
        assert_eq!(
            provider_error["startable_candidates"],
            deterministic_startable
        );
        assert_eq!(provider_error["agent_lifecycle"], deterministic_lifecycle);
    }

    #[test]
    fn planning_semantic_inference_is_explicitly_advisory_and_never_a_stop_gate() {
        const ANSWER: &str = r#"{"model":"jev-test","answers":{"requires_code_edit":{"type":"noul","noul":0.9},"requires_shell":{"type":"noul","noul":0.1},"requires_vision":{"type":"noul","noul":0.1},"destructive_production_mutation":{"type":"noul","noul":0.1},"delegates_other_workers":{"type":"noul","noul":0.1},"shared_runtime_state":{"type":"noul","noul":0.1},"independent_units":{"type":"choice","choice":"one","probabilities":{"one":0.9},"confidence":0.9},"reasoning_tier":{"type":"score","score":1.0,"probabilities":{"1":0.9},"confidence":0.9},"skill_route":{"type":"choice","choice":"agent-dispatch","probabilities":{"agent-dispatch":0.9},"confidence":0.9},"method_route":{"type":"choice","choice":"herdr_mcp.agent.closeout.advise","probabilities":{"herdr_mcp.agent.closeout.advise":0.9},"confidence":0.9},"agent_route":{"type":"choice","choice":"start:codex","probabilities":{"start:codex":0.9},"confidence":0.9}}}"#;
        const UNCERTAIN_ANSWER: &str = r#"{"model":"jev-test","answers":{"requires_code_edit":{"type":"noul","noul":0.5},"requires_shell":{"type":"noul","noul":0.5},"requires_vision":{"type":"noul","noul":0.5},"destructive_production_mutation":{"type":"noul","noul":0.5},"delegates_other_workers":{"type":"noul","noul":0.5},"shared_runtime_state":{"type":"noul","noul":0.5},"independent_units":{"type":"choice","choice":"one","probabilities":{"one":0.45,"two":0.35,"three":0.2},"confidence":0.45},"reasoning_tier":{"type":"score","score":1.0,"probabilities":{"0":0.34,"1":0.33,"2":0.33},"confidence":0.34},"skill_route":{"type":"choice","choice":"agent-dispatch","probabilities":{"agent-dispatch":0.4},"confidence":0.4},"method_route":{"type":"choice","choice":"herdr_mcp.agent.closeout.advise","probabilities":{"herdr_mcp.agent.closeout.advise":0.4},"confidence":0.4},"agent_route":{"type":"choice","choice":"start:codex","probabilities":{"start:codex":0.4},"confidence":0.4}}}"#;
        let service = ProgressiveSkillService::new();
        let inventory = vec![inventory_record("codex", true, None, Some(true))];
        let partial = json!({"task_text": "Review the change set.", "project_root": "/repo"});

        let unavailable = service.planning_advise_method_with_inventory_and_semantic(
            &partial,
            &json!({"agents": [], "panes": [], "workspaces": []}),
            &inventory,
            Some(&SemanticService::test_empty()),
        );
        assert_eq!(unavailable["semantic"]["advisory_only"], true);
        assert_eq!(
            unavailable["determinism"]["semantic_inferred_requirements"],
            json!([])
        );
        assert_eq!(
            unavailable["determinism"]["zero_candidates_blocks_execution"],
            false
        );
        assert_eq!(unavailable["agent_lifecycle"]["can_proceed"], true);
        assert_eq!(unavailable["agent_lifecycle"]["requires_human"], false);

        let timeout = service.planning_advise_method_with_inventory_and_semantic(
            &partial,
            &json!({"agents": [], "panes": [], "workspaces": []}),
            &inventory,
            Some(&SemanticService::test_error("planning-timeout", "timeout")),
        );
        assert_eq!(timeout["semantic"]["used"], false);
        assert_eq!(timeout["semantic"]["advisory_only"], true);
        assert_eq!(timeout["semantic"]["reason"], "timeout");
        assert_eq!(
            timeout["determinism"]["semantic_inferred_requirements"],
            json!([])
        );
        assert_eq!(timeout["agent_lifecycle"]["can_proceed"], true);
        assert_eq!(timeout["agent_lifecycle"]["requires_human"], false);
        assert_eq!(
            timeout["agent_lifecycle"]["action"],
            "start_agent_then_dispatch"
        );

        let uncertain_url = semantic_test_server(UNCERTAIN_ANSWER);
        let uncertain_semantic =
            SemanticService::test_decision_route("planning-uncertain", &uncertain_url).unwrap();
        let uncertain = service.planning_advise_method_with_inventory_and_semantic(
            &partial,
            &json!({"agents": [], "panes": [], "workspaces": []}),
            &inventory,
            Some(&uncertain_semantic),
        );
        assert_eq!(uncertain["semantic"]["used"], true);
        assert_eq!(uncertain["semantic"]["advisory_only"], true);
        assert_eq!(
            uncertain["determinism"]["semantic_inferred_requirements"],
            json!([])
        );
        assert_eq!(uncertain["agent_lifecycle"]["can_proceed"], true);
        assert_eq!(uncertain["agent_lifecycle"]["requires_human"], false);
        assert_eq!(
            uncertain["agent_lifecycle"]["action"],
            "start_agent_then_dispatch"
        );

        let url = semantic_test_server(ANSWER);
        let semantic = SemanticService::test_decision_route("planning-inference", &url).unwrap();
        let inferred = service.planning_advise_method_with_inventory_and_semantic(
            &partial,
            &json!({"agents": [], "panes": [], "workspaces": []}),
            &inventory,
            Some(&semantic),
        );
        assert_eq!(inferred["semantic"]["advisory_only"], true);
        assert_eq!(
            inferred["determinism"]["semantic_inferred_requirements"],
            json!(["requires_code_edit"])
        );
        assert_eq!(
            inferred["determinism"]["advice_basis"],
            "planner_requirements_plus_semantic_inferred"
        );
        assert_eq!(inferred["startable_candidates"]["compatible_total"], 0);
        assert_eq!(inferred["agent_lifecycle"]["requires_human"], false);
        assert_eq!(inferred["agent_lifecycle"]["can_proceed"], true);
        assert_eq!(
            inferred["agent_lifecycle"]["action"],
            "refresh_capability_evidence"
        );
        assert_eq!(
            inferred["determinism"]["zero_candidates_blocks_execution"],
            false
        );

        let explicit_url = semantic_test_server(ANSWER);
        let explicit_semantic =
            SemanticService::test_decision_route("planning-explicit", &explicit_url).unwrap();
        let explicit = service.planning_advise_method_with_inventory_and_semantic(
            &json!({
                "task_text": "Review the change set.",
                "project_root": "/repo",
                "requires_code_edit": false
            }),
            &json!({"agents": [], "panes": [], "workspaces": []}),
            &inventory,
            Some(&explicit_semantic),
        );
        assert_eq!(explicit["semantic"]["advisory_only"], true);
        assert_eq!(
            explicit["determinism"]["semantic_inferred_requirements"],
            json!([])
        );
        assert_eq!(
            explicit["determinism"]["planner_supplied_requirements"],
            json!(["requires_code_edit"])
        );
        assert_eq!(explicit["startable_candidates"]["compatible_total"], 1);
        assert_eq!(
            explicit["agent_lifecycle"]["action"],
            "start_agent_then_dispatch"
        );
        assert_eq!(SEMANTIC_INFERABLE_KEYS.len(), 6);
    }

    #[test]
    fn planning_advice_parameter_validation_fails_closed() {
        let service = ProgressiveSkillService::new();
        let result = service.planning_advise_method_with_inventory(
            &json!({"requires_shell": "yes"}),
            &planning_snapshot(),
            &[],
        );
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "invalid_params");

        let result = service.planning_advise_method_with_inventory(
            &json!({"independent_units": 0}),
            &planning_snapshot(),
            &[],
        );
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "invalid_params");
    }

    #[test]
    fn local_method_discovery_exposes_planning_schema() {
        let methods = local_method_schemas("planning");
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0]["method"], PLANNING_ADVISE_METHOD);
        assert_eq!(methods[0]["source"], "herdr_mcp_local");
        assert_eq!(methods[0]["access"], "read_only");
        assert_eq!(methods[0]["advisory_only"], true);
        assert_eq!(
            methods[0]["params"]["properties"]["independent_units"]["maximum"],
            64
        );

        let methods = local_method_schemas("text.");
        assert_eq!(methods.len(), 2);
        assert_eq!(methods[0]["method"], TEXT_READ_METHOD);
        assert_eq!(methods[1]["method"], TEXT_WRITE_METHOD);
        assert_eq!(methods[0]["source"], "herdr_mcp_local");

        let methods = local_method_schemas("github");
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0]["method"], GITHUB_STATUS_METHOD);
        assert_eq!(methods[0]["params"]["required"][0], "project_root");
        assert_eq!(
            methods[0]["params"]["properties"]["repository"]["type"],
            "string"
        );
        assert_eq!(
            methods[0]["params"]["properties"]["wait_ms"]["maximum"],
            20_000
        );

        let methods = local_method_schemas("exec.wait");
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0]["method"], EXEC_WAIT_METHOD);
        assert_eq!(methods[0]["source"], "herdr_mcp_local");
        assert_eq!(
            methods[0]["params"]["properties"]["timeout_ms"]["maximum"],
            20_000
        );

        let methods = local_method_schemas("cleanup");
        assert_eq!(methods.len(), 1);
        assert_eq!(methods[0]["method"], CLEANUP_PREVIEW_METHOD);
        assert_eq!(methods[0]["source"], "herdr_mcp_local");
        assert_eq!(methods[0]["params"]["required"], json!(["project_root"]));
        assert_eq!(
            methods[0]["params"]["properties"]["target_ref"]["type"],
            "string"
        );

        let service = ProgressiveSkillService::new();
        let bootstrap = service.bootstrap_with_inventory(&planning_snapshot(), &[]);
        assert_eq!(
            bootstrap["request_budget"]["default_strategy"],
            "coalesce logical work into the fewest high-value supported calls"
        );
        assert_eq!(
            bootstrap["request_budget"]["planning_gate"],
            "before the first remote call, derive the next dependency-aware call wave from facts already known"
        );
        assert_eq!(
            bootstrap["request_budget"]["call_admission"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            bootstrap["request_budget"]["default_shape"],
            "baseline -> independent read wave -> execution bundle -> verification wave -> event/delta follow-up only when change is expected"
        );
        assert_eq!(
            bootstrap["request_budget"]["capability_source"],
            "live runtime context is authoritative for JSON-RPC batch, multi-operation arguments, and concurrency"
        );
        assert_eq!(
            bootstrap["orchestration_consumption"]["authority"]["decisions"],
            "semantic advisory only"
        );
        assert_eq!(
            bootstrap["orchestration_consumption"]["boundaries"]["attention"]["method"],
            AGENT_ATTENTION_ADVISE_METHOD
        );
        assert_eq!(
            bootstrap["orchestration_consumption"]["boundaries"]["attention"]["states"]["continue_unobserved"],
            "continue independent parent work; do not wait or poll solely for progress"
        );
        assert_eq!(
            bootstrap["orchestration_consumption"]["boundaries"]["validation"]["method"],
            VALIDATION_ADVISE_METHOD
        );
        assert_eq!(
            bootstrap["orchestration_consumption"]["boundaries"]["closeout"]["method"],
            AGENT_CLOSEOUT_ADVISE_METHOD
        );
        assert_eq!(
            bootstrap["orchestration_consumption"]["boundaries"]["cleanup"]["params"]["advisory"],
            true
        );
        assert_eq!(
            bootstrap["orchestration_consumption"]["semantic_unavailable"],
            "same workflow and required gates remain valid; only advisory ordering/classification is absent"
        );

        let methods = local_method_schemas("work_memory.");
        assert_eq!(methods.len(), 6);
        assert_eq!(methods[0]["method"], WORK_MEMORY_BIND_METHOD);
        assert_eq!(methods[0]["schema_version"], 1);
        assert_eq!(methods[3]["method"], WORK_MEMORY_CHECKPOINT_PUT_METHOD);
        assert_eq!(
            methods[3]["params"]["anyOf"],
            json!([
                {
                    "properties": {
                        "through_message_id": {"type": "string", "minLength": 1, "maxLength": 512}
                    },
                    "required": ["through_message_id"]
                },
                {
                    "properties": {
                        "through_evidence_id": {"type": "string", "minLength": 1, "maxLength": 128}
                    },
                    "required": ["through_evidence_id"]
                }
            ])
        );
        assert_eq!(methods[4]["method"], WORK_MEMORY_RESUME_METHOD);
        assert_eq!(
            methods[4]["params"]["required"],
            json!(["project_ref", "repo_id", "work_chain_id"])
        );
        assert_eq!(methods[5]["method"], WORK_MEMORY_SEARCH_METHOD);
        assert_eq!(methods[5]["schema_version"], 2);
        assert_eq!(methods[5]["params"]["required"], json!([]));
        assert_eq!(
            methods[5]["params"]["properties"]["cursor"]["maxLength"],
            4096
        );
        assert_eq!(methods[5]["params"]["oneOf"].as_array().unwrap().len(), 2);

        let methods = local_method_schemas("herdr_mcp.browser_");
        assert_eq!(methods.len(), 20);
        assert_eq!(methods[0]["method"], BROWSER_ENDPOINT_LIST_METHOD);
        assert_eq!(methods[1]["method"], BROWSER_ENDPOINT_INSPECT_METHOD);
        assert_eq!(methods[2]["method"], BROWSER_RESOURCE_LIST_METHOD);
        assert_eq!(methods[3]["method"], BROWSER_RESOURCE_INSPECT_METHOD);
        assert_eq!(methods[4]["method"], BROWSER_RESOURCE_RESOLVE_METHOD);
        assert_eq!(methods[5]["method"], BROWSER_SPACE_CREATE_METHOD);
        assert_eq!(methods[7]["method"], BROWSER_SPACE_INSPECT_METHOD);
        assert_eq!(methods[8]["method"], BROWSER_SESSION_CREATE_METHOD);
        assert_eq!(
            methods[8]["params"]["required"],
            json!(["message", "idempotency_key"])
        );
        assert_eq!(
            methods[8]["params"]["properties"]["message"]["maxLength"],
            262144
        );
        assert_eq!(
            methods[8]["params"]["properties"]["source_url"]["maxLength"],
            2048
        );
        assert_eq!(
            methods[8]["params"]["properties"]["reasoning_effort"]["enum"],
            json!(["economy", "balanced", "thorough", null])
        );
        assert_eq!(
            methods[8]["params"]["properties"]["required_apps"]["maxItems"],
            32
        );
        assert_eq!(
            methods[8]["params"]["properties"]["work_chain_id"]["maxLength"],
            128
        );
        assert_eq!(
            methods[8]["params"]["properties"]["lane_id"]["maxLength"],
            160
        );
        assert_eq!(methods[9]["method"], BROWSER_SESSION_OPEN_METHOD);
        assert_eq!(methods[10]["method"], BROWSER_SESSION_ARCHIVE_METHOD);
        assert_eq!(
            methods[10]["params"]["required"],
            json!(["idempotency_key"])
        );
        assert_eq!(
            methods[10]["params"]["properties"]["current_user_message"]["maxLength"],
            262144
        );
        assert_eq!(methods[11]["method"], BROWSER_SESSION_ARCHIVE_STATUS_METHOD);
        assert_eq!(methods[11]["access"], "read_only");
        assert_eq!(
            methods[11]["params"]["required"],
            json!(["session_ref", "expected_generation"])
        );
        assert_eq!(methods[12]["method"], BROWSER_SESSION_INSPECT_METHOD);
        assert_eq!(methods[13]["method"], BROWSER_MESSAGE_APPEND_METHOD);
        assert_eq!(methods[14]["method"], BROWSER_COMPOSER_SET_REASONING_METHOD);
        assert_eq!(methods[15]["method"], BROWSER_COMPOSER_SET_APPS_METHOD);
        assert_eq!(methods[16]["method"], BROWSER_DISPATCH_SUBMIT_METHOD);
        assert_eq!(methods[17]["method"], BROWSER_DISPATCH_STATUS_METHOD);
        assert_eq!(methods[18]["method"], BROWSER_DISPATCH_STOP_METHOD);
        assert_eq!(methods[19]["method"], BROWSER_HANDOFF_PREPARE_METHOD);
        assert_eq!(methods[19]["access"], "read_only");
        assert_eq!(
            methods[19]["params"]["required"],
            json!(["continuity_id", "source_url"])
        );
        assert_eq!(
            methods
                .iter()
                .filter(|method| method["access"] == "read_only")
                .count(),
            10
        );
        assert!(methods.iter().all(|method| {
            let name = method["method"].as_str().unwrap();
            !name.contains("consent") && !name.contains("observe") && !name.contains("register")
        }));
    }

    #[test]
    fn file_skill_discovery_keeps_read_write_and_patch_intents_distinct() {
        let service = ProgressiveSkillService::new();
        let catalog = service.catalog();
        let search = catalog
            .iter()
            .find(|skill| skill.id == "files-search")
            .unwrap();
        let mutation = catalog
            .iter()
            .find(|skill| skill.id == "files-mutation")
            .unwrap();

        assert!(search.description.contains("READ ONLY"));
        assert!(search.owned_tools.contains(&"herdr_fs_read".to_owned()));
        assert!(!search.owned_tools.contains(&"herdr_fs_write".to_owned()));
        assert!(mutation.description.contains("CREATE / FULL REWRITE"));
        assert!(mutation.description.contains("PATCH EXISTING FILES"));
        assert!(mutation.owned_tools.contains(&"herdr_fs_write".to_owned()));
        assert!(mutation.owned_tools.contains(&"herdr_fs_patch".to_owned()));
    }

    #[test]
    fn empty_planning_advice_does_not_invent_task_independence() {
        let service = ProgressiveSkillService::new();
        let result =
            service.planning_advise_method_with_inventory(&json!({}), &planning_snapshot(), &[]);
        assert_eq!(
            result["advice"]["parallelism"]["reason"],
            "task_independence_unspecified"
        );
        assert_eq!(result["advice"]["parallelism"]["worth_considering"], false);
    }

    #[test]
    fn agent_attention_decision_mode_preserves_authoritative_child_facts_across_semantic_states() {
        let _guard = crate::test_env::lock();
        let previous_config = std::env::var_os("HERDR_MCP_CONFIG_DIR");
        let config_dir = temp_root("attention-semantic");
        unsafe {
            std::env::set_var("HERDR_MCP_CONFIG_DIR", &config_dir);
        }
        let service = ProgressiveSkillService::new();
        let params = json!({
            "children": [
                {
                    "task_id": "task_a",
                    "agent_id": "worker-a",
                    "terminal_state": "running",
                    "status": "working",
                    "recent_text": "Implementing the requested change.",
                    "age_ms": 1000,
                    "has_running_exec": true,
                    "dirty_worktree": true,
                    "open_pr": false
                },
                {
                    "task_id": "task_b",
                    "agent_id": "worker-b",
                    "terminal_state": "completed",
                    "status": "done",
                    "recent_text": "Implementation complete; tests passed.",
                    "age_ms": 2000,
                    "has_running_exec": false,
                    "dirty_worktree": false,
                    "open_pr": true
                }
            ]
        });

        let no_config = service.agent_attention_advise_method(&params);
        assert_eq!(no_config["used"], false);
        assert_eq!(no_config["reason"], "not_configured");
        assert_eq!(no_config["children"][1]["terminal_state"], "completed");
        assert!(no_config.get("terminal_state").is_none());
        assert!(no_config.get("safe_to_reclaim").is_none());

        let url = semantic_test_server(
            r#"{"model":"jev-test","answers":{"child_0_state":{"type":"choice","choice":"continue_unobserved","probabilities":{"continue_unobserved":0.8,"verify_completion":0.05,"needs_human":0.02,"blocked_external":0.03,"investigate_drift":0.05,"unclear":0.05},"confidence":0.8},"child_1_state":{"type":"choice","choice":"verify_completion","probabilities":{"continue_unobserved":0.02,"verify_completion":0.9,"needs_human":0.01,"blocked_external":0.01,"investigate_drift":0.01,"unclear":0.05},"confidence":0.9},"next_child":{"type":"choice","choice":"child_1","probabilities":{"child_0":0.1,"child_1":0.9},"confidence":0.9}}}"#,
        );
        write_semantic_test_config(&config_dir, &url);
        let configured = service.agent_attention_advise_method(&params);
        assert_eq!(configured["used"], true);
        assert_eq!(configured["assessments"][1]["state"], "verify_completion");
        assert_eq!(configured["attention_ranking"][0]["id"], "child_1");
        assert_eq!(configured["children"], no_config["children"]);
        assert!(configured.get("safe_to_reclaim").is_none());
        assert!(configured.get("terminal_complete").is_none());

        write_semantic_test_config(&config_dir, "http://127.0.0.1:1/v1");
        let provider_error = service.agent_attention_advise_method(&params);
        assert_eq!(provider_error["used"], false);
        assert_ne!(provider_error["reason"], "not_configured");
        assert_eq!(provider_error["children"], no_config["children"]);

        unsafe {
            match previous_config {
                Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
            }
        }
        let _ = std::fs::remove_dir_all(&config_dir);
    }

    #[test]
    fn validation_decision_mode_ranks_only_frozen_checks_and_preserves_projection() {
        let _guard = crate::test_env::lock();
        let previous_config = std::env::var_os("HERDR_MCP_CONFIG_DIR");
        let config_dir = temp_root("validation-semantic");
        unsafe {
            std::env::set_var("HERDR_MCP_CONFIG_DIR", &config_dir);
        }
        let service = ProgressiveSkillService::new();
        let params = json!({
            "summary": "Rust runtime and extension integration change",
            "changed_files": [
                "crates/herdr-mcp/src/mcp.rs",
                "extension/background.js",
                "docs/_wip/v1.0-status.md"
            ],
            "changed_symbols": ["local_call", "background wake"]
        });

        let no_config = service.validation_advise_method(&params);
        assert_eq!(no_config["used"], false);
        assert_eq!(no_config["reason"], "not_configured");
        let deterministic = no_config["deterministic"].clone();
        assert!(
            deterministic["candidate_checks"]
                .as_array()
                .unwrap()
                .contains(&json!("rust_gate"))
        );
        assert!(
            deterministic["candidate_checks"]
                .as_array()
                .unwrap()
                .contains(&json!("extension_smoke"))
        );

        let url = semantic_test_server(
            r#"{"model":"jev-test","answers":{"regression_surface":{"type":"choice","choice":"cross_boundary","probabilities":{"rust":0.1,"extension":0.1,"edge":0.02,"docs":0.03,"cross_boundary":0.7,"unknown":0.05},"confidence":0.7},"cross_boundary_risk":{"type":"noul","noul":0.95},"first_check":{"type":"choice","choice":"rust_gate","probabilities":{"rust_gate":0.61,"extension_targeted":0.15,"extension_smoke":0.1,"extension_background_bind":0.06,"docs_gate":0.04,"hygiene_gate":0.04},"confidence":0.61}}}"#,
        );
        write_semantic_test_config(&config_dir, &url);
        let configured = service.validation_advise_method(&params);
        assert_eq!(configured["used"], true);
        assert_eq!(configured["deterministic"], deterministic);
        assert_eq!(
            configured["likely_regression_surface"]["value"],
            "cross_boundary"
        );
        let allowed = deterministic["candidate_checks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>();
        assert!(
            configured["recommended_first_checks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["id"].as_str().is_some_and(|id| allowed.contains(id)))
        );

        let invalid_url = semantic_test_server(
            r#"{"model":"jev-test","answers":{"regression_surface":{"type":"choice","choice":"cross_boundary","probabilities":{"rust":0.1,"extension":0.1,"edge":0.02,"docs":0.03,"cross_boundary":0.7,"unknown":0.05},"confidence":0.7},"cross_boundary_risk":{"type":"noul","noul":0.95},"first_check":{"type":"choice","choice":"rogue","probabilities":{"rogue":1.0},"confidence":1.0}}}"#,
        );
        write_semantic_test_config(&config_dir, &invalid_url);
        let invalid_choice = service.validation_advise_method(&params);
        assert_eq!(invalid_choice["used"], false);
        assert_eq!(invalid_choice["deterministic"], deterministic);

        write_semantic_test_config(&config_dir, "http://127.0.0.1:1/v1");
        let provider_error = service.validation_advise_method(&params);
        assert_eq!(provider_error["used"], false);
        assert_ne!(provider_error["reason"], "not_configured");
        assert_eq!(provider_error["deterministic"], deterministic);

        unsafe {
            match previous_config {
                Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
            }
        }
        let _ = std::fs::remove_dir_all(&config_dir);
    }

    #[test]
    fn agent_closeout_advisory_no_config_stays_read_only_and_optional() {
        let _guard = crate::test_env::lock();
        let previous_config = std::env::var_os("HERDR_MCP_CONFIG_DIR");
        let config_dir = temp_root("closeout-semantic-empty");
        unsafe {
            std::env::set_var("HERDR_MCP_CONFIG_DIR", &config_dir);
        }

        let result = ProgressiveSkillService::new().agent_closeout_advise_method(&json!({
            "target": "worker",
            "recent_text": "Implementation finished; tests passed.",
            "agent_status": "done",
            "process_running": false,
            "worktree_dirty": true,
            "open_pr": false,
            "running_exec": false,
            "task_owned_resources": 1
        }));
        assert_eq!(result["ok"], true);
        assert_eq!(result["used"], false);
        assert_eq!(result["reason"], "not_configured");
        assert_eq!(result["advisory_only"], true);
        assert!(result.get("safe_to_reclaim").is_none());
        assert!(result.get("safe_to_delete").is_none());

        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 16 * 1024];
            let _ = stream.read(&mut request);
            let body = r#"{"model":"jev-test","answers":{"closeout_state":{"type":"choice","choice":"claims_complete","probabilities":{"working":0.02,"claims_complete":0.9,"waiting_user":0.02,"blocked_external":0.02,"unclear":0.04},"confidence":0.9},"needs_human":{"type":"noul","noul":0.1},"task_completed":{"type":"noul","noul":0.95},"needs_followup":{"type":"noul","noul":0.1},"needs_handoff":{"type":"noul","noul":0.05}}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let config_path = config_dir.join("config.json");
        std::fs::write(
            &config_path,
            format!(
                r#"{{"semantic":{{"routes":[{{"name":"agent-closeout-advisory","protocol":"decision","url":"http://127.0.0.1:{port}/v1","model":"jev-test","api_key":"test"}}]}}}}"#
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let configured = ProgressiveSkillService::new().agent_closeout_advise_method(&json!({
            "target": "worker",
            "recent_text": "Implementation finished; tests passed."
        }));
        assert_eq!(configured["ok"], true);
        assert_eq!(configured["used"], true);
        assert_eq!(configured["assessment"]["state"], "claims_complete");
        assert_eq!(configured["advisory_only"], true);
        assert!(configured.get("safe_to_reclaim").is_none());

        std::fs::write(
            &config_path,
            r#"{"semantic":{"routes":[{"name":"offline","protocol":"decision","url":"http://127.0.0.1:1/v1","model":"jev-test","api_key":"test"}]}}"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let provider_error = ProgressiveSkillService::new().agent_closeout_advise_method(&json!({
            "target": "worker",
            "recent_text": "Implementation finished; tests passed."
        }));
        assert_eq!(provider_error["ok"], true);
        assert_eq!(provider_error["used"], false);
        assert_ne!(provider_error["reason"], "not_configured");
        assert_eq!(provider_error["advisory_only"], true);
        assert!(provider_error.get("safe_to_reclaim").is_none());

        unsafe {
            match previous_config {
                Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
            }
        }
        let _ = std::fs::remove_dir_all(&config_dir);
    }

    #[test]
    fn capability_projection_keeps_unverified_traits_unknown() {
        let visibility = AgentVisibility::Allow(["pi".to_owned()].into_iter().collect());
        let result = crate::capability_resolver::project_capabilities(&snapshot(), &visibility);
        let worker = &result.workers[0];
        assert_eq!(worker.kind.as_deref(), Some("pi"));
        assert!(worker.provider.is_none());
        assert!(worker.model.is_none());
        assert!(worker.supports_vision.is_none());
        assert_eq!(worker.current_status, "idle");
    }

    #[test]
    fn unknown_local_method_fails_closed() {
        let service = ProgressiveSkillService::new();
        let cleanup = service
            .local_call(
                CLEANUP_PREVIEW_METHOD,
                &json!({"project_root": "/repo", "delete": true}),
                &snapshot(),
            )
            .unwrap();
        assert_eq!(cleanup["ok"], false);
        assert_eq!(cleanup["code"], "invalid_params");
        assert_eq!(cleanup["unknown"], json!(["delete"]));

        let result = service
            .local_call("herdr_mcp.skill.nope", &json!({}), &snapshot())
            .unwrap();
        assert_eq!(result["ok"], false);
        assert_eq!(result["code"], "unknown_local_method");
        assert!(
            service
                .local_call("agent.list", &json!({}), &snapshot())
                .is_none()
        );
    }

    // ---- local skill registry ----

    const USER_SKILL: &str = "---\nname: ego
version: 1.2.3
description: \"user ego\"
---\n# user body\n";

    #[test]
    fn local_skill_precedence_builtin_then_project_then_user() {
        let home = temp_root("precedence-home");
        let project = temp_root("precedence-project");
        let _guard = crate::test_env::lock();
        let previous = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &home);
        }
        // project skill
        write_project_skill(
            &project,
            "alpha",
            "---\nname: alpha\ndescription: \"project alpha\"\n---\n# project body\n",
        );
        // user skill with the SAME name must not override the project skill
        write_user_skill(
            &home,
            "alpha",
            "---\nname: alpha\ndescription: \"user alpha\"\n---\n# user body\n",
        );
        // user skill shadowing a builtin id must be dropped
        write_user_skill(
            &home,
            "files-search",
            "---\nname: files-search\ndescription: \"shadow attempt\"\n---\n# nope\n",
        );
        let service = ProgressiveSkillService::new();
        let listed = service
            .local_call(
                LOCAL_LIST_METHOD,
                &json!({"project_root": project.to_string_lossy()}),
                &snapshot(),
            )
            .unwrap();
        assert_eq!(listed["ok"], true);
        assert_eq!(listed["count"], 10); // 9 builtin + 1 unique project alpha
        let skills = listed["skills"].as_array().unwrap();
        let alpha = skills
            .iter()
            .find(|item| item["id"] == "alpha")
            .expect("alpha present");
        assert_eq!(alpha["description"], "project alpha");
        assert!(
            alpha["source_identity"]
                .as_str()
                .unwrap()
                .starts_with("project:")
        );
        assert!(
            !skills.iter().any(|item| item["id"] == "files-search"
                && item["source_identity"] != "herdr-mcp:builtin")
        );
        unsafe {
            match previous {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn local_skill_missing_dirs_discover_nothing() {
        let home = temp_root("missing-home");
        let project = temp_root("missing-project");
        with_isolated_home(|| {
            let service = ProgressiveSkillService::new();
            let listed = service
                .local_call(
                    LOCAL_LIST_METHOD,
                    &json!({"project_root": project.to_string_lossy()}),
                    &snapshot(),
                )
                .unwrap();
            assert_eq!(listed["ok"], true);
            assert_eq!(listed["count"], 9);
        });
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn local_skill_discovery_metadata_only_and_load_returns_body() {
        let home = temp_root("meta-home");
        let project = temp_root("meta-project");
        let _guard = crate::test_env::lock();
        let previous = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &home);
        }
        write_project_skill(&project, "beta", USER_SKILL);
        let service = ProgressiveSkillService::new();
        let listed = service
            .local_call(
                LOCAL_LIST_METHOD,
                &json!({"project_root": project.to_string_lossy()}),
                &snapshot(),
            )
            .unwrap();
        let skills = listed["skills"].as_array().unwrap();
        let beta = skills
            .iter()
            .find(|item| item["id"] == "beta")
            .expect("beta present");
        assert!(
            beta.get("content").is_none(),
            "discovery stays metadata-only"
        );
        assert_eq!(beta["version"], "1.2.3");
        assert_eq!(beta["name"], "ego");
        assert_eq!(service.cache_len(), 0);

        let loaded = service
            .local_call(
                LOCAL_LOAD_METHOD,
                &json!({"ids": ["beta"], "project_root": project.to_string_lossy()}),
                &snapshot(),
            )
            .unwrap();
        assert_eq!(loaded["ok"], true);
        assert!(
            loaded["skills"][0]["content"]
                .as_str()
                .is_some_and(|body| body.contains("# user body"))
        );
        assert_eq!(loaded["skills"][0]["cache_hit"], false);
        let reloaded = service
            .local_call(
                LOCAL_LOAD_METHOD,
                &json!({"ids": ["beta"], "project_root": project.to_string_lossy()}),
                &snapshot(),
            )
            .unwrap();
        assert_eq!(reloaded["skills"][0]["cache_hit"], true);
        unsafe {
            match previous {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn local_skill_symlink_escape_is_rejected() {
        let home = temp_root("escape-home");
        let project = temp_root("escape-project");
        let outside = temp_root("escape-outside");
        let outside_file = outside.join("secret.md");
        std::fs::write(
            &outside_file,
            "---\nname: evil\ndescription: outside\n---\n# evil body\n",
        )
        .unwrap();
        let skills_dir = project.join(".agents/skills").join("evil");
        std::fs::create_dir_all(&skills_dir).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside_file, skills_dir.join("SKILL.md")).unwrap();
        }
        let _guard = crate::test_env::lock();
        let previous = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &home);
        }
        let service = ProgressiveSkillService::new();
        let listed = service
            .local_call(
                LOCAL_LIST_METHOD,
                &json!({"project_root": project.to_string_lossy()}),
                &snapshot(),
            )
            .unwrap();
        assert!(
            !listed["skills"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == "evil"),
            "symlink escape must not be discovered"
        );
        let loaded = service
            .local_call(
                LOCAL_LOAD_METHOD,
                &json!({"ids": ["evil"], "project_root": project.to_string_lossy()}),
                &snapshot(),
            )
            .unwrap();
        assert_eq!(loaded["code"], "unknown_skill");
        unsafe {
            match previous {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&project);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn local_skill_external_identity_and_digest() {
        let home = temp_root("identity-home");
        let project = temp_root("identity-project");
        let _guard = crate::test_env::lock();
        let previous = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &home);
        }
        let body =
            "---\nname: gamma\nversion: 4.5.6\ndescription: gamma skill\n---\n# gamma body\n";
        let path = write_project_skill(&project, "gamma", body);
        let service = ProgressiveSkillService::new();
        let listed = service
            .local_call(
                LOCAL_LIST_METHOD,
                &json!({"project_root": project.to_string_lossy()}),
                &snapshot(),
            )
            .unwrap();
        let gamma = listed["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == "gamma")
            .expect("gamma present");
        assert_eq!(gamma["version"], "4.5.6");
        assert_eq!(gamma["size"], body.trim().len() as u64);
        let digest = Digest::from_content(body.trim()).as_str().to_owned();
        assert_eq!(gamma["digest"], digest);
        let loaded = service
            .local_call(
                LOCAL_LOAD_METHOD,
                &json!({"ids": ["gamma"], "project_root": project.to_string_lossy()}),
                &snapshot(),
            )
            .unwrap();
        assert_eq!(loaded["skills"][0]["digest"], digest);
        assert_eq!(loaded["skills"][0]["bytes"], body.trim().len() as u64);
        assert!(path.is_file());
        unsafe {
            match previous {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn local_skill_size_bound_is_enforced() {
        let home = temp_root("size-home");
        let project = temp_root("size-project");
        let _guard = crate::test_env::lock();
        let previous = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &home);
        }
        let dir = project.join(".agents/skills").join("huge");
        std::fs::create_dir_all(&dir).unwrap();
        let oversized = "# pad\n".repeat(local_skills::MAX_LOCAL_SKILL_BYTES / 6 + 1);
        std::fs::write(dir.join("SKILL.md"), &oversized).unwrap();
        let service = ProgressiveSkillService::new();
        let listed = service
            .local_call(
                LOCAL_LIST_METHOD,
                &json!({"project_root": project.to_string_lossy()}),
                &snapshot(),
            )
            .unwrap();
        assert_eq!(listed["count"], 9, "oversized skill is skipped");
        unsafe {
            match previous {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn project_root_parser_behavior() {
        assert_eq!(optional_project_root(&json!({})).unwrap(), None);
        assert_eq!(
            optional_project_root(&json!({ "project_root": null })).unwrap(),
            None
        );
        assert_eq!(
            optional_project_root(&json!({ "project_root": "/tmp/x" })).unwrap(),
            Some(PathBuf::from("/tmp/x"))
        );
        let error = optional_project_root(&json!({ "project_root": 42 })).unwrap_err();
        assert_eq!(error["code"], "invalid_params");
    }

    #[test]
    fn frontmatter_parser_handles_real_skills() {
        // ego-browser style: name + description + metadata.version.
        let ego = "---\nname: ego-browser\ndescription: drives a browser\nmetadata:\n  version: \"1.2.6\"\n  date: \"2026-07-20\"\n---\n# body\n";
        let fm = parse_frontmatter(ego);
        assert_eq!(fm.name.as_deref(), Some("ego-browser"));
        assert_eq!(fm.description.as_deref(), Some("drives a browser"));
        assert_eq!(fm.version.as_deref(), Some("1.2.6"));

        // opencli-usage style: plain single-line fields, unknown keys ignored.
        let opencli = "---\nname: opencli-usage\ndescription: top-level map\nallowed-tools: Bash(opencli:*), Read\n---\n# body\n";
        let fm = parse_frontmatter(opencli);
        assert_eq!(fm.name.as_deref(), Some("opencli-usage"));
        assert_eq!(fm.description.as_deref(), Some("top-level map"));
        assert!(fm.version.is_none());

        // quoted values and folded descriptions.
        let folded = "---\nname: glab\ndescription: >\n  Multi-line\n  folded description.\nversion: \"2.0\"\n---\n# body\n";
        let fm = parse_frontmatter(folded);
        assert_eq!(fm.name.as_deref(), Some("glab"));
        assert_eq!(
            fm.description.as_deref(),
            Some("Multi-line folded description.")
        );
        assert_eq!(fm.version.as_deref(), Some("2.0"));
    }
}
