use super::*;

pub(super) struct RoleTarget<'a> {
    selector: &'a str,
    kind: Option<&'a str>,
}

pub(super) struct Resolution {
    pub target: String,
    pub before: AgentState,
    pub candidates: Vec<Value>,
}

pub(super) fn not_delivered(code: &str) -> Value {
    json!({"ok": false, "code": code, "delivery_state": "not_delivered", "submitted": false})
}

impl<'a> RoleTarget<'a> {
    pub fn parse(target: &'a str) -> Result<Option<Self>, Value> {
        let Some(role) = target.strip_prefix('@') else {
            return Ok(None);
        };
        let kind = role.strip_suffix(":idle").unwrap_or(role);
        if kind.is_empty()
            || kind == "all"
            || !kind
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(not_delivered("role_target_unsupported"));
        }
        Ok(Some(Self {
            selector: target,
            kind: (kind != "idle").then_some(kind),
        }))
    }

    pub fn resolve(
        &self,
        client: &HerdrClient,
        registry: &PromptRegistry,
        workspace_id: Option<&str>,
        parent_target: Option<&str>,
        parent_session_ref: Option<&str>,
        max_concurrent: Option<u64>,
    ) -> Result<Resolution, Value> {
        // Never submit a role dispatch without durable tracking: a later
        // capped dispatch must be able to account for this task too.
        let store = registry
            .inner
            .store
            .as_ref()
            .ok_or_else(|| not_delivered("agent_task_store_unavailable"))?;
        if let Some(limit) = max_concurrent {
            let count = store
                .lock()
                .map_err(|_| not_delivered("agent_task_store_unavailable"))?
                .active_role_task_count(
                    self.selector,
                    parent_session_ref,
                    parent_target,
                    registry.now_ms(),
                )
                .map_err(|_| not_delivered("agent_task_store_unavailable"))?;
            if count >= limit {
                return Err(not_delivered("concurrency_limit_reached"));
            }
        }
        let Some(cache) = &registry.inner.cache else {
            return Err(not_delivered("role_target_unavailable"));
        };
        if !matches!(
            cache.classify_health(),
            crate::state_cache::EventCacheHealth::Healthy
        ) {
            return Err(not_delivered("role_target_unavailable"));
        }
        let snapshot = cache.snapshot();
        let agents = snapshot.get("agents").and_then(Value::as_array);
        let mut candidates = Vec::new();
        let mut selected = None;
        for agent in agents.into_iter().flatten() {
            let kind = agent
                .get("kind")
                .or_else(|| agent.get("agent"))
                .and_then(Value::as_str);
            if self.kind.is_some() && self.kind != kind {
                continue;
            }
            let Some(pane) = agent
                .get("pane_id")
                .and_then(Value::as_str)
                .filter(|pane| !pane.is_empty())
            else {
                continue;
            };
            let workspace = agent
                .get("workspace_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    snapshot
                        .get("panes")
                        .and_then(Value::as_array)
                        .and_then(|panes| {
                            panes
                                .iter()
                                .find(|entry| entry["pane_id"].as_str() == Some(pane))
                        })
                        .and_then(|entry| entry["workspace_id"].as_str())
                        .map(str::to_owned)
                });
            if workspace_id.is_some() && workspace_id != workspace.as_deref() {
                continue;
            }
            let status = agent
                .get("agent_status")
                .or_else(|| agent.get("status"))
                .and_then(Value::as_str);
            let before = AgentState {
                pane_id: Some(pane.to_owned()),
                agent_status: status.map(str::to_owned),
                state_change_seq: agent.get("state_change_seq").and_then(Value::as_u64),
            };
            let mut reason = (status != Some("idle")).then_some("not_idle".to_owned());
            // Snapshot aliases also fence the caller's own pane if a native
            // parent probe is unavailable. Never turn a failed probe into self-dispatch.
            if reason.is_none()
                && parent_target.is_some_and(|parent| {
                    parent == pane || agent.get("name").and_then(Value::as_str) == Some(parent)
                })
            {
                reason = Some("parent_target_is_target".to_owned());
            }
            if reason.is_none()
                && let Some(parent) = parent_target
                && let Some(rejection) =
                    delegation_guard(client, registry, pane, Some(&before), parent)
            {
                reason = Some(
                    rejection["code"]
                        .as_str()
                        .unwrap_or("delegation_rejected")
                        .to_owned(),
                );
            }
            candidates.push(json!({
                "pane_id": pane, "kind": kind, "status": status,
                "workspace_id": workspace, "eligible": reason.is_none(), "reason": reason,
            }));
            if selected.is_none() && reason.is_none() {
                selected = Some((pane.to_owned(), before));
            }
        }
        let Some((target, before)) = selected else {
            let mut error = not_delivered("role_target_unavailable");
            error["candidates"] = json!(candidates);
            return Err(error);
        };
        Ok(Resolution {
            target,
            before,
            candidates,
        })
    }
}
