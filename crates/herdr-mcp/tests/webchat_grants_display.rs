//! Process-level regression test for the #772 WebChat grant display
//! (review round 1, BLOCKING item): the **default** `herdr-mcp doctor`
//! (non-verbose) and `herdr-mcp status --verbose` outputs must contain the
//! grant state (`state=`) and per-endpoint consent revision
//! (`consent_revision`) plus the distinguishing hint.
//!
//! Runs the real binary with an isolated config dir, mirroring the
//! `tcc_broker_process.rs` convention.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn test_root(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "herdr-mcp-webchat-grants-display-{name}-{}-{nonce}",
        std::process::id()
    ))
}

fn binary_under_test() -> PathBuf {
    let mut root = std::env::current_dir().expect("test working directory");
    loop {
        let manifest = root.join("Cargo.toml");
        if manifest.is_file()
            && fs::read_to_string(&manifest).is_ok_and(|text| text.contains("[workspace]"))
        {
            break;
        }
        assert!(root.pop(), "cannot locate workspace root from test cwd");
    }
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let binary = root.join("target").join(profile).join("herdr-mcp");
    assert!(
        binary.is_file(),
        "missing binary under test: {} (run `cargo build -p herdr-mcp` first)",
        binary.display()
    );
    binary
}

fn run_with_config(args: &[&str], config: &Path, allowed_codes: &[u8]) -> String {
    let output = Command::new(binary_under_test())
        .args(args)
        .env("HERDR_MCP_CONFIG_DIR", config)
        .env_remove("HERDR_MCP_INSTANCE")
        .output()
        .expect("spawn herdr-mcp");
    let code = output.status.code().expect("clean exit");
    assert!(
        allowed_codes.contains(&(code as u8)),
        "unexpected exit {code} for {args:?}:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Minimal grant-surface tables matching the frozen read surface the
/// doctor/status snapshot SELECTs (`browser_endpoints` v7, `browser_dispatches`
/// v13 columns, `browser_caller_sessions` v15). Read-only: the snapshot only
/// ever SELECTs these named columns.
fn seed_state_db(db_path: &Path) {
    let conn = rusqlite::Connection::open(db_path).expect("open seed db");
    conn.execute_batch(
        r#"
        CREATE TABLE browser_endpoints (
            endpoint_ref TEXT PRIMARY KEY NOT NULL,
            device_id TEXT NOT NULL,
            browser_family TEXT NOT NULL,
            extension_version TEXT NOT NULL,
            webchat_control_allowed INTEGER NOT NULL DEFAULT 0,
            tool_bridge_allowed INTEGER NOT NULL DEFAULT 0,
            tool_bridge_mutation_allowed INTEGER NOT NULL DEFAULT 0,
            consent_revision INTEGER NOT NULL DEFAULT 0,
            first_observed_at INTEGER NOT NULL,
            last_observed_at INTEGER NOT NULL
        );
        CREATE TABLE browser_caller_sessions (
            principal_ref TEXT NOT NULL,
            provider TEXT NOT NULL,
            caller_session_sha256 TEXT NOT NULL,
            session_ref TEXT NOT NULL,
            first_bound_at INTEGER NOT NULL,
            last_bound_at INTEGER NOT NULL,
            PRIMARY KEY (principal_ref, provider, caller_session_sha256)
        );
        CREATE TABLE browser_dispatches (
            dispatch_id TEXT PRIMARY KEY NOT NULL,
            endpoint_ref TEXT NOT NULL,
            provider TEXT NOT NULL,
            operation TEXT NOT NULL,
            target_session_ref TEXT NOT NULL,
            request_digest TEXT NOT NULL,
            message_digest TEXT NOT NULL,
            reasoning_effort TEXT,
            required_apps_json TEXT NOT NULL,
            expected_generation INTEGER NOT NULL,
            idempotency_key_digest TEXT NOT NULL,
            delivery_state TEXT NOT NULL,
            generation_owner INTEGER,
            work_chain_id TEXT,
            lane_id TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            authorization_principal_ref TEXT,
            authorization_connector_id TEXT,
            authorization_grant_generation INTEGER
        );
        INSERT INTO browser_endpoints(
            endpoint_ref, device_id, browser_family, extension_version,
            webchat_control_allowed, tool_bridge_allowed,
            tool_bridge_mutation_allowed, consent_revision,
            first_observed_at, last_observed_at
        ) VALUES ('bep_test_endpoint_placeholder', 'dev_test', 'chrome', '0.0.0-test',
                  1, 0, 0, 4, 1, 2);
        "#,
    )
    .expect("seed grant tables");
}

#[test]
fn default_doctor_and_status_verbose_show_grant_state_and_consent_revision() {
    let root = test_root("display");
    let config = root.join("config");
    fs::create_dir_all(&config).unwrap();

    // An empty config dir has no runtime to probe, so doctor legitimately
    // exits 2 (service_health fail); exit 0 when the host is fully healthy.
    // The grant-display contract is independent of that outcome.
    let doctor = run_with_config(&["doctor"], &config, &[0, 2]);
    assert!(
        doctor.contains("state=unknown"),
        "default doctor missing grant state:\n{doctor}"
    );
    assert!(
        doctor.contains("grant_missing"),
        "default doctor missing grant hint:\n{doctor}"
    );

    let status = run_with_config(&["status", "--verbose"], &config, &[0]);
    assert!(
        status.contains("state=unknown"),
        "status --verbose missing grant state:\n{status}"
    );

    // With a state DB present, default doctor and status --verbose must show
    // the per-endpoint consent line including consent_revision.
    seed_state_db(&config.join("state.db"));
    let doctor = run_with_config(&["doctor"], &config, &[0, 2]);
    assert!(
        doctor.contains("state=absent"),
        "default doctor missing grant state=absent:\n{doctor}"
    );
    assert!(
        doctor.contains("consent_revision=4"),
        "default doctor missing consent_revision line:\n{doctor}"
    );
    assert!(
        doctor.contains("bep_test_endpoint_placeholder"),
        "default doctor missing endpoint ref:\n{doctor}"
    );

    let status = run_with_config(&["status", "--verbose"], &config, &[0]);
    assert!(
        status.contains("state=absent"),
        "status --verbose missing grant state=absent:\n{status}"
    );
    assert!(
        status.contains("consent_revision=4"),
        "status --verbose missing consent_revision line:\n{status}"
    );
    // No line may print twice: the `state=` rollup must not be repeated by
    // the detail iteration, and each consent line appears exactly once.
    let status_lines: Vec<&str> = status.lines().collect();
    let state_lines = status_lines
        .iter()
        .filter(|line| line.contains("WebChat grants: state="))
        .count();
    assert_eq!(
        state_lines, 1,
        "state= rollup printed {state_lines} times:\n{status}"
    );
    let consent_lines = status_lines
        .iter()
        .filter(|line| line.contains("consent_revision=4"))
        .count();
    assert_eq!(
        consent_lines, 1,
        "consent detail line printed {consent_lines} times:\n{status}"
    );

    let _ = fs::remove_dir_all(&root);
}
