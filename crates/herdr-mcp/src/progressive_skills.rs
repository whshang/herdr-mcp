use crate::agent_visibility::AgentVisibility;
use crate::skill_dispatch::{CapabilitySnapshot, project_capabilities};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub const LOCAL_LIST_METHOD: &str = "herdr_mcp.skill.list";
pub const LOCAL_DESCRIBE_METHOD: &str = "herdr_mcp.skill.describe";
pub const LOCAL_LOAD_METHOD: &str = "herdr_mcp.skill.load";

const BUILTIN_SOURCE_IDENTITY: &str = "herdr-mcp:builtin";
const GLOBAL_POLICY_URI: &str = "skill://herdr-mcp/AGENTS.md";
const GLOBAL_AGENTS: &str = include_str!("../../../assets/herdr/AGENTS.md");

const WORKSTATION_CONTROL: &str =
    include_str!("../../../assets/herdr/skills/workstation-control/SKILL.md");
const FILES_SEARCH: &str = include_str!("../../../assets/herdr/skills/files-search/SKILL.md");
const FILES_MUTATION: &str = include_str!("../../../assets/herdr/skills/files-mutation/SKILL.md");
const GIT_REPOSITORY: &str = include_str!("../../../assets/herdr/skills/git-repository/SKILL.md");
const EXECUTION: &str = include_str!("../../../assets/herdr/skills/execution/SKILL.md");
const AGENT_DISPATCH: &str = include_str!("../../../assets/herdr/skills/agent-dispatch/SKILL.md");
const DEVELOPMENT_ORCHESTRATION: &str =
    include_str!("../../../assets/herdr/skills/development-orchestration/SKILL.md");

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

const BUILTIN_SKILLS: [BuiltinSkillSpec; 7] = [
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
        ],
        risk_domains: &["cross-lane-mutation"],
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

    pub fn bootstrap(&self, snapshot: &Value) -> Value {
        let global_content = GLOBAL_AGENTS.trim();
        let global_digest = Digest::from_content(global_content);
        let catalog = self.catalog();
        let compact_catalog = catalog
            .iter()
            .map(|item| format!("- {}: {}", item.id, item.description))
            .collect::<Vec<_>>()
            .join("\n");
        let content = format!(
            "{}\n\n## Available policy modules\n\n{}\n\n## Load contract\n\nLoad the required capability domains in one call:\n\n```text\nherdr_call(method=\"{}\", params={{\"ids\":[\"files-search\",\"git-repository\"]}})\n```\n\nLoaded Skill text is sticky for the current conversation/context while source identity and digest remain unchanged. A new user turn does not reload it. Refresh live worker/pane/runtime state through inspect/since rather than reloading policy text.",
            global_content, compact_catalog, LOCAL_LOAD_METHOD
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
            "catalog": catalog.iter().map(descriptor_json).collect::<Vec<_>>(),
            "load": {
                "method": LOCAL_LOAD_METHOD,
                "params": {
                    "ids": "required array<string>, batched, first-request order preserved",
                    "expected_digests": "optional object keyed by skill id; mismatch fails closed"
                },
                "sticky": "conversation/task-context until source identity or digest changes, new capability domain, handoff, or explicit refresh",
                "authorization": "none"
            },
            "capability_snapshot": capability_snapshot(snapshot),
            "bytes": content.len(),
        })
    }

    pub fn local_call(&self, method: &str, params: &Value, _snapshot: &Value) -> Option<Value> {
        if !method.starts_with("herdr_mcp.") {
            return None;
        }
        Some(match method {
            LOCAL_LIST_METHOD => self.list_method(params),
            LOCAL_DESCRIBE_METHOD => self.describe_method(params),
            LOCAL_LOAD_METHOD => self.load_method(params),
            _ => json!({
                "ok": false,
                "code": "unknown_local_method",
                "method": method,
                "message": "unknown herdr-mcp local method; request was not forwarded to the Herdr socket",
            }),
        })
    }

    fn list_method(&self, params: &Value) -> Value {
        if let Err(error) = validate_object_keys(params, &[]) {
            return error;
        }
        let catalog = self.catalog();
        json!({
            "ok": true,
            "skills": catalog.iter().map(descriptor_json).collect::<Vec<_>>(),
            "count": catalog.len(),
            "loaded": false,
        })
    }

    fn describe_method(&self, params: &Value) -> Value {
        if let Err(error) = validate_object_keys(params, &["id"]) {
            return error;
        }
        let Some(id) = params.get("id").and_then(Value::as_str) else {
            return invalid_params("id must be a non-empty string");
        };
        if id.trim().is_empty() {
            return invalid_params("id must be a non-empty string");
        }
        match self.catalog().into_iter().find(|item| item.id == id) {
            Some(item) => json!({"ok": true, "skill": descriptor_json(&item), "loaded": false}),
            None => json!({"ok": false, "code": "unknown_skill", "id": id}),
        }
    }

    fn load_method(&self, params: &Value) -> Value {
        if let Err(error) = validate_object_keys(params, &["ids", "expected_digests"]) {
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

        let catalog = self.catalog();
        let mut loaded = Vec::with_capacity(requested.len());
        for id in requested {
            let Some(descriptor) = catalog.iter().find(|item| item.id == id) else {
                return json!({"ok": false, "code": "unknown_skill", "id": id});
            };
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
            let Some(spec) = BUILTIN_SKILLS.iter().find(|spec| spec.id == id) else {
                return json!({"ok": false, "code": "skill_source_missing", "id": id});
            };
            let (content, cache_hit) =
                match self.load_verified(&descriptor.identity, spec.content.trim()) {
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

fn capability_snapshot(snapshot: &Value) -> Value {
    let visibility = AgentVisibility::from_env();
    capability_snapshot_json(&project_capabilities(snapshot, &visibility))
}

fn capability_snapshot_json(snapshot: &CapabilitySnapshot) -> Value {
    let workers = snapshot
        .workers
        .iter()
        .map(|worker| {
            json!({
                "agent_id": worker.agent_id,
                "kind": worker.kind,
                "provider": worker.provider,
                "model": worker.model,
                "profile": worker.profile,
                "supports_code_edit": worker.supports_code_edit,
                "supports_shell": worker.supports_shell,
                "supports_vision": worker.supports_vision,
                "reasoning_tier": worker.reasoning_tier,
                "latency_tier": worker.latency_tier,
                "cost_tier": worker.cost_tier,
                "context_tier": worker.context_tier,
                "interactive_only": worker.interactive_only,
                "can_run_headless": worker.can_run_headless,
                "allowed_for_auto_dispatch": worker.allowed_for_auto_dispatch,
                "current_status": worker.current_status,
                "current_project": worker.current_project,
                "cwd": worker.cwd,
                "pane_id": worker.pane_id,
                "workspace_id": worker.workspace_id,
                "interactive_ready": worker.interactive_ready,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "source": snapshot.source,
        "revision": snapshot.revision,
        "workers": workers,
        "hidden_workers": snapshot.hidden_workers,
        "unknown_fields_are_null": true,
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

    #[test]
    fn catalog_is_stable_and_covers_all_non_skill_tools_once() {
        let service = ProgressiveSkillService::new();
        let catalog = service.catalog();
        assert_eq!(catalog.len(), 7);
        assert_eq!(catalog[0].id, "workstation-control");
        assert_eq!(catalog[6].id, "development-orchestration");
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
    fn discovery_does_not_load_and_batched_load_hits_immutable_cache() {
        let service = ProgressiveSkillService::new();
        let listed = service
            .local_call(LOCAL_LIST_METHOD, &json!({}), &snapshot())
            .unwrap();
        assert_eq!(listed["count"], 7);
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
        let bootstrap = service.bootstrap(&changed_snapshot);
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
        let result = service.bootstrap(&snapshot());
        assert_eq!(result["ok"], true);
        assert_eq!(result["mode"], "progressive");
        assert_eq!(result["catalog"].as_array().unwrap().len(), 7);
        assert_eq!(result["load"]["method"], LOCAL_LOAD_METHOD);
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("# Herdr Global AGENTS.md"));
        assert!(content.contains("Available policy modules"));
        assert!(content.contains("A new user turn does not reload it"));
        assert!(!content.contains("# Files Mutation"));
        assert!(!content.contains("# Agent Dispatch"));
    }

    #[test]
    fn capability_projection_keeps_unverified_traits_unknown() {
        let result = capability_snapshot(&snapshot());
        let worker = &result["workers"][0];
        assert_eq!(worker["kind"], "pi");
        assert!(worker["provider"].is_null());
        assert!(worker["model"].is_null());
        assert!(worker["supports_vision"].is_null());
        assert_eq!(worker["current_status"], "idle");
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
}
