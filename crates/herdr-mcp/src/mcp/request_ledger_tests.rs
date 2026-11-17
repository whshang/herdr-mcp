//! 1.2/S2 end-to-end coverage for `client_request_id` dedupe and
//! `herdr_mcp.request.status` through the real `tool_call` boundary.

use super::*;
use std::sync::{Arc, Mutex};

fn ledger_context<'a>(
    client: &'a HerdrClient,
    cache: &'a EventCache,
    exec: &'a ExecRegistry,
    prompt: &'a PromptRegistry,
    skill: &'a SkillService,
    state_store: &'a Arc<Mutex<StateStore>>,
    backend: &'a crate::runtime_adapter::RuntimeBackendSelection,
) -> RuntimeContext<'a> {
    RuntimeContext {
        client,
        cache,
        exec,
        prompt,
        skill,
        state_store,
        caller_webchat_control_grants: &[],
        caller_webchat_authorization: None,
        caller_browser_session: None,
        caller_page_assist_grants: &[],
        caller_browser_on_other_device: false,
        caller_relayed_source_turn: None,
        caller_lease_holder_id: None,
        caller_lease_epoch: None,
        caller_lease_token: None,
        local_device_id: None,
        browser_actuator: None,
        browser_mutation_gate: None,
        browser_mutation_admission: None,
        runtime_backend: backend,
        trusted_local_ipc: true,
        wire_request_id: None,
    }
}

fn call_body(result: &Value) -> Value {
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

fn status_request(key_field: &str, key: &str) -> Value {
    json!({"params": {"name": "herdr_call", "arguments": {
        "method": "herdr_mcp.request.status",
        "params": {key_field: key}
    }}})
}

#[test]
fn client_request_id_dedupes_and_status_resolves() {
    let root = std::env::temp_dir().join(format!(
        "herdr-ledger-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let client = HerdrClient::new(root.join("absent.sock"));
    let cache = EventCache::from_snapshot_for_test(json!({
        "workspaces": [], "tabs": [], "panes": [], "agents": []
    }));
    let exec = ExecRegistry::new(root.join("exec")).unwrap();
    let prompt = PromptRegistry::new();
    let skill = SkillService::new();
    let state_store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
    let backend = crate::test_env::tui_backend_selection();
    let context = ledger_context(
        &client,
        &cache,
        &exec,
        &prompt,
        &skill,
        &state_store,
        &backend,
    );

    // 1) First submission executes and records a request_id.
    let tracked = json!({"params": {"name": "herdr_call", "arguments": {
        "method": "herdr_mcp.request.status",
        "params": {"request_id": "never-recorded"},
        "client_request_id": "s2-test-key-1"
    }}});
    let first = call_body(&tool_call(&tracked, &context).unwrap());
    assert_eq!(first["ok"], true, "{first}");
    let request_id = first["request_id"].as_str().unwrap().to_owned();
    assert!(request_id.starts_with("req_"), "{first}");

    // 2) Same key replays the recorded result — deduplicated, same request_id.
    let replay = call_body(&tool_call(&tracked, &context).unwrap());
    assert_eq!(replay["deduplicated"], true, "{replay}");
    assert_eq!(replay["request_id"].as_str().unwrap(), request_id);

    // 3) request.status by client_request_id reports `applied`.
    let status = call_body(
        &tool_call(
            &status_request("client_request_id", "s2-test-key-1"),
            &context,
        )
        .unwrap(),
    );
    assert_eq!(status["ok"], true, "{status}");
    assert_eq!(status["found"], true, "{status}");
    assert_eq!(status["state"], "applied", "{status}");
    assert_eq!(status["request_id"].as_str().unwrap(), request_id);

    // 4) The same record resolves by its runtime request_id too.
    let by_id =
        call_body(&tool_call(&status_request("request_id", &request_id), &context).unwrap());
    assert_eq!(by_id["found"], true, "{by_id}");
    assert_eq!(by_id["state"], "applied", "{by_id}");

    // 5) An unrecorded key reports `unknown` without failing.
    let missing = call_body(
        &tool_call(
            &status_request("client_request_id", "s2-no-such-key"),
            &context,
        )
        .unwrap(),
    );
    assert_eq!(missing["ok"], true, "{missing}");
    assert_eq!(missing["found"], false, "{missing}");
    assert_eq!(missing["state"], "unknown", "{missing}");

    // 6) A pre-delivery failure is recorded as `not_delivered` and replayed,
    // not re-executed.
    let rejected_call = json!({"params": {"name": "herdr_call", "arguments": {
        "method": "herdr_mcp.request.status",
        "params": {},
        "client_request_id": "s2-test-key-rejected"
    }}});
    let rejected = call_body(&tool_call(&rejected_call, &context).unwrap());
    assert_eq!(rejected["ok"], false, "{rejected}");
    let rejected_status = call_body(
        &tool_call(
            &status_request("client_request_id", "s2-test-key-rejected"),
            &context,
        )
        .unwrap(),
    );
    assert_eq!(
        rejected_status["state"], "not_delivered",
        "{rejected_status}"
    );
    let rejected_replay = call_body(&tool_call(&rejected_call, &context).unwrap());
    assert_eq!(rejected_replay["deduplicated"], true, "{rejected_replay}");

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn client_request_id_hygiene_rejects_before_ledger() {
    let root = std::env::temp_dir().join(format!(
        "herdr-ledger-hygiene-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let client = HerdrClient::new(root.join("absent.sock"));
    let cache = EventCache::from_snapshot_for_test(json!({
        "workspaces": [], "tabs": [], "panes": [], "agents": []
    }));
    let exec = ExecRegistry::new(root.join("exec")).unwrap();
    let prompt = PromptRegistry::new();
    let skill = SkillService::new();
    let state_store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
    let backend = crate::test_env::tui_backend_selection();
    let context = ledger_context(
        &client,
        &cache,
        &exec,
        &prompt,
        &skill,
        &state_store,
        &backend,
    );

    for bad in [json!(" pad"), json!("tab\tinside"), json!(42)] {
        let request = json!({"params": {"name": "herdr_call", "arguments": {
            "method": "herdr_mcp.request.status",
            "params": {},
            "client_request_id": bad
        }}});
        let body = call_body(&tool_call(&request, &context).unwrap());
        assert_eq!(body["ok"], false, "{body}");
        assert_eq!(body["code"], "invalid_params", "{body}");
        assert_eq!(body["delivery_state"], "not_delivered", "{body}");
    }

    // An invalid key never lands in the ledger.
    let status =
        call_body(&tool_call(&status_request("client_request_id", " pad"), &context).unwrap());
    assert_eq!(status["found"], false, "{status}");

    std::fs::remove_dir_all(&root).ok();
}

/// user caller | Given one key already settled for a request | When the same
/// key arrives with different params | Then it is an idempotency conflict,
/// never a replay of the first request's result.
#[test]
fn same_key_different_request_is_idempotency_conflict() {
    let root = std::env::temp_dir().join(format!(
        "herdr-ledger-conflict-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let client = HerdrClient::new(root.join("absent.sock"));
    let cache = EventCache::from_snapshot_for_test(json!({
        "workspaces": [], "tabs": [], "panes": [], "agents": []
    }));
    let exec = ExecRegistry::new(root.join("exec")).unwrap();
    let prompt = PromptRegistry::new();
    let skill = SkillService::new();
    let state_store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
    let backend = crate::test_env::tui_backend_selection();
    let context = ledger_context(
        &client,
        &cache,
        &exec,
        &prompt,
        &skill,
        &state_store,
        &backend,
    );

    let first_call = |params: Value| {
        json!({"params": {"name": "herdr_call", "arguments": {
            "method": "herdr_mcp.request.status",
            "params": params,
            "client_request_id": "conflict-key"
        }}})
    };
    let first =
        call_body(&tool_call(&first_call(json!({"request_id": "a"})).clone(), &context).unwrap());
    assert_eq!(first["ok"], true, "{first}");

    // Same key, different params body: conflict, not replay.
    let conflict =
        call_body(&tool_call(&first_call(json!({"request_id": "different"})), &context).unwrap());
    assert_eq!(conflict["ok"], false, "{conflict}");
    assert_eq!(conflict["code"], "idempotency_conflict", "{conflict}");
    assert_eq!(conflict["delivery_state"], "not_delivered", "{conflict}");

    // The original request still replays with its own params.
    let replay = call_body(&tool_call(&first_call(json!({"request_id": "a"})), &context).unwrap());
    assert_eq!(replay["deduplicated"], true, "{replay}");

    std::fs::remove_dir_all(&root).ok();
}
