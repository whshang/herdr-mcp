use super::*;

pub(crate) const CREATE: &str = "herdr_mcp.agent.task.gate.create";
pub(crate) const STATUS: &str = "herdr_mcp.agent.task.gate.status";
pub(crate) const RESOLVE: &str = "herdr_mcp.agent.task.gate.resolve";
pub(crate) const CANCEL: &str = "herdr_mcp.agent.task.gate.cancel";

impl PromptRegistry {
    pub(super) fn gate_call(&self, method: &str, params: &Value, scope: Option<&str>) -> Value {
        let result = self.gate_call_inner(method, params, scope);
        match result {
            Ok(gate) => json!({"ok": true, "gate": gate}),
            Err(code) => json!({"ok": false, "code": code}),
        }
    }

    fn gate_call_inner(
        &self,
        method: &str,
        params: &Value,
        scope: Option<&str>,
    ) -> Result<DecisionGate, String> {
        let _guard = self
            .inner
            .task_mutations
            .lock()
            .map_err(|_| "agent_task_store_unavailable")?;
        let store = self
            .inner
            .store
            .as_ref()
            .ok_or("agent_task_store_unavailable")?;
        let field = if method == CREATE {
            "task_id"
        } else {
            "gate_id"
        };
        let id = params
            .get(field)
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty() && id.len() <= 128)
            .ok_or("invalid_params")?;
        let gate = if method == CREATE {
            None
        } else {
            Some(
                store
                    .lock()
                    .map_err(|_| "agent_task_store_unavailable")?
                    .decision_gate(id)?
                    .ok_or("gate_not_found")?,
            )
        };
        let task_id = gate.as_ref().map_or(id, |gate| gate.task_id.as_str());
        let task = self.load_task(task_id)?.ok_or("agent_task_not_found")?;
        if scope.is_some() && task.parent_session_ref.as_deref() != scope {
            return Err("agent_task_parent_mismatch".to_owned());
        }
        if let Some(gate) = gate {
            if method == STATUS {
                return Ok(gate);
            }
            if gate.creator_parent_session_ref.as_deref() != scope {
                return Err("agent_task_parent_mismatch".to_owned());
            }
            return store
                .lock()
                .map_err(|_| "agent_task_store_unavailable")?
                .close_decision_gate(id, None, self.now_ms());
        }
        if task.is_terminal() {
            return Err("agent_task_terminal".to_owned());
        }
        if task.gate.is_some() {
            return Err("task_gate_already_open".to_owned());
        }
        let question = params
            .get("question")
            .and_then(Value::as_str)
            .filter(|text| valid_text(text, 1024))
            .ok_or("invalid_params")?;
        let options = params
            .get("options")
            .and_then(Value::as_array)
            .filter(|options| (2..=8).contains(&options.len()))
            .ok_or("invalid_params")?;
        let options = options
            .iter()
            .map(|option| {
                option
                    .as_str()
                    .filter(|text| valid_text(text, 128))
                    .map(str::to_owned)
                    .ok_or("invalid_params")
            })
            .collect::<Result<Vec<_>, _>>()?;
        if options.iter().collect::<HashSet<_>>().len() != options.len() {
            return Err("invalid_params".to_owned());
        }
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|_| "gate_id_unavailable")?;
        let gate = DecisionGate {
            gate_id: format!(
                "gate_{}",
                random
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ),
            task_id: task.task_id,
            question: question.to_owned(),
            options,
            status: "open".to_owned(),
            resolution: None,
            resolved_by: None,
            created_at_ms: self.now_ms(),
            resolved_at_ms: None,
            creator_parent_session_ref: scope.map(str::to_owned),
        };
        store
            .lock()
            .map_err(|_| "agent_task_store_unavailable")?
            .insert_decision_gate(&gate)?;
        Ok(gate)
    }
}

fn valid_text(text: &str, max_bytes: usize) -> bool {
    !text.trim().is_empty() && text.len() <= max_bytes && !text.chars().any(char::is_control)
}
