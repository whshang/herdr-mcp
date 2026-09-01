//! End-to-end integration test for the stable TCC broker.
//!
//! This exercises the real `herdr-mcp` binary's `__tcc-broker` one-shot mode
//! over a real stdin/stdout process boundary, including CLI parsing and
//! `run_broker_once`. It also proves the broker path and identity are
//! generation-independent: simulating runtime generation rotation leaves the
//! installed broker bytes and SHA-256 untouched.

use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const PROTOCOL_VERSION: u32 = 1;

fn test_root(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "herdr-mcp-tcc-broker-{name}-{}-{nonce}",
        std::process::id()
    ))
}

fn init_git_repo(root: &Path) {
    let commands: [&[&str]; 3] = [
        &["init", "-q"],
        &["config", "user.email", "test@example.com"],
        &["config", "user.name", "Test"],
    ];
    for args in commands {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success());
    }
}

fn make_snapshot(root: &Path) -> Value {
    json!({
        "workspaces": [
            {
                "id": "ws-1",
                "name": "test",
                "panes": [
                    {
                        "pane_id": "pane-1",
                        "cwd": root.to_string_lossy(),
                        "agent_status": "idle"
                    }
                ]
            }
        ],
        "panes": [
            {
                "pane_id": "pane-1",
                "cwd": root.to_string_lossy(),
                "agent_status": "idle"
            }
        ],
        "agents": []
    })
}

fn request(op: &str, snapshot: &Value, args: Value) -> Value {
    json!({
        "protocol": "herdr-tcc-broker",
        "version": PROTOCOL_VERSION,
        "op": op,
        "snapshot": snapshot,
        "args": args,
    })
}

/// Run the real `herdr-mcp __tcc-broker` binary with a JSON request on stdin,
/// returning the parsed JSON response.
fn run_broker_binary(request_bytes: &[u8]) -> Value {
    let binary = env!("CARGO_BIN_EXE_herdr-mcp");
    let mut child = Command::new(binary)
        .arg("__tcc-broker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn herdr-mcp __tcc-broker");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(request_bytes)
        .unwrap();
    let output = child.wait_with_output().expect("wait for broker");
    assert!(
        output.status.success(),
        "broker exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("broker stdout is JSON")
}

fn sha256(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        use std::io::Read;
        let read = file.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    format!("{:x}", hash.finalize())
}

#[test]
fn real_binary_broker_round_trip_dispatch_and_rejection() {
    let root = test_root("roundtrip");
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    init_git_repo(&repo);
    fs::write(repo.join("hello.txt"), "hello world\n").unwrap();
    let snapshot = make_snapshot(&repo);

    // fs_read through the real binary.
    let read = request(
        "fs_read",
        &snapshot,
        json!({"path": repo.join("hello.txt")}),
    );
    let out = run_broker_binary(&serde_json::to_vec(&read).unwrap());
    assert_eq!(out["ok"].as_bool(), Some(true));
    assert!(out["content"].as_str().unwrap().contains("hello world"));

    // git status through the real binary.
    let git = request("git", &snapshot, json!({"root": repo, "action": "status"}));
    let out = run_broker_binary(&serde_json::to_vec(&git).unwrap());
    assert_eq!(out["ok"].as_bool(), Some(true));

    // Outside managed roots rejected.
    let outside = request("fs_read", &snapshot, json!({"path": "/etc/hosts"}));
    let out = run_broker_binary(&serde_json::to_vec(&outside).unwrap());
    assert_eq!(out["ok"].as_bool(), Some(false));
    assert_eq!(out["reason"].as_str(), Some("outside_managed_roots"));

    // Secret path rejected.
    let secret = request(
        "fs_read",
        &snapshot,
        json!({"path": repo.join(".git/config")}),
    );
    let out = run_broker_binary(&serde_json::to_vec(&secret).unwrap());
    assert_eq!(out["ok"].as_bool(), Some(false));
    assert_eq!(out["reason"].as_str(), Some("secret_path_denied"));

    // Unknown operation rejected at the process boundary.
    let bad_op = request("rm_rf", &snapshot, json!({}));
    let out = run_broker_binary(&serde_json::to_vec(&bad_op).unwrap());
    assert_eq!(out["ok"].as_bool(), Some(false));
    assert_eq!(out["code"].as_str(), Some("dispatch_failed"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn broker_identity_survives_generation_rotation() {
    let root = test_root("rotation");
    let config = root.join("config");
    let broker = config.join("tcc-broker").join("herdr-mcp-broker");

    // Install the broker via the real binary.
    let binary = env!("CARGO_BIN_EXE_herdr-mcp");
    let status = Command::new(binary)
        .args(["tcc-broker", "install"])
        .env("HERDR_MCP_CONFIG_DIR", &config)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(broker.is_file());
    let original_sha = sha256(&broker);
    let original_bytes = fs::read(&broker).unwrap();

    // Simulate two runtime generation rotations under runtime/generations.
    for generation in ["rust-aaaa", "rust-bbbb"] {
        let generation_dir = config.join("runtime").join("generations").join(generation);
        fs::create_dir_all(&generation_dir).unwrap();
        fs::write(generation_dir.join("herdr-mcp"), b"rotating-runtime-binary").unwrap();
        // Point runtime/current at the new generation.
        let current = config.join("runtime").join("current");
        let _ = fs::remove_dir_all(&current);
        fs::create_dir_all(current.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&generation_dir, &current).unwrap();
    }

    // The broker path and bytes must be untouched by rotation.
    assert!(broker.is_file());
    assert_eq!(fs::read(&broker).unwrap(), original_bytes);
    assert_eq!(sha256(&broker), original_sha);

    // Re-running install is idempotent and preserves the identity.
    let status = Command::new(binary)
        .args(["tcc-broker", "install"])
        .env("HERDR_MCP_CONFIG_DIR", &config)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(sha256(&broker), original_sha);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn broker_status_reports_installed_identity() {
    let root = test_root("status");
    let config = root.join("config");
    let binary = env!("CARGO_BIN_EXE_herdr-mcp");

    // Not installed yet.
    let output = Command::new(binary)
        .args(["tcc-broker", "status"])
        .env("HERDR_MCP_CONFIG_DIR", &config)
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("not installed"));

    // Install then status reports sha256.
    let status = Command::new(binary)
        .args(["tcc-broker", "install"])
        .env("HERDR_MCP_CONFIG_DIR", &config)
        .status()
        .unwrap();
    assert!(status.success());
    let output = Command::new(binary)
        .args(["tcc-broker", "status"])
        .env("HERDR_MCP_CONFIG_DIR", &config)
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("installed"));
    assert!(text.contains("sha256:"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn permissions_status_setup_verify_and_broker_preservation() {
    let root = test_root("permissions");
    let config = root.join("config");
    let broker = config.join("tcc-broker").join("herdr-mcp-broker");
    let binary = env!("CARGO_BIN_EXE_herdr-mcp");
    // Never let this process-level test probe the developer's real protected
    // ~/Documents. The broker still exercises the same read_dir + W_OK path,
    // but against an isolated synthetic HOME.
    fs::create_dir_all(root.join("Documents")).unwrap();

    let output = Command::new(binary)
        .args(["permissions", "status"])
        .env("HERDR_MCP_CONFIG_DIR", &config)
        .env("HOME", &root)
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("needs_setup")
            || text.contains("not_applicable")
            || text.contains("granted")
            || text.contains("denied")
            || text.contains("unknown")
            || text.contains("timeout")
    );
    assert!(text.contains("hint:"));
    assert!(text.contains("status:"));
    assert!(!text.to_ascii_lowercase().contains("developer id"));

    let output = Command::new(binary)
        .args(["permissions", "setup"])
        .env("HERDR_MCP_CONFIG_DIR", &config)
        .env("HERDR_MCP_PERMISSIONS_DRY_RUN", "1")
        .env("HOME", &root)
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(!text.to_ascii_lowercase().contains("granted permission"));
    assert!(
        text.contains("does not grant permission")
            || text.contains("not_applicable")
            || text.contains("broker_installed:")
    );
    assert!(broker.is_file());

    #[cfg(target_os = "macos")]
    {
        let marker = config.join("tcc-broker").join("authorization-required");
        assert!(
            marker.is_file(),
            "fresh setup must require explicit FDA verification"
        );
        let status = Command::new(binary)
            .args(["permissions", "status"])
            .env("HERDR_MCP_CONFIG_DIR", &config)
            .env("HOME", &root)
            .output()
            .unwrap();
        assert!(status.status.success());
        let status_text = String::from_utf8_lossy(&status.stdout);
        assert!(status_text.contains("status: needs_setup"));
        assert!(status_text.contains("probe: skipped_authorization_pending"));
    }

    let output = Command::new(binary)
        .args(["permissions", "verify"])
        .env("HERDR_MCP_CONFIG_DIR", &config)
        .env("HOME", &root)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("status:") || text.contains("not_applicable"));
    assert!(
        text.contains("granted")
            || text.contains("denied")
            || text.contains("needs_setup")
            || text.contains("unknown")
            || text.contains("timeout")
            || text.contains("not_applicable")
    );
    #[cfg(target_os = "macos")]
    if output.status.success() && text.contains("status: granted") {
        assert!(
            !config
                .join("tcc-broker")
                .join("authorization-required")
                .exists(),
            "successful verify must clear the onboarding marker"
        );
    }

    fs::write(&broker, b"different-stable-broker-identity").unwrap();
    let output = Command::new(binary)
        .args(["permissions", "setup"])
        .env("HERDR_MCP_CONFIG_DIR", &config)
        .env("HERDR_MCP_PERMISSIONS_DRY_RUN", "1")
        .env("HOME", &root)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        fs::read(&broker).unwrap(),
        b"different-stable-broker-identity"
    );

    let _ = fs::remove_dir_all(&root);
}
