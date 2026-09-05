use crate::agent_visibility::AgentVisibility;
use crate::capability_inventory::{AgentCapabilityRecord, CapabilityInventoryStore};
use crate::capability_resolver::{WorkerCapability, project_capabilities_with_inventory};
use crate::local_skills::{self, LocalSkillFile, parse_frontmatter, read_file_bounded};
use crate::paths::RuntimePaths;
use crate::skill_dispatch::{DispatchAdvice, TaskProfile, advise_dispatch};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub const LOCAL_LIST_METHOD: &str = "herdr_mcp.skill.list";
pub const LOCAL_DESCRIBE_METHOD: &str = "herdr_mcp.skill.describe";
pub const LOCAL_LOAD_METHOD: &str = "herdr_mcp.skill.load";
pub const PLANNING_ADVISE_METHOD: &str = "herdr_mcp.planning.advise";
pub const GITHUB_STATUS_METHOD: &str = "herdr_mcp.github.status";
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
            "params": {
                "properties": {
                    "deterministic_tool": {"type": "string"},
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
            "method": GITHUB_STATUS_METHOD,
            "source": "herdr_mcp_local",
            "params": {
                "properties": {
                    "project_root": {"type": "string"},
                    "pr_number": {"type": "integer", "minimum": 1},
                    "previous_fingerprint": {"type": "string"},
                },
                "required": ["project_root"],
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
            "schema_version": 1,
            "params": {
                "properties": {
                    "project_ref": {"type": "string", "maxLength": 512},
                    "repo_id": {"type": "string", "maxLength": 512},
                    "work_chain_id": {"type": "string", "maxLength": 128},
                    "query": {"type": "string", "maxLength": 512},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 20},
                },
                "required": ["project_ref", "repo_id", "work_chain_id", "query"],
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
        description: "Read, list, search, and inspect images inside managed project roots.",
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
        description: "Apply safe repository file edits, writes, and transactional patches.",
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
        description: "Select and submit safe compatible coding-agent work from live capability facts.",
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
            "github_status": {
                "method": GITHUB_STATUS_METHOD,
                "effect": "read_only_fresh_status",
                "source": "local authenticated gh API; bypasses connector cache",
                "params": {
                    "project_root": "required managed git project/worktree root",
                    "pr_number": "optional positive integer; omit for repository Auto-merge state only",
                    "previous_fingerprint": "optional fingerprint from the prior call; unchanged state returns a compact changed=false response"
                }
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
            GITHUB_STATUS_METHOD => crate::github_status::status(params, snapshot),
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
        const KEYS: &[&str] = &[
            "deterministic_tool",
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
        let task = match task_profile_from_params(params) {
            Ok(task) => task,
            Err(error) => return error,
        };
        let visibility = AgentVisibility::from_env();
        let capabilities = project_capabilities_with_inventory(snapshot, &visibility, inventory);
        let advice = advise_dispatch(&task, &capabilities);
        json!({
            "ok": true,
            "decision_owner": "web_planner",
            "advice": dispatch_advice_json(&advice),
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
                }
            },
            "requirements_resolution": {
                "level": "conditional_on_material_ambiguity",
                "detail_skill": "requirements-grilling",
                "question_mode": "one_at_a_time"
            },
            "startable_candidates": startable_candidates_json(inventory, &visibility, snapshot, &task),
            "resource_context": resource_context_json(snapshot),
            "refresh": {
                "live": "herdr_inspect/herdr_since",
                "capabilities": "herdr-mcp scan --probe",
            },
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
    for record in available {
        if let Some(reason) = inactive_candidate_reject_reason(record, task) {
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
        "meaning": "installed/startable evidence only; planner decides whether creating a new Agent lane is worth the resource cost",
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
        assert_eq!(result["resource_context"]["duplicate_utility_panes"], 1);
        assert_eq!(result["resource_context"]["working_agents"], 1);
        assert_eq!(
            result["resource_context"]["reusable_idle_or_done_agents"],
            1
        );
        assert!(result["advice"].get("selected_target").is_none());
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

        let methods = local_method_schemas("work_memory.");
        assert_eq!(methods.len(), 6);
        assert_eq!(methods[0]["method"], WORK_MEMORY_BIND_METHOD);
        assert_eq!(methods[0]["schema_version"], 1);
        assert_eq!(methods[4]["method"], WORK_MEMORY_RESUME_METHOD);
        assert_eq!(
            methods[4]["params"]["required"],
            json!(["project_ref", "repo_id", "work_chain_id"])
        );
        assert_eq!(methods[5]["method"], WORK_MEMORY_SEARCH_METHOD);

        let methods = local_method_schemas("herdr_mcp.browser_");
        assert_eq!(methods.len(), 5);
        assert_eq!(methods[0]["method"], BROWSER_ENDPOINT_LIST_METHOD);
        assert_eq!(methods[1]["method"], BROWSER_ENDPOINT_INSPECT_METHOD);
        assert_eq!(methods[2]["method"], BROWSER_RESOURCE_LIST_METHOD);
        assert_eq!(methods[3]["method"], BROWSER_RESOURCE_INSPECT_METHOD);
        assert_eq!(methods[4]["method"], BROWSER_RESOURCE_RESOLVE_METHOD);
        assert!(methods.iter().all(|method| method["access"] == "read_only"));
        assert!(methods.iter().all(|method| {
            let name = method["method"].as_str().unwrap();
            !name.contains("consent") && !name.contains("observe") && !name.contains("register")
        }));
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

    // ---- v0.4.2 local skill registry ----

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
