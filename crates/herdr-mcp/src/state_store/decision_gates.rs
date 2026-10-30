//! Durable gate facts. No RPC caller can attest human approval.
use super::StateStore;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct DecisionGate {
    pub gate_id: String,
    pub task_id: String,
    pub question: String,
    pub options: Vec<String>,
    pub status: String,
    pub resolution: Option<String>,
    pub resolved_by: Option<Value>,
    pub created_at_ms: u64,
    pub resolved_at_ms: Option<u64>,
    #[serde(skip)]
    pub creator_parent_session_ref: Option<String>,
}

impl StateStore {
    pub(crate) fn decision_gate(&self, gate_id: &str) -> Result<Option<DecisionGate>, String> {
        let row: Option<(String, Option<String>)> = self.conn.query_row(
            "SELECT record_json, creator_parent_session_ref FROM task_decision_gates WHERE gate_id = ?1",
            [gate_id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(|error| format!("cannot read decision gate: {error}"))?;
        row.map(decode_gate).transpose()
    }

    pub(crate) fn open_decision_gate(&self, task_id: &str) -> Result<Option<DecisionGate>, String> {
        // Read the open predicate and payload in one SQLite snapshot. A CLI
        // resolution between two SELECTs must not project a resolved gate as open.
        let row: Option<(String, Option<String>)> = self.conn.query_row(
            "SELECT record_json, creator_parent_session_ref FROM task_decision_gates WHERE task_id = ?1 AND status = 'open'",
            [task_id], |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(|error| format!("cannot read task decision gate: {error}"))?;
        row.map(decode_gate).transpose()
    }

    pub(crate) fn insert_decision_gate(&self, gate: &DecisionGate) -> Result<(), String> {
        let encoded = serde_json::to_string(gate).map_err(|error| error.to_string())?;
        self.conn.execute(
            "INSERT INTO task_decision_gates (gate_id, task_id, creator_parent_session_ref, status, record_json) VALUES (?1, ?2, ?3, 'open', ?4)",
            params![gate.gate_id, gate.task_id, gate.creator_parent_session_ref, encoded],
        ).map_err(|error| format!("cannot create decision gate: {error}"))?;
        Ok(())
    }

    /// Compare-and-set is shared by cancellation and the interactive CLI.
    /// Resolution is never reachable through an MCP/agent method.
    pub(crate) fn close_decision_gate(
        &self,
        gate_id: &str,
        resolution: Option<&str>,
        now: u64,
    ) -> Result<DecisionGate, String> {
        let mut gate = self.decision_gate(gate_id)?.ok_or("gate_not_found")?;
        if gate.status != "open" {
            return Err("gate_not_open".to_owned());
        }
        if let Some(resolution) = resolution {
            if !gate.options.iter().any(|option| option == resolution) {
                return Err("gate_invalid_resolution".to_owned());
            }
            gate.status = "resolved".to_owned();
            gate.resolution = Some(resolution.to_owned());
            gate.resolved_by = Some(json!({"class": "local_cli_interactive"}));
            gate.resolved_at_ms = Some(now);
        } else {
            gate.status = "cancelled".to_owned();
        }
        let encoded = serde_json::to_string(&gate).map_err(|error| error.to_string())?;
        let changed = self.conn.execute(
            "UPDATE task_decision_gates SET status = ?2, record_json = ?3 WHERE gate_id = ?1 AND status = 'open'",
            params![gate_id, gate.status, encoded],
        ).map_err(|error| format!("cannot close decision gate: {error}"))?;
        if changed != 1 {
            return Err("gate_not_open".to_owned());
        }
        Ok(gate)
    }
}

fn decode_gate((encoded, scope): (String, Option<String>)) -> Result<DecisionGate, String> {
    let mut gate: DecisionGate = serde_json::from_str(&encoded)
        .map_err(|error| format!("invalid decision gate: {error}"))?;
    gate.creator_parent_session_ref = scope;
    Ok(gate)
}
