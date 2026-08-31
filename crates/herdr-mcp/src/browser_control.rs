use crate::herdr::HerdrClient;
use crate::prompt::{self, PromptRegistry};
use crate::state_cache::EventCache;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

const MAX_CONTROL_TEXT_CHARS: usize = 64 * 1024;

pub fn target_revision(cache: &EventCache, pane: &Value) -> Option<String> {
    let pane_id = pane.get("pane_id").and_then(Value::as_str)?;
    let workspace_id = pane
        .get("workspace_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let pane_revision = pane
        .get("revision")
        .map(Value::to_string)
        .unwrap_or_default();
    let agent = pane
        .get("agent")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let session = pane.get("agent_session").and_then(Value::as_object);
    let session_source = session
        .and_then(|value| value.get("source"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let session_kind = session
        .and_then(|value| value.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let session_value = session
        .and_then(|value| value.get("value"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    let mut hash = Sha256::new();
    for part in [
        "browser-target-v1",
        cache.boot_id(),
        workspace_id,
        pane_id,
        pane_revision.as_str(),
        agent,
        session_source,
        session_kind,
        session_value,
    ] {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    let digest = hash.finalize();
    Some(format!(
        "btr1_{}",
        digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

pub fn pane_view(cache: &EventCache, pane: &Value) -> Value {
    let mut view = pane.clone();
    let Some(object) = view.as_object_mut() else {
        return view;
    };
    if let Some(revision) = target_revision(cache, pane) {
        object.insert("target_revision".to_owned(), json!(revision));
    }
    object.insert(
        "control_capabilities".to_owned(),
        control_capabilities(pane),
    );
    view
}

pub fn control_capabilities(pane: &Value) -> Value {
    let agent = pane.get("agent").and_then(Value::as_str);
    let status = pane
        .get("agent_status")
        .or_else(|| pane.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let steer = match agent {
        Some("codex") if status == "working" => json!({
            "available": false,
            "outcome": "session_not_resolved",
            "provider": "codex",
            "reason": "Herdr pane metadata does not expose a controllable Codex app-server thread/active-turn mapping",
        }),
        Some("codex") => json!({
            "available": false,
            "outcome": "no_active_turn",
            "provider": "codex",
        }),
        Some(provider) => json!({
            "available": false,
            "outcome": "unsupported_provider",
            "provider": provider,
        }),
        None => json!({
            "available": false,
            "outcome": "no_agent",
            "provider": Value::Null,
        }),
    };
    let interrupt = match agent {
        Some(_) if status == "working" => json!({
            "available": true,
            "outcome": "ready",
            "delivery_mode": "terminal_ctrl_c",
        }),
        Some(_) => json!({
            "available": false,
            "outcome": "no_active_turn",
            "delivery_mode": "terminal_ctrl_c",
        }),
        None => json!({
            "available": false,
            "outcome": "no_agent",
            "delivery_mode": "terminal_ctrl_c",
        }),
    };
    json!({
        "agent_prompt": { "available": agent.is_some() },
        "steer": steer,
        "terminal_input": {
            "available": agent.is_none(),
            "outcome": if agent.is_none() { "ready" } else { "agent_pane" },
            "delivery_mode": "pane_send_input",
        },
        "interrupt": interrupt,
    })
}

pub fn execute_action(
    client: &HerdrClient,
    cache: &EventCache,
    prompt_registry: &PromptRegistry,
    request: &Value,
) -> Value {
    let Some(request_object) = request.as_object() else {
        return invalid("request body must be a JSON object");
    };
    let action = match request_object.get("action").and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => value.trim(),
        _ => return invalid("action is required"),
    };
    let target = match request_object.get("target").and_then(Value::as_object) {
        Some(value) => value,
        None => return invalid("target object is required"),
    };
    let pane_id = match target.get("pane_id").and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => value.trim(),
        _ => return invalid("target.pane_id is required"),
    };
    let expected_revision = match target.get("target_revision").and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => value.trim(),
        _ => return invalid("target.target_revision is required"),
    };

    let snapshot = cache.snapshot();
    let pane = snapshot
        .get("panes")
        .and_then(Value::as_array)
        .and_then(|panes| {
            panes
                .iter()
                .find(|pane| pane.get("pane_id").and_then(Value::as_str) == Some(pane_id))
        });
    let Some(pane) = pane else {
        return action_result(
            false,
            action,
            "stale_target",
            "not_submitted",
            pane_id,
            Value::Null,
            json!({"reason": "pane_missing"}),
        );
    };
    let current_revision = target_revision(cache, pane).unwrap_or_default();
    if expected_revision != current_revision {
        return action_result(
            false,
            action,
            "stale_target",
            "not_submitted",
            pane_id,
            json!(current_revision),
            json!({"reason": "target_revision_changed"}),
        );
    }

    match action {
        "agent_prompt" => execute_agent_prompt(
            client,
            prompt_registry,
            request_object,
            pane,
            pane_id,
            &current_revision,
        ),
        "steer" => steer_capability_result(pane, pane_id, &current_revision),
        "terminal_input" => {
            execute_terminal_input(client, request_object, pane, pane_id, &current_revision)
        }
        "terminal_text" | "terminal_keys" => action_result(
            false,
            action,
            "rejected",
            "not_submitted",
            pane_id,
            json!(current_revision),
            json!({"reason": "raw_terminal_control_disabled"}),
        ),
        "interrupt" => execute_interrupt(client, pane, pane_id, &current_revision),
        _ => action_result(
            false,
            action,
            "unsupported",
            "not_submitted",
            pane_id,
            json!(current_revision),
            json!({"reason": "unsupported_action"}),
        ),
    }
}

fn execute_agent_prompt(
    client: &HerdrClient,
    prompt_registry: &PromptRegistry,
    request: &Map<String, Value>,
    pane: &Value,
    pane_id: &str,
    revision: &str,
) -> Value {
    if pane.get("agent").and_then(Value::as_str).is_none() {
        return action_result(
            false,
            "agent_prompt",
            "rejected",
            "not_submitted",
            pane_id,
            json!(revision),
            json!({"reason": "no_agent"}),
        );
    }
    let args = request.get("args").and_then(Value::as_object);
    let text = match args
        .and_then(|value| value.get("text"))
        .and_then(Value::as_str)
    {
        Some(value) if !value.trim().is_empty() => value,
        _ => return invalid("args.text is required for agent_prompt"),
    };
    if text.chars().count() > MAX_CONTROL_TEXT_CHARS {
        return invalid("args.text is too large");
    }
    let idempotency_key = match request.get("idempotency_key").and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => value.trim(),
        _ => return invalid("idempotency_key is required for agent_prompt"),
    };

    let prompt_result = prompt::run(
        client,
        prompt_registry,
        &json!({
            "target": pane_id,
            "text": text,
            "idempotency_key": idempotency_key,
        }),
    );
    normalize_prompt_result(prompt_result, pane_id, revision)
}

fn normalize_prompt_result(prompt_result: Value, pane_id: &str, revision: &str) -> Value {
    let ok = prompt_result.get("ok").and_then(Value::as_bool) == Some(true);
    let status = prompt_result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let submitted = prompt_result.get("submitted");
    let delivery_uncertain = prompt_result
        .get("delivery_uncertain")
        .and_then(Value::as_bool)
        == Some(true)
        || submitted.and_then(Value::as_str) == Some("unknown");
    let outcome = if ok {
        match status {
            "queued" => "queued",
            "agent_blocked" => "rejected",
            _ => "submitted",
        }
    } else if delivery_uncertain {
        "uncertain"
    } else if prompt_result.get("failure_phase").and_then(Value::as_str) == Some("resolve") {
        "rejected"
    } else {
        "failed"
    };
    let observed = prompt_result
        .get("state_observation")
        .and_then(|value| value.get("changed"))
        .and_then(Value::as_bool)
        == Some(true);
    let delivery_phase = if outcome == "uncertain" {
        "uncertain"
    } else if ok && observed {
        "observed"
    } else if ok {
        "submitted"
    } else {
        "not_submitted"
    };
    let op_id = prompt_result.get("op_id").cloned().unwrap_or(Value::Null);
    json!({
        "ok": ok,
        "action": "agent_prompt",
        "outcome": outcome,
        "delivery_phase": delivery_phase,
        "target": {
            "pane_id": pane_id,
            "target_revision": revision,
        },
        "op_id": op_id,
        "delivery_mode": "agent_prompt",
        "idempotent_replay": prompt_result.get("idempotent_replay").cloned().unwrap_or(Value::Bool(false)),
        "evidence": prompt_result,
    })
}

fn steer_capability_result(pane: &Value, pane_id: &str, revision: &str) -> Value {
    let capability = control_capabilities(pane)
        .get("steer")
        .cloned()
        .unwrap_or_else(|| json!({"available": false, "outcome": "not_steerable"}));
    let outcome = capability
        .get("outcome")
        .and_then(Value::as_str)
        .unwrap_or("not_steerable");
    action_result(
        false,
        "steer",
        outcome,
        "not_submitted",
        pane_id,
        json!(revision),
        json!({
            "capability": capability,
            "delivery_mode": "provider_probe_only",
            "prompt_fallback": false,
        }),
    )
}

fn execute_terminal_input(
    client: &HerdrClient,
    request: &Map<String, Value>,
    pane: &Value,
    pane_id: &str,
    revision: &str,
) -> Value {
    if pane.get("agent").and_then(Value::as_str).is_some() {
        return action_result(
            false,
            "terminal_input",
            "rejected",
            "not_submitted",
            pane_id,
            json!(revision),
            json!({"reason": "agent_pane", "delivery_mode": "pane_send_input"}),
        );
    }
    let text = match request
        .get("args")
        .and_then(Value::as_object)
        .and_then(|value| value.get("text"))
        .and_then(Value::as_str)
    {
        Some(value) if !value.trim().is_empty() => value,
        _ => return invalid("args.text is required for terminal_input"),
    };
    if text.chars().count() > MAX_CONTROL_TEXT_CHARS {
        return invalid("args.text is too large");
    }

    match client.call(
        "pane.send_input",
        json!({
            "pane_id": pane_id,
            "text": text,
            "keys": ["Enter"],
        }),
    ) {
        Ok(evidence) => action_result(
            true,
            "terminal_input",
            "submitted",
            "submitted",
            pane_id,
            json!(revision),
            json!({
                "delivery_mode": "pane_send_input",
                "evidence": evidence,
            }),
        ),
        Err(error) => {
            let uncertain = matches!(
                error.code.as_str(),
                "timeout" | "unexpected_eof" | "socket_error"
            );
            action_result(
                false,
                "terminal_input",
                if uncertain { "uncertain" } else { "failed" },
                if uncertain {
                    "uncertain"
                } else {
                    "not_submitted"
                },
                pane_id,
                json!(revision),
                json!({
                    "reason": error.code,
                    "message": error.message,
                    "delivery_mode": "pane_send_input",
                }),
            )
        }
    }
}

fn execute_interrupt(client: &HerdrClient, pane: &Value, pane_id: &str, revision: &str) -> Value {
    if pane.get("agent").and_then(Value::as_str).is_none() {
        return action_result(
            false,
            "interrupt",
            "rejected",
            "not_submitted",
            pane_id,
            json!(revision),
            json!({"reason": "no_agent", "delivery_mode": "terminal_ctrl_c"}),
        );
    }
    let status = pane
        .get("agent_status")
        .or_else(|| pane.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if status != "working" {
        return action_result(
            false,
            "interrupt",
            "no_active_turn",
            "not_submitted",
            pane_id,
            json!(revision),
            json!({"reason": "agent_not_working", "delivery_mode": "terminal_ctrl_c"}),
        );
    }

    match client.call(
        "pane.send_keys",
        json!({
            "pane_id": pane_id,
            "keys": ["C-c"],
        }),
    ) {
        Ok(evidence) => action_result(
            true,
            "interrupt",
            "interrupted",
            "submitted",
            pane_id,
            json!(revision),
            json!({
                "delivery_mode": "terminal_ctrl_c",
                "provider_interrupt": false,
                "evidence": evidence,
            }),
        ),
        Err(error) => {
            let uncertain = matches!(
                error.code.as_str(),
                "timeout" | "unexpected_eof" | "socket_error"
            );
            action_result(
                false,
                "interrupt",
                if uncertain { "uncertain" } else { "failed" },
                if uncertain {
                    "uncertain"
                } else {
                    "not_submitted"
                },
                pane_id,
                json!(revision),
                json!({
                    "reason": error.code,
                    "message": error.message,
                    "delivery_mode": "terminal_ctrl_c",
                    "provider_interrupt": false,
                }),
            )
        }
    }
}

fn action_result(
    ok: bool,
    action: &str,
    outcome: &str,
    delivery_phase: &str,
    pane_id: &str,
    target_revision: Value,
    detail: Value,
) -> Value {
    json!({
        "ok": ok,
        "action": action,
        "outcome": outcome,
        "delivery_phase": delivery_phase,
        "target": {
            "pane_id": pane_id,
            "target_revision": target_revision,
        },
        "detail": detail,
    })
}

fn invalid(message: &str) -> Value {
    json!({
        "ok": false,
        "outcome": "rejected",
        "delivery_phase": "not_submitted",
        "code": "invalid_params",
        "message": message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_cache::EventCache;
    use serde_json::json;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

    fn pane(agent: Option<&str>, status: &str, revision: u64) -> Value {
        json!({
            "workspace_id": "w1",
            "pane_id": "w1:p1",
            "revision": revision,
            "agent": agent,
            "agent_status": status,
            "agent_session": agent.map(|name| json!({
                "agent": name,
                "kind": "id",
                "source": format!("herdr:{name}"),
                "value": "session-1",
            })).unwrap_or(Value::Null),
        })
    }

    #[test]
    fn target_revision_changes_with_runtime_or_agent_identity() {
        let cache = EventCache::from_snapshot_for_test(json!({"panes": []}));
        let codex = pane(Some("codex"), "working", 1);
        let replaced = pane(Some("codex"), "working", 2);
        let mut new_session = codex.clone();
        new_session["agent_session"]["value"] = json!("session-2");
        assert_ne!(
            target_revision(&cache, &codex),
            target_revision(&cache, &replaced)
        );
        assert_ne!(
            target_revision(&cache, &codex),
            target_revision(&cache, &new_session)
        );
    }

    fn temp_socket() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "herdr-browser-control-{}-{unique}-{sequence}.sock",
            std::process::id()
        ))
    }

    #[test]
    fn agent_prompt_uses_existing_prompt_reliability_and_replays() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            for expected in ["agent.get", "agent.prompt"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], expected);
                let result = if expected == "agent.get" {
                    json!({"agent": {"pane_id": "w1:p1", "agent_status": "idle", "state_change_seq": 1}})
                } else {
                    assert_eq!(request["params"]["target"], "w1:p1");
                    assert_eq!(request["params"]["text"], "keep compatibility");
                    json!({"prompt": {
                        "status": "submitted",
                        "agent": {"pane_id": "w1:p1", "agent_status": "working", "state_change_seq": 2}
                    }})
                };
                writeln!(stream, "{}", json!({"id": request["id"], "result": result})).unwrap();
            }
        });

        let pane = pane(Some("pi"), "idle", 1);
        let cache = EventCache::from_snapshot_for_test(json!({"panes": [pane.clone()]}));
        let revision = target_revision(&cache, &pane).unwrap();
        let request = json!({
            "action": "agent_prompt",
            "target": {"pane_id": "w1:p1", "target_revision": revision},
            "args": {"text": "keep compatibility"},
            "idempotency_key": "browser-control-prompt-1"
        });
        let client = HerdrClient::new(&socket);
        let registry = PromptRegistry::new();
        let first = execute_action(&client, &cache, &registry, &request);
        assert_eq!(first["ok"], true);
        assert_eq!(first["outcome"], "submitted");
        assert_eq!(first["delivery_mode"], "agent_prompt");
        assert_eq!(first["evidence"]["submitted"], true);
        let replay = execute_action(&client, &cache, &registry, &request);
        assert_eq!(replay["idempotent_replay"], true);
        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
    }

    #[test]
    fn steer_probe_never_mislabels_prompt_as_true_steer() {
        let working_codex = pane(Some("codex"), "working", 1);
        let idle_codex = pane(Some("codex"), "idle", 1);
        let claude = pane(Some("claude"), "working", 1);
        assert_eq!(
            control_capabilities(&working_codex)["steer"]["outcome"],
            "session_not_resolved"
        );
        assert_eq!(
            control_capabilities(&idle_codex)["steer"]["outcome"],
            "no_active_turn"
        );
        assert_eq!(
            control_capabilities(&claude)["steer"]["outcome"],
            "unsupported_provider"
        );
        assert_eq!(
            control_capabilities(&working_codex)["interrupt"]["available"],
            true
        );
        assert_eq!(
            control_capabilities(&idle_codex)["interrupt"]["outcome"],
            "no_active_turn"
        );
        let terminal = pane(None, "unknown", 1);
        assert_eq!(
            control_capabilities(&terminal)["terminal_input"]["available"],
            true
        );
        assert_eq!(
            control_capabilities(&working_codex)["terminal_input"]["available"],
            false
        );
    }

    #[test]
    fn terminal_input_runs_on_the_fenced_terminal_pane() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "pane.send_input");
            assert_eq!(request["params"]["pane_id"], "w1:p1");
            assert_eq!(request["params"]["text"], "printf 'ok\\n'");
            assert_eq!(request["params"]["keys"], json!(["Enter"]));
            writeln!(
                stream,
                "{}",
                json!({"id": request["id"], "result": {"type": "ok"}})
            )
            .unwrap();
        });

        let pane = pane(None, "unknown", 1);
        let cache = EventCache::from_snapshot_for_test(json!({"panes": [pane.clone()]}));
        let revision = target_revision(&cache, &pane).unwrap();
        let request = json!({
            "action": "terminal_input",
            "target": {"pane_id": "w1:p1", "target_revision": revision},
            "args": {"text": "printf 'ok\\n'"},
            "idempotency_key": "browser-control-terminal-1"
        });
        let client = HerdrClient::new(&socket);
        let registry = PromptRegistry::new();
        let result = execute_action(&client, &cache, &registry, &request);
        assert_eq!(result["ok"], true);
        assert_eq!(result["outcome"], "submitted");
        assert_eq!(result["detail"]["delivery_mode"], "pane_send_input");
        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
    }

    #[test]
    fn interrupt_sends_ctrl_c_to_the_fenced_agent_pane() {
        let socket = temp_socket();
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "pane.send_keys");
            assert_eq!(request["params"]["pane_id"], "w1:p1");
            assert_eq!(request["params"]["keys"], json!(["C-c"]));
            writeln!(
                stream,
                "{}",
                json!({"id": request["id"], "result": {"type": "ok"}})
            )
            .unwrap();
        });

        let pane = pane(Some("pi"), "working", 1);
        let cache = EventCache::from_snapshot_for_test(json!({"panes": [pane.clone()]}));
        let revision = target_revision(&cache, &pane).unwrap();
        let request = json!({
            "action": "interrupt",
            "target": {"pane_id": "w1:p1", "target_revision": revision},
            "args": {},
            "idempotency_key": "browser-control-interrupt-1"
        });
        let client = HerdrClient::new(&socket);
        let registry = PromptRegistry::new();
        let result = execute_action(&client, &cache, &registry, &request);
        assert_eq!(result["ok"], true);
        assert_eq!(result["outcome"], "interrupted");
        assert_eq!(result["detail"]["delivery_mode"], "terminal_ctrl_c");
        assert_eq!(result["detail"]["provider_interrupt"], false);
        server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
    }
}
