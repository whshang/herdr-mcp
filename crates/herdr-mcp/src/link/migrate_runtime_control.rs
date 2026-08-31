//! Prepare / apply a Rust-compatible prod runtime-control generation.
//!
//! Node `link-prod` still owns LaunchAgent today. Its control document often
//! keeps Node-era ids like `stable-0.3.32` even while MCP `runtime/current` is
//! already a `rust-*` generation. This helper plans (and optionally writes) a
//! replacement `runtime-control-prod.json` that points `desired_active` at the
//! active managed Rust generation id while keeping the same loopback MCP
//! endpoint.
//!
//! Hard rules for this slice:
//! - Never mutates LaunchAgents (`link` / `link-prod` / candidate).
//! - Never points anything at checkout / `target/`.
//! - Default is dry-run; `--write-staging` writes a pending sibling file only;
//!   `--apply` requires `HERDR_LINK_MIGRATE_RUNTIME_CONTROL=1` and rewrites the
//!   live control document (with backup). Status is left for the running Link
//!   control loop to refresh after it polls the new revision.

use serde_json::{Map, Value, json};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use super::install::{managed_runtime_binary, resolve_managed_runtime_binary};
use super::ownership::{
    generation_looks_node_era, generation_looks_rust_compatible, read_control_desired_active,
    read_status_active_generation,
};
use super::runtime_control::validate_runtime_control_document;

/// Env guard required before `--apply` rewrites the live prod control file.
pub const MIGRATE_APPLY_ENV: &str = "HERDR_LINK_MIGRATE_RUNTIME_CONTROL";

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8772/mcp";
const DEFAULT_HEALTH_URL: &str = "http://127.0.0.1:8772/health";
const STAGING_SUFFIX: &str = ".rust-pending.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrateMode {
    DryRun,
    WriteStaging,
    Apply,
}

/// CLI entry for `herdr-mcp link migrate-runtime-control`.
pub fn run(mode: MigrateMode) -> Result<ExitCode, String> {
    let home =
        home_dir().ok_or_else(|| "HOME is required for link migrate-runtime-control".to_owned())?;
    let config_dir = home.join(".config").join("herdr-mcp");
    let report = match mode {
        MigrateMode::DryRun => plan_migrate(&home, &config_dir, MigrateMode::DryRun)?,
        MigrateMode::WriteStaging => write_staging(&home, &config_dir)?,
        MigrateMode::Apply => apply_live(&home, &config_dir)?,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    );
    let ok = report.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if ok {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(2))
    }
}

fn write_staging(home: &Path, config_dir: &Path) -> Result<Value, String> {
    let plan = plan_migrate(home, config_dir, MigrateMode::WriteStaging)?;
    if plan.get("already_rust_compatible").and_then(Value::as_bool) == Some(true) {
        return Ok(plan);
    }
    if plan.get("ok").and_then(Value::as_bool) != Some(true) {
        return Ok(plan);
    }
    let planned = plan
        .get("planned_control")
        .cloned()
        .ok_or_else(|| "migrate plan missing planned_control".to_owned())?;
    let staging_path = plan
        .get("staging_path")
        .and_then(Value::as_str)
        .ok_or_else(|| "migrate plan missing staging_path".to_owned())?;
    let staging = PathBuf::from(staging_path);
    atomic_json(&staging, &planned)?;
    let mut out = plan;
    if let Some(object) = out.as_object_mut() {
        object.insert("wrote_staging".to_owned(), json!(true));
        object.insert(
            "notes".to_owned(),
            json!([
                "Wrote staging control document only; live runtime-control-prod.json unchanged.",
                "Node link/link-prod LaunchAgents were not mutated.",
                "Use --apply with HERDR_LINK_MIGRATE_RUNTIME_CONTROL=1 to rewrite the live prod control file.",
                "After apply, wait for Link to poll and refresh runtime-status-prod.json before expecting the gate to flip.",
            ]),
        );
    }
    Ok(out)
}

fn apply_live(home: &Path, config_dir: &Path) -> Result<Value, String> {
    let understood = env::var_os(MIGRATE_APPLY_ENV)
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    if !understood {
        return Ok(json!({
            "ok": false,
            "mode": "apply",
            "applied": false,
            "launchd_mutated": false,
            "error": format!(
                "link migrate-runtime-control --apply refused without {MIGRATE_APPLY_ENV}=1"
            ),
            "notes": [
                "No control/status/launchd mutation occurred.",
                "Use: herdr-mcp link migrate-runtime-control --dry-run",
                "Or: herdr-mcp link migrate-runtime-control --write-staging",
            ],
        }));
    }

    let plan = plan_migrate(home, config_dir, MigrateMode::Apply)?;
    if plan.get("already_rust_compatible").and_then(Value::as_bool) == Some(true) {
        let mut out = plan;
        if let Some(object) = out.as_object_mut() {
            object.insert("applied".to_owned(), json!(false));
            object.insert("launchd_mutated".to_owned(), json!(false));
            object.insert(
                "notes".to_owned(),
                json!([
                    "Live prod control already uses a Rust-compatible generation; apply is a no-op.",
                    "Node link/link-prod LaunchAgents were not mutated.",
                ]),
            );
        }
        return Ok(out);
    }
    if plan.get("ok").and_then(Value::as_bool) != Some(true) {
        let mut out = plan;
        if let Some(object) = out.as_object_mut() {
            object.insert("applied".to_owned(), json!(false));
            object.insert("launchd_mutated".to_owned(), json!(false));
        }
        return Ok(out);
    }

    let control_path = plan
        .get("control_path")
        .and_then(Value::as_str)
        .ok_or_else(|| "migrate plan missing control_path".to_owned())?;
    let control_path = PathBuf::from(control_path);
    let planned = plan
        .get("planned_control")
        .cloned()
        .ok_or_else(|| "migrate plan missing planned_control".to_owned())?;

    let backup_path = backup_control_path(config_dir);
    if control_path.is_file() {
        if let Some(parent) = backup_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!("cannot create backup dir {}: {error}", parent.display())
            })?;
        }
        fs::copy(&control_path, &backup_path).map_err(|error| {
            format!(
                "cannot backup {} -> {}: {error}",
                control_path.display(),
                backup_path.display()
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&backup_path, fs::Permissions::from_mode(0o600));
        }
    }

    atomic_json(&control_path, &planned)?;

    let mut out = plan;
    if let Some(object) = out.as_object_mut() {
        object.insert("applied".to_owned(), json!(true));
        object.insert("launchd_mutated".to_owned(), json!(false));
        object.insert(
            "backup_path".to_owned(),
            json!(backup_path.display().to_string()),
        );
        object.insert(
            "notes".to_owned(),
            json!([
                "Rewrote live prod runtime-control document only.",
                "Did not mutate LaunchAgents, runtime/current, or runtime-status*.json.",
                "Node link-prod remains the production Link owner until a later cutover execute.",
                "Wait for Link to poll the new revision; then re-run link status / link cutover --dry-run.",
                "ready_for_execute still requires dual verification UAT and remaining LaunchAgent gates.",
            ]),
        );
    }
    Ok(out)
}

/// Internal lifecycle reconciliation used after the default service changes
/// `runtime/current`. Production is already Rust-owned here, so the live
/// control document must follow the exact managed generation automatically.
pub(crate) fn reconcile_current_generation(home: &Path, config_dir: &Path) -> Result<bool, String> {
    let plan = plan_migrate(home, config_dir, MigrateMode::Apply)?;
    if plan.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(format!("runtime-control reconcile plan failed: {plan}"));
    }
    if plan.get("already_rust_compatible").and_then(Value::as_bool) == Some(true) {
        return Ok(false);
    }
    let control_path = plan
        .get("control_path")
        .and_then(Value::as_str)
        .ok_or_else(|| "runtime-control reconcile plan missing control_path".to_owned())?;
    let planned = plan
        .get("planned_control")
        .cloned()
        .ok_or_else(|| "runtime-control reconcile plan missing planned_control".to_owned())?;
    atomic_json(Path::new(control_path), &planned)?;
    Ok(true)
}

/// Build the migration plan without writing files.
pub fn plan_migrate(home: &Path, config_dir: &Path, mode: MigrateMode) -> Result<Value, String> {
    // Touch managed runtime so we refuse planning against a missing/broken install.
    let _binary = resolve_managed_runtime_binary(home)?;
    let generation_id = active_rust_generation_id(home)?;
    if !generation_looks_rust_compatible(&generation_id) {
        return Err(format!(
            "active runtime generation is not Rust-compatible: {generation_id}"
        ));
    }

    let control_path = prefer_existing(&[
        config_dir.join("runtime-control-prod.json"),
        config_dir.join("runtime-control.json"),
    ])
    .unwrap_or_else(|| config_dir.join("runtime-control-prod.json"));
    let status_path = prefer_existing(&[
        config_dir.join("runtime-status-prod.json"),
        config_dir.join("runtime-status.json"),
    ]);
    let staging_path = staging_path_for(&control_path);

    let current = read_optional_json(&control_path)?;
    let desired = current
        .as_ref()
        .and_then(|value| value.get("desired_active"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| read_control_desired_active(&control_path));
    let active = status_path
        .as_ref()
        .and_then(|path| read_status_active_generation(path));
    let status = match status_path.as_ref() {
        Some(path) => read_optional_json(path)?,
        None => None,
    };
    let control_revision = current
        .as_ref()
        .and_then(|value| value.get("revision"))
        .and_then(Value::as_u64);
    let processed_revision = status
        .as_ref()
        .and_then(|value| value.get("processed_revision"))
        .and_then(Value::as_u64);

    // "Rust-compatible" is not sufficient here: after a runtime generation
    // switch an older rust-* id is still syntactically valid but stale.  The
    // live control document must target the exact managed runtime/current
    // generation.  However, do not churn revisions while the running Link has
    // not consumed the current desired-state revision yet.  A compatibility
    // retry is needed only when status proves that revision was consumed and
    // the manager nevertheless remained on an older generation.
    let desired_is_current = desired.as_deref() == Some(generation_id.as_str());
    let active_is_current = active.as_deref() == Some(generation_id.as_str());
    let status_consumed_current = matches!(
        (processed_revision, control_revision),
        (Some(processed), Some(control)) if processed >= control
    );
    let already = desired_is_current && (active_is_current || !status_consumed_current);

    let endpoint =
        extract_endpoint(current.as_ref()).unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned());
    let health = probe_health_version(&endpoint);
    let expected_version = health
        .as_ref()
        .ok()
        .cloned()
        .or_else(|| read_binary_version_hint(home));

    let mut planned = Map::new();
    let next_revision = current
        .as_ref()
        .and_then(|value| value.get("revision"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_add(1)
        .max(1);
    planned.insert("schema_version".to_owned(), json!(1));
    planned.insert("revision".to_owned(), json!(next_revision));
    planned.insert("desired_active".to_owned(), json!(generation_id));
    let mut generation = Map::new();
    generation.insert("generation".to_owned(), json!(generation_id));
    generation.insert("endpoint".to_owned(), json!(endpoint));
    if let Some(version) = expected_version.as_ref() {
        generation.insert("expected_runtime_version".to_owned(), json!(version));
    }
    planned.insert("generations".to_owned(), json!([generation]));
    if let Some(observation) = current
        .as_ref()
        .and_then(|value| value.get("observation"))
        .cloned()
    {
        planned.insert("observation".to_owned(), observation);
    } else {
        planned.insert(
            "observation".to_owned(),
            json!({ "checks": 3, "interval_ms": 500 }),
        );
    }
    let planned_value = Value::Object(planned);
    let validation = validate_runtime_control_document(&planned_value);
    let validation_ok = validation.is_ok();
    let validation_error = validation.err().map(|error| error.to_string());

    let mode_label = match mode {
        MigrateMode::DryRun => "dry-run",
        MigrateMode::WriteStaging => "write-staging",
        MigrateMode::Apply => "apply",
    };

    let ok = validation_ok && !generation_id.is_empty();
    Ok(json!({
        "ok": ok,
        "mode": mode_label,
        "applied": false,
        "wrote_staging": false,
        "launchd_mutated": false,
        "already_rust_compatible": already,
        "control_path": control_path.display().to_string(),
        "status_path": status_path.as_ref().map(|path| path.display().to_string()),
        "staging_path": staging_path.display().to_string(),
        "runtime_current_binary": managed_runtime_binary(home).display().to_string(),
        "active_rust_generation": generation_id,
        "current": {
            "desired_active": desired,
            "active_generation": active,
            "node_era_desired": desired.as_deref().is_some_and(generation_looks_node_era),
            "node_era_active": active.as_deref().is_some_and(generation_looks_node_era),
        },
        "planned_control": planned_value,
        "validation_ok": validation_ok,
        "validation_error": validation_error,
        "health_probe": match &health {
            Ok(version) => json!({ "ok": true, "server_version": version }),
            Err(error) => json!({ "ok": false, "error": error }),
        },
        "gate_after_plan": {
            "runtime_control_generation_rust_compatible": {
                "would_pass_on_desired_alone": generation_looks_rust_compatible(&generation_id),
                "note": "Gate prefers status active_generation when present; Link must poll/activate after --apply before active flips."
            }
        },
        "protected_labels_untouched": [
            "dev.herdr-mcp.link",
            "dev.herdr-mcp.link-prod",
            "dev.herdr-mcp.link-rust-candidate",
        ],
        "notes": [
            "Dry-run / staging do not rewrite live prod control.",
            "Apply rewrites runtime-control only; it does not cut Node link-prod.",
            "Credentials are never printed.",
        ],
    }))
}

/// Resolve `runtime/current` → `rust-<content-id>` generation id.
pub fn active_rust_generation_id(home: &Path) -> Result<String, String> {
    let current = home
        .join(".config")
        .join("herdr-mcp")
        .join("runtime")
        .join("current");
    let target = fs::read_link(&current).map_err(|error| {
        format!(
            "cannot read runtime/current symlink {}: {error}",
            current.display()
        )
    })?;
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            format!(
                "runtime/current target has no generation name: {}",
                target.display()
            )
        })?;
    if !name.starts_with("rust-") {
        return Err(format!(
            "runtime/current must point at generations/rust-*: got {}",
            target.display()
        ));
    }
    Ok(name.to_owned())
}

fn staging_path_for(control_path: &Path) -> PathBuf {
    let file_name = control_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("runtime-control-prod.json");
    let stem = file_name.strip_suffix(".json").unwrap_or(file_name);
    control_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{stem}{STAGING_SUFFIX}"))
}

fn backup_control_path(config_dir: &Path) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or(0);
    config_dir.join("backups").join(format!(
        "runtime-control-prod.before-rust-migrate-{millis}.json"
    ))
}

fn extract_endpoint(current: Option<&Value>) -> Option<String> {
    let generations = current?.get("generations")?.as_array()?;
    for item in generations {
        if let Some(endpoint) = item
            .get("endpoint")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(endpoint.to_owned());
        }
    }
    None
}

fn probe_health_version(endpoint: &str) -> Result<String, String> {
    let health_url = health_url_from_endpoint(endpoint);
    let response = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .map_err(|error| format!("health client build failed: {error}"))?
        .get(&health_url)
        .send()
        .map_err(|error| format!("health request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("health HTTP {}", response.status()));
    }
    let value: Value = response
        .json()
        .map_err(|error| format!("health JSON parse failed: {error}"))?;
    value
        .pointer("/build/server_version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("version")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
        .ok_or_else(|| "health missing build.server_version".to_owned())
}

fn health_url_from_endpoint(endpoint: &str) -> String {
    if let Some(base) = endpoint.strip_suffix("/mcp") {
        format!("{base}/health")
    } else {
        DEFAULT_HEALTH_URL.to_owned()
    }
}

pub(crate) fn read_binary_version_hint(home: &Path) -> Option<String> {
    let binary = managed_runtime_binary(home);
    let output = std::process::Command::new(&binary)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // "herdr-mcp 0.4.0-alpha.11"
    text.lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn read_optional_json(path: &Path) -> Result<Option<Value>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() > 64 * 1024 {
        return Err(format!(
            "control file exceeds size limit: {}",
            path.display()
        ));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
    Ok(Some(value))
}

fn prefer_existing(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|path| path.is_file()).cloned()
}

fn atomic_json(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("runtime-control"),
        std::process::id()
    ));
    let body = format!(
        "{}\n",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
    );
    fs::write(&tmp, body.as_bytes())
        .map_err(|error| format!("cannot write {}: {error}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("cannot chmod {}: {error}", tmp.display()))?;
    }
    fs::rename(&tmp, path).map_err(|error| {
        format!(
            "cannot activate {} from {}: {error}",
            path.display(),
            tmp.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_home() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "herdr-migrate-rc-{}-{}-{}",
            std::process::id(),
            n,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|v| v.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn setup_managed_runtime(home: &Path, generation: &str) {
        let generations = home
            .join(".config")
            .join("herdr-mcp")
            .join("runtime")
            .join("generations")
            .join(generation);
        fs::create_dir_all(&generations).unwrap();
        let binary = generations.join("herdr-mcp");
        fs::write(&binary, b"#!/bin/sh\necho herdr-mcp 0.4.0-alpha.11\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let current = home
            .join(".config")
            .join("herdr-mcp")
            .join("runtime")
            .join("current");
        fs::create_dir_all(current.parent().unwrap()).unwrap();
        symlink(format!("generations/{generation}"), &current).unwrap();
    }

    #[test]
    fn plans_rust_generation_from_node_era_control() {
        let home = test_home();
        setup_managed_runtime(&home, "rust-testhashmigrate01");
        let config_dir = home.join(".config").join("herdr-mcp");
        fs::write(
            config_dir.join("runtime-control-prod.json"),
            r#"{
              "schema_version": 1,
              "revision": 72,
              "desired_active": "stable-0.3.32",
              "generations": [
                {
                  "generation": "stable-0.3.32",
                  "endpoint": "http://127.0.0.1:8772/mcp",
                  "expected_runtime_version": "0.3.32"
                }
              ],
              "observation": { "checks": 3, "interval_ms": 500 }
            }"#,
        )
        .unwrap();
        fs::write(
            config_dir.join("runtime-status-prod.json"),
            r#"{"schema_version":1,"manager":{"active_generation":"stable-0.3.32"}}"#,
        )
        .unwrap();

        let report = plan_migrate(&home, &config_dir, MigrateMode::DryRun).unwrap();
        assert_eq!(report["ok"], true);
        assert_eq!(report["mode"], "dry-run");
        assert_eq!(report["already_rust_compatible"], false);
        assert_eq!(report["launchd_mutated"], false);
        assert_eq!(report["active_rust_generation"], "rust-testhashmigrate01");
        assert_eq!(
            report["planned_control"]["desired_active"],
            "rust-testhashmigrate01"
        );
        assert_eq!(report["planned_control"]["revision"], 73);
        assert_eq!(
            report["planned_control"]["generations"][0]["endpoint"],
            "http://127.0.0.1:8772/mcp"
        );
        assert_eq!(report["current"]["node_era_desired"], true);
        assert_eq!(report["validation_ok"], true);
        // Live files untouched by dry-run.
        let live = fs::read_to_string(config_dir.join("runtime-control-prod.json")).unwrap();
        assert!(live.contains("stable-0.3.32"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn stale_rust_generation_reconciles_to_exact_current_generation() {
        let home = test_home();
        setup_managed_runtime(&home, "rust-currentmigrate04");
        let config_dir = home.join(".config").join("herdr-mcp");
        let live_path = config_dir.join("runtime-control-prod.json");
        fs::write(
            &live_path,
            r#"{"schema_version":1,"revision":8,"desired_active":"rust-stalemigrate04","generations":[{"generation":"rust-stalemigrate04","endpoint":"http://127.0.0.1:8772/mcp"}]}"#,
        )
        .unwrap();
        fs::write(
            config_dir.join("runtime-status-prod.json"),
            r#"{"schema_version":1,"manager":{"active_generation":"rust-stalemigrate04"}}"#,
        )
        .unwrap();

        let report = plan_migrate(&home, &config_dir, MigrateMode::DryRun).unwrap();
        assert_eq!(report["already_rust_compatible"], false);
        assert_eq!(
            report["planned_control"]["desired_active"],
            "rust-currentmigrate04"
        );

        assert!(reconcile_current_generation(&home, &config_dir).unwrap());
        let live: Value = serde_json::from_slice(&fs::read(&live_path).unwrap()).unwrap();
        assert_eq!(live["desired_active"], "rust-currentmigrate04");
        assert!(!reconcile_current_generation(&home, &config_dir).unwrap());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn desired_current_but_status_stale_bumps_revision_for_running_old_link() {
        let home = test_home();
        setup_managed_runtime(&home, "rust-currentmigrate05");
        let config_dir = home.join(".config").join("herdr-mcp");
        let live_path = config_dir.join("runtime-control-prod.json");
        fs::write(
            &live_path,
            r#"{"schema_version":1,"revision":11,"desired_active":"rust-currentmigrate05","generations":[{"generation":"rust-currentmigrate05","endpoint":"http://127.0.0.1:8772/mcp"}]}"#,
        )
        .unwrap();
        fs::write(
            config_dir.join("runtime-status-prod.json"),
            r#"{"schema_version":1,"processed_revision":11,"outcome":"candidate_rejected:health_http_503","manager":{"active_generation":"rust-oldmigrate05"}}"#,
        )
        .unwrap();

        let report = plan_migrate(&home, &config_dir, MigrateMode::DryRun).unwrap();
        assert_eq!(report["already_rust_compatible"], false);
        assert_eq!(report["planned_control"]["revision"], 12);
        assert_eq!(
            report["planned_control"]["desired_active"],
            "rust-currentmigrate05"
        );
        assert!(reconcile_current_generation(&home, &config_dir).unwrap());
        let live: Value = serde_json::from_slice(&fs::read(&live_path).unwrap()).unwrap();
        assert_eq!(live["revision"], 12);
        // Until Link consumes revision 12, another reconciliation must not
        // immediately create revision 13.  This keeps compatibility retries
        // bounded and prevents revision churn under a slow/unavailable Link.
        assert!(!reconcile_current_generation(&home, &config_dir).unwrap());

        fs::write(
            config_dir.join("runtime-status-prod.json"),
            r#"{"schema_version":1,"processed_revision":12,"outcome":"activated","manager":{"active_generation":"rust-currentmigrate05"}}"#,
        )
        .unwrap();
        assert!(!reconcile_current_generation(&home, &config_dir).unwrap());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn write_staging_leaves_live_control_untouched() {
        let home = test_home();
        setup_managed_runtime(&home, "rust-testhashmigrate02");
        let config_dir = home.join(".config").join("herdr-mcp");
        let live_path = config_dir.join("runtime-control-prod.json");
        fs::write(
            &live_path,
            r#"{"schema_version":1,"revision":1,"desired_active":"stable-0.3.32","generations":[{"generation":"stable-0.3.32","endpoint":"http://127.0.0.1:8772/mcp"}]}"#,
        )
        .unwrap();

        let report = write_staging(&home, &config_dir).unwrap();
        assert_eq!(report["ok"], true);
        assert_eq!(report["wrote_staging"], true);
        assert_eq!(report["launchd_mutated"], false);
        let staging = PathBuf::from(report["staging_path"].as_str().unwrap());
        assert!(staging.is_file());
        let staging_doc: Value =
            serde_json::from_str(&fs::read_to_string(&staging).unwrap()).unwrap();
        assert_eq!(staging_doc["desired_active"], "rust-testhashmigrate02");
        let live = fs::read_to_string(&live_path).unwrap();
        assert!(live.contains("stable-0.3.32"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn apply_requires_env_guard_and_rewrites_live_control() {
        let _env_guard = crate::test_env::lock();
        let home = test_home();
        setup_managed_runtime(&home, "rust-testhashmigrate03");
        let config_dir = home.join(".config").join("herdr-mcp");
        let live_path = config_dir.join("runtime-control-prod.json");
        fs::write(
            &live_path,
            r#"{"schema_version":1,"revision":5,"desired_active":"stable-0.3.32","generations":[{"generation":"stable-0.3.32","endpoint":"http://127.0.0.1:8772/mcp"}]}"#,
        )
        .unwrap();

        // SAFETY: test process isolates env for this apply check.
        let previous = env::var_os(MIGRATE_APPLY_ENV);
        unsafe {
            env::remove_var(MIGRATE_APPLY_ENV);
        }
        let refused = apply_live(&home, &config_dir).unwrap();
        assert_eq!(refused["ok"], false);
        assert_eq!(refused["applied"], false);
        let live_before = fs::read_to_string(&live_path).unwrap();
        assert!(live_before.contains("stable-0.3.32"));

        unsafe {
            env::set_var(MIGRATE_APPLY_ENV, "1");
        }
        let applied = apply_live(&home, &config_dir).unwrap();
        match previous {
            Some(value) => unsafe { env::set_var(MIGRATE_APPLY_ENV, value) },
            None => unsafe { env::remove_var(MIGRATE_APPLY_ENV) },
        }
        assert_eq!(applied["ok"], true);
        assert_eq!(applied["applied"], true);
        assert_eq!(applied["launchd_mutated"], false);
        let live_after: Value =
            serde_json::from_str(&fs::read_to_string(&live_path).unwrap()).unwrap();
        assert_eq!(live_after["desired_active"], "rust-testhashmigrate03");
        assert_eq!(live_after["revision"], 6);
        assert!(
            applied["backup_path"]
                .as_str()
                .is_some_and(|path| Path::new(path).is_file())
        );
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn staging_path_uses_pending_suffix() {
        let path = PathBuf::from("/tmp/runtime-control-prod.json");
        assert_eq!(
            staging_path_for(&path),
            PathBuf::from("/tmp/runtime-control-prod.rust-pending.json")
        );
    }
}
