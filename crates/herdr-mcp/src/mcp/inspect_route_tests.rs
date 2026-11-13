use super::*;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

struct MockSocket {
    calls: Arc<Mutex<Vec<String>>>,
    drift_on_hello: Arc<Mutex<Option<(PathBuf, Value)>>>,
    stop: mpsc::Sender<()>,
    worker: thread::JoinHandle<()>,
}

impl MockSocket {
    fn start(path: &Path, core: bool, status: bool) -> Self {
        let listener = UnixListener::bind(path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let received = Arc::clone(&calls);
        let drift_on_hello: Arc<Mutex<Option<(PathBuf, Value)>>> = Arc::new(Mutex::new(None));
        let pending_drift = Arc::clone(&drift_on_hello);
        let (stop, stopped) = mpsc::channel();
        let worker = thread::spawn(move || {
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut reader = BufReader::new(stream.try_clone().unwrap());
                        loop {
                            let mut line = String::new();
                            match reader.read_line(&mut line) {
                                Ok(0) => break,
                                Ok(_) => {}
                                Err(error) => panic!("mock socket read: {error}"),
                            }
                            let request: Value = serde_json::from_str(&line).unwrap();
                            let method = request["method"].as_str().unwrap();
                            received.lock().unwrap().push(method.to_owned());
                            if core
                                && method == "hello"
                                && let Some((path, new_config)) =
                                    pending_drift.lock().unwrap().take()
                            {
                                write_config(&path, &new_config);
                            }
                            let result = match (core, method) {
                                (false, "ping") => json!({"version": "fixture", "protocol": 1}),
                                (true, "hello") => json!({
                                    "generation": 1,
                                    "version": "fixture",
                                    "core": {"id": "fixture", "name": "fixture"},
                                    "framed": false,
                                    "methods": if status { vec!["hello", "server.status"] } else { vec!["hello"] },
                                    "features": []
                                }),
                                (true, "server.status") => json!({"live": 7, "finished": 2}),
                                _ => panic!("unexpected fixture request: {method}"),
                            };
                            writeln!(
                                stream,
                                "{}",
                                json!({
                                    "jsonrpc": "2.0", "id": request["id"], "result": result
                                })
                            )
                            .unwrap();
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stopped.recv_timeout(Duration::from_millis(2)).is_ok() {
                            break;
                        }
                    }
                    Err(error) => panic!("mock socket accept: {error}"),
                }
            }
        });
        Self {
            calls,
            drift_on_hello,
            stop,
            worker,
        }
    }

    fn take_calls(&self) -> Vec<String> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }

    fn finish(self) {
        self.stop.send(()).unwrap();
        self.worker.join().unwrap();
    }
}

fn config(backend: &str, socket: Option<&Path>) -> Value {
    let mut runtime = json!({"backend": backend});
    if let Some(socket) = socket {
        runtime["herdr_core_socket"] = json!(socket.to_str().unwrap());
    }
    json!({"runtime": runtime})
}

fn write_config(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

#[test]
fn selection_drift_uses_admitted_backend_and_socket() {
    const CHILD: &str = "HERDR_INSPECT_ROUTE_TEST_ROOT";
    let Some(root) = std::env::var_os(CHILD).map(PathBuf::from) else {
        // Isolate process-wide environment, config, state and all sockets even
        // when this test is run as part of the ordinary parallel test suite.
        let root = std::env::temp_dir().join(format!("hri-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let bin = root.join("herdr");
        fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
        let config_dir = root.join("config");
        fs::create_dir(&config_dir).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "mcp::inspect_route_tests::selection_drift_uses_admitted_backend_and_socket",
                "--nocapture",
            ])
            .env_clear()
            .env(CHILD, &root)
            .env("HOME", &root)
            .env("TMPDIR", &root)
            .env("PATH", &root)
            .env("HERDR_BIN", &bin)
            .env("HERDR_MCP_CONFIG_DIR", &config_dir)
            .env("HERDR_MCP_DEV_STATE_DIR", root.join("state"))
            .env("HERDR_SOCKET_PATH", root.join("t.sock"))
            .env("XDG_CONFIG_HOME", root.join("xdg-config"))
            .env("XDG_STATE_HOME", root.join("xdg-state"))
            .env("HERDR_MCP_AGENT_ALLOW", "pi")
            .output()
            .unwrap();
        fs::remove_dir_all(&root).unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "child failed:\n{}\n{}",
            stdout,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("1 passed; 0 failed"),
            "child test did not run: {stdout}"
        );
        print!("{stdout}");
        return;
    };

    let tui_path = root.join("t.sock");
    let a_path = root.join("a.sock");
    let b_path = root.join("b.sock");
    let no_status_path = root.join("n.sock");
    let tui = MockSocket::start(&tui_path, false, false);
    let a = MockSocket::start(&a_path, true, true);
    let b = MockSocket::start(&b_path, true, true);
    let no_status = MockSocket::start(&no_status_path, true, false);
    let config_path = root.join("config/config.json");
    let client = HerdrClient::new(&tui_path);
    let cache = EventCache::from_snapshot_for_test(json!({
        "workspaces": [], "tabs": [],
        "panes": [{"pane_id": "p1"}, {"pane_id": "p2"}],
        "agents": [
            {"name": "pi-visible", "agent": "pi", "pane_id": "p1"},
            {"name": "claude-hidden", "agent": "claude", "pane_id": "p2"}
        ]
    }));
    let exec = ExecRegistry::new(root.join("exec")).unwrap();
    let prompt = PromptRegistry::new();
    let skill = SkillService::new();
    let state_store = Arc::new(Mutex::new(StateStore::open(":memory:").unwrap()));
    let context = RuntimeContext {
        client: &client,
        cache: &cache,
        exec: &exec,
        prompt: &prompt,
        skill: &skill,
        state_store: &state_store,
        caller_webchat_control_grants: &[],
        caller_webchat_authorization: None,
        caller_browser_session: None,
        caller_page_assist_grants: &[],
        caller_browser_on_other_device: false,
        caller_relayed_source_turn: None,
        caller_lease_holder_id: None,
        caller_lease_epoch: None,
        caller_lease_token: None,
        browser_actuator: None,
        browser_mutation_gate: None,
        browser_mutation_admission: None,
        trusted_local_ipc: true,
    };

    for preflight in [false, true] {
        let request = if preflight {
            json!({"params": {"name": "herdr_call", "arguments": {
                "method": WORKSTATION_PREFLIGHT_METHOD, "params": {"endpoint_limit": 0}
            }}})
        } else {
            json!({"params": {"name": "herdr_inspect", "arguments": {}}})
        };
        for (label, before, after, expect_tui) in [
            (
                "tui-to-core",
                config("tui", None),
                config("herdr-core", Some(&b_path)),
                true,
            ),
            (
                "tui-to-invalid",
                config("tui", None),
                json!({"runtime": {"backend": "invalid"}}),
                true,
            ),
            (
                "core-to-tui",
                config("herdr-core", Some(&a_path)),
                config("tui", None),
                false,
            ),
            (
                "core-a-to-b",
                config("herdr-core", Some(&a_path)),
                config("herdr-core", Some(&b_path)),
                false,
            ),
        ] {
            write_config(&config_path, &before);
            let name = request["params"]["name"].as_str().unwrap();
            let arguments = request["params"]["arguments"].clone();
            let route = crate::runtime_adapter::public_tool_route(name)
                .unwrap_or_else(|error| panic!("admission failed for {label}: {error}"));
            write_config(&config_path, &after);
            let result = dispatch_admitted_tool(name, arguments, &context, route).unwrap();
            let body: Value =
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(body["ok"], true, "{label}, preflight={preflight}: {body}");
            if expect_tui {
                assert_eq!(tui.take_calls(), ["ping"], "{label}, preflight={preflight}");
                assert_eq!(
                    a.take_calls(),
                    Vec::<String>::new(),
                    "{label}, preflight={preflight}"
                );
                assert_eq!(body["agents"].as_array().unwrap().len(), 1, "{body}");
                assert_eq!(body["agents"][0]["name"], "pi-visible", "{body}");
                assert_eq!(body["agents_hidden"], 1, "{body}");
                if !preflight {
                    assert_eq!(body["panes"][0]["agent"]["name"], "pi-visible", "{body}");
                    assert_eq!(body["panes"][1]["agent"], Value::Null, "{body}");
                    assert_eq!(body["runtime_backend"]["selected"], "tui");
                    assert_eq!(body["runtime_backend"]["source"], "default");
                }
            } else {
                assert_eq!(
                    tui.take_calls(),
                    Vec::<String>::new(),
                    "{label}, preflight={preflight}"
                );
                assert_eq!(
                    a.take_calls(),
                    ["hello", "hello", "server.status"],
                    "{label}, preflight={preflight}"
                );
                assert_eq!(body["agents"], json!([]));
                if !preflight {
                    assert_eq!(body["runtime_backend"]["selected"], "herdr-core");
                    assert_eq!(body["runtime_backend"]["herdr_core_socket"], json!(a_path));
                    assert_eq!(body["herdr_core"]["live_terminals"], 7);
                    assert_eq!(
                        body["herdr_core"]["terminal_counts_scope"],
                        "global, not visibility-filtered"
                    );
                }
            }
            assert_eq!(
                b.take_calls(),
                Vec::<String>::new(),
                "non-admitted socket contacted: {label}, preflight={preflight}"
            );
            assert_eq!(no_status.take_calls(), Vec::<String>::new());
        }
    }

    // Exercise the complete public entry as well: the HC fixture rewrites
    // config after selection, before returning the admission hello.
    for preflight in [false, true] {
        write_config(&config_path, &config("herdr-core", Some(&a_path)));
        *a.drift_on_hello.lock().unwrap() =
            Some((config_path.clone(), config("herdr-core", Some(&b_path))));
        let request = if preflight {
            json!({"params": {"name": "herdr_call", "arguments": {
                "method": WORKSTATION_PREFLIGHT_METHOD, "params": {"endpoint_limit": 0}
            }}})
        } else {
            json!({"params": {"name": "herdr_inspect", "arguments": {}}})
        };
        let result = tool_call(&request, &context).unwrap();
        let body: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            body["ok"], true,
            "public call, preflight={preflight}: {body}"
        );
        assert_eq!(
            a.take_calls(),
            ["hello", "hello", "server.status"],
            "preflight={preflight}"
        );
        assert_eq!(
            b.take_calls(),
            Vec::<String>::new(),
            "new socket contacted: preflight={preflight}"
        );
        assert_eq!(tui.take_calls(), Vec::<String>::new());
        assert!(a.drift_on_hello.lock().unwrap().is_none());
    }
    println!(
        "validated direct inspect and preflight with TUI, invalid config, backend drift, and socket drift"
    );

    // Invalid selection must fail at admission, before contacting any socket.
    write_config(&config_path, &json!({"runtime": {"backend": "invalid"}}));
    let direct = json!({"params": {"name": "herdr_inspect", "arguments": {}}});
    let result = tool_call(&direct, &context).unwrap();
    let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["code"], "invalid_runtime_backend", "{body}");
    assert_eq!(result["isError"], true);
    assert_eq!(tui.take_calls(), Vec::<String>::new());
    assert_eq!(a.take_calls(), Vec::<String>::new());
    assert_eq!(b.take_calls(), Vec::<String>::new());
    assert_eq!(no_status.take_calls(), Vec::<String>::new());

    // An HC hello without server.status must still reject herdr_inspect before
    // execution, while preflight's nested inspect must not call that method.
    write_config(&config_path, &config("herdr-core", Some(&no_status_path)));
    let direct = json!({"params": {"name": "herdr_inspect", "arguments": {}}});
    let result = tool_call(&direct, &context).unwrap();
    let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["code"], "runtime_capability_unsupported", "{body}");
    assert_eq!(no_status.take_calls(), ["hello"]);
    let preflight = json!({"params": {"name": "herdr_call", "arguments": {
        "method": WORKSTATION_PREFLIGHT_METHOD, "params": {"endpoint_limit": 0}
    }}});
    let result = tool_call(&preflight, &context).unwrap();
    let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(no_status.take_calls(), ["hello", "hello"]);
    assert_eq!(tui.take_calls(), Vec::<String>::new());
    assert_eq!(a.take_calls(), Vec::<String>::new());
    assert_eq!(b.take_calls(), Vec::<String>::new());

    tui.finish();
    a.finish();
    b.finish();
    no_status.finish();
}
