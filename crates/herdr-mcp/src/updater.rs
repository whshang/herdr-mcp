//! Native release updater.
//!
//! The updater intentionally does not own launchd or generation activation.
//! It verifies a release manifest + raw binary, stages the candidate under the
//! local state directory, then launches that downloaded binary as a detached
//! worker. The worker reuses `service install`, which already owns generation
//! staging, health verification, automatic rollback, and service evidence.

#[cfg(any(target_os = "macos", target_os = "linux"))]
use crate::cli::ServiceCommand;
use crate::cli::UpdateCommand;
use crate::config::{Config, UpdateChannel};
use crate::paths::RuntimePaths;
use crate::release_trust::{self, ReleaseIdentity};
use crate::standalone_extension::{ExtensionVersionReport, StagedExtension, StagedExtensionState};
use crate::state_store::SCHEMA_VERSION;
use crate::updater_store::{UpdateJobRecord, UpdateStore};
use reqwest::blocking::{Client, Response};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use semver::Version;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::env;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::fs::{self, OpenOptions};
use std::io::Read;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::io::Write;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::{Path, PathBuf};
use std::process::ExitCode;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::process::{Command, Stdio};
use std::time::Duration;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::time::Instant;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::os::unix::process::CommandExt;

mod release_plan;

use release_plan::{
    OlderRelease, OlderReleasePolicy, ReleaseAsset, ReleaseDiscovery, ReleasePlan,
    StateSchemaMigration, fetch_release_plan,
};

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod scheduled_tests;

const ATTESTATION_MAX_BYTES: usize = 2 * 1024 * 1024;
const BINARY_MAX_BYTES: u64 = 64 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(45);
const METADATA_FETCH_ATTEMPTS: usize = 2;
const METADATA_RETRY_DELAY: Duration = Duration::from_millis(750);
const MAX_REDIRECTS: usize = 5;
/// The only host that may receive the optional operator GitHub token.
const GITHUB_API_HOST: &str = "api.github.com";
/// Stable error code for an exhausted GitHub API rate limit.
const GITHUB_RATE_LIMITED_CODE: &str = "github_rate_limited";
#[cfg(any(target_os = "macos", target_os = "linux"))]
const DOWNLOAD_PROGRESS_STEP_PERCENT: u64 = 5;
#[cfg(any(target_os = "macos", target_os = "linux"))]
const UPDATE_WATCH_POLL_INTERVAL: Duration = Duration::from_millis(250);
#[cfg(any(target_os = "macos", target_os = "linux"))]
const UPDATE_WATCH_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAJOR_SOURCE_SCHEMA: i64 = 5;
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAJOR_UPGRADE_RECORD_VERSION: u64 = 1;
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAJOR_BACKUP_MAX_BYTES: u64 = 256 * 1024 * 1024;
#[cfg(any(target_os = "macos", target_os = "linux"))]
const LEGACY_CONFIG_MAX_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoUpdatePolicy {
    RunStable,
    Skip(&'static str),
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct MajorUpgradeRecord {
    record_version: u64,
    #[serde(default)]
    job_id: Option<String>,
    source_state_schema: i64,
    target_state_schema: i64,
    source_binary: String,
    source_binary_sha256: String,
    state_backup: String,
    state_backup_sha256: String,
    created_at_ms: i64,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct UpdateProgress<W: Write> {
    writer: W,
    enabled: bool,
    last_download_bucket: Option<u64>,
    last_job_snapshot: Option<(String, Option<String>)>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
impl<W: Write> UpdateProgress<W> {
    fn new(writer: W, enabled: bool) -> Self {
        Self {
            writer,
            enabled,
            last_download_bucket: None,
            last_job_snapshot: None,
        }
    }

    fn phase(&mut self, message: impl AsRef<str>) {
        if !self.enabled {
            return;
        }
        let _ = writeln!(self.writer, "[herdr-mcp update] {}", message.as_ref());
        let _ = self.writer.flush();
    }

    fn download(&mut self, downloaded: u64, total: u64) {
        if !self.enabled || total == 0 {
            return;
        }
        let percent = downloaded
            .saturating_mul(100)
            .saturating_div(total)
            .min(100);
        let bucket = if percent == 100 {
            100
        } else {
            (percent / DOWNLOAD_PROGRESS_STEP_PERCENT) * DOWNLOAD_PROGRESS_STEP_PERCENT
        };
        if self.last_download_bucket == Some(bucket) {
            return;
        }
        self.last_download_bucket = Some(bucket);
        let mib = 1024.0 * 1024.0;
        let _ = writeln!(
            self.writer,
            "[herdr-mcp update] Download {bucket:>3}% ({:.1}/{:.1} MiB)",
            downloaded as f64 / mib,
            total as f64 / mib
        );
        let _ = self.writer.flush();
    }

    fn job(&mut self, job: &UpdateJobRecord) {
        if !self.enabled {
            return;
        }
        let snapshot = (job.state.clone(), job.detail.clone());
        if self.last_job_snapshot.as_ref() == Some(&snapshot) {
            return;
        }
        self.last_job_snapshot = Some(snapshot);
        let detail = job.detail.as_deref().unwrap_or("no detail");
        self.phase(format!("Installer {}: {detail}", job.state));
    }

    #[cfg(test)]
    fn into_inner(self) -> W {
        self.writer
    }
}

fn auto_update_policy(
    named_instance: bool,
    update_check: bool,
    channel: UpdateChannel,
    runtime_channel: &str,
) -> AutoUpdatePolicy {
    if named_instance {
        AutoUpdatePolicy::Skip("named_instance")
    } else if runtime_channel != "prod" {
        AutoUpdatePolicy::Skip("runtime_channel_not_prod")
    } else if !update_check {
        AutoUpdatePolicy::Skip("update_check_disabled")
    } else if channel != UpdateChannel::Stable {
        AutoUpdatePolicy::Skip("preview_channel_requires_manual_update")
    } else {
        AutoUpdatePolicy::RunStable
    }
}

pub fn run(command: UpdateCommand) -> Result<ExitCode, String> {
    match command {
        UpdateCommand::Check { manifest_url } => check(manifest_url.as_deref()),
        UpdateCommand::Apply {
            manifest_url,
            with_worker,
        } => apply(manifest_url.as_deref(), with_worker),
        UpdateCommand::MajorApply => major_apply(),
        UpdateCommand::MajorRollback => major_rollback(),
        UpdateCommand::Auto => auto(),
        UpdateCommand::Status => status(),
        UpdateCommand::Worker { job_id } => worker(&job_id),
    }
}

fn check(manifest_override: Option<&str>) -> Result<ExitCode, String> {
    let channel = load_update_channel()?;
    let plan = match fetch_release_plan(manifest_override, channel, OlderReleasePolicy::Refuse)
        .map_err(|error| update_check_indeterminate_error(&error))?
    {
        ReleaseDiscovery::Ready(plan) => *plan,
        // `Refuse` never classifies an older release; keep the operator-facing
        // downgrade refusal if that policy ever changes.
        ReleaseDiscovery::Older(older) => {
            return Err(format!(
                "release {} is older than current {}; refusing downgrade",
                older.version,
                current_version()?
            ));
        }
    };
    let current = current_version()?;
    let available = plan.version > current;
    let paths = RuntimePaths::discover()?;
    let release_version = plan.version.to_string();
    let worker_update = worker_update_status_view(&paths, &release_version);
    let worker_update_required = worker_update_required(&worker_update);
    let migration_warning = plan
        .state_schema_migration
        .as_ref()
        .map(StateSchemaMigration::warning);
    if let Some(warning) = migration_warning.as_deref() {
        eprintln!("[herdr-mcp update] {warning}");
    }
    let next_action = if available {
        Some("herdr-mcp update apply")
    } else if worker_update_required {
        Some(WORKER_UPDATE_NEXT_ACTION)
    } else {
        None
    };
    print_json(&json!({
        "ok": true,
        "code": "update_check",
        "update_channel": channel.as_str(),
        "current_version": current.to_string(),
        "available": available,
        "release_version": release_version,
        "tag": plan.tag,
        "source_commit": plan.identity.source_commit,
        "repository": plan.identity.repository,
        "provenance_verified": true,
        "target": plan.asset.target,
        "asset": plan.asset.name,
        "sha256": plan.asset.sha256,
        "size": plan.asset.size,
        "state_schema_migration": plan.state_schema_migration.as_ref().map(|migration| json!({
            "from": migration.from,
            "to": migration.to,
            "reversible": migration.reversible,
        })),
        "warning": migration_warning,
        "worker_update_required": worker_update_required,
        "worker_update": worker_update,
        "notices": worker_update_notices(&worker_update),
        "next_action": next_action,
    }))?;
    Ok(ExitCode::SUCCESS)
}

fn update_check_indeterminate_error(error: &str) -> String {
    format!(
        "could not determine whether an update is available ({error}); run `herdr-mcp update apply` to retry"
    )
}

fn worker_update_status_view(paths: &RuntimePaths, target_version: &str) -> Value {
    match crate::worker_bootstrap::existing_worker_status(paths, target_version) {
        Ok(Some(status)) => status.public_view(),
        Ok(None) => json!({
            "configured": false,
            "update_required": false,
            "reason": "edge_not_configured",
        }),
        Err(error) => json!({
            "configured": true,
            "update_required": null,
            "status": "unknown",
            "error": error,
            "next_action": "herdr-mcp worker update",
        }),
    }
}

/// The separate operator command for the fleet Worker release plane.
const WORKER_UPDATE_NEXT_ACTION: &str = "herdr-mcp worker update";

fn worker_update_required(worker_update: &Value) -> bool {
    worker_update
        .get("update_required")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// A non-fatal `worker_update_required` notice for a Runtime update whose
/// separate fleet Worker is behind the installed release. The Runtime update
/// already succeeded, so this never changes its exit status.
fn worker_update_notice(worker_update: &Value) -> Option<Value> {
    worker_update_required(worker_update).then(|| {
        json!({
            "code": "worker_update_required",
            "severity": "notice",
            "fatal": false,
            "message": "the Runtime update succeeded; this computer's fleet Worker is behind the installed release and Worker updates are a separate operator-run release step",
            "edge_version": worker_update.get("edge_version").cloned().unwrap_or(Value::Null),
            "target_version": worker_update.get("target_version").cloned().unwrap_or(Value::Null),
            "next_action": WORKER_UPDATE_NEXT_ACTION,
        })
    })
}

fn worker_update_notices(worker_update: &Value) -> Value {
    worker_update_notice(worker_update)
        .map(|notice| json!([notice]))
        .unwrap_or_else(|| json!([]))
}

/// Stage the standalone browser extension that belongs to the release this
/// process is running (§833).
///
/// `update apply` used to leave `extensions/standalone/current` on whatever ref
/// was last installed by hand, so the browser kept running old extension code
/// with no way to notice. Staging reuses the CLI install path (same manifest
/// `key`, same atomic swap), only when the user already has a standalone
/// install, and never fails the Runtime update: the caller reports the outcome
/// as a notice and the exact fix command.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn stage_standalone_extension_after_update(
    reference: Option<&str>,
) -> Result<StagedExtension, String> {
    let reference = match reference {
        Some(reference) => reference.to_owned(),
        // The detached worker is the newly downloaded release binary, so its own
        // build commit is the release it just activated. The interactive
        // foreground process is still the *old* release and passes the target
        // commit explicitly instead.
        None => crate::runtime_meta::compiled_source_commit()
            .map(str::to_owned)
            .or_else(|| {
                env::var("HERDR_MCP_BUILD_COMMIT").ok().filter(|value| {
                    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            })
            .ok_or_else(|| {
                "this runtime build carries no source commit; run `herdr-mcp extension standalone install` to stage the extension".to_owned()
            })?,
    };
    crate::standalone_extension::stage_release_extension(&reference)
}

/// Report the result of update-time extension staging without changing the
/// Runtime update result.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn report_standalone_extension_staging(staging: &Result<StagedExtension, String>) {
    match staging {
        Ok(staged) => match staged.state {
            StagedExtensionState::NotInstalled => {}
            _ => eprintln!(
                "[herdr-mcp update] The standalone browser extension was staged for this release: {}. Reload the Herdr extension in chrome://extensions.",
                staged.summary()
            ),
        },
        Err(error) => eprintln!(
            "[herdr-mcp update] The Runtime update succeeded, but staging the standalone browser extension did not complete ({error}); run `herdr-mcp extension standalone install`."
        ),
    }
}

/// Extension notices for a committed Runtime update. A staged extension the
/// browser has not loaded yet is a notice; a staged extension that does not
/// belong to this release is a warning. Neither is fatal.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn append_standalone_extension_notice(
    notices: Value,
    extension: &crate::standalone_extension::ExtensionVersionReport,
) -> Value {
    let mut notices = notices.as_array().cloned().unwrap_or_default();
    if let Some(notice) = extension.update_notice() {
        notices.push(notice);
    }
    Value::Array(notices)
}

/// Extension status for one committed Runtime update.
///
/// `expected_release_commit` is the release this update activates (it can differ
/// from the running binary on the interactive path, where the old Runtime is
/// still the foreground process).
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn standalone_extension_report(
    paths: &RuntimePaths,
    expected_release_commit: Option<&str>,
) -> crate::standalone_extension::ExtensionVersionReport {
    crate::standalone_extension::version_report(&paths.config_dir, None, expected_release_commit)
}

/// Notices for a committed Runtime update. Drift is a notice; a failed
/// `--with-worker` attempt is a warning. Neither is fatal, because the Runtime
/// update itself succeeded.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn runtime_update_notices(
    worker_update: &Value,
    chain: WorkerChain,
    extension: &crate::standalone_extension::ExtensionVersionReport,
) -> Value {
    let notices = if chain == WorkerChain::WithWorker
        && worker_update.get("ok").and_then(Value::as_bool) == Some(false)
    {
        json!([{
            "code": "worker_update_failed",
            "severity": "warning",
            "fatal": false,
            "message": "the Runtime update succeeded, but the requested --with-worker Worker update did not complete; run `herdr-mcp worker update` to retry that separate step",
            "next_action": WORKER_UPDATE_NEXT_ACTION,
        }])
    } else {
        worker_update_notices(worker_update)
    };
    append_standalone_extension_notice(notices, extension)
}

/// Notice for the one-time macOS consent prompt a newly activated generation may
/// trigger.
///
/// Every update installs a *new* `runtime/generations/rust-<sha256>/` binary, and
/// an unsigned binary is a new TCC identity: the first Documents/Desktop/
/// Downloads access of a new generation asks for permission once. A pending
/// prompt blocks that access until a human answers it, so an unattended update
/// must say so in its own result instead of looking like a control-plane
/// failure. This is a notice: the Runtime update already succeeded.
#[cfg(target_os = "macos")]
fn macos_tcc_prompt_notice() -> Option<Value> {
    Some(json!({
        "code": "macos_documents_tcc_prompt",
        "severity": "notice",
        "fatal": false,
        "message": "macOS may show a one-time Documents access prompt for the newly installed runtime generation; click Allow. Protected-folder control requests can stall until it is answered.",
        "next_action": "click Allow on the macOS Documents prompt, then `herdr-mcp doctor`",
    }))
}

#[cfg(not(target_os = "macos"))]
fn macos_tcc_prompt_notice() -> Option<Value> {
    None
}

/// Append the macOS consent notice to a result that activates a new generation.
///
/// `update_current` does not activate one, so it never carries this notice.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn with_macos_tcc_prompt_notice(notices: Value) -> Value {
    let mut notices = notices.as_array().cloned().unwrap_or_default();
    notices.extend(macos_tcc_prompt_notice());
    Value::Array(notices)
}

/// Human-visible counterpart of `runtime_update_notices`.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn report_worker_update_notice(worker_update: &Value, chain: WorkerChain) {
    if chain == WorkerChain::WithWorker {
        if worker_update.get("ok").and_then(Value::as_bool) == Some(false) {
            eprintln!(
                "[herdr-mcp update] Runtime update succeeded, but the requested Worker update did not complete; run `{WORKER_UPDATE_NEXT_ACTION}`."
            );
        }
        return;
    }
    if !worker_update_required(worker_update) {
        return;
    }
    let edge_version = worker_update
        .get("edge_version")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    eprintln!(
        "[herdr-mcp update] Runtime update succeeded. The fleet Worker is behind the installed release (Worker {edge_version}); Worker updates are a separate step, run `{WORKER_UPDATE_NEXT_ACTION}`."
    );
}

/// Whether this invocation may run the separate fleet Worker update.
///
/// `update apply` is runtime-only by default: the Worker is its own release
/// plane, so the Runtime updater must never start `worker update`, a wrangler
/// invocation, or an interactive Cloudflare device authorization on its own.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerChain {
    /// Default. Report Worker drift as a notice and exit with the runtime result.
    RuntimeOnly,
    /// Explicit `--with-worker` opt-in, admitted only from an interactive TTY.
    WithWorker,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
const WITH_WORKER_REQUIRES_TTY: &str = "with_worker_requires_interactive_tty";

/// `--with-worker` starts the Worker authorization flow, which prints a device
/// code and may open a browser. A detached, nohup or CI invocation has no way
/// to complete it, so refuse it before any update work instead of blocking.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn with_worker_refusal_code(
    requested: bool,
    stdin_is_tty: bool,
    stdout_is_tty: bool,
) -> Option<&'static str> {
    if !requested {
        return None;
    }
    if stdin_is_tty && stdout_is_tty {
        None
    } else {
        Some(WITH_WORKER_REQUIRES_TTY)
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn with_worker_refusal_payload(code: &str) -> Value {
    json!({
        "ok": false,
        "code": code,
        "runtime_updated": false,
        "update_queued": false,
        "worker_update_requested": true,
        "worker_update_started": false,
        "message": "`--with-worker` needs an interactive stdin and stdout terminal because the Cloudflare authorization it starts asks for approval; run `herdr-mcp update apply` for the runtime-only update and run `herdr-mcp worker update` yourself afterwards",
        "next_action": "herdr-mcp update apply",
    })
}

/// Resolve the fleet Worker outcome after a Runtime commit.
///
/// Without an explicit opt-in the Worker is only *reported* from the read-only
/// status view; `run_worker_update` is never called, so no detached or
/// unattended `update apply` can start (or block on) Worker authorization.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn resolve_worker_update_after_runtime_commit<F>(
    paths: &RuntimePaths,
    target_version: &str,
    chain: WorkerChain,
    run_worker_update: F,
) -> Value
where
    F: FnOnce() -> Value,
{
    match chain {
        WorkerChain::WithWorker => run_worker_update(),
        WorkerChain::RuntimeOnly => worker_update_status_view(paths, target_version),
    }
}

/// Everything a committed Runtime update reports, so the runtime-only outcome
/// (successful Runtime + optional non-fatal Worker notice) is inspectable
/// without running an update.
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct RuntimeUpdateOutcome<'a> {
    job_id: String,
    target_version: String,
    target: String,
    asset: String,
    worker_pid: u32,
    worker_pid_persisted: bool,
    job: Value,
    migration: Option<&'a StateSchemaMigration>,
    worker_update: Value,
    worker_chain: WorkerChain,
    extension: ExtensionVersionReport,
    extension_staging: Option<StagedExtension>,
}

/// The Runtime update is what `update apply` guarantees. Worker drift is
/// reported as a non-fatal notice with the separate operator command, so it
/// never changes this result to a failure.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn runtime_update_succeeded_result(outcome: RuntimeUpdateOutcome<'_>) -> (Value, ExitCode) {
    let worker_update_required = worker_update_required(&outcome.worker_update);
    let payload = json!({
        "ok": true,
        "code": "update_succeeded",
        "job_id": outcome.job_id,
        "version": outcome.target_version,
        "target": outcome.target,
        "asset": outcome.asset,
        "worker_pid": outcome.worker_pid,
        "worker_pid_persisted": outcome.worker_pid_persisted,
        "job": outcome.job,
        "state_schema_migration": outcome.migration.map(|migration| json!({
            "from": migration.from,
            "to": migration.to,
            "reversible": migration.reversible,
        })),
        "warning": outcome.migration.map(StateSchemaMigration::warning),
        "worker_update_chained": outcome.worker_chain == WorkerChain::WithWorker,
        "worker_update_required": worker_update_required,
        "worker_update": outcome.worker_update,
        "standalone_extension": outcome.extension.as_json(),
        "standalone_extension_staging": outcome.extension_staging.as_ref().map(StagedExtension::as_json).unwrap_or(Value::Null),
        "notices": with_macos_tcc_prompt_notice(runtime_update_notices(
            &outcome.worker_update,
            outcome.worker_chain,
            &outcome.extension,
        )),
        "next_action": if worker_update_required {
            Some(WORKER_UPDATE_NEXT_ACTION)
        } else {
            None
        },
    });
    (payload, ExitCode::SUCCESS)
}

/// Pure decision behind [`migrate_broker_after_update`]: `installed` is the
/// revision the config dir records (or `None` when it cannot be read), and
/// `migrate` performs the real replacement.
fn broker_migration_decision<F>(
    installed: Option<u32>,
    target: u32,
    migrate: F,
    config_dir: &std::path::Path,
) -> Value
where
    F: FnOnce(&std::path::Path) -> Result<bool, String>,
{
    let Some(installed) = installed else {
        return json!({
            "state": "unknown",
            "installed_revision": Value::Null,
            "target_revision": target,
            "next_action": "herdr-mcp permissions setup --upgrade-broker",
        });
    };
    if installed >= target {
        return json!({
            "state": "current",
            "installed_revision": installed,
            "target_revision": target,
        });
    }
    match migrate(config_dir) {
        Ok(true) => json!({
            "state": "migrated",
            "installed_revision": installed,
            "target_revision": target,
            "needs_reauthorization": true,
            "message": "the TCC broker was replaced with the revision that implements the Documents alias subcommands; the next Documents access asks for authorization once",
            "next_action": "answer the macOS Documents prompt",
        }),
        Ok(false) => json!({
            "state": "current",
            "installed_revision": installed,
            "target_revision": target,
        }),
        Err(error) => json!({
            "state": "failed",
            "installed_revision": installed,
            "target_revision": target,
            "error": error,
            "next_action": "herdr-mcp permissions setup --upgrade-broker",
        }),
    }
}

/// Attach the broker migration outcome to an update result.
///
/// A migration that ran is a notice (one re-authorization follows); a failed one
/// is a warning with the command that performs the same migration by hand.
/// Neither is fatal: the Runtime already committed.
fn attach_tcc_broker_migration(payload: &mut Value, migration: Value) {
    let state = migration
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    object.insert("tcc_broker_migration".to_owned(), migration);
    if !matches!(state.as_str(), "migrated" | "failed") {
        return;
    }
    let notice = if state == "migrated" {
        json!({
            "code": "tcc_broker_migrated",
            "severity": "notice",
            "fatal": false,
            "message": "the TCC broker was migrated to the revision that implements the Documents alias subcommands; answer the one macOS Documents prompt that follows",
            "next_action": "answer the macOS Documents prompt",
        })
    } else {
        json!({
            "code": "tcc_broker_migration_failed",
            "severity": "warning",
            "fatal": false,
            "message": "the TCC broker still predates the Documents alias subcommands; run the migration command to install them",
            "next_action": "herdr-mcp permissions setup --upgrade-broker",
        })
    };
    let notices = object
        .entry("notices".to_owned())
        .or_insert_with(|| json!([]));
    if let Some(list) = notices.as_array_mut() {
        list.push(notice);
    }
}

/// Run the post-commit TCC steps in the only order that costs one prompt.
///
/// The migration has no Documents syscall; it replaces the broker file. Running
/// the preflight first would raise the prompt for the broker that the migration
/// then discards (a new ad-hoc identity), so the operator would answer twice and
/// the surviving broker would still be ungranted. Migrate, then probe the broker
/// that stays installed. Both steps are injected so the order is testable.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn attach_post_commit_tcc<M, P>(
    payload: &mut Value,
    config_dir: &std::path::Path,
    installed_revision: Option<u32>,
    migrate: M,
    preflight: P,
) where
    M: FnOnce(&std::path::Path) -> Result<bool, String>,
    P: FnOnce(&std::path::Path) -> Value,
{
    let migration = broker_migration_decision(
        installed_revision,
        crate::tcc_broker::BROKER_COMPAT_REVISION,
        migrate,
        config_dir,
    );
    attach_tcc_broker_migration(payload, migration);
    attach_tcc_preflight(payload, preflight(config_dir));
}

/// Persist the broker migration outcome next to the other update artifacts so
/// `update status` can report it without re-running codesign or a migration.
fn persist_broker_migration(config_dir: &std::path::Path, migration: &Value) {
    let path = config_dir.join("update").join("tcc-broker-migration.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = crate::mutation::atomic_write(&path, migration.to_string().as_bytes());
}

/// Persisted broker migration outcome, or an explicit `unknown` shape.
fn persisted_broker_migration(config_dir: &std::path::Path) -> Value {
    std::fs::read_to_string(config_dir.join("update").join("tcc-broker-migration.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or_else(|| json!({"state": "unknown", "reported_by": Value::Null}))
}

/// Attach the post-activation macOS Documents preflight to an update result.
///
/// TCC prompts are an operator-visible condition, not an update failure: the
/// Runtime already committed, so a pending prompt is a non-fatal notice with the
/// separate recovery step. The raw preflight object is always included so a
/// caller can tell "no prompt" from "not probed on this platform".
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn attach_tcc_preflight(payload: &mut Value, preflight: Value) {
    let pending = preflight
        .get("prompt_pending")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    object.insert("tcc_preflight".to_owned(), preflight);
    object.insert("tcc_prompt_pending".to_owned(), json!(pending));
    if !pending {
        return;
    }
    let notices = object
        .entry("notices".to_owned())
        .or_insert_with(|| json!([]));
    if let Some(list) = notices.as_array_mut() {
        list.push(json!({
            "code": "tcc_prompt_pending",
            "severity": "notice",
            "fatal": false,
            "message": "the Runtime update succeeded, but a macOS Documents permission prompt is still unanswered; answer it once and control requests stay responsive",
            "next_action": "answer the macOS Documents prompt",
        }));
    }
}

/// Result of an `update apply` that found the Runtime already on the target
/// release.
///
/// The Runtime did not change, so there is no newly activated generation and no
/// macOS consent prompt to announce. A Worker problem — including a failed
/// explicit `--with-worker` reconcile — is only a notice; `ok` and the exit code
/// stay with the Runtime outcome.
#[cfg(any(target_os = "macos", target_os = "linux"))]
struct UpdateCurrentOutcome<'a> {
    channel: UpdateChannel,
    current_version: &'a str,
    target_version: &'a str,
    tag: &'a str,
    migration: Option<&'a StateSchemaMigration>,
    worker_update: Value,
    worker_chain: WorkerChain,
    extension: &'a ExtensionVersionReport,
    extension_staging: Option<&'a StagedExtension>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn update_current_result(outcome: UpdateCurrentOutcome<'_>) -> (Value, ExitCode) {
    let worker_update_required = worker_update_required(&outcome.worker_update);
    let payload = json!({
        "ok": true,
        "code": "update_current",
        "update_channel": outcome.channel.as_str(),
        "current_version": outcome.current_version,
        "release_version": outcome.target_version,
        "tag": outcome.tag,
        "provenance_verified": true,
        "runtime_changed": false,
        "worker_update_chained": outcome.worker_chain == WorkerChain::WithWorker,
        "worker_update_required": worker_update_required,
        "warning": outcome.migration.map(StateSchemaMigration::warning),
        "worker_update": outcome.worker_update,
        "standalone_extension": outcome.extension.as_json(),
        "standalone_extension_staging": outcome.extension_staging.map(StagedExtension::as_json).unwrap_or(Value::Null),
        "notices": runtime_update_notices(&outcome.worker_update, outcome.worker_chain, outcome.extension),
        "next_action": if worker_update_required {
            Some(WORKER_UPDATE_NEXT_ACTION)
        } else {
            None
        },
    });
    (payload, ExitCode::SUCCESS)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn reconcile_worker_with_active_runtime(paths: &RuntimePaths) -> Value {
    let binary = match active_runtime_binary(paths) {
        Ok(binary) => binary,
        Err(error) => {
            return json!({
                "ok": false,
                "code": "worker_update_active_runtime_unavailable",
                "error": error,
                "next_action": "herdr-mcp worker update",
            });
        }
    };
    let output = match Command::new(&binary)
        .arg("worker")
        .arg("update")
        .env_remove("HERDR_MCP_EXEC_ID")
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            return json!({
                "ok": false,
                "code": "worker_update_launch_failed",
                "error": format!("cannot launch active Runtime Worker reconciliation: {error}"),
                "next_action": "herdr-mcp worker update",
            });
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed = serde_json::from_str::<Value>(stdout.trim()).ok();
    if output.status.success() {
        return parsed.unwrap_or_else(|| {
            json!({
                "ok": false,
                "code": "worker_update_invalid_result",
                "error": "active Runtime Worker reconciliation returned no valid JSON result",
                "next_action": "herdr-mcp worker update",
            })
        });
    }
    match parsed {
        Some(mut value) => {
            if let Some(object) = value.as_object_mut() {
                object.insert("ok".to_owned(), json!(false));
                object
                    .entry("next_action".to_owned())
                    .or_insert_with(|| json!("herdr-mcp worker update"));
            }
            value
        }
        None => json!({
            "ok": false,
            "code": "worker_update_failed",
            "exit_code": output.status.code(),
            "error": "active Runtime Worker reconciliation did not complete; diagnostics were written to stderr",
            "next_action": "herdr-mcp worker update",
        }),
    }
}

fn apply(manifest_override: Option<&str>, with_worker: bool) -> Result<ExitCode, String> {
    apply_inner(manifest_override, false, None, true, with_worker)
}

fn auto() -> Result<ExitCode, String> {
    let paths = RuntimePaths::discover()?;
    if let AutoUpdatePolicy::Skip(reason) = auto_update_policy(
        paths.instance.is_named(),
        true,
        UpdateChannel::Stable,
        crate::runtime_meta::runtime_channel(),
    ) {
        print_json(&json!({
            "ok": true,
            "code": "auto_update_skipped",
            "reason": reason,
            "network_checked": false,
        }))?;
        return Ok(ExitCode::SUCCESS);
    }
    let config = Config::load_for_instance(&paths.config_file, &paths.instance)?;
    if let AutoUpdatePolicy::Skip(reason) = auto_update_policy(
        false,
        config.update_check,
        config.update_channel,
        crate::runtime_meta::runtime_channel(),
    ) {
        print_json(&json!({
            "ok": true,
            "code": "auto_update_skipped",
            "reason": reason,
            "update_channel": config.update_channel.as_str(),
            "network_checked": false,
        }))?;
        return Ok(ExitCode::SUCCESS);
    }
    // Freeze the trust decision made above. Do not re-read the mutable config
    // inside the apply path and accidentally widen a scheduled Stable update
    // into Preview if the config changes concurrently.
    // Scheduled/automatic updates are runtime-only and never opt into the
    // separate fleet Worker update.
    apply_inner(None, true, Some(UpdateChannel::Stable), false, false)
}

/// No-op record for the scheduled/automatic path when the discovered release
/// is older than the installed runtime. It is deliberately a success result:
/// a background job that has nothing to install must not report a failure.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn auto_downgrade_skip_record(
    channel: UpdateChannel,
    current: &Version,
    release: &Version,
    tag: &str,
) -> Value {
    json!({
        "ok": true,
        "code": "auto_update_skipped",
        "reason": downgrade_skip_reason(channel),
        "update_channel": channel.as_str(),
        "current_version": current.to_string(),
        "release_version": release.to_string(),
        "tag": tag,
        "provenance_verified": true,
        "network_checked": true,
        "update_queued": false,
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn downgrade_skip_reason(channel: UpdateChannel) -> &'static str {
    match channel {
        UpdateChannel::Stable => "stable_older_than_current",
        UpdateChannel::Preview => "release_older_than_current",
    }
}

/// No-op record for the scheduled/automatic path when the discovered release is
/// older on contract identity or state schema (#829). The manifest was
/// discovered and matches the update channel, but nothing is staged, so its
/// attestation is deliberately not verified.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn auto_older_release_skip_record(
    channel: UpdateChannel,
    current: &Version,
    older: &OlderRelease,
) -> Value {
    json!({
        "ok": true,
        "code": "auto_update_skipped",
        "reason": downgrade_skip_reason(channel),
        "reason_detail": older.detail,
        "update_channel": channel.as_str(),
        "current_version": current.to_string(),
        "release_version": older.version.to_string(),
        "tag": &older.tag,
        "release_contract_epoch": older.contract_epoch,
        "release_state_schema": older.state_schema,
        "provenance_verified": false,
        "network_checked": true,
        "update_queued": false,
    })
}

fn apply_inner(
    manifest_override: Option<&str>,
    allow_current: bool,
    channel_override: Option<UpdateChannel>,
    progress_enabled: bool,
    with_worker: bool,
) -> Result<ExitCode, String> {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (
            manifest_override,
            allow_current,
            channel_override,
            progress_enabled,
            with_worker,
        );
        Err(
            "native update apply is currently supported on macOS and Linux service managers"
                .to_owned(),
        )
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        // `update apply` is runtime-only. The separate Worker update is only
        // ever run on an explicit `--with-worker` opt-in, and that opt-in is
        // refused before any network/service work when this process cannot
        // show or answer the Cloudflare authorization it starts.
        let worker_chain = {
            use std::io::IsTerminal;
            let stdin_is_tty = std::io::stdin().is_terminal();
            let stdout_is_tty = std::io::stdout().is_terminal();
            match with_worker_refusal_code(with_worker, stdin_is_tty, stdout_is_tty) {
                Some(code) => {
                    print_json(&with_worker_refusal_payload(code))?;
                    return Ok(ExitCode::FAILURE);
                }
                None if with_worker => WorkerChain::WithWorker,
                None => WorkerChain::RuntimeOnly,
            }
        };
        crate::update_scheduler::ensure_updates_allowed()?;
        let channel = match channel_override {
            Some(channel) => channel,
            None => load_update_channel()?,
        };
        let stderr = std::io::stderr();
        let mut progress = UpdateProgress::new(stderr.lock(), progress_enabled);
        progress.phase(format!(
            "Checking {} release metadata and provenance...",
            channel.as_str()
        ));
        let plan = match fetch_release_plan(
            manifest_override,
            channel,
            if channel_override.is_some() {
                OlderReleasePolicy::SkipInScheduled
            } else {
                OlderReleasePolicy::Refuse
            },
        )? {
            ReleaseDiscovery::Ready(plan) => *plan,
            ReleaseDiscovery::Older(older) => {
                // Scheduled/automatic runs have nothing to install, so an older
                // release is a successful no-op instead of the contract-identity
                // or downgrade failure an operator-facing command must report.
                if channel_override.is_none() {
                    return Err(format!(
                        "release {} is older than current {}; refusing downgrade",
                        older.version,
                        current_version()?
                    ));
                }
                print_json(&auto_older_release_skip_record(
                    channel,
                    &current_version()?,
                    &older,
                ))?;
                return Ok(ExitCode::SUCCESS);
            }
        };
        if channel_override.is_some()
            && let Some(migration) = plan.state_schema_migration.as_ref()
        {
            return Err(format!(
                "scheduled/automatic updates refuse state-schema migrations; run explicit `herdr-mcp update apply` ({})",
                migration.warning()
            ));
        }
        let current = current_version()?;
        progress.phase(format!(
            "Release {} verified; current version is {}.",
            plan.version, current
        ));
        let paths = RuntimePaths::discover()?;
        if plan.version < current {
            // A scheduled/automatic run is not an operator request: when the
            // release it discovered is behind the installed runtime (for
            // example Stable v1.0.5 while a v1.1.0 rc is installed) there is
            // nothing to do, so it reports a no-op skip and exits successfully
            // instead of failing the daily LaunchAgent run. Only the explicit
            // `update apply` path keeps refusing the downgrade.
            if channel_override.is_some() {
                print_json(&auto_downgrade_skip_record(
                    channel,
                    &current,
                    &plan.version,
                    &plan.tag,
                ))?;
                return Ok(ExitCode::SUCCESS);
            }
            return Err(format!(
                "release {} is older than current {}; refusing downgrade",
                plan.version, current
            ));
        }
        if plan.version == current {
            let target_version = plan.version.to_string();
            if allow_current {
                let worker_update = worker_update_status_view(&paths, &target_version);
                let worker_update_required = worker_update_required(&worker_update);
                print_json(&json!({
                    "ok": true,
                    "code": "auto_update_current",
                    "update_channel": channel.as_str(),
                    "current_version": current.to_string(),
                    "release_version": target_version,
                    "tag": plan.tag,
                    "provenance_verified": true,
                    "network_checked": true,
                    "warning": plan.state_schema_migration.as_ref().map(StateSchemaMigration::warning),
                    "update_queued": false,
                    "worker_update_required": worker_update_required,
                    "worker_update": worker_update,
                    "notices": worker_update_notices(&worker_update),
                    "next_action": if worker_update_required {
                        Some(WORKER_UPDATE_NEXT_ACTION)
                    } else {
                        None
                    },
                }))?;
                return Ok(ExitCode::SUCCESS);
            }
            // Runtime is already on the target release. `update apply` stays
            // runtime-only: Worker drift is reported as a separate operator
            // step instead of starting Worker authorization from an update.
            let worker_update = resolve_worker_update_after_runtime_commit(
                &paths,
                &target_version,
                worker_chain,
                || reconcile_worker_with_active_runtime(&paths),
            );
            report_worker_update_notice(&worker_update, worker_chain);
            // The Runtime is already on this release, so re-stage the matching
            // standalone extension if this workstation has one: that is the
            // repair path for a workstation whose extension was left behind by
            // an older updater (§833).
            let staging =
                stage_standalone_extension_after_update(Some(&plan.identity.source_commit));
            report_standalone_extension_staging(&staging);
            let staged = staging.as_ref().ok();
            let extension = standalone_extension_report(&paths, Some(&plan.identity.source_commit));
            let (payload, code) = update_current_result(UpdateCurrentOutcome {
                channel,
                current_version: &current.to_string(),
                target_version: &target_version,
                tag: &plan.tag,
                migration: plan.state_schema_migration.as_ref(),
                worker_update,
                worker_chain,
                extension: &extension,
                extension_staging: staged,
            });
            print_json(&payload)?;
            return Ok(code);
        }
        let store = UpdateStore::open(&paths)?;
        recover_or_reject_active_update(&store, &paths)?;
        if let Some(migration) = plan.state_schema_migration.as_ref() {
            progress.phase(migration.warning());
        }
        progress.phase("Preparing a rollback-safe update job...");
        let (job_id, binary_path) = stage_release(&paths, &plan, &mut progress)?;
        progress.phase("Download verified. Probing candidate runtime identity...");
        let (expected_source_schema, expected_schema) = plan
            .state_schema_migration
            .as_ref()
            .map_or((SCHEMA_VERSION, SCHEMA_VERSION), |migration| {
                (migration.from, migration.to)
            });
        if let Err(error) =
            probe_candidate_binary(&binary_path, &plan.version, &plan.contract, expected_schema)
        {
            cleanup_staging(&binary_path);
            return Err(error);
        }
        progress.phase("Candidate identity verified. Starting installer...");

        let now = now_ms_i64();
        let job = UpdateJobRecord {
            job_id: job_id.clone(),
            version: plan.version.to_string(),
            target: plan.asset.target.clone(),
            asset_name: plan.asset.name.clone(),
            sha256: plan.asset.sha256.clone(),
            binary_path: binary_path.to_string_lossy().into_owned(),
            state_schema_source: expected_source_schema,
            state_schema_target: expected_schema,
            state: "queued".to_owned(),
            detail: Some("verified candidate staged".to_owned()),
            worker_pid: None,
            created_at: now,
            updated_at: now,
        };
        if let Err(error) = store.create_update_job(&job) {
            cleanup_staging(&binary_path);
            return Err(error);
        }

        let migration_record = match plan.state_schema_migration.as_ref() {
            Some(migration) => {
                match prepare_state_schema_migration_record(&paths, &job_id, migration) {
                    Ok(record) => Some(record),
                    Err(error) => {
                        let _ = store.update_update_job(
                            &job_id,
                            "failed",
                            Some("state-schema migration backup failed"),
                            None,
                            now_ms_i64(),
                        );
                        cleanup_staging(&binary_path);
                        return Err(error);
                    }
                }
            }
            None => None,
        };
        let child = spawn_worker(&paths, &binary_path, &job_id);
        let mut child = match child {
            Ok(child) => child,
            Err(error) => {
                let _ = store.update_update_job(
                    &job_id,
                    "failed",
                    Some("worker spawn failed"),
                    None,
                    now_ms_i64(),
                );
                cleanup_staging(&binary_path);
                let cleanup = migration_record
                    .as_ref()
                    .map(|record| discard_state_schema_migration_record(&paths, record));
                return Err(match cleanup {
                    Some(Ok(())) | None => error,
                    Some(Err(cleanup_error)) => {
                        format!("{error}; migration backup cleanup failed: {cleanup_error}")
                    }
                });
            }
        };
        let worker_pid_persisted = store
            .set_update_worker_pid(&job_id, child.id(), now_ms_i64())
            .is_ok();

        if progress.enabled {
            if let Some(job) = watch_update_job(&store, &job_id, &mut child, &mut progress)? {
                let target_version = plan.version.to_string();
                if worker_chain == WorkerChain::WithWorker {
                    progress.phase(
                        "Runtime update succeeded. Reconciling the fleet Worker because --with-worker was given...",
                    );
                } else {
                    progress
                        .phase("Runtime update succeeded. Checking the fleet Worker release...");
                }
                // Runtime-only by default: this only reads the Worker status
                // and reports drift. `worker update` runs solely for the
                // explicit `--with-worker` opt-in, and then against the
                // just-activated runtime/current binary so future
                // same-contract releases can evolve their Worker metadata.
                let worker_update = resolve_worker_update_after_runtime_commit(
                    &paths,
                    &target_version,
                    worker_chain,
                    || reconcile_worker_with_active_runtime(&paths),
                );
                report_worker_update_notice(&worker_update, worker_chain);
                // The detached worker stages the extension right after its
                // service install commits. Re-running staging here with the
                // target release commit is idempotent (the worker already
                // recorded that commit) and lets the foreground result carry the
                // exact Reload/refresh action.
                let staging =
                    stage_standalone_extension_after_update(Some(&plan.identity.source_commit));
                let extension =
                    standalone_extension_report(&paths, Some(&plan.identity.source_commit));
                let (mut payload, code) = runtime_update_succeeded_result(RuntimeUpdateOutcome {
                    target_version,
                    target: plan.asset.target.clone(),
                    asset: plan.asset.name.clone(),
                    job_id: job_id.clone(),
                    worker_pid: child.id(),
                    worker_pid_persisted,
                    job: public_job_view(&job),
                    migration: plan.state_schema_migration.as_ref(),
                    worker_update,
                    worker_chain,
                    extension,
                    extension_staging: staging.ok(),
                });
                // #844: migrate the broker first (no Documents syscall), then run
                // exactly one bounded preflight against the broker that stays
                // installed. Both results are persisted so `update status` never
                // has to probe.
                let installed_revision =
                    crate::tcc_broker::installed_compat_revision(&paths.config_dir);
                attach_post_commit_tcc(
                    &mut payload,
                    &paths.config_dir,
                    installed_revision,
                    |config_dir| {
                        crate::tcc_broker::migrate_for_explicit_reauthorization(config_dir)
                    },
                    crate::macos_privacy::preflight_after_activation,
                );
                if let Some(migration) = payload.get("tcc_broker_migration") {
                    persist_broker_migration(&paths.config_dir, migration);
                }
                print_json(&payload)?;
                return Ok(code);
            }
            progress.phase(
                "Installer is still running in the background; use `herdr-mcp update status` for the final state.",
            );
        }

        print_json(&json!({
            "ok": true,
            "code": "update_queued",
            "job_id": job_id,
            "version": plan.version.to_string(),
            "target": plan.asset.target,
            "asset": plan.asset.name,
            "state_schema_migration": plan.state_schema_migration.as_ref().map(|migration| json!({
                "from": migration.from,
                "to": migration.to,
                "reversible": migration.reversible,
            })),
            "warning": plan.state_schema_migration.as_ref().map(StateSchemaMigration::warning),
            "worker_pid": child.id(),
            "worker_pid_persisted": worker_pid_persisted,
            "worker_update": {
                "state": "not_attempted",
                "reason": "worker_update_is_a_separate_operator_step",
                "next_action": WORKER_UPDATE_NEXT_ACTION,
            },
            "notices": with_macos_tcc_prompt_notice(json!([])),
            "next_action": "herdr-mcp update status",
        }))?;
        Ok(ExitCode::SUCCESS)
    }
}

fn major_apply() -> Result<ExitCode, String> {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("major state-schema upgrade is currently supported on macOS and Linux".to_owned())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        refuse_major_upgrade_inside_managed_exec()?;
        if SCHEMA_VERSION <= MAJOR_SOURCE_SCHEMA {
            return Err(format!(
                "this binary uses state schema {SCHEMA_VERSION}; no schema-{MAJOR_SOURCE_SCHEMA} major upgrade is required"
            ));
        }
        let paths = RuntimePaths::discover()?;
        let state_path = paths.config_dir.join("state.db");
        let live_schema = read_raw_state_schema(&state_path)?
            .ok_or_else(|| format!("state database is missing: {}", state_path.display()))?;
        if live_schema == SCHEMA_VERSION {
            return Err(format!(
                "state database is already schema {SCHEMA_VERSION}; use the normal install/update path"
            ));
        }
        if live_schema != MAJOR_SOURCE_SCHEMA {
            return Err(format!(
                "major upgrade is qualified only from state schema {MAJOR_SOURCE_SCHEMA}, found {live_schema}"
            ));
        }

        let record = prepare_major_upgrade_record(&paths, &state_path)?;

        let install = crate::service_lifecycle::run_major_upgrade_install();
        let install_ok = matches!(install, Ok(code) if code == ExitCode::SUCCESS);
        let migrated = read_raw_state_schema(&state_path).ok().flatten() == Some(SCHEMA_VERSION);
        if install_ok && migrated {
            crate::local_agent_skill::sync_after_install_best_effort();
            print_json(&json!({
                "ok": true,
                "code": "major_update_succeeded",
                "source_state_schema": record.source_state_schema,
                "state_schema": SCHEMA_VERSION,
                "rollback_command": "herdr-mcp update major-rollback",
                "rollback_backup": record.state_backup,
            }))?;
            return Ok(ExitCode::SUCCESS);
        }

        let install_detail = match install {
            Ok(code) if code == ExitCode::SUCCESS => format!(
                "service install reported success but the state database is not schema {SCHEMA_VERSION}"
            ),
            Ok(code) => format!("service install exited with {code:?}"),
            Err(error) => error,
        };
        let recovery = recover_major_upgrade(&paths, &record);
        let message = match recovery {
            Ok(()) => format!(
                "major update did not complete ({install_detail}); schema-{MAJOR_SOURCE_SCHEMA} state and the previous runtime were restored"
            ),
            Err(recovery_error) => format!(
                "major update did not complete ({install_detail}); automatic recovery also failed: {recovery_error}"
            ),
        };
        Err(match record_major_failure(&paths.config_dir, &message) {
            Some(path) => format!("{message}; full detail: {}", path.display()),
            None => message,
        })
    }
}

/// Fixed location of the last `major-apply` failure. The v0.4.8 updater keeps
/// only the first 512 bytes of an update job detail and the v0.4.9 bridge
/// deletes its captured `major-apply.log`, while the verbose install report is
/// printed before the failure reason. Without this file a field failure has no
/// recoverable cause (`cat ~/.config/herdr-mcp/major-upgrade-last-failure.log`).
#[cfg(any(target_os = "macos", target_os = "linux"))]
const MAJOR_FAILURE_LOG_NAME: &str = "major-upgrade-last-failure.log";

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn record_major_failure(config_dir: &Path, message: &str) -> Option<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;

    let path = config_dir.join(MAJOR_FAILURE_LOG_NAME);
    let temp = config_dir.join(format!(
        ".{MAJOR_FAILURE_LOG_NAME}.{}.tmp",
        std::process::id()
    ));
    let body = format!(
        "herdr-mcp {} update major-apply failed at unix_ms={}\n{message}\n",
        crate::runtime_meta::runtime_version(),
        now_ms_i64()
    );
    let written = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temp, &path)
    })();
    match written {
        Ok(()) => Some(path),
        Err(error) => {
            let _ = fs::remove_file(&temp);
            eprintln!("warning: could not record major-apply failure detail: {error}");
            None
        }
    }
}

fn major_rollback() -> Result<ExitCode, String> {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("major state-schema rollback is currently supported on macOS and Linux".to_owned())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        refuse_major_upgrade_inside_managed_exec()?;
        let paths = RuntimePaths::discover()?;
        let record = load_major_upgrade_record(&paths)?
            .ok_or_else(|| "no major-upgrade rollback backup is available".to_owned())?;
        validate_major_upgrade_record(&paths, &record)?;
        let state_path = paths.config_dir.join("state.db");
        let schema = read_raw_state_schema(&state_path)?;
        if schema.is_some_and(|version| {
            !(record.source_state_schema..=record.target_state_schema).contains(&version)
        }) {
            return Err(format!(
                "major rollback expected live state schema {} through {}, found {}",
                record.source_state_schema,
                record.target_state_schema,
                schema.map_or_else(|| "missing".to_owned(), |value| value.to_string())
            ));
        }
        recover_major_upgrade(&paths, &record)?;
        crate::local_agent_skill::sync_after_install_best_effort();
        print_json(&json!({
            "ok": true,
            "code": "major_update_rolled_back",
            "state_schema": record.source_state_schema,
            "restored_binary": record.source_binary,
        }))?;
        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn refuse_major_upgrade_inside_managed_exec() -> Result<(), String> {
    if env::var_os("HERDR_MCP_EXEC_ID").is_some() {
        return Err(
            "major update/rollback must run from an independent terminal, not herdr_exec"
                .to_owned(),
        );
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn major_upgrade_dir(paths: &RuntimePaths) -> PathBuf {
    paths.config_dir.join("backups").join("major-upgrade-v1")
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn major_upgrade_record_path(paths: &RuntimePaths) -> PathBuf {
    major_upgrade_dir(paths).join("rollback.json")
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
fn major_upgrade_backup_path(paths: &RuntimePaths) -> PathBuf {
    schema_upgrade_backup_path(paths, MAJOR_SOURCE_SCHEMA, SCHEMA_VERSION)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn schema_upgrade_backup_path(
    paths: &RuntimePaths,
    source_schema: i64,
    target_schema: i64,
) -> PathBuf {
    major_upgrade_dir(paths).join(format!(
        "state-schema-{source_schema}-before-{target_schema}.db"
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn major_upgrade_source_binary_path(paths: &RuntimePaths) -> PathBuf {
    major_upgrade_dir(paths).join("source-herdr-mcp")
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn active_runtime_binary(paths: &RuntimePaths) -> Result<PathBuf, String> {
    let path = paths.config_dir.join("runtime/current/herdr-mcp");
    fs::canonicalize(&path)
        .map_err(|error| format!("cannot resolve active runtime {}: {error}", path.display()))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn read_raw_state_schema(path: &Path) -> Result<Option<i64>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("cannot open state database {}: {error}", path.display()))?;
    let value = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("cannot read state schema {}: {error}", path.display()))?;
    value
        .map(|value| {
            value.parse::<i64>().map_err(|_| {
                format!(
                    "state database {} has an invalid schema version",
                    path.display()
                )
            })
        })
        .transpose()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn prepare_major_upgrade_record(
    paths: &RuntimePaths,
    state_path: &Path,
) -> Result<MajorUpgradeRecord, String> {
    prepare_schema_upgrade_record(
        paths,
        state_path,
        MAJOR_SOURCE_SCHEMA,
        SCHEMA_VERSION,
        "major state migration",
        None,
    )
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn prepare_state_schema_migration_record(
    paths: &RuntimePaths,
    job_id: &str,
    migration: &StateSchemaMigration,
) -> Result<MajorUpgradeRecord, String> {
    if migration.reversible || migration.to != migration.from.saturating_add(1) {
        return Err(
            "state-schema migration declaration is not a single irreversible forward step"
                .to_owned(),
        );
    }
    prepare_schema_upgrade_record(
        paths,
        &paths.config_dir.join("state.db"),
        migration.from,
        migration.to,
        "state-schema migration",
        Some(job_id),
    )
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn discard_state_schema_migration_record(
    paths: &RuntimePaths,
    record: &MajorUpgradeRecord,
) -> Result<(), String> {
    run_service_command(
        Path::new(&record.source_binary),
        paths,
        &["service", "start"],
    )
    .map_err(|error| {
        format!("cannot restart previous service after migration staging failure: {error}")
    })?;

    fs::remove_file(major_upgrade_record_path(paths))
        .map_err(|error| format!("cannot remove migration rollback record: {error}"))?;
    let backup = Path::new(&record.state_backup);
    if backup.is_file() {
        fs::remove_file(backup)
            .map_err(|error| format!("cannot remove migration state backup: {error}"))?;
    }
    let source_binary = Path::new(&record.source_binary);
    if source_binary.is_file() {
        fs::remove_file(source_binary)
            .map_err(|error| format!("cannot remove migration source binary: {error}"))?;
    }
    let dir = major_upgrade_dir(paths);
    if dir.is_dir() {
        fs::remove_dir(&dir)
            .map_err(|error| format!("cannot remove migration rollback directory: {error}"))?;
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn recover_failed_state_schema_migration(
    paths: &RuntimePaths,
    job: &UpdateJobRecord,
) -> Result<(), String> {
    // A schema migration is represented by a distinct source/target pair in
    // the durable job. Ordinary jobs must never consume an unrelated major
    // rollback record left by an earlier update.
    if job.state_schema_source == job.state_schema_target {
        return Ok(());
    }
    let Some(record) = load_major_upgrade_record(paths)? else {
        return Ok(());
    };
    if record.job_id.as_deref() != Some(job.job_id.as_str())
        || record.source_state_schema != job.state_schema_source
        || record.target_state_schema != job.state_schema_target
    {
        return Ok(());
    }
    let state_path = paths.config_dir.join("state.db");
    let live_schema = read_raw_state_schema(&state_path)?;
    if live_schema == Some(record.source_state_schema) {
        discard_state_schema_migration_record(paths, &record)
    } else {
        // A failed install may have migrated the live database before the
        // service manager rolled the binary back. Restore the backup first;
        // recover_major_upgrade starts the source binary only after this
        // restore succeeds.
        recover_major_upgrade(paths, &record)
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn prepare_schema_upgrade_record(
    paths: &RuntimePaths,
    state_path: &Path,
    source_schema: i64,
    target_schema: i64,
    label: &str,
    job_id: Option<&str>,
) -> Result<MajorUpgradeRecord, String> {
    let active_source_binary = active_runtime_binary(paths)?;
    let dir = major_upgrade_dir(paths);
    if schema_upgrade_backup_path(paths, source_schema, target_schema).exists()
        || major_upgrade_source_binary_path(paths).exists()
        || major_upgrade_record_path(paths).exists()
    {
        return Err(format!(
            "major-upgrade rollback material already exists under {}; run major-rollback or inspect it before retrying",
            dir.display()
        ));
    }
    // The final snapshot must include writes committed while the old service stops.
    run_service_command(&active_source_binary, paths, &["service", "stop"])
        .map_err(|error| format!("cannot stop service before {label}: {error}"))?;
    match snapshot_schema_upgrade_record(
        paths,
        state_path,
        &active_source_binary,
        source_schema,
        target_schema,
        job_id,
    ) {
        Ok(record) => Ok(record),
        Err(error) => {
            let restart = run_service_command(&active_source_binary, paths, &["service", "start"]);
            Err(match restart {
                Ok(()) => {
                    format!("{label} backup failed ({error}); previous service restarted")
                }
                Err(restart_error) => format!(
                    "{label} backup failed ({error}); previous service restart failed: {restart_error}"
                ),
            })
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn snapshot_schema_upgrade_record(
    paths: &RuntimePaths,
    state_path: &Path,
    active_source_binary: &Path,
    source_schema: i64,
    target_schema: i64,
    job_id: Option<&str>,
) -> Result<MajorUpgradeRecord, String> {
    let dir = major_upgrade_dir(paths);
    fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create major-upgrade backup directory: {error}"))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure major-upgrade backup directory: {error}"))?;
    let backup = schema_upgrade_backup_path(paths, source_schema, target_schema);
    let source_binary = major_upgrade_source_binary_path(paths);
    if backup.exists() || source_binary.exists() || major_upgrade_record_path(paths).exists() {
        return Err(format!(
            "major-upgrade rollback material already exists under {}; run major-rollback or inspect it before retrying",
            dir.display()
        ));
    }

    snapshot_state_database(state_path, &backup)?;
    let backup_schema = match read_raw_state_schema(&backup) {
        Ok(Some(value)) => value,
        Ok(None) => {
            let _ = fs::remove_file(&backup);
            return Err("major-upgrade backup has no schema version".to_owned());
        }
        Err(error) => {
            let _ = fs::remove_file(&backup);
            return Err(error);
        }
    };
    if backup_schema != source_schema {
        let _ = fs::remove_file(&backup);
        return Err(format!(
            "state backup schema changed during snapshot: expected {source_schema}, got {backup_schema}"
        ));
    }
    let state_backup_sha256 = match sha256_file(&backup, MAJOR_BACKUP_MAX_BYTES) {
        Ok(value) => value,
        Err(error) => {
            let _ = fs::remove_file(&backup);
            return Err(error);
        }
    };
    if let Err(error) = copy_regular_file_with_mode(
        active_source_binary,
        &source_binary,
        BINARY_MAX_BYTES,
        0o700,
    ) {
        let _ = fs::remove_file(&source_binary);
        let _ = fs::remove_file(&backup);
        return Err(error);
    }
    let source_binary_sha256 = match sha256_file(&source_binary, BINARY_MAX_BYTES) {
        Ok(value) => value,
        Err(error) => {
            let _ = fs::remove_file(&source_binary);
            let _ = fs::remove_file(&backup);
            return Err(error);
        }
    };
    let record = MajorUpgradeRecord {
        record_version: MAJOR_UPGRADE_RECORD_VERSION,
        job_id: job_id.map(str::to_owned),
        source_state_schema: source_schema,
        target_state_schema: target_schema,
        source_binary: source_binary.to_string_lossy().into_owned(),
        source_binary_sha256,
        state_backup: backup.to_string_lossy().into_owned(),
        state_backup_sha256,
        created_at_ms: now_ms_i64(),
    };
    if let Err(error) = write_major_upgrade_record(paths, &record) {
        let _ = fs::remove_file(&source_binary);
        let _ = fs::remove_file(&backup);
        return Err(error);
    }
    Ok(record)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn validate_major_upgrade_record(
    paths: &RuntimePaths,
    record: &MajorUpgradeRecord,
) -> Result<(), String> {
    if record.record_version != MAJOR_UPGRADE_RECORD_VERSION
        || record.target_state_schema != SCHEMA_VERSION
        || (record.source_state_schema != MAJOR_SOURCE_SCHEMA
            && record.target_state_schema != record.source_state_schema.saturating_add(1))
    {
        return Err("major-upgrade rollback record is incompatible with this binary".to_owned());
    }
    if Path::new(&record.source_binary) != major_upgrade_source_binary_path(paths)
        || Path::new(&record.state_backup)
            != schema_upgrade_backup_path(
                paths,
                record.source_state_schema,
                record.target_state_schema,
            )
    {
        return Err(
            "major-upgrade rollback record points outside its private backup set".to_owned(),
        );
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn load_major_upgrade_record(paths: &RuntimePaths) -> Result<Option<MajorUpgradeRecord>, String> {
    let path = major_upgrade_record_path(paths);
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&path)
        .map_err(|error| format!("cannot read major-upgrade rollback record: {error}"))?;
    if bytes.len() > 64 * 1024 {
        return Err("major-upgrade rollback record is too large".to_owned());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("cannot decode major-upgrade rollback record: {error}"))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn write_major_upgrade_record(
    paths: &RuntimePaths,
    record: &MajorUpgradeRecord,
) -> Result<(), String> {
    let path = major_upgrade_record_path(paths);
    if path.exists() {
        return Err(format!(
            "major-upgrade rollback record already exists: {}",
            path.display()
        ));
    }
    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|error| format!("cannot encode major-upgrade rollback record: {error}"))?;
    let temp = path.with_file_name(format!(".rollback-{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|error| format!("cannot create major-upgrade rollback record: {error}"))?;
    if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&temp);
        return Err(format!(
            "cannot persist major-upgrade rollback record: {error}"
        ));
    }
    drop(file);
    if let Err(error) = fs::rename(&temp, &path) {
        let _ = fs::remove_file(&temp);
        return Err(format!(
            "cannot commit major-upgrade rollback record: {error}"
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn snapshot_state_database(source: &Path, target: &Path) -> Result<(), String> {
    if target.exists() {
        return Err(format!(
            "major-upgrade state snapshot already exists: {}",
            target.display()
        ));
    }
    let conn = Connection::open(source)
        .map_err(|error| format!("cannot open state database for snapshot: {error}"))?;
    let target_text = target
        .to_str()
        .ok_or_else(|| "major-upgrade snapshot path is not valid UTF-8".to_owned())?;
    if let Err(error) = conn.execute("VACUUM INTO ?1", [target_text]) {
        let _ = fs::remove_file(target);
        return Err(format!(
            "cannot snapshot state database before major upgrade: {error}"
        ));
    }
    if let Err(error) = fs::set_permissions(target, fs::Permissions::from_mode(0o600)) {
        let _ = fs::remove_file(target);
        return Err(format!("cannot secure state snapshot: {error}"));
    }
    if let Err(error) = fs::File::open(target).and_then(|file| file.sync_all()) {
        let _ = fs::remove_file(target);
        return Err(format!("cannot sync state snapshot: {error}"));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn copy_regular_file(source: &Path, target: &Path, max_bytes: u64) -> Result<(), String> {
    copy_regular_file_with_mode(source, target, max_bytes, 0o600)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn copy_regular_file_with_mode(
    source: &Path,
    target: &Path,
    max_bytes: u64,
    mode: u32,
) -> Result<(), String> {
    let meta = fs::symlink_metadata(source)
        .map_err(|error| format!("cannot inspect {}: {error}", source.display()))?;
    if !meta.file_type().is_file() || meta.len() > max_bytes {
        return Err(format!(
            "refusing unsafe or oversized file {}",
            source.display()
        ));
    }
    fs::copy(source, target).map_err(|error| {
        format!(
            "cannot copy {} -> {}: {error}",
            source.display(),
            target.display()
        )
    })?;
    fs::set_permissions(target, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("cannot secure {}: {error}", target.display()))?;
    fs::File::open(target)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("cannot sync {}: {error}", target.display()))?;
    Ok(())
}

/// v1.0.0-v1.0.2 renamed the released 0.4.x `config.toml` to
/// `config.toml.migrated` the first time any 1.x command loaded configuration.
/// The schema-5 source Runtime reads only `config.toml`, so a failed or
/// rolled-back major upgrade silently lost its Edge origin, device id, Link
/// upstream and update channel. Before the source Runtime is reinstalled, put
/// the preserved legacy file back when (and only when) `config.toml` is absent.
///
/// Returns the restored path, or `None` when nothing needed restoring. Never
/// overwrites an existing `config.toml`, and refuses symlinks/non-regular files.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn restore_legacy_toml_config(config_dir: &Path) -> Result<Option<PathBuf>, String> {
    let legacy = config_dir.join("config.toml");
    if fs::symlink_metadata(&legacy).is_ok() {
        return Ok(None);
    }
    let migrated = config_dir.join("config.toml.migrated");
    if fs::symlink_metadata(&migrated).is_err() {
        return Ok(None);
    }
    let temp = config_dir.join(format!(".config.toml.restore-{}.tmp", std::process::id()));
    if fs::symlink_metadata(&temp).is_ok() {
        fs::remove_file(&temp)
            .map_err(|error| format!("cannot clear stale legacy config restore file: {error}"))?;
    }
    copy_regular_file_with_mode(&migrated, &temp, LEGACY_CONFIG_MAX_BYTES, 0o600)?;
    if fs::symlink_metadata(&legacy).is_ok() {
        // Someone recreated config.toml while we copied; never overwrite it.
        let _ = fs::remove_file(&temp);
        return Ok(None);
    }
    fs::rename(&temp, &legacy).map_err(|error| {
        let _ = fs::remove_file(&temp);
        format!("cannot restore legacy config {}: {error}", legacy.display())
    })?;
    Ok(Some(legacy))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn sha256_file(path: &Path, max_bytes: u64) -> Result<String, String> {
    let meta = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if !meta.file_type().is_file() || meta.len() > max_bytes {
        return Err(format!(
            "refusing unsafe or oversized file {}",
            path.display()
        ));
    }
    let mut file =
        fs::File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot hash {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn verify_file_sha256(
    path: &Path,
    expected: &str,
    max_bytes: u64,
    label: &str,
) -> Result<(), String> {
    if sha256_file(path, max_bytes)? != expected {
        return Err(format!("{label} sha256 mismatch"));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn run_service_command(binary: &Path, paths: &RuntimePaths, args: &[&str]) -> Result<(), String> {
    let mut command = Command::new(binary);
    command
        .args(args)
        .env("HERDR_MCP_CONFIG_DIR", &paths.config_dir)
        .env_remove("HERDR_MCP_EXEC_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(name) = paths.instance.name() {
        command.env("HERDR_MCP_INSTANCE", name);
    }
    let output = command
        .output()
        .map_err(|error| format!("cannot execute {}: {error}", binary.display()))?;
    if output.status.success() {
        #[cfg(target_os = "macos")]
        if args == ["service", "stop"] {
            verify_major_upgrade_service_stopped(paths)?;
        }
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "{} {} failed: {}",
        binary.display(),
        args.join(" "),
        stderr.chars().take(1024).collect::<String>().trim()
    ))
}

#[cfg(target_os = "macos")]
fn verify_major_upgrade_service_stopped(paths: &RuntimePaths) -> Result<(), String> {
    // Older source binaries treat every failed launchctl lookup as "not loaded".
    // Independently require an explicit absence before taking or restoring a snapshot.
    let uid = unsafe { libc::getuid() };
    let label = paths.instance.service_label();
    let target = format!("gui/{uid}/{label}");
    let output = Command::new("/bin/launchctl")
        .args(["print", &target])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("cannot verify stopped service {target}: {error}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success()
        && stderr.lines().any(|line| {
            line == format!("Could not find service \"{label}\" in domain for user gui: {uid}")
        })
    {
        return Ok(());
    }
    Err(format!(
        "cannot confirm service {target} is stopped ({}): {}",
        output.status,
        stderr.chars().take(1024).collect::<String>().trim()
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn restore_state_backup(record: &MajorUpgradeRecord, state_path: &Path) -> Result<(), String> {
    verify_file_sha256(
        Path::new(&record.state_backup),
        &record.state_backup_sha256,
        MAJOR_BACKUP_MAX_BYTES,
        "major-upgrade state backup",
    )?;
    let temp = state_path.with_file_name(format!(".state-restore-{}.tmp", std::process::id()));
    if temp.exists() {
        fs::remove_file(&temp)
            .map_err(|error| format!("cannot clear stale state restore temp file: {error}"))?;
    }
    copy_regular_file(
        Path::new(&record.state_backup),
        &temp,
        MAJOR_BACKUP_MAX_BYTES,
    )?;
    fs::rename(&temp, state_path)
        .map_err(|error| format!("cannot restore pre-upgrade state database: {error}"))?;
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = state_path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
        if sidecar.exists() {
            fs::remove_file(&sidecar).map_err(|error| {
                format!(
                    "cannot remove restored SQLite sidecar {}: {error}",
                    sidecar.display()
                )
            })?;
        }
    }
    let schema = read_raw_state_schema(state_path)?
        .ok_or_else(|| "restored state database has no schema version".to_owned())?;
    if schema != record.source_state_schema {
        return Err(format!(
            "restored state schema mismatch: expected {}, got {schema}",
            record.source_state_schema
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn validate_restored_source_install_after_error(
    paths: &RuntimePaths,
    record: &MajorUpgradeRecord,
    install_error: &str,
) -> Result<(), String> {
    if !install_error.contains("production Link generation reconcile failed") {
        return Err(format!(
            "major rollback source install failed outside the recoverable production-Link reconciliation class: {install_error}"
        ));
    }
    let active = active_runtime_binary(paths).map_err(|error| {
        format!(
            "major rollback source install failed and active runtime could not be resolved: {install_error}; {error}"
        )
    })?;
    verify_file_sha256(
        &active,
        &record.source_binary_sha256,
        BINARY_MAX_BYTES,
        "restored major-upgrade source runtime",
    )
    .map_err(|error| {
        format!(
            "major rollback source install failed without restoring the recorded source runtime: {install_error}; {error}"
        )
    })?;
    run_service_command(
        Path::new(&record.source_binary),
        paths,
        &["service", "status"],
    )
    .map_err(|error| {
        format!(
            "major rollback source install failed and the restored source service is not healthy: {install_error}; {error}"
        )
    })?;
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn recover_major_upgrade(paths: &RuntimePaths, record: &MajorUpgradeRecord) -> Result<(), String> {
    validate_major_upgrade_record(paths, record)?;
    verify_file_sha256(
        Path::new(&record.source_binary),
        &record.source_binary_sha256,
        BINARY_MAX_BYTES,
        "major-upgrade source binary",
    )?;
    let stop_binary =
        active_runtime_binary(paths).unwrap_or_else(|_| PathBuf::from(&record.source_binary));
    run_service_command(&stop_binary, paths, &["service", "stop"])
        .map_err(|error| format!("cannot stop service before major state restore: {error}"))?;
    let state_path = paths.config_dir.join("state.db");
    restore_state_backup(record, &state_path)?;
    // The source installer below renders LaunchAgents/units from the legacy
    // config, so it should see the pre-upgrade file. A restore failure must not
    // prevent the source Runtime/schema from being put back, but it also must
    // not be reported as a complete rollback or retire the rollback evidence.
    let legacy_config_restore_error = match restore_legacy_toml_config(&paths.config_dir) {
        Ok(Some(path)) => {
            eprintln!(
                "restored released 0.4.x configuration {} from config.toml.migrated",
                path.display()
            );
            None
        }
        Ok(None) => None,
        Err(error) => {
            eprintln!(
                "warning: could not restore released 0.4.x config.toml before source reinstall: {error}"
            );
            Some(error)
        }
    };
    let source_install_error = run_service_command(
        Path::new(&record.source_binary),
        paths,
        &["service", "install"],
    )
    .err();
    if let Some(error) = source_install_error.as_deref() {
        // Older source installers can report a sidecar-generation failure after
        // committing the exact source runtime and restarting a healthy service.
        // Treat that as recoverable only after proving those postconditions.
        validate_restored_source_install_after_error(paths, record, error)?;
    }
    #[cfg(target_os = "macos")]
    if !paths.instance.is_named() {
        crate::native_host_install::sync_owned_runtime_from_active().map_err(|error| {
            format!(
                "major rollback restored the source runtime but native-host reconciliation failed: {error}"
            )
        })?;
    }
    if let Some(error) = source_install_error.as_deref() {
        crate::link::reconcile_after_service_generation_change(paths).map_err(|link_error| {
            format!(
                "major rollback source install reported an error after restoring the source runtime, and current Link reconciliation also failed: {error}; {link_error}"
            )
        })?;
    }
    let schema = read_raw_state_schema(&state_path)?
        .ok_or_else(|| "restored runtime did not expose a state database".to_owned())?;
    if schema != record.source_state_schema {
        return Err(format!(
            "restored runtime changed state schema unexpectedly: expected {}, got {schema}",
            record.source_state_schema
        ));
    }
    if let Some(error) = legacy_config_restore_error {
        return Err(format!(
            "major rollback restored source Runtime and schema {}, but legacy config.toml recovery is incomplete: {error}; rollback material was retained",
            record.source_state_schema
        ));
    }
    let record_path = major_upgrade_record_path(paths);
    fs::remove_file(&record_path)
        .map_err(|error| format!("cannot retire major-upgrade rollback record: {error}"))?;
    let backup = Path::new(&record.state_backup);
    if backup.is_file() {
        fs::remove_file(backup).map_err(|error| {
            format!("cannot retire consumed major-upgrade state backup: {error}")
        })?;
    }
    let source_binary = Path::new(&record.source_binary);
    if source_binary.is_file() {
        fs::remove_file(source_binary).map_err(|error| {
            format!("cannot retire consumed major-upgrade source binary: {error}")
        })?;
    }
    let dir = major_upgrade_dir(paths);
    if dir.is_dir() {
        fs::remove_dir(&dir).map_err(|error| {
            format!("cannot retire empty major-upgrade backup directory: {error}")
        })?;
    }
    Ok(())
}

/// Runtime outcome recorded by a durable update job.
///
/// The Runtime is what `update apply` guarantees; the fleet Worker is a separate
/// release plane with its own operator command. `update status` classifies the
/// Runtime job only, so a Worker problem recorded alongside a committed Runtime
/// never turns the update into a failure.
fn runtime_job_outcome(state: &str) -> &'static str {
    match state {
        "succeeded" => "succeeded",
        "failed" => "failed",
        "queued" | "installing" => "in_progress",
        _ => "unknown",
    }
}

/// Whether a job's detail is the legacy `<= v1.1.0-rc.8` chained-Worker text.
///
/// Updaters up to v1.1.0-rc.8 chained `worker update` into the Runtime result and
/// recorded codes such as `update_succeeded_worker_update_required` there. The
/// Runtime had already committed, so such a job is a success plus a Worker
/// notice, not a failed update.
fn legacy_worker_update_detail(detail: Option<&str>) -> bool {
    let Some(detail) = detail else {
        return false;
    };
    let lower = detail.to_ascii_lowercase();
    lower.contains("update_succeeded_worker_update_required")
        || lower.contains("worker_update_failed")
}

fn legacy_worker_update_notice() -> Value {
    json!({
        "code": "worker_update_failed_legacy",
        "severity": "notice",
        "fatal": false,
        "message": "this update job was recorded by an updater <= v1.1.0-rc.8, which chained the separate fleet Worker update into the Runtime result; the Runtime update itself succeeded",
        "next_action": WORKER_UPDATE_NEXT_ACTION,
    })
}

/// Whether a legacy chained-Worker job provably finished its Runtime swap.
///
/// A `<= v1.1.0-rc.8` updater could record the chained-Worker code and a
/// non-successful job state even though the Runtime had already been activated.
/// The recorded state alone cannot settle that, so the decisive evidence is the
/// installed Runtime: when it already reports the job's target version, the swap
/// committed and only the separate fleet Worker step is outstanding. When the
/// installed version is still something else the Runtime genuinely did not
/// commit and the job stays a failure.
fn legacy_worker_swap_committed(job: &UpdateJobRecord, current_version: &str) -> bool {
    legacy_worker_update_detail(job.detail.as_deref()) && job.version == current_version
}

/// Read-only `update status` view.
///
/// `ok` and `runtime_outcome` describe the recorded Runtime job; Worker drift or
/// a legacy Worker failure is only a notice. Verification commands for an
/// operator are `herdr-mcp --version`, `herdr-mcp update status` and
/// `herdr-mcp doctor`.
fn update_status_view(
    job: Option<&UpdateJobRecord>,
    current_version: &str,
    worker_update: Value,
) -> Value {
    let worker_update_required = worker_update_required(&worker_update);
    let recorded_outcome = job.map_or("none", |job| runtime_job_outcome(&job.state));
    // An older updater could record the chained-Worker text next to a
    // non-successful state after the Runtime had in fact been activated. The
    // installed Runtime is the proof, so a job whose target version is already
    // installed reports the Runtime success the operator actually got.
    let legacy_swap_committed =
        job.is_some_and(|job| legacy_worker_swap_committed(job, current_version));
    let outcome = if recorded_outcome == "failed" && legacy_swap_committed {
        "succeeded"
    } else {
        recorded_outcome
    };
    let mut notices = worker_update_notices(&worker_update)
        .as_array()
        .cloned()
        .unwrap_or_default();
    if outcome == "succeeded"
        && job.is_some_and(|job| legacy_worker_update_detail(job.detail.as_deref()))
    {
        notices.push(legacy_worker_update_notice());
    }
    let next_action = if worker_update_required {
        Some(WORKER_UPDATE_NEXT_ACTION)
    } else {
        match outcome {
            "failed" => Some("herdr-mcp update apply"),
            "in_progress" => Some("herdr-mcp update status"),
            _ => None,
        }
    };
    json!({
        "ok": outcome != "failed",
        "code": "update_status",
        "runtime_outcome": outcome,
        "recorded_runtime_state": job.map(|job| job.state.clone()),
        "job": job.map(public_job_view),
        "current_version": current_version,
        "worker_update_required": worker_update_required,
        "worker_update": worker_update,
        "notices": notices,
        "next_action": next_action,
    })
}

fn status() -> Result<ExitCode, String> {
    let paths = RuntimePaths::discover()?;
    let store = UpdateStore::open(&paths)?;
    let latest = store.latest_update_job()?;
    let current_version = current_version()?.to_string();
    let worker_update = worker_update_status_view(&paths, &current_version);
    let mut view = update_status_view(latest.as_ref(), &current_version, worker_update);
    // #844: report the recorded preflight. A status query must never become the
    // syscall that blocks on a macOS permission dialog.
    if let Some(object) = view.as_object_mut() {
        object.insert(
            "tcc_preflight".to_owned(),
            crate::macos_privacy::persisted_tcc_preflight(&paths.config_dir),
        );
        object.insert(
            "tcc_broker_migration".to_owned(),
            persisted_broker_migration(&paths.config_dir),
        );
    }
    print_json(&view)?;
    Ok(ExitCode::SUCCESS)
}

fn worker(job_id: &str) -> Result<ExitCode, String> {
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = job_id;
        Err(
            "native update worker is currently supported on macOS and Linux service managers"
                .to_owned(),
        )
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if !valid_job_id(job_id) {
            return Err("invalid update job id".to_owned());
        }
        let paths = RuntimePaths::discover()?;
        let store = UpdateStore::open(&paths)?;
        let job = store
            .update_job(job_id)?
            .ok_or_else(|| "update job not found".to_owned())?;
        if job.state != "queued" {
            return Err(format!(
                "update job {job_id} is {}, expected queued",
                job.state
            ));
        }
        let binary = PathBuf::from(&job.binary_path);
        if !binary_is_confined(&paths, &binary, job_id) {
            store.update_update_job(
                job_id,
                "failed",
                Some("staged binary escaped update job directory"),
                None,
                now_ms_i64(),
            )?;
            return Err("staged update binary is outside its job directory".to_owned());
        }
        verify_staged_file(&binary, &job.sha256)?;
        probe_candidate_binary(
            &binary,
            &Version::parse(&job.version)
                .map_err(|_| "durable update job contains an invalid version".to_owned())?,
            &crate::contract::identity()?,
            job.state_schema_target,
        )?;
        store.update_update_job(
            job_id,
            "installing",
            Some("candidate verified; service install and health gate started"),
            None,
            now_ms_i64(),
        )?;

        if let Err(error) = crate::update_scheduler::ensure_updates_allowed() {
            store.update_update_job(
                job_id,
                "failed",
                Some("service uninstall fence blocked update activation"),
                None,
                now_ms_i64(),
            )?;
            cleanup_staging(&binary);
            return match recover_failed_state_schema_migration(&paths, &job) {
                Ok(()) => Err(error),
                Err(recovery_error) => Err(format!(
                    "{error}; state-schema migration cleanup failed: {recovery_error}"
                )),
            };
        }

        let install = crate::service_lifecycle::run(ServiceCommand::Install { adopt_node: false });
        let succeeded = matches!(install, Ok(code) if code == ExitCode::SUCCESS);
        if succeeded {
            // The newly activated release is the only process that knows which
            // extension version belongs to it, and the detached worker is where
            // both the interactive and the queued `update apply` paths converge.
            // Staging is best-effort: the Runtime update already committed.
            let staging = stage_standalone_extension_after_update(None);
            report_standalone_extension_staging(&staging);
            let detail = match staging {
                Ok(staged) => format!(
                    "service install committed and health gate passed; {}",
                    staged.summary()
                ),
                Err(error) => format!(
                    "service install committed and health gate passed; extension staging did not complete: {error}"
                ),
            };
            // #844: this process is the newly downloaded binary, and every apply
            // path (interactive, `update auto`, and the queued path with no
            // foreground watcher) converges here. That makes it the only place
            // where the broker migration reliably runs on the first apply of this
            // release: the foreground updater that installed it is the previous
            // binary and does not contain this code. Migration has no Documents
            // syscall and is best-effort, so a failure never changes the job state.
            if let Ok(paths) = crate::paths::RuntimePaths::discover() {
                // Same ordering as the foreground: migrate (no Documents syscall)
                // and only then run the one bounded preflight, so the single
                // prompt belongs to the broker that stays installed.
                let mut scratch = json!({});
                let installed_revision =
                    crate::tcc_broker::installed_compat_revision(&paths.config_dir);
                attach_post_commit_tcc(
                    &mut scratch,
                    &paths.config_dir,
                    installed_revision,
                    |config_dir| {
                        crate::tcc_broker::migrate_for_explicit_reauthorization(config_dir)
                    },
                    crate::macos_privacy::preflight_after_activation,
                );
                if let Some(migration) = scratch.get("tcc_broker_migration") {
                    persist_broker_migration(&paths.config_dir, migration);
                }
            }
            store.update_update_job(job_id, "succeeded", Some(&detail), None, now_ms_i64())?;
            crate::local_agent_skill::sync_after_install_best_effort();
            cleanup_staging(&binary);
            Ok(ExitCode::SUCCESS)
        } else {
            let detail = match &install {
                Ok(_) => "service install returned non-zero",
                Err(_) => "service install failed; service manager rollback policy applied",
            };
            store.update_update_job(job_id, "failed", Some(detail), None, now_ms_i64())?;
            cleanup_staging(&binary);
            let migration_recovery = recover_failed_state_schema_migration(&paths, &job);
            match migration_recovery {
                Ok(()) => install,
                Err(recovery_error) => Err(match install {
                    Ok(code) => format!(
                        "service install returned {code:?}; state-schema migration recovery failed: {recovery_error}"
                    ),
                    Err(error) => {
                        format!("{error}; state-schema migration recovery failed: {recovery_error}")
                    }
                }),
            }
        }
    }
}

fn load_update_channel() -> Result<UpdateChannel, String> {
    let paths = RuntimePaths::discover()?;
    Ok(Config::load(&paths.config_file)?.update_channel)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn recover_or_reject_active_update(
    store: &UpdateStore,
    paths: &RuntimePaths,
) -> Result<(), String> {
    let Some(active) = store.active_update_job()? else {
        return Ok(());
    };
    let now = now_ms_i64();
    let worker_alive = active.worker_pid.is_some_and(process_alive);
    let awaiting_pid =
        active.worker_pid.is_none() && now.saturating_sub(active.updated_at) < 30_000;
    if worker_alive || awaiting_pid {
        return Err(format!(
            "update already in progress: {} ({})",
            active.job_id, active.state
        ));
    }
    store.update_update_job(
        &active.job_id,
        "failed",
        Some("previous update worker is no longer running"),
        None,
        now,
    )?;
    let binary = PathBuf::from(&active.binary_path);
    if binary_is_confined(paths, &binary, &active.job_id) {
        cleanup_staging(&binary);
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn stage_release<W: Write>(
    paths: &RuntimePaths,
    plan: &ReleasePlan,
    progress: &mut UpdateProgress<W>,
) -> Result<(String, PathBuf), String> {
    let root = paths.config_dir.join("update").join("jobs");
    ensure_real_dir(&paths.config_dir)?;
    ensure_real_dir(&paths.config_dir.join("update"))?;
    ensure_real_dir(&root)?;
    let job_id = format!(
        "upd-{}-{}-{}",
        now_ms_i64(),
        std::process::id(),
        plan.asset.sha256.get(..8).unwrap_or("candidate")
    );
    let job_dir = root.join(&job_id);
    fs::create_dir(&job_dir)
        .map_err(|error| format!("cannot create update job directory: {error}"))?;
    #[cfg(unix)]
    fs::set_permissions(&job_dir, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure update job directory: {error}"))?;
    let binary = job_dir.join(if cfg!(windows) {
        "herdr-mcp-candidate.exe"
    } else {
        "herdr-mcp-candidate"
    });
    let client = update_client()?;
    progress.phase(format!("Verifying attestation for {}...", plan.asset.name));
    verify_artifact_attestation(
        &client,
        &plan.asset.name,
        &plan.asset.sha256,
        &plan.identity,
    )?;
    progress.phase(format!(
        "Downloading {} ({:.1} MiB)...",
        plan.asset.name,
        plan.asset.size as f64 / (1024.0 * 1024.0)
    ));
    if let Err(error) = download_asset(&client, &plan.asset, &binary, progress) {
        let _ = fs::remove_dir_all(&job_dir);
        return Err(error);
    }
    Ok((job_id, binary))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn download_asset<W: Write>(
    client: &Client,
    asset: &ReleaseAsset,
    target: &Path,
    progress: &mut UpdateProgress<W>,
) -> Result<(), String> {
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    let url = scheduled_tests::asset_url(&asset.url);
    #[cfg(not(test))]
    let url = asset.url.clone();
    let token = github_api_token();
    let mut response = authorized_get(client, &url, token.as_deref())
        .send()
        .map_err(|error| format!("release asset download failed: {}", error_kind(&error)))?;
    validate_response(&response)?;
    if let Some(length) = response.content_length()
        && length != asset.size
    {
        return Err("release asset content-length does not match manifest".to_owned());
    }
    let temp = target.with_extension(format!("tmp-{}", std::process::id()));
    #[cfg(unix)]
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|error| format!("cannot create staged update binary: {error}"))?;
    #[cfg(not(unix))]
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|error| format!("cannot create staged update binary: {error}"))?;
    let result = (|| {
        let mut digest = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        progress.download(0, asset.size);
        loop {
            let read = response
                .read(&mut buffer)
                .map_err(|error| format!("cannot read release asset: {error}"))?;
            if read == 0 {
                break;
            }
            total = total.saturating_add(read as u64);
            if total > asset.size || total > BINARY_MAX_BYTES {
                return Err("release asset exceeded declared size".to_owned());
            }
            digest.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .map_err(|error| format!("cannot write staged update binary: {error}"))?;
            progress.download(total, asset.size);
        }
        if total != asset.size {
            return Err("release asset byte count does not match manifest".to_owned());
        }
        let actual = format!("{:x}", digest.finalize());
        if actual != asset.sha256 {
            return Err("release asset sha256 mismatch".to_owned());
        }
        file.sync_all()
            .map_err(|error| format!("cannot sync staged update binary: {error}"))?;
        Ok(())
    })();
    // Publish a closed inode: Linux rejects an executable with ETXTBSY while any
    // writer still owns the inode, including a write descriptor inherited by a
    // concurrently forked child of this process.
    drop(file);
    let result = result.and_then(|()| {
        #[cfg(unix)]
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot make staged update binary executable: {error}"))?;
        fs::rename(&temp, target)
            .map_err(|error| format!("cannot activate staged update binary: {error}"))?;
        Ok(())
    });
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn watch_update_job<W: Write>(
    store: &UpdateStore,
    job_id: &str,
    child: &mut std::process::Child,
    progress: &mut UpdateProgress<W>,
) -> Result<Option<UpdateJobRecord>, String> {
    let started = Instant::now();
    loop {
        let job = store.update_job(job_id)?.ok_or_else(|| {
            format!("update job {job_id} disappeared while installer was running")
        })?;
        progress.job(&job);
        match job.state.as_str() {
            "succeeded" => return Ok(Some(job)),
            "failed" => {
                return Err(format!(
                    "update job {job_id} failed: {}",
                    job.detail.as_deref().unwrap_or("no detail")
                ));
            }
            "queued" | "installing" => {}
            other => {
                return Err(format!(
                    "update job {job_id} entered unexpected state {other}"
                ));
            }
        }

        if started.elapsed() >= UPDATE_WATCH_TIMEOUT {
            return Ok(None);
        }
        if child
            .try_wait()
            .map_err(|error| format!("cannot inspect update worker: {error}"))?
            .is_some()
        {
            std::thread::sleep(UPDATE_WATCH_POLL_INTERVAL);
            let final_job = store
                .update_job(job_id)?
                .ok_or_else(|| format!("update job {job_id} disappeared after worker exit"))?;
            progress.job(&final_job);
            return match final_job.state.as_str() {
                "succeeded" => Ok(Some(final_job)),
                "failed" => Err(format!(
                    "update job {job_id} failed: {}",
                    final_job.detail.as_deref().unwrap_or("no detail")
                )),
                other => Err(format!(
                    "update worker exited while job {job_id} was still {other}; inspect with `herdr-mcp update status`"
                )),
            };
        }
        std::thread::sleep(UPDATE_WATCH_POLL_INTERVAL);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn verify_staged_file(path: &Path, expected_sha256: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect staged update binary: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("staged update binary is not a real file".to_owned());
    }
    let mut file = fs::File::open(path)
        .map_err(|error| format!("cannot open staged update binary: {error}"))?;
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot hash staged update binary: {error}"))?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        if total > BINARY_MAX_BYTES {
            return Err("staged update binary is too large".to_owned());
        }
        digest.update(&buffer[..read]);
    }
    let actual = format!("{:x}", digest.finalize());
    if actual != expected_sha256 {
        return Err("staged update binary changed after verification".to_owned());
    }
    Ok(())
}

/// Linux rejects `execve` with `ETXTBSY` while any process holds a write
/// descriptor to the file, and a concurrently forked child of this process can
/// inherit the updater's staging descriptor for a moment. That window is
/// unrelated to the update itself, so execution of a just-staged candidate is
/// retried briefly instead of failing the update.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const EXEC_BUSY_ATTEMPTS: usize = 10;
#[cfg(any(target_os = "macos", target_os = "linux"))]
const EXEC_BUSY_RETRY_DELAY: Duration = Duration::from_millis(20);

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn executable_is_busy(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ETXTBSY)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn retry_text_file_busy<T>(
    attempts: usize,
    delay: Duration,
    mut operation: impl FnMut() -> std::io::Result<T>,
) -> std::io::Result<T> {
    let attempts = attempts.max(1);
    let mut attempt = 1;
    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) => {
                if attempt >= attempts || !executable_is_busy(&error) {
                    return Err(error);
                }
                attempt += 1;
                if !delay.is_zero() {
                    std::thread::sleep(delay);
                }
            }
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn probe_candidate_binary(
    path: &Path,
    expected: &Version,
    expected_contract: &crate::contract::ContractIdentity,
    expected_schema: i64,
) -> Result<(), String> {
    let output = retry_text_file_busy(EXEC_BUSY_ATTEMPTS, EXEC_BUSY_RETRY_DELAY, || {
        Command::new(path)
            .arg("version")
            .stdin(Stdio::null())
            .output()
    })
    .map_err(|error| format!("cannot execute staged update binary: {error}"))?;
    if !output.status.success() || output.stdout.len() > 16 * 1024 {
        return Err("staged update binary version probe failed".to_owned());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    verify_candidate_identity(&text, expected, expected_contract, expected_schema)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn verify_candidate_identity(
    text: &str,
    expected: &Version,
    expected_contract: &crate::contract::ContractIdentity,
    expected_schema: i64,
) -> Result<(), String> {
    let expected_first = format!("herdr-mcp {expected}");
    let expected_contract_line = format!(
        "contract epoch {} / {} tools",
        expected_contract.epoch, expected_contract.tool_count
    );
    let expected_hash = format!("contract hash {}", expected_contract.hash);
    let expected_schema = format!("state schema {expected_schema}");
    // Older same-contract releases did not print their hash. A new epoch must
    // report it; the detached worker is already the attested target binary.
    let hash_matches = match text.lines().find(|line| line.starts_with("contract hash ")) {
        Some(line) => line == expected_hash,
        None => *expected_contract == crate::contract::identity()?,
    };
    if text.lines().next() != Some(expected_first.as_str())
        || !text.lines().any(|line| line == expected_contract_line)
        || !hash_matches
        || !text.lines().any(|line| line == expected_schema)
    {
        return Err("staged update binary identity does not match release manifest".to_owned());
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn spawn_worker(
    paths: &RuntimePaths,
    binary: &Path,
    job_id: &str,
) -> Result<std::process::Child, String> {
    let mut command = Command::new(binary);
    command
        .arg("update")
        .arg("worker")
        .arg("--job")
        .arg(job_id)
        .env("HERDR_MCP_CONFIG_DIR", &paths.config_dir)
        .env_remove("HERDR_MCP_EXEC_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(name) = paths.instance.name() {
        command.env("HERDR_MCP_INSTANCE", name);
    } else {
        command.env_remove("HERDR_MCP_INSTANCE");
    }
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    retry_text_file_busy(EXEC_BUSY_ATTEMPTS, EXEC_BUSY_RETRY_DELAY, || {
        command.spawn()
    })
    .map_err(|error| format!("cannot spawn detached update worker: {error}"))
}

fn fetch_bounded(client: &Client, url: Url, max: usize, label: &str) -> Result<Vec<u8>, String> {
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    if let Some(bytes) = scheduled_tests::metadata(&url, label) {
        return Ok(bytes);
    }
    fetch_bounded_with_retry(
        client,
        url,
        max,
        label,
        METADATA_FETCH_ATTEMPTS,
        METADATA_RETRY_DELAY,
    )
}

fn fetch_bounded_with_retry(
    client: &Client,
    url: Url,
    max: usize,
    label: &str,
    attempts: usize,
    retry_delay: Duration,
) -> Result<Vec<u8>, String> {
    let attempts = attempts.max(1);
    let token = github_api_token();
    for attempt_index in 0..attempts {
        let mut response = match authorized_get(client, &url, token.as_deref()).send() {
            Ok(response) => response,
            Err(error) => {
                let retryable = error.is_timeout() || error.is_connect();
                if retryable && attempt_index + 1 < attempts {
                    if !retry_delay.is_zero() {
                        std::thread::sleep(retry_delay);
                    }
                    continue;
                }
                return Err(format!("{label} download failed: {}", error_kind(&error)));
            }
        };
        validate_response(&response)?;
        if response
            .content_length()
            .is_some_and(|length| length > max as u64)
        {
            return Err(format!("{label} is too large"));
        }
        let mut bytes = Vec::new();
        response
            .by_ref()
            .take(max as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("cannot read {label}: {error}"))?;
        if bytes.len() > max {
            return Err(format!("{label} is too large"));
        }
        return Ok(bytes);
    }
    unreachable!("metadata fetch attempts are clamped to at least one")
}

fn verify_artifact_attestation(
    client: &Client,
    artifact_name: &str,
    sha256: &str,
    identity: &ReleaseIdentity,
) -> Result<(), String> {
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    if let Some(result) = scheduled_tests::attestation(artifact_name, sha256, identity) {
        return result;
    }
    let url = release_trust::attestation_api_url(sha256)?;
    let token = github_api_token();
    let mut response = authorized_get(client, &url, token.as_deref())
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .map_err(|error| format!("GitHub attestation lookup failed: {}", error_kind(&error)))?;
    validate_response(&response)?;
    if response
        .content_length()
        .is_some_and(|length| length > ATTESTATION_MAX_BYTES as u64)
    {
        return Err("GitHub attestation response is too large".to_owned());
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(ATTESTATION_MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read GitHub attestation response: {error}"))?;
    if bytes.len() > ATTESTATION_MAX_BYTES {
        return Err("GitHub attestation response is too large".to_owned());
    }
    release_trust::verify_github_attestations(&bytes, artifact_name, sha256, identity)
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

fn update_client() -> Result<Client, String> {
    update_client_with_github_auth(github_api_token().is_some())
}

fn update_client_with_github_auth(github_auth_configured: bool) -> Result<Client, String> {
    Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .user_agent(format!("herdr-mcp-updater/{}", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many update redirects");
            }
            let previous = attempt.previous().last();
            if update_redirect_allowed(previous, attempt.url(), github_auth_configured) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
        .build()
        .map_err(|error| format!("cannot create update HTTP client: {error}"))
}

/// The optional operator token used to lift GitHub's anonymous API quota.
///
/// It is read from `GH_TOKEN` first, then `GITHUB_TOKEN`, and only a non-empty
/// value is used. The value is never logged, printed, embedded in an error, or
/// exposed through `Debug`/doctor output: it only ever reaches
/// [`authorized_get`], which attaches it to `api.github.com` requests.
fn github_api_token() -> Option<String> {
    for name in ["GH_TOKEN", "GITHUB_TOKEN"] {
        let Ok(value) = env::var(name) else {
            continue;
        };
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

/// True only for the exact GitHub API origin: HTTPS, `api.github.com`, no
/// userinfo. Lookalike hosts (`api.github.com.example.test`), HTTP, and other
/// GitHub hosts (release assets, `objects.githubusercontent.com`) are excluded
/// so the operator token can never leak to them.
fn github_api_auth_allowed(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case(GITHUB_API_HOST))
}

fn authorized_get(
    client: &Client,
    url: &Url,
    token: Option<&str>,
) -> reqwest::blocking::RequestBuilder {
    let request = client.get(url.clone());
    match token
        .filter(|_| github_api_auth_allowed(url))
        .and_then(sensitive_bearer_header)
    {
        Some(value) => request.header(reqwest::header::AUTHORIZATION, value),
        None => request,
    }
}

/// Marks the bearer header sensitive so the value is redacted ("Sensitive")
/// even if a request is ever formatted with `Debug`.
fn sensitive_bearer_header(token: &str) -> Option<reqwest::header::HeaderValue> {
    let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")).ok()?;
    value.set_sensitive(true);
    Some(value)
}

fn validate_response(response: &Response) -> Result<(), String> {
    if !update_url_allowed(response.url()) {
        return Err("release download redirected to a disallowed URL".to_owned());
    }
    if !response.status().is_success() {
        let status = response.status().as_u16();
        if let Some(error) = github_rate_limit_error(
            status,
            header_str(response, "x-ratelimit-remaining"),
            header_str(response, "x-ratelimit-reset"),
            header_str(response, "retry-after"),
            now_unix_seconds(),
        ) {
            return Err(error);
        }
        return Err(format!("release download returned HTTP {status}"));
    }
    Ok(())
}

fn header_str<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// Maps GitHub's unauthenticated quota exhaustion to one distinct, actionable
/// error instead of a generic `HTTP 403`.
///
/// A primary rate limit reports `403` (or `429`) with `x-ratelimit-remaining: 0`
/// plus `x-ratelimit-reset`; a secondary limit reports `429` or a `retry-after`
/// header. Every other `403` keeps the previous generic message.
fn github_rate_limit_error(
    status: u16,
    remaining: Option<&str>,
    reset_unix: Option<&str>,
    retry_after: Option<&str>,
    now_unix: i64,
) -> Option<String> {
    if status != 403 && status != 429 {
        return None;
    }
    let quota_exhausted = remaining.map(str::trim) == Some("0");
    if !quota_exhausted && status != 429 && retry_after.is_none() {
        return None;
    }
    let reset_at = reset_unix
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .or_else(|| {
            retry_after
                .and_then(|value| value.parse::<i64>().ok())
                .filter(|value| *value >= 0)
                .map(|seconds| now_unix.saturating_add(seconds))
        })
        .and_then(unix_seconds_rfc3339);
    let hint = "set GH_TOKEN or GITHUB_TOKEN to raise the GitHub API limit, or retry later";
    Some(match reset_at {
        Some(reset_at) => format!(
            "{GITHUB_RATE_LIMITED_CODE}: GitHub API rate limit exhausted (HTTP {status}), resets at {reset_at}; {hint}"
        ),
        None => format!(
            "{GITHUB_RATE_LIMITED_CODE}: GitHub API rate limit exhausted (HTTP {status}); {hint}"
        ),
    })
}

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or_default()
}

fn unix_seconds_rfc3339(seconds: i64) -> Option<String> {
    time::OffsetDateTime::from_unix_timestamp(seconds)
        .ok()
        .and_then(|value| {
            value
                .format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
}

fn parse_update_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|_| "update URL is invalid".to_owned())?;
    if !update_url_allowed(&url) {
        return Err("update URL must be HTTPS or loopback HTTP without credentials".to_owned());
    }
    Ok(url)
}

fn update_url_allowed(url: &Url) -> bool {
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    if url.scheme() == "https" {
        return true;
    }
    if url.scheme() != "http" {
        return false;
    }
    loopback_host(url.host_str())
}

fn update_redirect_allowed(
    previous: Option<&Url>,
    url: &Url,
    github_auth_configured: bool,
) -> bool {
    if !update_url_allowed(url) {
        return false;
    }
    // The operator token is attached only to `api.github.com` requests. Refuse
    // any redirect that would carry the request off that host (including onto a
    // loopback host) so a token can never reach a release-asset, lookalike, or
    // local address, even if the transport stops stripping sensitive headers on
    // cross-host redirects.
    if github_auth_configured
        && previous.is_some_and(github_api_auth_allowed)
        && !github_api_auth_allowed(url)
    {
        return false;
    }
    if loopback_host(url.host_str()) {
        return true;
    }
    matches!(
        url.host_str().map(|host| host.to_ascii_lowercase()),
        Some(host)
            if host == "github.com"
                || host == GITHUB_API_HOST
                || host == "objects.githubusercontent.com"
                || host == "release-assets.githubusercontent.com"
    )
}

fn loopback_host(host: Option<&str>) -> bool {
    matches!(
        host.map(str::to_ascii_lowercase),
        Some(host) if host == "localhost" || host == "127.0.0.1" || host == "::1"
    )
}

fn current_target() -> Result<&'static str, String> {
    target_for_platform(env::consts::OS, env::consts::ARCH)
}

fn target_for_platform(os: &str, arch: &str) -> Result<&'static str, String> {
    match (os, arch) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-musl"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-musl"),
        ("windows", "aarch64") => Ok("aarch64-pc-windows-msvc"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        (os, arch) => Err(format!("unsupported update target {os}/{arch}")),
    }
}

fn current_version() -> Result<Version, String> {
    Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|_| "current herdr-mcp version is not valid semver".to_owned())
}

fn valid_asset_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && !name.contains("..")
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn valid_job_id(value: &str) -> bool {
    (8..=96).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn binary_is_confined(paths: &RuntimePaths, binary: &Path, job_id: &str) -> bool {
    binary
        .parent()
        .is_some_and(|parent| parent == paths.config_dir.join("update").join("jobs").join(job_id))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn ensure_real_dir(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("update state path is not a real directory".to_owned());
        }
    } else {
        fs::create_dir_all(path)
            .map_err(|error| format!("cannot create update state directory: {error}"))?;
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure update state directory: {error}"))?;
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn cleanup_staging(binary: &Path) {
    let parent = binary.parent().map(Path::to_path_buf);
    let _ = fs::remove_file(binary);
    if let Some(parent) = parent {
        let _ = fs::remove_dir(parent);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn process_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn now_ms_i64() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn public_job_view(job: &UpdateJobRecord) -> Value {
    json!({
        "job_id": job.job_id,
        "version": job.version,
        "target": job.target,
        "asset": job.asset_name,
        "sha256": job.sha256,
        "state": job.state,
        "detail": job.detail,
        "worker_pid": job.worker_pid,
        "created_at": job.created_at,
        "updated_at": job.updated_at,
    })
}

fn print_json(value: &Value) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|error| format!("cannot encode updater result: {error}"))?
    );
    Ok(())
}

fn error_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_request() {
        "request"
    } else {
        "transport"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn exec_busy_retry_retries_text_file_busy_until_the_probe_runs() {
        let mut calls = 0;
        let result = retry_text_file_busy(10, Duration::ZERO, || {
            calls += 1;
            if calls < 3 {
                Err(std::io::Error::from_raw_os_error(libc::ETXTBSY))
            } else {
                Ok("probed")
            }
        });
        assert_eq!(result.unwrap(), "probed");
        assert_eq!(calls, 3, "each busy attempt must be retried");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn exec_busy_retry_does_not_retry_unrelated_errors() {
        let mut calls = 0;
        let error = retry_text_file_busy(10, Duration::ZERO, || -> std::io::Result<()> {
            calls += 1;
            Err(std::io::Error::from_raw_os_error(libc::ENOENT))
        })
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
        assert_eq!(calls, 1, "only ETXTBSY may be retried");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn exec_busy_retry_is_bounded_for_a_persistently_busy_binary() {
        let mut calls = 0;
        let error = retry_text_file_busy(10, Duration::ZERO, || -> std::io::Result<()> {
            calls += 1;
            Err(std::io::Error::from_raw_os_error(libc::ETXTBSY))
        })
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ETXTBSY));
        assert_eq!(
            calls, 10,
            "a stuck executable must fail after bounded retries"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn candidate_probe_verifies_the_manifest_target_epoch_hash_count_and_schema() {
        let local = crate::contract::identity().unwrap();
        let target = crate::contract::ContractIdentity {
            epoch: local.epoch + 1,
            hash: format!("sha256:{}", "c".repeat(64)),
            tool_count: local.tool_count + 1,
        };
        let version = Version::parse("9.9.9").unwrap();
        let root = env::temp_dir().join(format!(
            "herdr-candidate-probe-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        let binary = root.join("candidate");
        let output = format!(
            "herdr-mcp {version}\ncontract epoch {} / {} tools\nstate schema {SCHEMA_VERSION}\ncontract hash {}\n",
            target.epoch, target.tool_count, target.hash
        );
        write_test_executable(&binary, &format!("#!/bin/sh\nprintf '%s' '{output}'\n"));
        assert!(probe_candidate_binary(&binary, &version, &target, SCHEMA_VERSION).is_ok());
        assert!(probe_candidate_binary(&binary, &version, &target, MAJOR_SOURCE_SCHEMA).is_err());
        assert!(probe_candidate_binary(&binary, &version, &local, SCHEMA_VERSION).is_err());
        for bad in [
            output.replace(&target.hash, &local.hash),
            output.replace(
                &format!("epoch {}", target.epoch),
                &format!("epoch {}", local.epoch),
            ),
            output.replace(
                &format!("{} tools", target.tool_count),
                &format!("{} tools", local.tool_count),
            ),
            output.replace(
                &format!("state schema {SCHEMA_VERSION}"),
                "state schema 999",
            ),
            output.replace(&format!("contract hash {}\n", target.hash), ""),
            output.replace("herdr-mcp 9.9.9", "herdr-mcp 9.9.8"),
        ] {
            write_test_executable(&binary, &format!("#!/bin/sh\nprintf '%s' '{bad}'\n"));
            assert!(probe_candidate_binary(&binary, &version, &target, SCHEMA_VERSION).is_err());
        }
        let legacy = format!(
            "herdr-mcp {version}\ncontract epoch {} / {} tools\nstate schema {SCHEMA_VERSION}\n",
            local.epoch, local.tool_count
        );
        write_test_executable(&binary, &format!("#!/bin/sh\nprintf '%s' '{legacy}'\n"));
        assert!(probe_candidate_binary(&binary, &version, &local, SCHEMA_VERSION).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn test_runtime_paths(root: &Path) -> RuntimePaths {
        RuntimePaths {
            instance: crate::instance::InstanceId::parse(&format!(
                "major-test-{}",
                std::process::id()
            ))
            .unwrap(),
            config_dir: root.to_path_buf(),
            config_file: root.join("config.json"),
            dev_state_dir: root.join("dev-state"),
            herdr_socket: Some(root.join("herdr.sock")),
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn write_test_executable(path: &Path, body: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let staged = path.with_file_name(format!(".{name}.staged"));
        let mut file = fs::File::create(&staged).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        file.sync_all().unwrap();
        // Publish a closed inode before the fixture is executed: Linux rejects an
        // executable with ETXTBSY while any writer still owns the inode.
        drop(file);
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o700)).unwrap();
        fs::rename(&staged, path).unwrap();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn write_test_state(path: &Path, schema: i64, sentinel: &str) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE sentinel (value TEXT NOT NULL);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)",
            [schema.to_string()],
        )
        .unwrap();
        conn.execute("INSERT INTO sentinel(value) VALUES (?1)", [sentinel])
            .unwrap();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn ordinary_update_job(job_id: &str) -> UpdateJobRecord {
        let now = now_ms_i64();
        UpdateJobRecord {
            job_id: job_id.to_owned(),
            version: "9.9.9".to_owned(),
            target: current_target().unwrap().to_owned(),
            asset_name: "candidate".to_owned(),
            sha256: "a".repeat(64),
            binary_path: "/tmp/candidate".to_owned(),
            state_schema_source: SCHEMA_VERSION,
            state_schema_target: SCHEMA_VERSION,
            state: "installing".to_owned(),
            detail: None,
            worker_pid: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn legacy_config_test_root(label: &str) -> PathBuf {
        let root = env::temp_dir().join(format!(
            "herdr-legacy-config-{label}-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn major_recovery_restores_legacy_toml_renamed_by_earlier_1x() {
        let root = legacy_config_test_root("restore");
        let text = "[edge]\npublic_origin = \"https://herdr-edge-mac.example.workers.dev\"\ndevice_id = \"dev_01ARZ3NDEKTSV4RRFFQ69G5FAV\"\n";
        fs::write(root.join("config.toml.migrated"), text).unwrap();

        let restored = restore_legacy_toml_config(&root).unwrap();

        assert_eq!(restored, Some(root.join("config.toml")));
        assert_eq!(fs::read_to_string(root.join("config.toml")).unwrap(), text);
        assert_eq!(
            fs::metadata(root.join("config.toml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        // The preserved copy stays as evidence; no temp file is left behind.
        assert!(root.join("config.toml.migrated").is_file());
        let leftovers: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn major_failure_detail_survives_in_a_fixed_private_file() {
        let root = legacy_config_test_root("failure-log");
        let first = record_major_failure(&root, "first failure").unwrap();
        let second = record_major_failure(&root, "Link sidecar refused the Edge").unwrap();

        assert_eq!(first, root.join(MAJOR_FAILURE_LOG_NAME));
        assert_eq!(first, second);
        let text = fs::read_to_string(&second).unwrap();
        // Only the latest failure is kept, with the full, untruncated reason.
        assert!(text.contains("update major-apply failed"));
        assert!(text.contains("Link sidecar refused the Edge"));
        assert!(!text.contains("first failure"));
        assert_eq!(
            fs::metadata(&second).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn major_recovery_never_overwrites_existing_legacy_toml() {
        let root = legacy_config_test_root("keep");
        fs::write(root.join("config.toml"), "current\n").unwrap();
        fs::write(root.join("config.toml.migrated"), "older\n").unwrap();

        assert_eq!(restore_legacy_toml_config(&root).unwrap(), None);
        assert_eq!(
            fs::read_to_string(root.join("config.toml")).unwrap(),
            "current\n"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn major_recovery_legacy_toml_restore_is_a_noop_without_evidence() {
        let root = legacy_config_test_root("none");
        assert_eq!(restore_legacy_toml_config(&root).unwrap(), None);
        assert!(!root.join("config.toml").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn major_recovery_refuses_symlinked_legacy_toml_backup() {
        let root = legacy_config_test_root("symlink");
        let outside = root.join("outside.toml");
        fs::write(&outside, "[edge]\n").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("config.toml.migrated")).unwrap();

        assert!(restore_legacy_toml_config(&root).is_err());
        assert!(!root.join("config.toml").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn major_upgrade_backup_restores_schema5_state_exactly() {
        let root = env::temp_dir().join(format!(
            "herdr-major-upgrade-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        fs::create_dir_all(&root).unwrap();
        let state = root.join("state.db");
        let backup = root.join("backup.db");
        let conn = Connection::open(&state).unwrap();
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta(key, value) VALUES ('schema_version', '5');
             CREATE TABLE sentinel (value TEXT NOT NULL);
             INSERT INTO sentinel(value) VALUES ('before-major-upgrade');",
        )
        .unwrap();
        drop(conn);
        snapshot_state_database(&state, &backup).unwrap();
        let backup_sha256 = sha256_file(&backup, MAJOR_BACKUP_MAX_BYTES).unwrap();

        let conn = Connection::open(&state).unwrap();
        conn.execute(
            "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
            [SCHEMA_VERSION.to_string()],
        )
        .unwrap();
        conn.execute("UPDATE sentinel SET value = 'after-major-upgrade'", [])
            .unwrap();
        drop(conn);

        let record = MajorUpgradeRecord {
            record_version: MAJOR_UPGRADE_RECORD_VERSION,
            job_id: None,
            source_state_schema: MAJOR_SOURCE_SCHEMA,
            target_state_schema: SCHEMA_VERSION,
            source_binary: "/unused/source".to_owned(),
            source_binary_sha256: "0".repeat(64),
            state_backup: backup.to_string_lossy().into_owned(),
            state_backup_sha256: backup_sha256,
            created_at_ms: now_ms_i64(),
        };
        restore_state_backup(&record, &state).unwrap();
        assert_eq!(
            read_raw_state_schema(&state).unwrap(),
            Some(MAJOR_SOURCE_SCHEMA)
        );
        let conn = Connection::open(&state).unwrap();
        let value: String = conn
            .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "before-major-upgrade");
        drop(conn);
        fs::remove_dir_all(root).ok();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn major_upgrade_record_keeps_private_executable_source_binary() {
        let root = env::temp_dir().join(format!(
            "herdr-major-source-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        fs::create_dir_all(&root).unwrap();
        let paths = test_runtime_paths(&root);
        let active = root.join("runtime/current/herdr-mcp");
        write_test_executable(
            &active,
            "#!/bin/sh\nif [ \"$1 $2\" = \"service stop\" ]; then\n/usr/bin/python3 -c \"import os,sqlite3; c=sqlite3.connect(os.environ['HERDR_MCP_CONFIG_DIR']+'/state.db'); c.execute(\\\"UPDATE sentinel SET value='committed-before-stop'\\\"); c.commit()\"\nfi\n",
        );
        let active_sha256 = sha256_file(&active, BINARY_MAX_BYTES).unwrap();
        let state = root.join("state.db");
        write_test_state(&state, MAJOR_SOURCE_SCHEMA, "before-major-upgrade");

        let record = prepare_major_upgrade_record(&paths, &state).unwrap();
        let rollback_binary = major_upgrade_source_binary_path(&paths);
        assert_eq!(Path::new(&record.source_binary), rollback_binary);
        assert!(rollback_binary.is_file());
        assert_ne!(
            fs::metadata(&rollback_binary).unwrap().permissions().mode() & 0o111,
            0
        );
        assert_eq!(record.source_binary_sha256, active_sha256);
        assert_eq!(
            sha256_file(&rollback_binary, BINARY_MAX_BYTES).unwrap(),
            active_sha256
        );
        validate_major_upgrade_record(&paths, &record).unwrap();
        let backup = Connection::open(&record.state_backup).unwrap();
        let value: String = backup
            .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "committed-before-stop");
        drop(backup);
        let live = Connection::open(&state).unwrap();
        live.execute("UPDATE sentinel SET value='later-write'", [])
            .unwrap();
        assert!(prepare_major_upgrade_record(&paths, &state).is_err());
        let value: String = live
            .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            value, "later-write",
            "a retry must not stop or reuse old material"
        );
        drop(live);
        fs::remove_dir_all(root).ok();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn migration_snapshot_failure_restarts_service_and_removes_rollback_material() {
        let root = env::temp_dir().join(format!(
            "herdr-state-migration-cleanup-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        fs::create_dir_all(&root).unwrap();
        let paths = test_runtime_paths(&root);
        let dir = major_upgrade_dir(&paths);
        fs::create_dir_all(&dir).unwrap();
        let source_binary = major_upgrade_source_binary_path(&paths);
        write_test_executable(
            &source_binary,
            "#!/bin/sh\nif [ \"$1 $2\" = \"service start\" ]; then : > \"$HERDR_MCP_CONFIG_DIR/restarted\"; fi\nexit 0\n",
        );
        let state = root.join("state.db");
        write_test_state(&state, 15, "before-migration");
        let backup = schema_upgrade_backup_path(&paths, 15, 16);
        write_test_state(&backup, 15, "before-migration");
        let record = MajorUpgradeRecord {
            record_version: MAJOR_UPGRADE_RECORD_VERSION,
            job_id: None,
            source_state_schema: 15,
            target_state_schema: 16,
            source_binary: source_binary.to_string_lossy().into_owned(),
            source_binary_sha256: sha256_file(&source_binary, BINARY_MAX_BYTES).unwrap(),
            state_backup: backup.to_string_lossy().into_owned(),
            state_backup_sha256: sha256_file(&backup, MAJOR_BACKUP_MAX_BYTES).unwrap(),
            created_at_ms: now_ms_i64(),
        };
        write_major_upgrade_record(&paths, &record).unwrap();

        discard_state_schema_migration_record(&paths, &record).unwrap();

        assert!(root.join("restarted").is_file());
        assert!(!major_upgrade_record_path(&paths).exists());
        assert!(!backup.exists());
        assert!(!source_binary.exists());
        assert!(!dir.exists());
        fs::remove_dir_all(root).ok();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn rollback_restores_schema_fifteen_before_starting_the_old_binary() {
        let root = env::temp_dir().join(format!(
            "herdr-state-migration-rollback-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        fs::create_dir_all(&root).unwrap();
        let paths = test_runtime_paths(&root);
        let active = root.join("runtime/current/herdr-mcp");
        write_test_executable(&active, "#!/bin/sh\nexit 0\n");
        let dir = major_upgrade_dir(&paths);
        fs::create_dir_all(&dir).unwrap();
        let source_binary = major_upgrade_source_binary_path(&paths);
        let source_script = concat!(
            "#!/bin/sh\n",
            "if [ \"$1 $2\" = \"service install\" ]; then\n",
            "/usr/bin/python3 - <<'PY'\n",
            "import os, sqlite3\n",
            "root = os.environ['HERDR_MCP_CONFIG_DIR']\n",
            "conn = sqlite3.connect(os.path.join(root, 'state.db'))\n",
            "schema = conn.execute(\n",
            "    \"SELECT value FROM meta WHERE key='schema_version'\"\n",
            ").fetchone()[0]\n",
            "open(os.path.join(root, 'old-binary-schema'), 'w').write(schema)\n",
            "PY\n",
            "fi\n",
            "exit 0\n",
        );
        write_test_executable(&source_binary, source_script);
        let state = root.join("state.db");
        write_test_state(&state, 16, "after-migration");
        let backup = schema_upgrade_backup_path(&paths, 15, 16);
        write_test_state(&backup, 15, "before-migration");
        let record = MajorUpgradeRecord {
            record_version: MAJOR_UPGRADE_RECORD_VERSION,
            job_id: None,
            source_state_schema: 15,
            target_state_schema: 16,
            source_binary: source_binary.to_string_lossy().into_owned(),
            source_binary_sha256: sha256_file(&source_binary, BINARY_MAX_BYTES).unwrap(),
            state_backup: backup.to_string_lossy().into_owned(),
            state_backup_sha256: sha256_file(&backup, MAJOR_BACKUP_MAX_BYTES).unwrap(),
            created_at_ms: now_ms_i64(),
        };
        write_major_upgrade_record(&paths, &record).unwrap();

        recover_major_upgrade(&paths, &record).unwrap();

        assert_eq!(read_raw_state_schema(&state).unwrap(), Some(15));
        assert_eq!(
            fs::read_to_string(root.join("old-binary-schema")).unwrap(),
            "15"
        );
        assert!(!dir.exists());
        fs::remove_dir_all(root).ok();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn install_failure_after_migration_restores_backup_before_old_binary_and_clears_material() {
        let root = env::temp_dir().join(format!(
            "herdr-state-migration-install-failure-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        fs::create_dir_all(&root).unwrap();
        let paths = test_runtime_paths(&root);
        let active = root.join("runtime/current/herdr-mcp");
        write_test_executable(&active, "#!/bin/sh\nexit 0\n");
        let dir = major_upgrade_dir(&paths);
        fs::create_dir_all(&dir).unwrap();
        let source_binary = major_upgrade_source_binary_path(&paths);
        let source_script = concat!(
            "#!/bin/sh\n",
            "if [ \"$1 $2\" = \"service install\" ]; then\n",
            "/usr/bin/python3 - <<'PY'\n",
            "import os, sqlite3\n",
            "root = os.environ['HERDR_MCP_CONFIG_DIR']\n",
            "conn = sqlite3.connect(os.path.join(root, 'state.db'))\n",
            "schema = conn.execute(\"SELECT value FROM meta WHERE key='schema_version'\").fetchone()[0]\n",
            "open(os.path.join(root, 'old-binary-schema'), 'w').write(schema)\n",
            "PY\n",
            "fi\n",
            "exit 0\n",
        );
        write_test_executable(&source_binary, source_script);
        let state = root.join("state.db");
        write_test_state(&state, SCHEMA_VERSION, "after-migration");
        let backup = schema_upgrade_backup_path(&paths, 15, 16);
        write_test_state(&backup, 15, "before-migration");
        let record = MajorUpgradeRecord {
            record_version: MAJOR_UPGRADE_RECORD_VERSION,
            job_id: Some("upd-migrating-12345678".to_owned()),
            source_state_schema: 15,
            target_state_schema: 16,
            source_binary: source_binary.to_string_lossy().into_owned(),
            source_binary_sha256: sha256_file(&source_binary, BINARY_MAX_BYTES).unwrap(),
            state_backup: backup.to_string_lossy().into_owned(),
            state_backup_sha256: sha256_file(&backup, MAJOR_BACKUP_MAX_BYTES).unwrap(),
            created_at_ms: now_ms_i64(),
        };
        write_major_upgrade_record(&paths, &record).unwrap();

        let job = UpdateJobRecord {
            job_id: "upd-migrating-12345678".to_owned(),
            version: "9.9.9".to_owned(),
            target: current_target().unwrap().to_owned(),
            asset_name: "candidate".to_owned(),
            sha256: "a".repeat(64),
            binary_path: "/tmp/candidate".to_owned(),
            state_schema_source: 15,
            state_schema_target: 16,
            state: "installing".to_owned(),
            detail: None,
            worker_pid: None,
            created_at: now_ms_i64(),
            updated_at: now_ms_i64(),
        };
        recover_failed_state_schema_migration(&paths, &job).unwrap();

        assert_eq!(read_raw_state_schema(&state).unwrap(), Some(15));
        assert_eq!(
            fs::read_to_string(root.join("old-binary-schema")).unwrap(),
            "15"
        );
        assert!(!major_upgrade_record_path(&paths).exists());
        assert!(!backup.exists());
        assert!(!source_binary.exists());
        assert!(!dir.exists());
        fs::remove_dir_all(root).ok();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn failed_ordinary_update_does_not_consume_leftover_major_upgrade_record() {
        let root = env::temp_dir().join(format!(
            "herdr-state-migration-leftover-major-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        fs::create_dir_all(&root).unwrap();
        let paths = test_runtime_paths(&root);
        let dir = major_upgrade_dir(&paths);
        fs::create_dir_all(&dir).unwrap();
        let state = root.join("state.db");
        write_test_state(&state, SCHEMA_VERSION, "ordinary-state");
        let backup = schema_upgrade_backup_path(&paths, 5, SCHEMA_VERSION);
        write_test_state(&backup, 5, "major-backup");
        let record = MajorUpgradeRecord {
            record_version: MAJOR_UPGRADE_RECORD_VERSION,
            job_id: None,
            source_state_schema: 5,
            target_state_schema: SCHEMA_VERSION,
            source_binary: dir.join("source-herdr-mcp").to_string_lossy().into_owned(),
            source_binary_sha256: "0".repeat(64),
            state_backup: backup.to_string_lossy().into_owned(),
            state_backup_sha256: sha256_file(&backup, MAJOR_BACKUP_MAX_BYTES).unwrap(),
            created_at_ms: now_ms_i64(),
        };
        write_major_upgrade_record(&paths, &record).unwrap();

        recover_failed_state_schema_migration(&paths, &ordinary_update_job("upd-ordinary-major"))
            .unwrap();

        assert_eq!(read_raw_state_schema(&state).unwrap(), Some(SCHEMA_VERSION));
        assert!(major_upgrade_record_path(&paths).exists());
        assert!(backup.exists());
        fs::remove_dir_all(root).ok();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn failed_later_ordinary_update_does_not_consume_leftover_migration_record() {
        let root = env::temp_dir().join(format!(
            "herdr-state-migration-leftover-forward-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        fs::create_dir_all(&root).unwrap();
        let paths = test_runtime_paths(&root);
        let dir = major_upgrade_dir(&paths);
        fs::create_dir_all(&dir).unwrap();
        let state = root.join("state.db");
        write_test_state(&state, SCHEMA_VERSION, "migrated-state");
        let backup = schema_upgrade_backup_path(&paths, 15, SCHEMA_VERSION);
        write_test_state(&backup, 15, "migration-backup");
        let record = MajorUpgradeRecord {
            record_version: MAJOR_UPGRADE_RECORD_VERSION,
            job_id: Some("upd-earlier-migration".to_owned()),
            source_state_schema: 15,
            target_state_schema: SCHEMA_VERSION,
            source_binary: dir.join("source-herdr-mcp").to_string_lossy().into_owned(),
            source_binary_sha256: "0".repeat(64),
            state_backup: backup.to_string_lossy().into_owned(),
            state_backup_sha256: sha256_file(&backup, MAJOR_BACKUP_MAX_BYTES).unwrap(),
            created_at_ms: now_ms_i64(),
        };
        write_major_upgrade_record(&paths, &record).unwrap();

        recover_failed_state_schema_migration(&paths, &ordinary_update_job("upd-later-ordinary"))
            .unwrap();

        assert_eq!(read_raw_state_schema(&state).unwrap(), Some(SCHEMA_VERSION));
        assert!(major_upgrade_record_path(&paths).exists());
        assert!(backup.exists());
        fs::remove_dir_all(root).ok();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn migration_rollback_record_validation_accepts_only_schema_fifteen_to_sixteen() {
        let root = env::temp_dir().join(format!(
            "herdr-state-migration-record-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        fs::create_dir_all(&root).unwrap();
        let paths = test_runtime_paths(&root);
        let valid = |source_state_schema, target_state_schema| MajorUpgradeRecord {
            record_version: MAJOR_UPGRADE_RECORD_VERSION,
            job_id: None,
            source_state_schema,
            target_state_schema,
            source_binary: major_upgrade_source_binary_path(&paths)
                .to_string_lossy()
                .into_owned(),
            source_binary_sha256: "0".repeat(64),
            state_backup: schema_upgrade_backup_path(
                &paths,
                source_state_schema,
                target_state_schema,
            )
            .to_string_lossy()
            .into_owned(),
            state_backup_sha256: "0".repeat(64),
            created_at_ms: now_ms_i64(),
        };
        validate_major_upgrade_record(&paths, &valid(15, 16)).unwrap();
        assert!(validate_major_upgrade_record(&paths, &valid(14, 16)).is_err());
        assert!(validate_major_upgrade_record(&paths, &valid(15, 17)).is_err());
        fs::remove_dir_all(root).ok();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn major_rollback_stop_failure_preserves_live_current_schema_state() {
        let root = env::temp_dir().join(format!(
            "herdr-major-stop-failure-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        fs::create_dir_all(&root).unwrap();
        let paths = test_runtime_paths(&root);
        let active = root.join("runtime/current/herdr-mcp");
        write_test_executable(&active, "#!/bin/sh\necho stop-failed >&2\nexit 23\n");

        let dir = major_upgrade_dir(&paths);
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let source_binary = major_upgrade_source_binary_path(&paths);
        write_test_executable(&source_binary, "#!/bin/sh\nexit 0\n");
        let state = root.join("state.db");
        write_test_state(&state, SCHEMA_VERSION, "after-major-upgrade");
        let backup = major_upgrade_backup_path(&paths);
        write_test_state(&backup, MAJOR_SOURCE_SCHEMA, "before-major-upgrade");
        let record = MajorUpgradeRecord {
            record_version: MAJOR_UPGRADE_RECORD_VERSION,
            job_id: None,
            source_state_schema: MAJOR_SOURCE_SCHEMA,
            target_state_schema: SCHEMA_VERSION,
            source_binary: source_binary.to_string_lossy().into_owned(),
            source_binary_sha256: sha256_file(&source_binary, BINARY_MAX_BYTES).unwrap(),
            state_backup: backup.to_string_lossy().into_owned(),
            state_backup_sha256: sha256_file(&backup, MAJOR_BACKUP_MAX_BYTES).unwrap(),
            created_at_ms: now_ms_i64(),
        };

        let error = recover_major_upgrade(&paths, &record).unwrap_err();
        assert!(error.contains("cannot stop service before major state restore"));
        assert_eq!(read_raw_state_schema(&state).unwrap(), Some(SCHEMA_VERSION));
        let conn = Connection::open(&state).unwrap();
        let value: String = conn
            .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "after-major-upgrade");
        drop(conn);
        fs::remove_dir_all(root).ok();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn major_rollback_accepts_source_install_error_only_after_restored_runtime_is_proven() {
        let root = env::temp_dir().join(format!(
            "herdr-major-source-install-postcondition-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        fs::create_dir_all(&root).unwrap();
        let paths = test_runtime_paths(&root);
        let active = root.join("runtime/current/herdr-mcp");
        write_test_executable(&active, "#!/bin/sh\nexit 0\n");

        let dir = major_upgrade_dir(&paths);
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let source_binary = major_upgrade_source_binary_path(&paths);
        write_test_executable(
            &source_binary,
            "#!/bin/sh\nset -e\ncase \"$1 $2\" in\n  \"service install\")\n    cp \"$0\" \"$HERDR_MCP_CONFIG_DIR/runtime/current/herdr-mcp\"\n    chmod 700 \"$HERDR_MCP_CONFIG_DIR/runtime/current/herdr-mcp\"\n    echo 'production Link generation reconcile failed: simulated contract mismatch' >&2\n    exit 17\n    ;;\n  \"service status\") exit 0 ;;\n  *) exit 0 ;;\nesac\n",
        );
        let state = root.join("state.db");
        write_test_state(&state, SCHEMA_VERSION, "after-major-upgrade");
        let backup = major_upgrade_backup_path(&paths);
        write_test_state(&backup, MAJOR_SOURCE_SCHEMA, "before-major-upgrade");
        let record = MajorUpgradeRecord {
            record_version: MAJOR_UPGRADE_RECORD_VERSION,
            job_id: None,
            source_state_schema: MAJOR_SOURCE_SCHEMA,
            target_state_schema: SCHEMA_VERSION,
            source_binary: source_binary.to_string_lossy().into_owned(),
            source_binary_sha256: sha256_file(&source_binary, BINARY_MAX_BYTES).unwrap(),
            state_backup: backup.to_string_lossy().into_owned(),
            state_backup_sha256: sha256_file(&backup, MAJOR_BACKUP_MAX_BYTES).unwrap(),
            created_at_ms: now_ms_i64(),
        };
        write_major_upgrade_record(&paths, &record).unwrap();

        recover_major_upgrade(&paths, &record).unwrap();
        assert_eq!(
            sha256_file(&active, BINARY_MAX_BYTES).unwrap(),
            record.source_binary_sha256
        );
        assert_eq!(
            read_raw_state_schema(&state).unwrap(),
            Some(MAJOR_SOURCE_SCHEMA)
        );
        let conn = Connection::open(&state).unwrap();
        let value: String = conn
            .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "before-major-upgrade");
        drop(conn);
        assert!(
            !dir.exists(),
            "successful postcondition recovery retires rollback material"
        );
        fs::remove_dir_all(root).ok();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn major_rollback_keeps_failure_when_source_install_did_not_restore_recorded_runtime() {
        let root = env::temp_dir().join(format!(
            "herdr-major-source-install-unrestored-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        fs::create_dir_all(&root).unwrap();
        let paths = test_runtime_paths(&root);
        let active = root.join("runtime/current/herdr-mcp");
        write_test_executable(&active, "#!/bin/sh\nexit 0\n");

        let dir = major_upgrade_dir(&paths);
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let source_binary = major_upgrade_source_binary_path(&paths);
        write_test_executable(
            &source_binary,
            "#!/bin/sh\nif [ \"$1 $2\" = \"service install\" ]; then echo 'production Link generation reconcile failed: simulated contract mismatch' >&2; exit 17; fi\nexit 0\n",
        );
        let state = root.join("state.db");
        write_test_state(&state, SCHEMA_VERSION, "after-major-upgrade");
        let backup = major_upgrade_backup_path(&paths);
        write_test_state(&backup, MAJOR_SOURCE_SCHEMA, "before-major-upgrade");
        let record = MajorUpgradeRecord {
            record_version: MAJOR_UPGRADE_RECORD_VERSION,
            job_id: None,
            source_state_schema: MAJOR_SOURCE_SCHEMA,
            target_state_schema: SCHEMA_VERSION,
            source_binary: source_binary.to_string_lossy().into_owned(),
            source_binary_sha256: sha256_file(&source_binary, BINARY_MAX_BYTES).unwrap(),
            state_backup: backup.to_string_lossy().into_owned(),
            state_backup_sha256: sha256_file(&backup, MAJOR_BACKUP_MAX_BYTES).unwrap(),
            created_at_ms: now_ms_i64(),
        };
        write_major_upgrade_record(&paths, &record).unwrap();

        let error = recover_major_upgrade(&paths, &record).unwrap_err();
        assert!(error.contains("without restoring the recorded source runtime"));
        assert!(dir.exists(), "failed recovery must keep rollback material");
        assert_ne!(
            sha256_file(&active, BINARY_MAX_BYTES).unwrap(),
            record.source_binary_sha256
        );
        fs::remove_dir_all(root).ok();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn major_rollback_rejects_non_link_install_error_even_if_source_runtime_was_committed() {
        let root = env::temp_dir().join(format!(
            "herdr-major-source-install-wrong-class-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        fs::create_dir_all(&root).unwrap();
        let paths = test_runtime_paths(&root);
        let active = root.join("runtime/current/herdr-mcp");
        write_test_executable(&active, "#!/bin/sh\nexit 0\n");

        let dir = major_upgrade_dir(&paths);
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let source_binary = major_upgrade_source_binary_path(&paths);
        write_test_executable(
            &source_binary,
            "#!/bin/sh\nset -e\nif [ \"$1 $2\" = \"service install\" ]; then\n  cp \"$0\" \"$HERDR_MCP_CONFIG_DIR/runtime/current/herdr-mcp\"\n  chmod 700 \"$HERDR_MCP_CONFIG_DIR/runtime/current/herdr-mcp\"\n  echo 'service install committed but Herdr supervisor activation failed: simulated' >&2\n  exit 17\nfi\nexit 0\n",
        );
        let state = root.join("state.db");
        write_test_state(&state, SCHEMA_VERSION, "after-major-upgrade");
        let backup = major_upgrade_backup_path(&paths);
        write_test_state(&backup, MAJOR_SOURCE_SCHEMA, "before-major-upgrade");
        let record = MajorUpgradeRecord {
            record_version: MAJOR_UPGRADE_RECORD_VERSION,
            job_id: None,
            source_state_schema: MAJOR_SOURCE_SCHEMA,
            target_state_schema: SCHEMA_VERSION,
            source_binary: source_binary.to_string_lossy().into_owned(),
            source_binary_sha256: sha256_file(&source_binary, BINARY_MAX_BYTES).unwrap(),
            state_backup: backup.to_string_lossy().into_owned(),
            state_backup_sha256: sha256_file(&backup, MAJOR_BACKUP_MAX_BYTES).unwrap(),
            created_at_ms: now_ms_i64(),
        };
        write_major_upgrade_record(&paths, &record).unwrap();

        let error = recover_major_upgrade(&paths, &record).unwrap_err();
        assert!(error.contains("outside the recoverable production-Link reconciliation class"));
        assert!(
            dir.exists(),
            "unclassified failure must keep rollback material"
        );
        fs::remove_dir_all(root).ok();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn major_apply_stop_failure_preserves_schema5_without_stale_retry_material() {
        let _guard = crate::test_env::lock();
        let root = env::temp_dir().join(format!(
            "herdr-major-apply-stop-failure-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        fs::create_dir_all(&root).unwrap();
        let previous_config = env::var_os("HERDR_MCP_CONFIG_DIR");
        let previous_exec = env::var_os("HERDR_MCP_EXEC_ID");
        let previous_instance = env::var_os("HERDR_MCP_INSTANCE");
        unsafe {
            env::set_var("HERDR_MCP_CONFIG_DIR", &root);
            env::remove_var("HERDR_MCP_EXEC_ID");
            env::set_var(
                "HERDR_MCP_INSTANCE",
                format!("major-test-{}", std::process::id()),
            );
        }

        let active = root.join("runtime/current/herdr-mcp");
        write_test_executable(&active, "#!/bin/sh\necho stop-failed >&2\nexit 23\n");
        let state = root.join("state.db");
        write_test_state(&state, MAJOR_SOURCE_SCHEMA, "before-major-upgrade");

        let error = major_apply().unwrap_err();
        assert!(error.contains("cannot stop service before major state migration"));
        assert_eq!(
            read_raw_state_schema(&state).unwrap(),
            Some(MAJOR_SOURCE_SCHEMA)
        );
        let paths = test_runtime_paths(&root);
        assert!(!major_upgrade_record_path(&paths).exists());
        assert!(!major_upgrade_backup_path(&paths).exists());
        assert!(!major_upgrade_source_binary_path(&paths).exists());

        unsafe {
            match previous_config {
                Some(value) => env::set_var("HERDR_MCP_CONFIG_DIR", value),
                None => env::remove_var("HERDR_MCP_CONFIG_DIR"),
            }
            match previous_exec {
                Some(value) => env::set_var("HERDR_MCP_EXEC_ID", value),
                None => env::remove_var("HERDR_MCP_EXEC_ID"),
            }
            match previous_instance {
                Some(value) => env::set_var("HERDR_MCP_INSTANCE", value),
                None => env::remove_var("HERDR_MCP_INSTANCE"),
            }
        }
        fs::remove_dir_all(root).ok();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn major_rollback_accepts_source_and_partial_migration_states() {
        let _guard = crate::test_env::lock();
        let root = env::temp_dir().join(format!(
            "herdr-major-half-recovered-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        fs::create_dir_all(&root).unwrap();
        let previous_config = env::var_os("HERDR_MCP_CONFIG_DIR");
        let previous_exec = env::var_os("HERDR_MCP_EXEC_ID");
        let previous_instance = env::var_os("HERDR_MCP_INSTANCE");
        unsafe {
            env::set_var("HERDR_MCP_CONFIG_DIR", &root);
            env::remove_var("HERDR_MCP_EXEC_ID");
            env::set_var(
                "HERDR_MCP_INSTANCE",
                format!("major-test-{}", std::process::id()),
            );
        }

        for live_schema in [MAJOR_SOURCE_SCHEMA, MAJOR_SOURCE_SCHEMA + 1] {
            let paths = test_runtime_paths(&root);
            let active = root.join("runtime/current/herdr-mcp");
            write_test_executable(&active, "#!/bin/sh\nexit 0\n");
            let dir = major_upgrade_dir(&paths);
            fs::create_dir_all(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
            let source_binary = major_upgrade_source_binary_path(&paths);
            write_test_executable(&source_binary, "#!/bin/sh\nexit 0\n");
            let state = root.join("state.db");
            write_test_state(&state, live_schema, "half-recovered");
            let backup = major_upgrade_backup_path(&paths);
            write_test_state(&backup, MAJOR_SOURCE_SCHEMA, "before-major-upgrade");
            let record = MajorUpgradeRecord {
                record_version: MAJOR_UPGRADE_RECORD_VERSION,
                job_id: None,
                source_state_schema: MAJOR_SOURCE_SCHEMA,
                target_state_schema: SCHEMA_VERSION,
                source_binary: source_binary.to_string_lossy().into_owned(),
                source_binary_sha256: sha256_file(&source_binary, BINARY_MAX_BYTES).unwrap(),
                state_backup: backup.to_string_lossy().into_owned(),
                state_backup_sha256: sha256_file(&backup, MAJOR_BACKUP_MAX_BYTES).unwrap(),
                created_at_ms: now_ms_i64(),
            };
            write_major_upgrade_record(&paths, &record).unwrap();

            assert_eq!(major_rollback().unwrap(), ExitCode::SUCCESS);
            assert_eq!(
                read_raw_state_schema(&state).unwrap(),
                Some(MAJOR_SOURCE_SCHEMA)
            );
            let conn = Connection::open(&state).unwrap();
            let value: String = conn
                .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
                .unwrap();
            assert_eq!(value, "before-major-upgrade");
            drop(conn);
            assert!(!dir.exists());
            fs::remove_file(&state).unwrap();
        }

        unsafe {
            match previous_config {
                Some(value) => env::set_var("HERDR_MCP_CONFIG_DIR", value),
                None => env::remove_var("HERDR_MCP_CONFIG_DIR"),
            }
            match previous_exec {
                Some(value) => env::set_var("HERDR_MCP_EXEC_ID", value),
                None => env::remove_var("HERDR_MCP_EXEC_ID"),
            }
            match previous_instance {
                Some(value) => env::set_var("HERDR_MCP_INSTANCE", value),
                None => env::remove_var("HERDR_MCP_INSTANCE"),
            }
        }
        fs::remove_dir_all(root).ok();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn major_snapshot_failure_restarts_source_without_rollback_material() {
        let root = env::temp_dir().join(format!(
            "herdr-major-snapshot-failure-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        fs::create_dir_all(&root).unwrap();
        let paths = test_runtime_paths(&root);
        let active = root.join("runtime/current/herdr-mcp");
        write_test_executable(
            &active,
            "#!/bin/sh\nif [ \"$1 $2\" = \"service start\" ]; then\n: > \"$HERDR_MCP_CONFIG_DIR/restarted\"\nfi\n",
        );
        let state = root.join("state.db");
        write_test_state(&state, MAJOR_SOURCE_SCHEMA + 1, "untouched");
        let error = prepare_major_upgrade_record(&paths, &state).unwrap_err();
        assert!(error.contains("backup schema changed during snapshot"));
        assert!(error.contains("previous service restarted"));
        assert!(root.join("restarted").exists());
        assert_eq!(
            read_raw_state_schema(&state).unwrap(),
            Some(MAJOR_SOURCE_SCHEMA + 1)
        );
        assert!(!major_upgrade_record_path(&paths).exists());
        assert!(!major_upgrade_backup_path(&paths).exists());
        assert!(!major_upgrade_source_binary_path(&paths).exists());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn update_check_failure_is_explicitly_indeterminate() {
        let message = update_check_indeterminate_error("release manifest download failed: timeout");
        assert!(message.contains("could not determine whether an update is available"));
        assert!(message.contains("release manifest download failed: timeout"));
        assert!(message.contains("herdr-mcp update apply"));
    }

    #[test]
    fn metadata_fetch_retries_one_transient_timeout() {
        use std::io::{Read as _, Write as _};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut first, _) = listener.accept().unwrap();
            let delayed = thread::spawn(move || {
                let mut request = [0_u8; 1024];
                let _ = first.read(&mut request);
                thread::sleep(Duration::from_millis(100));
            });

            let (mut second, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = second.read(&mut request);
            second
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nretry-ok",
                )
                .unwrap();
            delayed.join().unwrap();
        });

        let client = Client::builder()
            .timeout(Duration::from_millis(30))
            .build()
            .unwrap();
        let url = Url::parse(&format!("http://{address}/manifest.json")).unwrap();
        let bytes = fetch_bounded_with_retry(
            &client,
            url,
            1024,
            "release manifest",
            2,
            Duration::from_millis(1),
        )
        .unwrap();
        assert_eq!(bytes, b"retry-ok");
        server.join().unwrap();
    }

    #[test]
    fn release_target_mapping_uses_portable_musl_for_linux() {
        assert_eq!(
            target_for_platform("linux", "aarch64").unwrap(),
            "aarch64-unknown-linux-musl"
        );
        assert_eq!(
            target_for_platform("linux", "x86_64").unwrap(),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            target_for_platform("macos", "aarch64").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            target_for_platform("windows", "x86_64").unwrap(),
            "x86_64-pc-windows-msvc"
        );
        assert_eq!(
            target_for_platform("windows", "aarch64").unwrap(),
            "aarch64-pc-windows-msvc"
        );
    }

    #[test]
    fn scheduled_auto_update_is_default_instance_stable_only() {
        assert_eq!(
            auto_update_policy(false, true, UpdateChannel::Stable, "prod"),
            AutoUpdatePolicy::RunStable
        );
        assert_eq!(
            auto_update_policy(true, true, UpdateChannel::Stable, "prod"),
            AutoUpdatePolicy::Skip("named_instance")
        );
        assert_eq!(
            auto_update_policy(false, true, UpdateChannel::Stable, "dev"),
            AutoUpdatePolicy::Skip("runtime_channel_not_prod")
        );
        assert_eq!(
            auto_update_policy(false, false, UpdateChannel::Stable, "prod"),
            AutoUpdatePolicy::Skip("update_check_disabled")
        );
        assert_eq!(
            auto_update_policy(false, true, UpdateChannel::Preview, "prod"),
            AutoUpdatePolicy::Skip("preview_channel_requires_manual_update")
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn manual_update_progress_reports_phases_download_and_installer_state() {
        let mut progress = UpdateProgress::new(Vec::<u8>::new(), true);
        progress.phase("Checking stable release metadata and provenance...");
        let total = 20 * 1024 * 1024;
        progress.download(0, total);
        progress.download(1024 * 1024, total);
        progress.download(1024 * 1024 + 1, total);
        progress.download(total, total);

        let now = now_ms_i64();
        let mut job = UpdateJobRecord {
            job_id: "upd-progress-12345678".to_owned(),
            version: "9.9.9".to_owned(),
            target: current_target().unwrap().to_owned(),
            asset_name: "candidate".to_owned(),
            sha256: "a".repeat(64),
            binary_path: "/tmp/candidate".to_owned(),
            state_schema_source: SCHEMA_VERSION,
            state_schema_target: SCHEMA_VERSION,
            state: "queued".to_owned(),
            detail: Some("verified candidate staged".to_owned()),
            worker_pid: Some(std::process::id()),
            created_at: now,
            updated_at: now,
        };
        progress.job(&job);
        progress.job(&job);
        job.state = "installing".to_owned();
        job.detail = Some("candidate verified; service install and health gate started".to_owned());
        progress.job(&job);
        job.state = "succeeded".to_owned();
        job.detail = Some("service install committed and health gate passed".to_owned());
        progress.job(&job);

        let output = String::from_utf8(progress.into_inner()).unwrap();
        assert!(output.contains("Checking stable release metadata"));
        assert!(output.contains("Download   0%"));
        assert!(output.contains("Download   5%"));
        assert_eq!(output.matches("Download   5%").count(), 1);
        assert!(output.contains("Download 100%"));
        assert_eq!(output.matches("Installer queued").count(), 1);
        assert!(output.contains("Installer installing"));
        assert!(output.contains("Installer succeeded"));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn automatic_update_progress_stays_silent() {
        let mut progress = UpdateProgress::new(Vec::<u8>::new(), false);
        progress.phase("hidden");
        progress.download(10, 100);
        assert!(progress.into_inner().is_empty());
    }

    #[test]
    fn update_urls_are_https_or_loopback_http_and_never_carry_credentials() {
        assert!(parse_update_url("https://github.com/a/b").is_ok());
        assert!(parse_update_url("http://127.0.0.1:9000/manifest.json").is_ok());
        assert!(parse_update_url("http://localhost:9000/manifest.json").is_ok());
        assert!(parse_update_url("http://example.com/manifest.json").is_err());
        assert!(parse_update_url("https://user:pass@example.com/manifest.json").is_err());
        assert!(parse_update_url("file:///tmp/manifest.json").is_err());
        assert!(update_redirect_allowed(
            None,
            &Url::parse("https://release-assets.githubusercontent.com/a/b").unwrap(),
            false
        ));
        assert!(update_redirect_allowed(
            None,
            &Url::parse("https://objects.githubusercontent.com/a/b").unwrap(),
            false
        ));
        assert!(update_redirect_allowed(
            None,
            &Url::parse("http://127.0.0.1:9000/artifact").unwrap(),
            false
        ));
        assert!(!update_redirect_allowed(
            None,
            &Url::parse("https://example.com/artifact").unwrap(),
            false
        ));
    }

    #[test]
    fn github_token_is_attached_only_to_the_exact_api_origin() {
        let client = Client::builder().build().unwrap();
        let request = |raw: &str| {
            authorized_get(&client, &Url::parse(raw).unwrap(), Some("ghp_test_token"))
                .build()
                .unwrap()
        };
        let api = request("https://api.github.com/repos/whshang/herdr-mcp/releases");
        let header = api.headers().get(reqwest::header::AUTHORIZATION).unwrap();
        assert_eq!(header.to_str().unwrap(), "Bearer ghp_test_token");
        assert!(header.is_sensitive());
        for anonymous in [
            "https://objects.githubusercontent.com/a/b",
            "https://release-assets.githubusercontent.com/a/b",
            "https://github.com/whshang/herdr-mcp/releases/download/v1/x",
            "https://api.github.com.example.test/releases",
            "https://sub.api.github.com/releases",
            "http://api.github.com/repos/whshang/herdr-mcp/releases",
            "https://API.github.com.example.test/releases",
            "https://[::1]/releases",
        ] {
            assert!(
                !request(anonymous)
                    .headers()
                    .get_all(reqwest::header::AUTHORIZATION)
                    .into_iter()
                    .any(|value| value
                        .to_str()
                        .is_ok_and(|value| value.contains("ghp_test_token"))),
                "github token leaked to {anonymous}"
            );
        }
        // A URL carrying credentials never receives the operator token either.
        assert!(
            !request("https://user:pass@api.github.com/releases")
                .headers()
                .get_all(reqwest::header::AUTHORIZATION)
                .into_iter()
                .any(|value| value
                    .to_str()
                    .is_ok_and(|value| value.contains("ghp_test_token")))
        );
        assert!(
            authorized_get(
                &client,
                &Url::parse("https://api.github.com/releases").unwrap(),
                None
            )
            .build()
            .unwrap()
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .is_none()
        );
        // The header is marked sensitive, so even Debug formatting a request
        // redacts it instead of printing the operator token.
        assert!(!format!("{api:?}").contains("ghp_test_token"));
    }

    #[test]
    fn github_api_redirect_never_carries_the_token_off_the_api_host() {
        let api = Url::parse("https://api.github.com/repos/a/b/releases").unwrap();
        let asset = Url::parse("https://objects.githubusercontent.com/a/b").unwrap();
        let home = Url::parse("https://github.com/a/b").unwrap();
        let loopback = Url::parse("http://127.0.0.1:9000/a/b").unwrap();
        // With a token configured, a redirect off api.github.com is refused even
        // though the target host is on the ordinary download/loopback allowlist.
        assert!(!update_redirect_allowed(Some(&api), &asset, true));
        assert!(!update_redirect_allowed(Some(&api), &home, true));
        assert!(!update_redirect_allowed(Some(&api), &loopback, true));
        assert!(update_redirect_allowed(Some(&api), &api, true));
        // Anonymous requests keep the previous redirect behavior.
        assert!(update_redirect_allowed(Some(&api), &asset, false));
        assert!(update_redirect_allowed(Some(&api), &loopback, false));
        assert!(update_redirect_allowed(Some(&home), &asset, true));
    }

    #[test]
    fn exhausted_github_rate_limit_maps_to_one_actionable_error() {
        let reset_unix = "1761742080";
        let message = github_rate_limit_error(403, Some("0"), Some(reset_unix), None, 0).unwrap();
        assert!(message.starts_with("github_rate_limited: "));
        assert!(message.contains("HTTP 403"));
        assert!(
            message.contains("resets at 2025-10-29T12:48:00Z"),
            "{message}"
        );
        assert!(message.contains("GH_TOKEN"));
        assert!(message.contains("GITHUB_TOKEN"));

        // 429 and retry-after are rate limits even without a zeroed budget.
        let limited = github_rate_limit_error(429, None, None, Some("60"), 1_000).unwrap();
        assert!(limited.contains("HTTP 429"));
        assert!(
            limited.contains("resets at 1970-01-01T00:17:40Z"),
            "{limited}"
        );
        assert!(github_rate_limit_error(403, Some("7"), None, Some("30"), 0).is_some());
        // A rate-limit signal without a reset still names the code and the hint.
        let bare = github_rate_limit_error(429, None, None, None, 0).unwrap();
        assert!(bare.starts_with("github_rate_limited: "));
        assert!(!bare.contains("resets at"));
        assert!(bare.contains("retry later"));

        // Every other 403 keeps the pre-existing generic behavior.
        for (status, remaining, retry_after) in [
            (403, Some("7"), None),
            (403, None, None),
            (401, Some("0"), None),
            (500, Some("0"), None),
            (404, None, Some("30")),
        ] {
            assert!(
                github_rate_limit_error(status, remaining, None, retry_after, 0).is_none(),
                "unexpected rate-limit mapping for {status}/{remaining:?}/{retry_after:?}"
            );
        }
    }

    #[test]
    fn rate_limit_error_does_not_echo_the_configured_token() {
        // The token reaches only the Authorization header. Errors are built from
        // the status and response headers, so no configured token can appear in
        // a message the user sees.
        let token = "ghp_unit_test_only_token_value";
        let client = Client::builder().build().unwrap();
        let request = authorized_get(
            &client,
            &Url::parse("https://api.github.com/releases").unwrap(),
            Some(token),
        )
        .build()
        .unwrap();
        assert!(
            request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .unwrap()
                .to_str()
                .unwrap()
                .contains(token)
        );
        let message = github_rate_limit_error(403, Some("0"), Some("1761742080"), None, 0).unwrap();
        assert!(!message.contains(token));
        // The token is also absent from the only derived message the CLI prints.
        let wrapped = update_check_indeterminate_error(&message);
        assert!(wrapped.contains(GITHUB_RATE_LIMITED_CODE));
        assert!(!wrapped.contains(token));
    }

    #[test]
    fn asset_and_job_identifiers_are_path_safe() {
        assert!(valid_asset_name("herdr-mcp-1.0.0-aarch64-apple-darwin"));
        assert!(!valid_asset_name("../herdr-mcp"));
        assert!(!valid_asset_name("dir/herdr-mcp"));
        assert!(valid_sha256(&"f".repeat(64)));
        assert!(!valid_sha256(&"F".repeat(64)));
        assert!(!valid_sha256("not-a-hash"));
        assert!(valid_job_id("upd-12345678-1234-abcdef12"));
        assert!(!valid_job_id("../bad"));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn active_update_recovery_blocks_live_worker_and_reaps_stale_queue() {
        let root = env::temp_dir().join(format!(
            "herdr-updater-active-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        let paths = RuntimePaths {
            instance: crate::instance::InstanceId::default_instance(),
            config_dir: root.clone(),
            config_file: root.join("config.json"),
            dev_state_dir: root.join("dev"),
            herdr_socket: None,
        };
        fs::create_dir_all(root.join("update/jobs/upd-live-12345678")).unwrap();
        let live_binary = root.join("update/jobs/upd-live-12345678/herdr-mcp-candidate");
        fs::write(&live_binary, b"live").unwrap();
        let store = UpdateStore::open(&paths).unwrap();
        store
            .create_update_job(&UpdateJobRecord {
                job_id: "upd-live-12345678".to_owned(),
                version: "9.9.9".to_owned(),
                target: current_target().unwrap().to_owned(),
                asset_name: "candidate".to_owned(),
                sha256: "a".repeat(64),
                binary_path: live_binary.to_string_lossy().into_owned(),
                state_schema_source: SCHEMA_VERSION,
                state_schema_target: SCHEMA_VERSION,
                state: "queued".to_owned(),
                detail: None,
                worker_pid: Some(std::process::id()),
                created_at: now_ms_i64(),
                updated_at: now_ms_i64(),
            })
            .unwrap();
        assert!(recover_or_reject_active_update(&store, &paths).is_err());
        assert!(live_binary.exists());

        store
            .update_update_job(
                "upd-live-12345678",
                "failed",
                Some("test transition"),
                None,
                now_ms_i64(),
            )
            .unwrap();
        fs::create_dir_all(root.join("update/jobs/upd-stale-12345678")).unwrap();
        let stale_binary = root.join("update/jobs/upd-stale-12345678/herdr-mcp-candidate");
        fs::write(&stale_binary, b"stale").unwrap();
        store
            .create_update_job(&UpdateJobRecord {
                job_id: "upd-stale-12345678".to_owned(),
                version: "9.9.10".to_owned(),
                target: current_target().unwrap().to_owned(),
                asset_name: "candidate".to_owned(),
                sha256: "b".repeat(64),
                binary_path: stale_binary.to_string_lossy().into_owned(),
                state_schema_source: SCHEMA_VERSION,
                state_schema_target: SCHEMA_VERSION,
                state: "queued".to_owned(),
                detail: None,
                worker_pid: None,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
        recover_or_reject_active_update(&store, &paths).unwrap();
        assert!(!stale_binary.exists());
        assert_eq!(
            store
                .update_job("upd-stale-12345678")
                .unwrap()
                .unwrap()
                .state,
            "failed"
        );
        let _ = fs::remove_dir_all(root);
    }

    /// Given a Runtime update that already committed
    /// When it reports a fleet Worker that is behind the installed release
    /// Then the Runtime result stays successful and Worker drift is a notice.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn runtime_update_reports_worker_drift_as_a_non_fatal_notice() {
        let behind = json!({
            "configured": true,
            "edge_origin": "https://edge.example.test",
            "worker": "herdr-edge",
            "edge_version": "1.0.0",
            "target_version": "9.9.9",
            "update_required": true,
        });
        let (payload, code) = runtime_update_succeeded_result(RuntimeUpdateOutcome {
            job_id: "upd-test".to_owned(),
            target_version: "9.9.9".to_owned(),
            target: "aarch64-apple-darwin".to_owned(),
            asset: "herdr-mcp".to_owned(),
            worker_pid: 42,
            worker_pid_persisted: true,
            job: json!({ "job_id": "upd-test", "state": "succeeded" }),
            migration: None,
            worker_update: behind,
            worker_chain: WorkerChain::RuntimeOnly,
            extension: ExtensionVersionReport::unknown("test"),
            extension_staging: None,
        });
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["code"], "update_succeeded");
        assert_eq!(payload["worker_update_required"], true);
        assert_eq!(payload["worker_update_chained"], false);
        assert_eq!(payload["next_action"], "herdr-mcp worker update");
        let notices = payload["notices"].as_array().unwrap();
        assert_eq!(
            notices
                .iter()
                .filter(|notice| notice["code"] == "worker_update_required")
                .count(),
            1
        );
        let notice = notices
            .iter()
            .find(|notice| notice["code"] == "worker_update_required")
            .unwrap();
        assert_eq!(notice["severity"], "notice");
        assert_eq!(notice["fatal"], false);
        assert_eq!(notice["next_action"], "herdr-mcp worker update");

        // A current (or unconfigured) Worker is never a notice.
        let current = json!({ "configured": true, "update_required": false });
        assert!(worker_update_notice(&current).is_none());
        assert!(
            worker_update_notice(&json!({ "configured": false, "update_required": false }))
                .is_none()
        );
        assert!(!worker_update_required(
            &json!({ "configured": true, "update_required": null })
        ));

        // A failed explicit `--with-worker` attempt is reported as a warning on
        // a Runtime update that still succeeded.
        let failed_worker = json!({ "ok": false, "code": "worker_update_failed" });
        let (payload, code) = runtime_update_succeeded_result(RuntimeUpdateOutcome {
            job_id: "upd-test".to_owned(),
            target_version: "9.9.9".to_owned(),
            target: "aarch64-apple-darwin".to_owned(),
            asset: "herdr-mcp".to_owned(),
            worker_pid: 42,
            worker_pid_persisted: true,
            job: json!({ "job_id": "upd-test", "state": "succeeded" }),
            migration: None,
            worker_update: failed_worker,
            worker_chain: WorkerChain::WithWorker,
            extension: ExtensionVersionReport::unknown("test"),
            extension_staging: None,
        });
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(payload["code"], "update_succeeded");
        assert_eq!(payload["worker_update_chained"], true);
        assert_eq!(payload["notices"][0]["code"], "worker_update_failed");
        assert_eq!(payload["notices"][0]["severity"], "warning");
        assert_eq!(payload["notices"][0]["fatal"], false);
        assert_eq!(
            payload["notices"][0]["next_action"],
            "herdr-mcp worker update"
        );
    }

    /// Given a Worker status that is behind
    /// When the update resolves the Worker outcome without `--with-worker`
    /// Then it only reports that status and never runs the Worker update.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn update_apply_never_runs_worker_update_without_opt_in() {
        let root = env::temp_dir().join(format!(
            "herdr-worker-notice-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        let paths = test_runtime_paths(&root);
        let mut worker_update_invocations = 0;
        let reported = resolve_worker_update_after_runtime_commit(
            &paths,
            "9.9.9",
            WorkerChain::RuntimeOnly,
            || {
                worker_update_invocations += 1;
                json!({ "ok": true, "code": "worker_update_succeeded" })
            },
        );
        assert_eq!(
            worker_update_invocations, 0,
            "runtime-only update apply must never run worker update"
        );
        assert_eq!(reported["configured"], false);
        assert_eq!(reported["update_required"], false);

        // The same seam does chain the Worker update when it is opted into.
        let chained = resolve_worker_update_after_runtime_commit(
            &paths,
            "9.9.9",
            WorkerChain::WithWorker,
            || {
                worker_update_invocations += 1;
                json!({ "ok": true, "code": "worker_update_succeeded" })
            },
        );
        assert_eq!(worker_update_invocations, 1);
        assert_eq!(chained["code"], "worker_update_succeeded");
    }

    /// user answers at most one prompt | Given a committed Runtime update whose
    /// installed broker predates the alias subcommands | When the post-commit TCC
    /// steps run | Then the broker is migrated BEFORE the single preflight, and
    /// both outcomes are attached to the result.
    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn post_commit_tcc_migrates_before_the_single_preflight() {
        let config_dir = Path::new("/tmp/herdr-post-commit-tcc-order");
        let order = std::cell::RefCell::new(Vec::<&'static str>::new());
        let mut payload = json!({"ok": true, "code": "update_succeeded", "notices": []});
        // The migration writes the broker file only; the preflight is the step
        // that can raise a Documents prompt, so it must observe the migrated
        // broker, never the one the migration replaces.
        attach_post_commit_tcc(
            &mut payload,
            config_dir,
            Some(4),
            |_config_dir| {
                order.borrow_mut().push("migrate");
                Ok(true)
            },
            |_config_dir| {
                order.borrow_mut().push("preflight");
                json!({"probe": "pending", "prompt_pending": true, "detail": null})
            },
        );
        assert_eq!(*order.borrow(), vec!["migrate", "preflight"]);
        assert_eq!(payload["tcc_broker_migration"]["state"], "migrated");
        assert_eq!(payload["tcc_prompt_pending"], true);
        assert_eq!(payload["ok"], true);

        // A current broker is not migrated, but the single preflight still runs.
        let order = std::cell::RefCell::new(Vec::<&'static str>::new());
        let mut payload = json!({"ok": true, "notices": []});
        attach_post_commit_tcc(
            &mut payload,
            config_dir,
            Some(crate::tcc_broker::BROKER_COMPAT_REVISION),
            |_config_dir| {
                order.borrow_mut().push("migrate");
                Ok(true)
            },
            |_config_dir| {
                order.borrow_mut().push("preflight");
                json!({"probe": "available", "prompt_pending": false, "detail": null})
            },
        );
        assert_eq!(*order.borrow(), vec!["preflight"]);
        assert_eq!(payload["tcc_broker_migration"]["state"], "current");
        assert_eq!(payload["tcc_prompt_pending"], false);
    }

    /// user gets the alias subcommands on the first apply of this release |
    /// Given the worker persisted a migration outcome | When `update status`
    /// reads it | Then the recorded state is reported without re-running codesign
    /// or a migration.
    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn broker_migration_outcome_round_trips_through_the_update_record() {
        let root = std::env::temp_dir().join(format!(
            "herdr-broker-record-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        assert_eq!(persisted_broker_migration(&root)["state"], "unknown");
        let migration = json!({
            "state": "migrated",
            "installed_revision": 4,
            "target_revision": crate::tcc_broker::BROKER_COMPAT_REVISION,
            "needs_reauthorization": true,
        });
        persist_broker_migration(&root, &migration);
        let recorded = persisted_broker_migration(&root);
        assert_eq!(recorded["state"], "migrated");
        assert_eq!(recorded["installed_revision"], 4);
        assert_eq!(recorded["needs_reauthorization"], true);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// user gets the alias subcommands installed by the upgrade | Given an    /// user gets the alias subcommands installed by the upgrade | Given an
    /// installed broker at an older compatibility revision | When the committed
    /// update path runs the broker migration | Then it performs exactly one
    /// explicit-reauthorization migration and reports it as a non-fatal notice,
    /// while a current revision is left untouched.
    #[test]
    fn broker_migration_runs_once_for_an_older_revision() {
        let config_dir = Path::new("/tmp/herdr-broker-migration-decision");
        let target = crate::tcc_broker::BROKER_COMPAT_REVISION;
        let mut invocations = 0;
        let migration = broker_migration_decision(
            Some(4),
            target,
            |_config_dir| {
                invocations += 1;
                Ok(true)
            },
            config_dir,
        );
        assert_eq!(invocations, 1, "an older revision must be migrated once");
        assert_eq!(migration["state"], "migrated");
        assert_eq!(migration["installed_revision"], 4);
        assert_eq!(migration["target_revision"], target);
        assert_eq!(migration["needs_reauthorization"], true);

        let mut payload = json!({"ok": true, "code": "update_succeeded", "notices": []});
        attach_tcc_broker_migration(&mut payload, migration);
        assert_eq!(payload["ok"], true, "a migration is never fatal");
        assert_eq!(payload["tcc_broker_migration"]["state"], "migrated");
        let notice = payload["notices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|notice| notice["code"] == "tcc_broker_migrated")
            .expect("a migration must be reported");
        assert_eq!(notice["fatal"], false);

        // A current broker is not migrated again.
        let mut second = 0;
        let current = broker_migration_decision(
            Some(target),
            target,
            |_config_dir| {
                second += 1;
                Ok(true)
            },
            config_dir,
        );
        assert_eq!(second, 0, "a current broker must not be migrated");
        assert_eq!(current["state"], "current");
        let mut payload = json!({"ok": true, "notices": []});
        attach_tcc_broker_migration(&mut payload, current);
        assert!(payload["notices"].as_array().unwrap().is_empty());

        // An unreadable revision is reported, never silently migrated or failed.
        let unknown = broker_migration_decision(
            None,
            target,
            |_config_dir| panic!("must not migrate without a known installed revision"),
            config_dir,
        );
        assert_eq!(unknown["state"], "unknown");
        assert_eq!(
            unknown["next_action"],
            "herdr-mcp permissions setup --upgrade-broker"
        );
    }

    /// user is told the exact command that works | Given a migration that fails |
    /// When the result is reported | Then it is a non-fatal warning naming
    /// `permissions setup --upgrade-broker`, never `tcc-broker install --force`.
    #[test]
    fn failed_broker_migration_reports_the_working_command() {
        let config_dir = Path::new("/tmp/herdr-broker-migration-decision-fail");
        let migration = broker_migration_decision(
            Some(4),
            crate::tcc_broker::BROKER_COMPAT_REVISION,
            |_config_dir| Err("codesign verification failed".to_owned()),
            config_dir,
        );
        assert_eq!(migration["state"], "failed");
        assert_eq!(
            migration["next_action"],
            "herdr-mcp permissions setup --upgrade-broker"
        );
        let mut payload = json!({"ok": true, "code": "update_succeeded", "notices": []});
        attach_tcc_broker_migration(&mut payload, migration);
        assert_eq!(
            payload["ok"], true,
            "a failed migration is not an update failure"
        );
        let notice = payload["notices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|notice| notice["code"] == "tcc_broker_migration_failed")
            .expect("a failed migration must be reported");
        assert_eq!(notice["severity"], "warning");
        assert_eq!(
            notice["next_action"],
            "herdr-mcp permissions setup --upgrade-broker"
        );
    }

    /// user keeps a working control path | Given a Runtime update that
    /// committed while a macOS Documents prompt is still unanswered | When the
    /// result is reported | Then the prompt is a non-fatal notice and the update
    /// still succeeds, and an answered probe adds no notice.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn tcc_preflight_pending_is_a_non_fatal_update_notice() {
        let mut payload = json!({
            "ok": true,
            "code": "update_succeeded",
            "notices": [],
        });
        attach_tcc_preflight(
            &mut payload,
            json!({"probe": "pending", "prompt_pending": true, "detail": null}),
        );
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["code"], "update_succeeded");
        assert_eq!(payload["tcc_prompt_pending"], true);
        assert_eq!(payload["tcc_preflight"]["probe"], "pending");
        let notice = payload["notices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|notice| notice["code"] == "tcc_prompt_pending")
            .expect("an unanswered Documents prompt must be reported");
        assert_eq!(notice["severity"], "notice");
        assert_eq!(notice["fatal"], false);

        // An answered probe (or one that never needed a prompt) adds no notice
        // and leaves the update result untouched.
        let mut answered = json!({"ok": true, "code": "update_succeeded", "notices": []});
        attach_tcc_preflight(
            &mut answered,
            json!({"probe": "available", "prompt_pending": false, "detail": null}),
        );
        assert_eq!(answered["tcc_prompt_pending"], false);
        assert!(
            answered["notices"].as_array().unwrap().is_empty(),
            "an answered probe must not add a notice: {answered}"
        );
    }

    /// Given a stable-channel `update apply --manifest <newer manifest>` whose
    /// Runtime update already committed
    /// When the fleet Worker is behind the installed release
    /// Then the Worker step is never invoked, the command reports success, and
    /// the only Worker signal is a non-fatal notice.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn update_apply_with_manifest_on_stable_never_invokes_worker_update() {
        let root = env::temp_dir().join(format!(
            "herdr-manifest-stable-{}-{}",
            std::process::id(),
            now_ms_i64()
        ));
        let paths = test_runtime_paths(&root);
        let mut worker_update_invocations = 0;
        // `--manifest` and the update channel only select release metadata; the
        // post-commit Worker decision is this runtime-only seam, which every
        // stable and preview apply reaches.
        let _ = resolve_worker_update_after_runtime_commit(
            &paths,
            "1.1.0-rc.11",
            WorkerChain::RuntimeOnly,
            || {
                worker_update_invocations += 1;
                json!({ "ok": true, "code": "worker_update_succeeded" })
            },
        );
        assert_eq!(
            worker_update_invocations, 0,
            "a stable --manifest apply must never run worker update"
        );

        let outdated_worker = json!({
            "configured": true,
            "edge_origin": "https://edge.example.test",
            "worker": "herdr-edge",
            "edge_version": "1.1.0-rc.6",
            "target_version": "1.1.0-rc.11",
            "update_required": true,
        });
        let (payload, code) = runtime_update_succeeded_result(RuntimeUpdateOutcome {
            job_id: "upd-manifest-stable".to_owned(),
            target_version: "1.1.0-rc.11".to_owned(),
            target: "aarch64-apple-darwin".to_owned(),
            asset: "herdr-mcp".to_owned(),
            worker_pid: 7,
            worker_pid_persisted: true,
            job: json!({ "job_id": "upd-manifest-stable", "state": "succeeded" }),
            migration: None,
            worker_update: outdated_worker,
            worker_chain: WorkerChain::RuntimeOnly,
            extension: ExtensionVersionReport::unknown("test"),
            extension_staging: None,
        });
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["code"], "update_succeeded");
        assert_eq!(payload["worker_update_chained"], false);
        assert_eq!(payload["worker_update_required"], true);
        let notice = payload["notices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|notice| notice["code"] == "worker_update_required")
            .expect("an outdated Worker must be a non-fatal notice");
        assert_eq!(notice["severity"], "notice");
        assert_eq!(notice["fatal"], false);
        assert_eq!(notice["next_action"], "herdr-mcp worker update");
        let _ = fs::remove_dir_all(root);
    }

    /// Given a `<= v1.1.0-rc.8` job whose detail carries the chained-Worker code
    /// When the installed Runtime already reports that job's target version
    /// Then `update status` reports the Runtime success the operator got, with a
    /// non-fatal notice, and a job whose Runtime never committed stays a failure.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn update_status_reports_a_legacy_failed_job_as_success_when_the_swap_committed() {
        let mut job = ordinary_update_job("upd-legacy-failed");
        job.version = "1.1.0-rc.11".to_owned();
        job.state = "failed".to_owned();
        job.detail =
            Some("update_succeeded_worker_update_required worker_update_failed".to_owned());

        let committed = update_status_view(
            Some(&job),
            "1.1.0-rc.11",
            json!({ "configured": true, "update_required": false }),
        );
        assert_eq!(committed["ok"], true);
        assert_eq!(committed["runtime_outcome"], "succeeded");
        assert_eq!(committed["recorded_runtime_state"], "failed");
        assert_eq!(committed["next_action"], Value::Null);
        assert!(
            committed["notices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|notice| notice["code"] == "worker_update_failed_legacy"),
            "the legacy chained-Worker text must stay a notice"
        );

        // Same record while the installed Runtime is still the previous release:
        // the swap never happened, so it must stay a failure.
        let not_committed = update_status_view(
            Some(&job),
            "1.1.0-rc.6",
            json!({ "configured": true, "update_required": false }),
        );
        assert_eq!(not_committed["ok"], false);
        assert_eq!(not_committed["runtime_outcome"], "failed");
        assert_eq!(not_committed["next_action"], "herdr-mcp update apply");
        assert!(
            !not_committed["notices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|notice| notice["code"] == "worker_update_failed_legacy"),
            "an uncommitted Runtime swap must not be excused as a legacy Worker notice"
        );
    }

    /// Given a `--with-worker` request in a detached or CI invocation
    /// When update apply admits the Worker chain
    /// Then it is refused with a clear machine-readable code before any work.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn with_worker_is_refused_without_an_interactive_terminal() {
        assert_eq!(
            with_worker_refusal_code(true, false, false),
            Some(WITH_WORKER_REQUIRES_TTY)
        );
        assert_eq!(
            with_worker_refusal_code(true, true, false),
            Some(WITH_WORKER_REQUIRES_TTY)
        );
        assert_eq!(
            with_worker_refusal_code(true, false, true),
            Some(WITH_WORKER_REQUIRES_TTY)
        );
        assert_eq!(with_worker_refusal_code(true, true, true), None);
        assert_eq!(with_worker_refusal_code(false, false, false), None);

        let payload = with_worker_refusal_payload(WITH_WORKER_REQUIRES_TTY);
        assert_eq!(payload["ok"], false);
        assert_eq!(payload["code"], "with_worker_requires_interactive_tty");
        assert_eq!(payload["runtime_updated"], false);
        assert_eq!(payload["worker_update_requested"], true);
        assert_eq!(payload["worker_update_started"], false);
        assert_eq!(payload["next_action"], "herdr-mcp update apply");
    }

    /// Given a Runtime update that already committed
    /// When `update status` classifies a job recorded by an updater <= v1.1.0-rc.8
    /// Then the succeeded Runtime job is a success and the chained Worker failure
    /// is only a notice with the separate operator command.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn update_status_reports_a_succeeded_runtime_job_as_success() {
        let mut job = ordinary_update_job("upd-legacy-worker");
        job.state = "succeeded".to_owned();
        job.detail =
            Some("update_succeeded_worker_update_required worker_update_failed".to_owned());
        let view = update_status_view(
            Some(&job),
            "1.1.0-rc.9",
            json!({ "configured": true, "update_required": false }),
        );
        assert_eq!(view["ok"], true);
        assert_eq!(view["runtime_outcome"], "succeeded");
        assert_eq!(view["job"]["state"], "succeeded");
        assert_eq!(view["next_action"], Value::Null);
        let legacy = view["notices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|notice| notice["code"] == "worker_update_failed_legacy")
            .expect("the legacy chained-Worker text must be a notice");
        assert_eq!(legacy["severity"], "notice");
        assert_eq!(legacy["fatal"], false);
        assert_eq!(legacy["next_action"], "herdr-mcp worker update");

        // A plain success carries no legacy notice.
        job.detail = Some("service install committed and health gate passed".to_owned());
        let clean = update_status_view(
            Some(&job),
            "1.1.0-rc.9",
            json!({ "configured": false, "update_required": false }),
        );
        assert_eq!(clean["ok"], true);
        assert!(clean["notices"].as_array().unwrap().is_empty());
        assert_eq!(
            clean["job"]["detail"],
            "service install committed and health gate passed"
        );
    }

    /// Given each durable job state
    /// When `update status` classifies it
    /// Then only a failed Runtime job is unsuccessful, and Worker text never decides.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn update_status_runtime_outcome_ignores_worker_text() {
        assert_eq!(runtime_job_outcome("succeeded"), "succeeded");
        assert_eq!(runtime_job_outcome("failed"), "failed");
        assert_eq!(runtime_job_outcome("queued"), "in_progress");
        assert_eq!(runtime_job_outcome("installing"), "in_progress");
        assert_eq!(runtime_job_outcome("something-else"), "unknown");
        assert!(legacy_worker_update_detail(Some(
            "update_succeeded_worker_update_required"
        )));
        assert!(legacy_worker_update_detail(Some("worker_update_failed")));
        assert!(!legacy_worker_update_detail(None));
        assert!(!legacy_worker_update_detail(Some("health gate passed")));

        let mut failed = ordinary_update_job("upd-failed");
        failed.state = "failed".to_owned();
        failed.detail = Some("service install failed".to_owned());
        let view = update_status_view(
            Some(&failed),
            "1.1.0-rc.9",
            json!({ "configured": false, "update_required": false }),
        );
        assert_eq!(view["ok"], false);
        assert_eq!(view["runtime_outcome"], "failed");
        assert_eq!(view["next_action"], "herdr-mcp update apply");

        let empty = update_status_view(
            None,
            "1.1.0-rc.9",
            json!({ "configured": false, "update_required": false }),
        );
        assert_eq!(empty["ok"], true);
        assert_eq!(empty["runtime_outcome"], "none");
        assert_eq!(empty["job"], Value::Null);
    }

    /// Given the Runtime already on the target release
    /// When the resolved Worker step fails
    /// Then `update_current` still reports success and the Worker problem is a notice.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn update_current_stays_successful_when_the_worker_step_fails() {
        let (payload, code) = update_current_result(UpdateCurrentOutcome {
            channel: UpdateChannel::Stable,
            current_version: "1.1.0-rc.9",
            target_version: "1.1.0-rc.9",
            tag: "v1.1.0-rc.9",
            migration: None,
            worker_update: json!({ "ok": false, "code": "worker_update_failed", "error": "reconciliation did not complete" }),
            worker_chain: WorkerChain::WithWorker,
            extension: &ExtensionVersionReport::unknown("test"),
            extension_staging: None,
        });
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["code"], "update_current");
        assert_eq!(payload["runtime_changed"], false);
        let notice = payload["notices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|notice| notice["code"] == "worker_update_failed")
            .expect("the failed Worker step must stay a notice");
        assert_eq!(notice["fatal"], false);
        assert_eq!(notice["next_action"], "herdr-mcp worker update");

        // The default runtime-only path never chains the Worker at all.
        let (payload, code) = update_current_result(UpdateCurrentOutcome {
            channel: UpdateChannel::Stable,
            current_version: "1.1.0-rc.9",
            target_version: "1.1.0-rc.9",
            tag: "v1.1.0-rc.9",
            migration: None,
            worker_update: json!({ "configured": false, "update_required": false }),
            worker_chain: WorkerChain::RuntimeOnly,
            extension: &ExtensionVersionReport::unknown("test"),
            extension_staging: None,
        });
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["worker_update_chained"], false);
    }

    /// Given a Runtime update that committed
    /// When the standalone browser extension needs a Reload
    /// Then the result stays successful and the Reload is a non-fatal notice
    ///     carrying the exact user action (§833).
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn runtime_update_reports_a_pending_extension_reload_as_a_notice() {
        let (payload, code) = runtime_update_succeeded_result(RuntimeUpdateOutcome {
            job_id: "upd-833".to_owned(),
            target_version: "1.1.0-rc.12".to_owned(),
            target: "aarch64-apple-darwin".to_owned(),
            asset: "herdr-mcp".to_owned(),
            worker_pid: 7,
            worker_pid_persisted: true,
            job: json!({ "job_id": "upd-833", "state": "succeeded" }),
            migration: None,
            worker_update: json!({ "configured": false, "update_required": false }),
            worker_chain: WorkerChain::RuntimeOnly,
            extension: ExtensionVersionReport::for_test("reload_pending"),
            extension_staging: None,
        });
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(payload["ok"], true);
        assert_eq!(payload["code"], "update_succeeded");
        assert_eq!(payload["standalone_extension"]["state"], "reload_pending");
        assert_eq!(payload["standalone_extension_staging"], Value::Null);
        let notice = payload["notices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|notice| notice["code"] == "extension_reload_required")
            .expect("a staged extension the browser has not loaded is a notice");
        assert_eq!(notice["severity"], "notice");
        assert_eq!(notice["fatal"], false);
        assert!(
            notice["next_action"]
                .as_str()
                .unwrap()
                .contains("chrome://extensions")
        );

        // A staged extension that belongs to another release is a warning with
        // the exact fix command, and still not a failed Runtime update.
        let (payload, code) = runtime_update_succeeded_result(RuntimeUpdateOutcome {
            job_id: "upd-833-stale".to_owned(),
            target_version: "1.1.0-rc.12".to_owned(),
            target: "aarch64-apple-darwin".to_owned(),
            asset: "herdr-mcp".to_owned(),
            worker_pid: 7,
            worker_pid_persisted: true,
            job: json!({ "job_id": "upd-833-stale", "state": "succeeded" }),
            migration: None,
            worker_update: json!({ "configured": false, "update_required": false }),
            worker_chain: WorkerChain::RuntimeOnly,
            extension: ExtensionVersionReport::for_test("extension_stale"),
            extension_staging: None,
        });
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(payload["ok"], true);
        let notice = payload["notices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|notice| notice["code"] == "extension_stale")
            .expect("a staged extension from another release is reported");
        assert_eq!(notice["severity"], "warning");
        assert_eq!(notice["fatal"], false);
        assert_eq!(
            notice["next_action"],
            "herdr-mcp extension standalone install"
        );
    }

    /// Given a Runtime update that activated a new generation
    /// When its result carries notices
    /// Then the one-time macOS Documents prompt is announced, without replacing
    /// the Worker notice.
    #[cfg(target_os = "macos")]
    #[test]
    fn runtime_update_announces_the_macos_documents_prompt() {
        let notice = macos_tcc_prompt_notice().expect("macOS must announce the prompt");
        assert_eq!(notice["code"], "macos_documents_tcc_prompt");
        assert_eq!(notice["severity"], "notice");
        assert_eq!(notice["fatal"], false);
        assert!(notice["message"].as_str().unwrap().contains("Documents"));

        let merged = with_macos_tcc_prompt_notice(json!([{ "code": "worker_update_required" }]));
        let codes = merged
            .as_array()
            .unwrap()
            .iter()
            .map(|notice| notice["code"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            codes,
            vec![
                json!("worker_update_required"),
                json!("macos_documents_tcc_prompt")
            ]
        );
    }
}
