//! Best-effort local human attestation. Never called from an RPC handler.
use crate::cli::AgentGateCommand;
use crate::local_agent_cli::{call_agent_gate, print_private_result};
use crate::paths::RuntimePaths;
use crate::prompt::decision_gates::{CANCEL, CREATE, STATUS};
use crate::state_store::{StateStore, decision_gates::DecisionGate};
use serde_json::json;
#[cfg(unix)]
use std::io::IsTerminal;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

const MANAGED_MARKERS: &[&str] = &[
    "HERDR_ENV",
    "HERDR_PANE_ID",
    "HERDR_PANE",
    "HERDR_AGENT",
    "HERDR_AGENT_NAME",
    "HERDR_AGENT_ID",
    "HERDR_SESSION",
    "HERDR_SESSION_ID",
    "HERDR_TAB_ID",
    "HERDR_WORKSPACE_ID",
    "HERDR_MCP_EXEC_ID",
];

pub(crate) fn run(command: AgentGateCommand) -> Result<ExitCode, String> {
    let (method, params) = match command {
        AgentGateCommand::Create {
            task_id,
            question,
            options,
        } => (
            CREATE,
            json!({"task_id": task_id, "question": question, "options": options}),
        ),
        AgentGateCommand::Status { gate_id } => (STATUS, json!({"gate_id": gate_id})),
        AgentGateCommand::Cancel { gate_id } => (CANCEL, json!({"gate_id": gate_id})),
        AgentGateCommand::Resolve {
            gate_id,
            resolution,
        } => {
            // Refuse managed contexts before opening any database or prompt.
            check_environment(|key| std::env::var_os(key).is_some())?;
            let mut terminal = ControllingTerminal::open()?;
            let paths = RuntimePaths::discover()?;
            let state_dir = std::env::var_os("HERDR_MCP_STATE_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or(paths.config_dir);
            let path = state_dir.join("state.db");
            if !path.is_file() {
                return Err("gate_store_unavailable".to_owned());
            }
            let store = StateStore::open(path)?;
            let now = || {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64
            };
            let gate = resolve_interactively(&store, &gate_id, &resolution, &mut terminal, now)?;
            return print_private_result(json!({"ok": true, "gate": gate}));
        }
    };
    print_private_result(call_agent_gate(method, params)?)
}

fn check_environment(present: impl Fn(&str) -> bool) -> Result<(), String> {
    if MANAGED_MARKERS.iter().any(|key| present(key)) {
        return Err("gate_resolution_requires_human: managed pane/agent/exec context".to_owned());
    }
    Ok(())
}

trait GateTerminal {
    fn is_interactive(&self) -> bool;
    fn confirm(&mut self, gate: &DecisionGate, resolution: &str) -> Result<String, String>;
}

fn resolve_interactively(
    store: &StateStore,
    gate_id: &str,
    resolution: &str,
    terminal: &mut impl GateTerminal,
    now: impl FnOnce() -> u64,
) -> Result<DecisionGate, String> {
    if !terminal.is_interactive() {
        return Err("gate_resolution_requires_human".to_owned());
    }
    let gate = store.decision_gate(gate_id)?.ok_or("gate_not_found")?;
    if gate.status != "open" {
        return Err("gate_not_open".to_owned());
    }
    if !gate.options.iter().any(|option| option == resolution) {
        return Err("gate_invalid_resolution".to_owned());
    }
    if terminal.confirm(&gate, resolution)? != resolution {
        return Err("gate_confirmation_mismatch".to_owned());
    }
    // A concurrent cancel/resolve wins; the compare-and-set never overwrites it.
    store.close_decision_gate(gate_id, Some(resolution), now())
}

struct ControllingTerminal(std::fs::File);
impl ControllingTerminal {
    fn open() -> Result<Self, String> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map_err(|_| "gate_resolution_requires_human: no controlling TTY".to_owned())?;
        let terminal = Self(file);
        if !terminal.is_interactive() {
            return Err("gate_resolution_requires_human".to_owned());
        }
        Ok(terminal)
    }
}
impl GateTerminal for ControllingTerminal {
    fn is_interactive(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            self.0.is_terminal()
                && unsafe { libc::tcgetpgrp(self.0.as_raw_fd()) == libc::getpgrp() }
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
    fn confirm(&mut self, gate: &DecisionGate, resolution: &str) -> Result<String, String> {
        writeln!(
            self.0,
            "Gate {}: {}\nOptions: {:?}\nRetype the chosen option exactly: {}",
            gate.gate_id, gate.question, gate.options, resolution
        )
        .and_then(|_| self.0.flush())
        .map_err(|error| error.to_string())?;
        let mut answer = String::new();
        BufReader::new((&mut self.0).take(130))
            .read_line(&mut answer)
            .map_err(|error| error.to_string())?;
        if !answer.ends_with('\n') {
            return Err("gate_confirmation_mismatch".to_owned());
        }
        answer.pop();
        if answer.ends_with('\r') {
            answer.pop();
        }
        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_store::{OperationLedgerInput, decision_gates::DecisionGate};

    struct Terminal {
        interactive: bool,
        answer: String,
        prompts: usize,
    }
    impl GateTerminal for Terminal {
        fn is_interactive(&self) -> bool {
            self.interactive
        }
        fn confirm(&mut self, _: &DecisionGate, _: &str) -> Result<String, String> {
            self.prompts += 1;
            Ok(self.answer.clone())
        }
    }
    fn fixture() -> StateStore {
        let mut store = StateStore::open(":memory:").unwrap();
        store
            .put_operation_ledger(OperationLedgerInput {
                op_id: "task",
                kind: "herdr_prompt_task",
                idempotency_key: None,
                request_hash: None,
                phase: "active",
                state: "running",
                result_json: "{}",
                now_ms: 10,
                expires_at: Some(20),
            })
            .unwrap();
        store
            .insert_decision_gate(&DecisionGate {
                gate_id: "gate".to_owned(),
                task_id: "task".to_owned(),
                question: "Choose".to_owned(),
                options: vec!["continue".to_owned(), "stop".to_owned()],
                status: "open".to_owned(),
                resolution: None,
                resolved_by: None,
                created_at_ms: 10,
                resolved_at_ms: None,
                creator_parent_session_ref: None,
            })
            .unwrap();
        store
    }
    /// user owner | Given a gate in real SQLite and an injected terminal |
    /// When confirmation is non-interactive, invalid, mistyped, or valid |
    /// Then only valid interactive confirmation resolves once with attribution.
    #[test]
    fn human_gate_resolution_requires_interactive_exact_confirmation() {
        let store = fixture();
        let before = store.decision_gate("gate").unwrap();
        let mut terminal = Terminal {
            interactive: false,
            answer: "continue".to_owned(),
            prompts: 0,
        };
        assert_eq!(
            resolve_interactively(&store, "gate", "continue", &mut terminal, || 30).unwrap_err(),
            "gate_resolution_requires_human"
        );
        assert_eq!(terminal.prompts, 0);
        terminal.interactive = true;
        assert_eq!(
            resolve_interactively(&store, "gate", "invalid", &mut terminal, || 30).unwrap_err(),
            "gate_invalid_resolution"
        );
        assert_eq!(terminal.prompts, 0);
        terminal.answer = "stop".to_owned();
        assert_eq!(
            resolve_interactively(&store, "gate", "continue", &mut terminal, || 30).unwrap_err(),
            "gate_confirmation_mismatch"
        );
        assert_eq!(store.decision_gate("gate").unwrap(), before);
        terminal.answer = "continue".to_owned();
        let resolved =
            resolve_interactively(&store, "gate", "continue", &mut terminal, || 30).unwrap();
        assert_eq!(resolved.status, "resolved");
        assert_eq!(resolved.resolution.as_deref(), Some("continue"));
        assert_eq!(
            resolved.resolved_by,
            Some(json!({"class": "local_cli_interactive"}))
        );
        assert_eq!(resolved.resolved_at_ms, Some(30));
        assert_eq!(
            resolve_interactively(&store, "gate", "continue", &mut terminal, || 31).unwrap_err(),
            "gate_not_open"
        );
        assert_eq!(store.decision_gate("gate").unwrap(), Some(resolved));
    }
    /// user owner | Given any managed pane/agent/exec marker | When local
    /// resolution is requested | Then approval is refused even with a TTY.
    #[test]
    fn human_gate_resolution_refuses_every_managed_environment_marker() {
        assert!(check_environment(|_| false).is_ok());
        for marker in MANAGED_MARKERS {
            assert!(
                check_environment(|key| key == *marker)
                    .unwrap_err()
                    .starts_with("gate_resolution_requires_human")
            );
        }
    }
    /// user parent | Given an open gate on an expired task | When unrelated
    /// reservations prune old work | Then the blocked task and gate survive.
    #[test]
    fn open_gate_preserves_task_during_ledger_pruning() {
        let mut store = fixture();
        store
            .reserve_operation("other", "key", "hash", "other-task", 100, None)
            .unwrap();
        assert!(store.operation_ledger("task").unwrap().is_some());
        assert_eq!(store.decision_gate("gate").unwrap().unwrap().status, "open");
        store.close_decision_gate("gate", None, 101).unwrap();
        store
            .reserve_operation("other", "key2", "hash", "other-task2", 102, None)
            .unwrap();
        assert!(store.operation_ledger("task").unwrap().is_none());
        assert!(store.decision_gate("gate").unwrap().is_none());
    }
}
