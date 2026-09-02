//! Keep the production Rust Link aligned with the exact managed runtime generation.
//!
//! A service generation switch updates `runtime/current`. The production Link has
//! two additional generation references: its launchd environment and the live
//! runtime-control document. The running Link watches runtime-control and can
//! switch generations without dropping its Edge WebSocket; the plist is updated
//! only so a later natural Link restart starts with the same generation identity.

use crate::paths::RuntimePaths;

#[cfg(target_os = "macos")]
use plist::{Dictionary, Value as PlistValue};
#[cfg(target_os = "macos")]
use std::env;
#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;

use super::cutover_execute::LINK_LAUNCHD_THROTTLE_SECONDS;
#[cfg(any(target_os = "macos", test))]
use super::cutover_execute::LaunchdOps;
#[cfg(target_os = "macos")]
use super::cutover_execute::{RealLaunchd, atomic_write, encode_prod_rust_plist};
#[cfg(target_os = "macos")]
use super::install::{
    candidate_program_arguments, configured_edge_device_identity, configured_edge_ws_url,
    inherited_proxy_env,
};
#[cfg(target_os = "macos")]
use super::migrate_runtime_control::{
    active_rust_generation_id, read_binary_version_hint, reconcile_current_generation,
};
#[cfg(any(target_os = "macos", test))]
use super::ownership::LINK_PROD_LABEL;
#[cfg(target_os = "macos")]
use super::ownership::{
    LinkAgentView, LinkImplementation, assess_agent, parse_launchd_environment_value,
    program_points_at_managed_runtime, read_status_active_generation,
};
use super::runtime_control::retryable_candidate_outcome;
use super::runtime_generation::RUNTIME_GENERATION_DEFAULT_TIMEOUT_MS;
#[cfg(target_os = "macos")]
use serde_json::Value;

// Runtime-control validation is a loopback RPC, but its own bounded request
// timeout is still 30s. The outer service transaction must not interrupt that
// legitimate in-flight validation and misclassify it as a stalled Link.
const ACTIVE_WAIT_BUDGET: Duration =
    Duration::from_millis(RUNTIME_GENERATION_DEFAULT_TIMEOUT_MS + 5_000);
const ACTIVE_RECONCILE_ATTEMPTS: usize = 2;
#[cfg(target_os = "macos")]
const ACTIVE_POLL_INTERVAL: Duration = Duration::from_millis(100);

// Final bounded convergence phase after an owned kickstart. The Link may need
// several polls to restart, re-read runtime-control, and hot-switch to the new
// generation. We wait on provable state (control desired/revision, status
// processed_revision/outcome, manager active/transition) rather than a fixed
// sleep, and only roll back on a definitive non-retryable outcome, a stall, or
// ownership drift. Wall-clock maximum = CONVERGENCE_MAX_POLLS * poll_interval
// (20s) plus the hot-switch budget (8s) and kickstart, so the whole reconcile
// stays bounded.
// Post-kickstart convergence covers both launchd's throttle window and one
// complete runtime-control validation timeout, with a small scheduling margin.
const CONVERGENCE_MAX_POLLS: usize = (RUNTIME_GENERATION_DEFAULT_TIMEOUT_MS as usize / 1_000)
    + LINK_LAUNCHD_THROTTLE_SECONDS as usize
    + 5;
const CONVERGENCE_MAX_STALLED_POLLS: usize = 3;
const CONVERGENCE_POLL_INTERVAL: Duration = Duration::from_secs(1);
// `launchctl kickstart -k` can return while launchd is still honoring the
// loaded Link job's ThrottleInterval. During that restart window the persisted
// status can legitimately stay byte-for-byte stale because the replacement
// process has not started and consumed runtime-control yet. Give only this
// pre-progress phase a throttle-aware grace period; after any evidence changes,
// the normal tight stall detector applies again.
const CONVERGENCE_RESTART_GRACE_POLLS: usize = LINK_LAUNCHD_THROTTLE_SECONDS as usize + 4;

/// Reconcile production Link generation state after a successful service
/// generation change. Named instances never touch production Link state.
pub(crate) fn reconcile_after_service_generation_change(
    paths: &RuntimePaths,
) -> Result<(), String> {
    if paths.instance.is_named() {
        return Ok(());
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = paths;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        let home = home_dir()
            .ok_or_else(|| "HOME is required for Link generation reconcile".to_owned())?;
        let launchd = RealLaunchd;
        let Some(prod) = ensure_enrolled_rust_prod_link(&home, paths, &launchd)? else {
            return Ok(());
        };

        let generation = active_rust_generation_id(&home)?;
        let runtime_version = read_binary_version_hint(&home);
        // Update runtime-control first. The already-running Link polls this file and
        // hot-switches its local runtime generation while keeping the Edge WebSocket
        // alive. Updating the plist is persistence for the next natural restart; do
        // not bootout/bootstrap link-prod merely to refresh generation metadata.
        reconcile_current_generation(&home, &paths.config_dir)?;
        refresh_prod_plist_generation(
            &home,
            &prod.plist_path,
            &generation,
            runtime_version.as_deref(),
        )?;

        let status_path = prod
            .status_path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| paths.config_dir.join("runtime-status-prod.json"));
        let control_path = prefer_existing_control(&paths.config_dir);
        let prod_plist_path = prod.plist_path.clone();
        converge_active_generation_with_fallback(
            &generation,
            || reconcile_current_generation(&home, &paths.config_dir).map(|_| ()),
            || {
                let loaded = launchd.is_loaded(LINK_PROD_LABEL)?;
                let current = assess_agent(&home, LINK_PROD_LABEL, loaded);
                if !current.present
                    || !current.loaded
                    || current.implementation != LinkImplementation::Rust
                    || !program_points_at_managed_runtime(&current.program_arguments, &home)
                {
                    return Err(
                        "production Link ownership changed before bounded kickstart; refusing mutation"
                            .to_owned(),
                    );
                }
                Ok(())
            },
            || restart_prod_link_for_generation(&launchd, &prod_plist_path, &generation),
            |budget| wait_for_active_generation(&status_path, &generation, budget),
            || read_convergence_evidence(&control_path, &status_path),
        )
    }
}

/// Remove a production Link that was created by the current higher-level
/// transaction after that transaction has failed. Callers must only invoke
/// this when they captured evidence that link-prod did not exist before the
/// transaction. The helper re-checks ownership and refuses to remove a
/// Node/foreign or non-managed-runtime Link.
#[cfg(target_os = "macos")]
pub(crate) fn remove_fresh_owned_prod_link_after_failed_activation(
    home: &Path,
) -> Result<bool, String> {
    remove_fresh_owned_prod_link_after_failed_activation_with(home, &RealLaunchd)
}

#[cfg(target_os = "macos")]
fn remove_fresh_owned_prod_link_after_failed_activation_with<L: LaunchdOps>(
    home: &Path,
    launchd: &L,
) -> Result<bool, String> {
    let plist_path = home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LINK_PROD_LABEL}.plist"));
    let loaded = launchd.is_loaded(LINK_PROD_LABEL)?;
    if !loaded && !plist_path.exists() {
        return Ok(false);
    }

    let prod = assess_agent(home, LINK_PROD_LABEL, loaded);
    if !prod.present
        || prod.implementation != LinkImplementation::Rust
        || !program_points_at_managed_runtime(&prod.program_arguments, home)
        || prod.plist_path != plist_path
    {
        return Err(
            "refusing failed-activation cleanup because production Link ownership changed or is not the standard owned Rust managed-runtime link"
                .to_owned(),
        );
    }

    if loaded {
        launchd.bootout_prod(LINK_PROD_LABEL)?;
        if launchd.is_loaded(LINK_PROD_LABEL)? {
            return Err(format!(
                "production Link {LINK_PROD_LABEL} is still loaded after failed-activation cleanup"
            ));
        }
    }
    if plist_path.exists() {
        fs::remove_file(&plist_path).map_err(|error| {
            format!(
                "cannot remove fresh production Link plist {} after failed activation: {error}",
                plist_path.display()
            )
        })?;
    }
    Ok(true)
}

/// Ensure that a default-instance workstation which already has an immutable
/// per-device Edge identity also has a live Rust production Link. This is not
/// a general ownership takeover: a missing Link is created only for an enrolled
/// device, while an existing Node/foreign Link remains untouched. A stopped
/// owned Rust Link may be restarted after refreshing its persisted identity.
#[cfg(target_os = "macos")]
fn ensure_enrolled_rust_prod_link<L: LaunchdOps>(
    home: &Path,
    paths: &RuntimePaths,
    launchd: &L,
) -> Result<Option<LinkAgentView>, String> {
    let enrolled =
        configured_edge_ws_url(home).is_some() && configured_edge_device_identity(home).is_some();
    let loaded = launchd.is_loaded(LINK_PROD_LABEL)?;
    let mut prod = assess_agent(home, LINK_PROD_LABEL, loaded);

    if !prod.present {
        if !enrolled {
            // Preserve historical behavior for ordinary service installation
            // before Worker enrollment.
            return Ok(None);
        }
        install_fresh_rust_prod_link(home, launchd)?;
        let loaded = launchd.is_loaded(LINK_PROD_LABEL)?;
        prod = assess_agent(home, LINK_PROD_LABEL, loaded);
    }

    if prod.implementation != LinkImplementation::Rust
        || !program_points_at_managed_runtime(&prod.program_arguments, home)
    {
        // Never rewrite or bootstrap an existing Node/foreign production Link.
        if enrolled {
            return Err(
                "enrolled device requires an owned Rust production Link; refusing to seize an existing Node/foreign link-prod"
                    .to_owned(),
            );
        }
        return Ok(None);
    }

    if !prod.loaded {
        let generation = active_rust_generation_id(home)?;
        let runtime_version = read_binary_version_hint(home);
        refresh_prod_plist_generation(
            home,
            &prod.plist_path,
            &generation,
            runtime_version.as_deref(),
        )?;
        launchd.bootstrap_prod(&prod.plist_path, LINK_PROD_LABEL)?;
        if !launchd.is_loaded(LINK_PROD_LABEL)? {
            return Err(format!(
                "production Link {LINK_PROD_LABEL} is still not loaded after bootstrap"
            ));
        }
        prod = assess_agent(home, LINK_PROD_LABEL, true);
    }

    if !prod.loaded {
        return Err(format!("production Link {LINK_PROD_LABEL} is not loaded"));
    }

    let _ = paths;
    Ok(Some(prod))
}

#[cfg(target_os = "macos")]
fn install_fresh_rust_prod_link<L: LaunchdOps>(home: &Path, launchd: &L) -> Result<(), String> {
    let plist_path = home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LINK_PROD_LABEL}.plist"));
    if plist_path.exists() {
        return Err(format!(
            "refusing fresh production Link install because {} already exists",
            plist_path.display()
        ));
    }

    let parent = plist_path
        .parent()
        .ok_or_else(|| "production Link plist has no parent directory".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "cannot create production Link directory {}: {error}",
            parent.display()
        )
    })?;

    let mut source = Dictionary::new();
    source.insert(
        "Label".to_owned(),
        PlistValue::String(LINK_PROD_LABEL.to_owned()),
    );
    // Fresh link-prod must watch the production runtime-control/status pair.
    // Without these explicit paths the Rust daemon falls back to the plain
    // candidate files and generation reconciliation can observe a different
    // state plane from the running production Link.
    let config_dir = home.join(".config").join("herdr-mcp");
    let mut source_env = Dictionary::new();
    source_env.insert(
        "HERDR_RUNTIME_CONTROL_PATH".to_owned(),
        PlistValue::String(
            config_dir
                .join("runtime-control-prod.json")
                .to_string_lossy()
                .into_owned(),
        ),
    );
    source_env.insert(
        "HERDR_RUNTIME_STATUS_PATH".to_owned(),
        PlistValue::String(
            config_dir
                .join("runtime-status-prod.json")
                .to_string_lossy()
                .into_owned(),
        ),
    );
    source.insert(
        "EnvironmentVariables".to_owned(),
        PlistValue::Dictionary(source_env),
    );
    let mut source_bytes = Vec::new();
    PlistValue::Dictionary(source)
        .to_writer_xml(&mut source_bytes)
        .map_err(|error| format!("cannot encode fresh production Link source plist: {error}"))?;
    let program = candidate_program_arguments(home)?;
    let (rust_bytes, _) = encode_prod_rust_plist(home, &source_bytes, &program)?;
    atomic_write(&plist_path, &rust_bytes, 0o600)?;

    if let Err(error) = launchd.bootstrap_prod(&plist_path, LINK_PROD_LABEL) {
        let _ = fs::remove_file(&plist_path);
        return Err(format!(
            "cannot bootstrap fresh production Link; plist was removed: {error}"
        ));
    }
    if !launchd.is_loaded(LINK_PROD_LABEL)? {
        let _ = launchd.bootout_prod(LINK_PROD_LABEL);
        let _ = fs::remove_file(&plist_path);
        return Err(
            "fresh production Link bootstrap returned success but launchd reports it unloaded; plist was removed"
                .to_owned(),
        );
    }
    Ok(())
}

/// Resolve the live prod runtime-control document path, preferring the prod
/// variant when present.
#[cfg(target_os = "macos")]
fn prefer_existing_control(config_dir: &Path) -> PathBuf {
    let prod = config_dir.join("runtime-control-prod.json");
    let plain = config_dir.join("runtime-control.json");
    if prod.is_file() { prod } else { plain }
}

/// Read the provable Link-generation evidence from the control and status
/// documents. Missing/unparseable documents yield a default (all-None) evidence,
/// which the convergence loop treats as a stall rather than success.
#[cfg(target_os = "macos")]
fn read_convergence_evidence(
    control_path: &Path,
    status_path: &Path,
) -> Result<ConvergenceEvidence, String> {
    let control = read_optional_json(control_path)?;
    let status = read_optional_json(status_path)?;
    let mut evidence = ConvergenceEvidence::default();
    if let Some(control) = control {
        evidence.control_desired = control
            .get("desired_active")
            .and_then(Value::as_str)
            .map(str::to_owned);
        evidence.control_revision = control.get("revision").and_then(Value::as_u64);
    }
    if let Some(status) = status {
        evidence.status_processed_revision =
            status.get("processed_revision").and_then(Value::as_u64);
        evidence.status_outcome = status
            .get("outcome")
            .and_then(Value::as_str)
            .map(str::to_owned);
        evidence.active_generation = status
            .pointer("/manager/active_generation")
            .and_then(Value::as_str)
            .map(str::to_owned);
        evidence.transition_seq = status
            .pointer("/manager/transition_seq")
            .and_then(Value::as_u64);
        evidence.last_transition_to = status
            .pointer("/manager/last_transition/to")
            .and_then(Value::as_str)
            .map(str::to_owned);
        evidence.last_transition_outcome = status
            .pointer("/manager/last_transition/outcome")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    Ok(evidence)
}

#[cfg(target_os = "macos")]
fn read_optional_json(path: &Path) -> Result<Option<Value>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() > 64 * 1024 {
        return Err(format!(
            "convergence evidence file exceeds size limit: {}",
            path.display()
        ));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
    Ok(Some(value))
}

fn converge_active_generation_with_fallback<Reconcile, VerifyOwnership, Restart, Wait, Probe>(
    generation: &str,
    mut reconcile: Reconcile,
    mut verify_ownership: VerifyOwnership,
    mut restart: Restart,
    mut wait: Wait,
    mut probe: Probe,
) -> Result<(), String>
where
    Reconcile: FnMut() -> Result<(), String>,
    VerifyOwnership: FnMut() -> Result<(), String>,
    Restart: FnMut() -> Result<(), String>,
    Wait: FnMut(Duration) -> Result<(), String>,
    Probe: FnMut() -> Result<ConvergenceEvidence, String>,
{
    let wait_slice = ACTIVE_WAIT_BUDGET / ACTIVE_RECONCILE_ATTEMPTS as u32;
    let mut last_error = None;
    for attempt in 0..ACTIVE_RECONCILE_ATTEMPTS {
        if attempt > 0 {
            // An already-running Link may have consumed the first revision while
            // the single-port runtime was restarting. Bump desired state once
            // more without interrupting its Edge WebSocket.
            reconcile()?;
        }
        match wait(wait_slice) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }

    // Hot-switching is the normal zero-disconnect path. If it cannot converge
    // within the bounded window, the loaded Link process may still be running
    // older code or may have missed control revisions during the server restart.
    // Restart exactly the still-owned production Link once. Ownership is
    // re-proved immediately before the mutation so a concurrent replacement
    // cannot turn the bounded recovery into a foreign-job restart. The restart
    // strategy also checks the *loaded* launchd generation: a plain kickstart
    // does not re-read a changed plist, so a stale loaded environment must be
    // bootout/bootstrap reloaded from the already-refreshed owned plist.
    verify_ownership()?;
    restart().map_err(|error| {
        format!(
            "production Link hot-switch to {generation} timed out and bounded restart failed: {error}; prior={}",
            last_error.as_deref().unwrap_or("unknown")
        )
    })?;
    reconcile()?;
    // The kickstarted Link restarts and re-reads runtime-control; it may take a
    // few polls to re-register, health-check, and activate the generation while
    // the single-port server finishes coming up. Instead of failing on a fixed
    // short window, run a final bounded convergence phase keyed to provable
    // Link state: control desired/revision, status processed_revision/outcome,
    // and manager active/transition. We only roll back when that evidence is
    // definitively failed, stalled, or ownership-drifted; a Link that is still
    // visibly progressing toward the expected generation is given the full
    // bounded budget (wild-clock maximum documented on the constants above).
    bounded_convergence_phase(
        generation,
        &mut verify_ownership,
        &mut probe,
        CONVERGENCE_MAX_POLLS,
        CONVERGENCE_MAX_STALLED_POLLS,
        CONVERGENCE_RESTART_GRACE_POLLS,
        CONVERGENCE_POLL_INTERVAL,
    )
    .map_err(|error| {
        format!(
            "production Link did not activate generation {generation} after bounded restart: {error}; prior={}",
            last_error.as_deref().unwrap_or("unknown")
        )
    })
}

#[cfg(target_os = "macos")]
fn restart_prod_link_for_generation<L: LaunchdOps>(
    launchd: &L,
    plist_path: &Path,
    generation: &str,
) -> Result<(), String> {
    let loaded_generation = read_loaded_prod_generation()?;
    restart_prod_link_for_observed_generation(
        launchd,
        plist_path,
        generation,
        loaded_generation.as_deref(),
    )
}

#[cfg(target_os = "macos")]
fn read_loaded_prod_generation() -> Result<Option<String>, String> {
    let target = format!("gui/{}/{}", unsafe { libc::geteuid() }, LINK_PROD_LABEL);
    let output = Command::new("/bin/launchctl")
        .args(["print", target.as_str()])
        .output()
        .map_err(|error| format!("cannot inspect loaded production Link: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(format!(
            "cannot inspect loaded production Link generation: {detail}"
        ));
    }
    Ok(parse_launchd_environment_value(
        &String::from_utf8_lossy(&output.stdout),
        "HERDR_RUNTIME_GENERATION",
    ))
}

#[cfg(target_os = "macos")]
fn restart_prod_link_for_observed_generation<L: LaunchdOps>(
    launchd: &L,
    plist_path: &Path,
    generation: &str,
    loaded_generation: Option<&str>,
) -> Result<(), String> {
    if loaded_generation == Some(generation) {
        return launchd.kickstart_prod(LINK_PROD_LABEL);
    }

    launchd.bootout_prod(LINK_PROD_LABEL)?;
    launchd
        .bootstrap_prod(plist_path, LINK_PROD_LABEL)
        .map_err(|error| {
            format!(
                "production Link loaded generation {} did not match {generation}; plist reload failed: {error}",
                loaded_generation.unwrap_or("missing")
            )
        })
}

/// Provable Link-generation state used by the post-kickstart convergence phase.
/// Every field is optional because the control/status documents are read from
/// disk and may be absent, empty, or mid-rewrite during a Link restart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ConvergenceEvidence {
    control_desired: Option<String>,
    control_revision: Option<u64>,
    status_processed_revision: Option<u64>,
    status_outcome: Option<String>,
    active_generation: Option<String>,
    transition_seq: Option<u64>,
    last_transition_to: Option<String>,
    last_transition_outcome: Option<String>,
}

impl ConvergenceEvidence {
    /// No control/status evidence was readable at all (files missing,
    /// unparseable, or mid-rewrite). The convergence loop treats this as a
    /// stall, never as success or as a definitive direction verdict.
    fn is_empty(&self) -> bool {
        self.control_desired.is_none()
            && self.control_revision.is_none()
            && self.status_processed_revision.is_none()
            && self.status_outcome.is_none()
            && self.active_generation.is_none()
            && self.transition_seq.is_none()
            && self.last_transition_to.is_none()
            && self.last_transition_outcome.is_none()
    }

    /// The Link's manager has activated exactly the expected generation.
    fn active_on(&self, generation: &str) -> bool {
        self.active_generation.as_deref() == Some(generation)
    }

    /// Control still desires exactly the expected generation (no drift).
    fn control_desires(&self, generation: &str) -> bool {
        self.control_desired.as_deref() == Some(generation)
    }

    /// The runtime-control loop reports a definitive, non-retryable failure of
    /// the target candidate. Retryable outcomes (transient health/catalog
    /// failures) are progress, not failure, and are left to the stall/budget
    /// logic. Reuses the Link's own retry policy rather than duplicating it.
    fn definitive_failure(&self) -> Option<String> {
        let outcome = self.status_outcome.as_deref()?;
        let raw = outcome.strip_prefix("retrying:").unwrap_or(outcome);
        let is_failure = raw.starts_with("candidate_rejected:")
            || raw.starts_with("activation_blocked:")
            || raw.starts_with("rolled_back:");
        if !is_failure || retryable_candidate_outcome(raw) {
            return None;
        }
        Some(format!("runtime-control outcome {raw}"))
    }

    /// The Link has not yet consumed the latest desired control revision, or is
    /// actively retrying a transient revision, or has recorded a transition
    /// toward the expected generation. Direction alone (a static difference in
    /// revisions, or a static retrying outcome) proves intent but not movement;
    /// the convergence loop detects movement separately by comparing successive
    /// fingerprints.
    fn directed_toward(&self, target: &str) -> bool {
        if self.active_on(target) || self.control_desires(target) {
            return true;
        }
        if let (Some(processed), Some(control)) =
            (self.status_processed_revision, self.control_revision)
            && processed < control
        {
            return true;
        }
        if let Some(outcome) = self.status_outcome.as_deref()
            && outcome.starts_with("retrying:")
        {
            return true;
        }
        if self.last_transition_to.as_deref() == Some(target) {
            return true;
        }
        false
    }
}

/// Bounded, evidence-driven convergence loop after an owned kickstart.
///
/// Generalize the `probe` closure so unit tests can drive deterministic
/// sequences of evidence without real Link processes or sleeps.
fn bounded_convergence_phase<VerifyOwnership, Probe>(
    expected: &str,
    verify_ownership: &mut VerifyOwnership,
    probe: &mut Probe,
    max_polls: usize,
    max_stalled_polls: usize,
    restart_grace_polls: usize,
    poll_interval: Duration,
) -> Result<(), String>
where
    VerifyOwnership: FnMut() -> Result<(), String>,
    Probe: FnMut() -> Result<ConvergenceEvidence, String>,
{
    let mut probe_errors = 0usize;
    let mut stalled_polls = 0usize;
    let mut last_fingerprint = None;
    let mut saw_forward_progress = false;
    for poll in 0..max_polls {
        match probe() {
            Err(error) => {
                probe_errors += 1;
                if probe_errors >= max_stalled_polls {
                    return Err(format!(
                        "convergence evidence unreadable for {probe_errors} consecutive polls: {error}"
                    ));
                }
            }
            Ok(evidence) => {
                probe_errors = 0;
                // Success requires the control document and the live manager to
                // both agree on the expected generation, and a final ownership
                // re-check so a concurrent replacement cannot turn late
                // convergence into a foreign takeover.
                if evidence.active_on(expected) && evidence.control_desires(expected) {
                    verify_ownership()?;
                    return Ok(());
                }
                // Definitive, non-retryable failure of the target candidate
                // means rollback is correct; do not wait out the budget.
                if let Some(reason) = evidence.definitive_failure() {
                    return Err(format!(
                        "production Link reported a definitive failure while converging to {expected}: {reason}"
                    ));
                }
                // Control desired drifting away from the expected generation is
                // its own failure mode (ownership/control tampering or stale
                // reconcile); fail closed instead of waiting forever.
                if evidence.control_desired.is_some() && !evidence.control_desires(expected) {
                    return Err(format!(
                        "production Link control desired drifted away from {expected} (desired={:?}) while converging",
                        evidence.control_desired
                    ));
                }
                // A Link that is not directed toward the expected generation at
                // all (no pending revision, no retry, no transition) has nothing
                // to converge to. Missing/unparseable evidence is a stall, not a
                // direction verdict, so it is left to the stall counter below.
                if !evidence.is_empty() && !evidence.directed_toward(expected) {
                    return Err(format!(
                        "production Link is not directed toward generation {expected} during bounded convergence"
                    ));
                }
                // A pending control revision or an explicit retrying outcome is
                // itself an in-flight runtime-control operation. Its inner
                // health/catalog RPC may legally consume the full validation
                // timeout, so do not let the much tighter static-fingerprint
                // stall detector preempt it. The overall max-poll budget still
                // bounds a genuinely wedged retry/pending state.
                let revision_in_flight = matches!(
                    (evidence.status_processed_revision, evidence.control_revision),
                    (Some(processed), Some(control)) if processed < control
                );
                let retry_in_flight = evidence
                    .status_outcome
                    .as_deref()
                    .is_some_and(|outcome| outcome.starts_with("retrying:"));
                if revision_in_flight || retry_in_flight {
                    stalled_polls = 0;
                    last_fingerprint = Some(evidence_fingerprint(&evidence));
                    if poll + 1 < max_polls && !poll_interval.is_zero() {
                        std::thread::sleep(poll_interval);
                    }
                    continue;
                }

                // Forward progress is defined by the evidence fingerprint
                // changing between polls while still directed toward expected.
                // A static fingerprint (same revision/outcome/transition) is a
                // stall, not progress. The first observation establishes the
                // baseline and counts as the first stalled poll, so a stall is
                // detected after exactly `max_stalled_polls` identical polls.
                let fingerprint = evidence_fingerprint(&evidence);
                if last_fingerprint.as_deref() == Some(fingerprint.as_str()) {
                    stalled_polls += 1;
                } else {
                    if last_fingerprint.is_some() {
                        saw_forward_progress = true;
                    }
                    stalled_polls = 1;
                }
                last_fingerprint = Some(fingerprint);
                let stall_limit = if saw_forward_progress {
                    max_stalled_polls
                } else {
                    restart_grace_polls.max(max_stalled_polls)
                };
                if stalled_polls >= stall_limit {
                    return Err(format!(
                        "production Link stalled while converging to {expected}: no forward progress across {stalled_polls} consecutive polls"
                    ));
                }
            }
        }
        if poll + 1 < max_polls && !poll_interval.is_zero() {
            std::thread::sleep(poll_interval);
        }
    }
    Err(format!(
        "production Link did not activate generation {expected} within the bounded convergence budget ({max_polls} polls)"
    ))
}

fn evidence_fingerprint(evidence: &ConvergenceEvidence) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|{}",
        evidence.control_revision.unwrap_or(0),
        evidence.status_processed_revision.unwrap_or(0),
        evidence.status_outcome.as_deref().unwrap_or(""),
        evidence.active_generation.as_deref().unwrap_or(""),
        evidence.transition_seq.unwrap_or(0),
        evidence.last_transition_to.as_deref().unwrap_or(""),
        evidence.last_transition_outcome.as_deref().unwrap_or(""),
    )
}

#[cfg(target_os = "macos")]
fn refresh_prod_plist_generation(
    home: &Path,
    plist_path: &Path,
    generation: &str,
    runtime_version: Option<&str>,
) -> Result<bool, String> {
    let original = fs::read(plist_path)
        .map_err(|error| format!("cannot read {}: {error}", plist_path.display()))?;
    let mut plist = PlistValue::from_reader(std::io::Cursor::new(&original))
        .map_err(|error| format!("cannot parse {}: {error}", plist_path.display()))?;
    let dict = plist
        .as_dictionary_mut()
        .ok_or_else(|| "prod Link plist root must be a dict".to_owned())?;
    let label = dict
        .get("Label")
        .and_then(PlistValue::as_string)
        .unwrap_or("");
    if label != LINK_PROD_LABEL {
        return Err(format!(
            "refusing to refresh plist Label={label} (expected {LINK_PROD_LABEL})"
        ));
    }

    let existing_env = dict
        .get("EnvironmentVariables")
        .and_then(PlistValue::as_dictionary)
        .cloned()
        .unwrap_or_default();
    let mut env_out = Dictionary::new();
    for (key, value) in &existing_env {
        let upper = key.to_ascii_uppercase();
        let raw_secret =
            (upper.contains("TOKEN") || upper.contains("PASSWORD") || upper.ends_with("_SECRET"))
                && upper != "HERDR_LINK_KEYCHAIN_SERVICE";
        if !raw_secret {
            env_out.insert(key.clone(), value.clone());
        }
    }

    env_out.insert(
        "HERDR_RUNTIME_GENERATION".to_owned(),
        PlistValue::String(generation.to_owned()),
    );
    match runtime_version {
        Some(version) if !version.trim().is_empty() => {
            env_out.insert(
                "HERDR_RUNTIME_VERSION".to_owned(),
                PlistValue::String(version.trim().to_owned()),
            );
        }
        _ => {
            env_out.remove("HERDR_RUNTIME_VERSION");
        }
    }
    if let Some(edge_url) = configured_edge_ws_url(home) {
        env_out.insert("HERDR_EDGE_URL".to_owned(), PlistValue::String(edge_url));
    }
    if let Some((device_id, keychain_service)) = configured_edge_device_identity(home) {
        env_out.insert(
            "HERDR_WORKSTATION_ID".to_owned(),
            PlistValue::String(device_id),
        );
        env_out.insert(
            "HERDR_LINK_KEYCHAIN_SERVICE".to_owned(),
            PlistValue::String(keychain_service),
        );
    }
    for (key, value) in inherited_proxy_env() {
        if !env_out.contains_key(&key) {
            env_out.insert(key, PlistValue::String(value));
        }
    }
    if env_out == existing_env {
        return Ok(false);
    }
    dict.insert(
        "EnvironmentVariables".to_owned(),
        PlistValue::Dictionary(env_out),
    );

    let mut encoded = Vec::new();
    plist
        .to_writer_xml(&mut encoded)
        .map_err(|error| format!("cannot encode prod Link plist: {error}"))?;
    atomic_write(plist_path, &encoded, 0o600)?;
    Ok(true)
}

#[cfg(target_os = "macos")]
fn wait_for_active_generation(
    status_path: &Path,
    generation: &str,
    budget: Duration,
) -> Result<(), String> {
    let started = Instant::now();
    loop {
        if read_status_active_generation(status_path).as_deref() == Some(generation) {
            return Ok(());
        }
        if started.elapsed() >= budget {
            let observed =
                read_status_active_generation(status_path).unwrap_or_else(|| "missing".to_owned());
            return Err(format!(
                "production Link did not activate generation {generation} within {}ms; observed={observed} status={}",
                budget.as_millis(),
                status_path.display()
            ));
        }
        std::thread::sleep(ACTIVE_POLL_INTERVAL);
    }
}

#[cfg(target_os = "macos")]
fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::cutover_execute::FakeLaunchd;
    use std::path::Path;

    #[cfg(target_os = "macos")]
    fn enrolled_test_home(label: &str) -> (PathBuf, RuntimePaths) {
        use std::os::unix::fs::symlink;

        let home = std::env::temp_dir().join(format!(
            "herdr-link-enrolled-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        let config_dir = home.join(".config/herdr-mcp");
        let runtime = config_dir.join("runtime");
        let generation = runtime.join("generations/rust-testconnect01");
        std::fs::create_dir_all(&generation).unwrap();
        std::fs::write(generation.join("herdr-mcp"), b"test-binary").unwrap();
        symlink("generations/rust-testconnect01", runtime.join("current")).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "[edge]\npublic_origin = \"https://edge.example\"\ndevice_id = \"dev_01ARZ3NDEKTSV4RRFFQ69G5FAV\"\n",
        )
        .unwrap();
        let paths = RuntimePaths {
            instance: crate::instance::InstanceId::default_instance(),
            config_file: config_dir.join("config.toml"),
            config_dir: config_dir.clone(),
            dev_state_dir: home.join(".config/herdr-mcp-dev"),
            herdr_socket: None,
        };
        (home, paths)
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn enrolled_device_bootstraps_missing_rust_prod_link() {
        let (home, paths) = enrolled_test_home("fresh");
        let launchd = FakeLaunchd::new();

        let prod = ensure_enrolled_rust_prod_link(&home, &paths, &launchd)
            .unwrap()
            .expect("enrolled device should create link-prod");

        assert!(prod.loaded);
        assert_eq!(prod.implementation, LinkImplementation::Rust);
        assert!(program_points_at_managed_runtime(
            &prod.program_arguments,
            &home
        ));
        assert_eq!(launchd.bootstraps().len(), 1);
        assert_eq!(launchd.bootstraps()[0].0, LINK_PROD_LABEL);

        let plist = std::fs::read_to_string(&prod.plist_path).unwrap();
        assert!(plist.contains("dev_01ARZ3NDEKTSV4RRFFQ69G5FAV"));
        assert!(plist.contains("herdr-edge-link-dev_01ARZ3NDEKTSV4RRFFQ69G5FAV"));
        assert!(plist.contains("HERDR_EDGE_URL"));
        assert!(plist.contains("HERDR_RUNTIME_CONTROL_PATH"));
        assert!(plist.contains("runtime-control-prod.json"));
        assert!(plist.contains("HERDR_RUNTIME_STATUS_PATH"));
        assert!(plist.contains("runtime-status-prod.json"));
        assert!(!plist.contains("devsec_"));

        let _ = std::fs::remove_dir_all(home);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn fresh_prod_link_bootstrap_failure_removes_new_plist() {
        let (home, paths) = enrolled_test_home("bootstrap-fail");
        let launchd = FakeLaunchd::new();
        launchd.fail_next_bootstrap();

        let error = ensure_enrolled_rust_prod_link(&home, &paths, &launchd).unwrap_err();
        assert!(error.contains("plist was removed"));
        assert!(
            !home
                .join("Library/LaunchAgents")
                .join(format!("{LINK_PROD_LABEL}.plist"))
                .exists()
        );

        let _ = std::fs::remove_dir_all(home);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn failed_activation_cleanup_removes_only_fresh_owned_rust_prod_link() {
        let (home, paths) = enrolled_test_home("failed-activation-cleanup");
        let launchd = FakeLaunchd::new();
        ensure_enrolled_rust_prod_link(&home, &paths, &launchd)
            .unwrap()
            .expect("enrolled device should create link-prod");

        assert!(
            remove_fresh_owned_prod_link_after_failed_activation_with(&home, &launchd).unwrap()
        );
        assert!(!launchd.is_loaded(LINK_PROD_LABEL).unwrap());
        assert!(
            !home
                .join("Library/LaunchAgents")
                .join(format!("{LINK_PROD_LABEL}.plist"))
                .exists()
        );

        let _ = std::fs::remove_dir_all(home);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn prod_plist_refresh_pins_generation_and_runtime_version_together() {
        let root = std::env::temp_dir().join(format!(
            "herdr-link-generation-refresh-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let config_dir = root.join(".config/herdr-mcp");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "[edge]\npublic_origin = \"https://edge.example\"\ndevice_id = \"dev_01ARZ3NDEKTSV4RRFFQ69G5FAV\"\n",
        )
        .unwrap();
        let plist_path = root.join("link-prod.plist");
        let mut env = Dictionary::new();
        env.insert(
            "HERDR_RUNTIME_GENERATION".to_owned(),
            PlistValue::String("rust-old".to_owned()),
        );
        env.insert(
            "HERDR_RUNTIME_VERSION".to_owned(),
            PlistValue::String("0.4.2".to_owned()),
        );
        let mut root_dict = Dictionary::new();
        root_dict.insert(
            "Label".to_owned(),
            PlistValue::String(LINK_PROD_LABEL.to_owned()),
        );
        root_dict.insert(
            "EnvironmentVariables".to_owned(),
            PlistValue::Dictionary(env),
        );
        let mut bytes = Vec::new();
        PlistValue::Dictionary(root_dict)
            .to_writer_xml(&mut bytes)
            .unwrap();
        std::fs::write(&plist_path, bytes).unwrap();

        assert!(
            refresh_prod_plist_generation(&root, &plist_path, "rust-new", Some("0.4.3-dev"),)
                .unwrap()
        );
        let updated = PlistValue::from_file(&plist_path).unwrap();
        let env = updated
            .as_dictionary()
            .unwrap()
            .get("EnvironmentVariables")
            .unwrap()
            .as_dictionary()
            .unwrap();
        assert_eq!(
            env.get("HERDR_RUNTIME_GENERATION")
                .and_then(PlistValue::as_string),
            Some("rust-new")
        );
        assert_eq!(
            env.get("HERDR_RUNTIME_VERSION")
                .and_then(PlistValue::as_string),
            Some("0.4.3-dev")
        );
        assert_eq!(
            env.get("HERDR_EDGE_URL").and_then(PlistValue::as_string),
            Some("wss://edge.example/ws")
        );
        assert_eq!(
            env.get("HERDR_WORKSTATION_ID")
                .and_then(PlistValue::as_string),
            Some("dev_01ARZ3NDEKTSV4RRFFQ69G5FAV")
        );
        assert_eq!(
            env.get("HERDR_LINK_KEYCHAIN_SERVICE")
                .and_then(PlistValue::as_string),
            Some("herdr-edge-link-dev_01ARZ3NDEKTSV4RRFFQ69G5FAV")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn prod_plist_refresh_uses_link_upstream_origin_when_configured() {
        let root = std::env::temp_dir().join(format!(
            "herdr-link-gen-refresh-upstream-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let config_dir = root.join(".config/herdr-mcp");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "[edge]\npublic_origin = \"https://custom.example\"\nlink_upstream_origin = \"https://backend.workers.dev\"\ndevice_id = \"dev_01ARZ3NDEKTSV4RRFFQ69G5FAV\"\n",
        )
        .unwrap();
        let plist_path = root.join("link-prod.plist");
        let mut env = Dictionary::new();
        env.insert(
            "HERDR_RUNTIME_GENERATION".to_owned(),
            PlistValue::String("rust-old".to_owned()),
        );
        let mut root_dict = Dictionary::new();
        root_dict.insert(
            "Label".to_owned(),
            PlistValue::String(LINK_PROD_LABEL.to_owned()),
        );
        root_dict.insert(
            "EnvironmentVariables".to_owned(),
            PlistValue::Dictionary(env),
        );
        let mut bytes = Vec::new();
        PlistValue::Dictionary(root_dict)
            .to_writer_xml(&mut bytes)
            .unwrap();
        std::fs::write(&plist_path, bytes).unwrap();

        assert!(
            refresh_prod_plist_generation(&root, &plist_path, "rust-new", Some("0.4.3-dev"),)
                .unwrap()
        );
        let updated = PlistValue::from_file(&plist_path).unwrap();
        let env = updated
            .as_dictionary()
            .unwrap()
            .get("EnvironmentVariables")
            .unwrap()
            .as_dictionary()
            .unwrap();
        assert_eq!(
            env.get("HERDR_EDGE_URL").and_then(PlistValue::as_string),
            Some("wss://backend.workers.dev/ws")
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_generation_hot_switch_does_not_restart_link() {
        let launchd = FakeLaunchd::with_loaded(LINK_PROD_LABEL, Path::new("/tmp/link-prod.plist"));
        let mut reconciles = 0;
        let mut waits = 0;
        converge_active_generation_with_fallback(
            "rust-new",
            || {
                reconciles += 1;
                Ok(())
            },
            || Ok(()),
            || launchd.kickstart_prod(LINK_PROD_LABEL),
            |_| {
                waits += 1;
                Ok(())
            },
            || Ok(ConvergenceEvidence::default()),
        )
        .unwrap();
        assert_eq!(waits, 1);
        assert_eq!(reconciles, 0);
        assert!(launchd.kickstarts().is_empty());
    }

    #[test]
    fn stale_hot_switch_restarts_owned_link_once_then_converges() {
        let launchd = FakeLaunchd::with_loaded(LINK_PROD_LABEL, Path::new("/tmp/link-prod.plist"));
        let mut reconciles = 0;
        let mut waits = 0;
        let mut probe_calls = 0;
        converge_active_generation_with_fallback(
            "rust-new",
            || {
                reconciles += 1;
                Ok(())
            },
            || Ok(()),
            || launchd.kickstart_prod(LINK_PROD_LABEL),
            |_| {
                waits += 1;
                if waits < 3 {
                    Err(format!("synthetic stale observation {waits}"))
                } else {
                    Ok(())
                }
            },
            || {
                probe_calls += 1;
                Ok(ConvergenceEvidence {
                    control_desired: Some("rust-new".to_owned()),
                    control_revision: Some(1),
                    status_processed_revision: Some(1),
                    status_outcome: Some("activated".to_owned()),
                    active_generation: Some("rust-new".to_owned()),
                    transition_seq: Some(1),
                    last_transition_to: Some("rust-new".to_owned()),
                    last_transition_outcome: Some("activated".to_owned()),
                })
            },
        )
        .unwrap();
        assert_eq!(waits, 2);
        assert_eq!(probe_calls, 1);
        assert_eq!(reconciles, 2);
        assert_eq!(launchd.kickstarts(), vec![LINK_PROD_LABEL.to_owned()]);
    }

    #[test]
    fn ownership_change_before_restart_fails_without_kickstart() {
        let launchd = FakeLaunchd::with_loaded(LINK_PROD_LABEL, Path::new("/tmp/link-prod.plist"));
        let error = converge_active_generation_with_fallback(
            "rust-new",
            || Ok(()),
            || Err("ownership changed".to_owned()),
            || -> Result<(), String> { panic!("restart must not run after ownership drift") },
            |_| Err("still stale".to_owned()),
            || Ok(ConvergenceEvidence::default()),
        )
        .unwrap_err();
        assert_eq!(error, "ownership changed");
        assert!(launchd.kickstarts().is_empty());
    }

    #[test]
    fn stale_after_single_restart_remains_a_transaction_failure() {
        let launchd = FakeLaunchd::with_loaded(LINK_PROD_LABEL, Path::new("/tmp/link-prod.plist"));
        let mut reconciles = 0;
        let error = converge_active_generation_with_fallback(
            "rust-new",
            || {
                reconciles += 1;
                Ok(())
            },
            || Ok(()),
            || launchd.kickstart_prod(LINK_PROD_LABEL),
            |_| Err("still stale".to_owned()),
            // Missing/unparseable evidence is a stall, not a direction verdict;
            // the bounded convergence phase must fail after the stall budget
            // rather than succeed or hang.
            || Ok(ConvergenceEvidence::default()),
        )
        .unwrap_err();
        assert!(error.contains("after bounded restart"));
        assert!(error.contains("stalled"));
        assert_eq!(reconciles, 2);
        assert_eq!(launchd.kickstarts(), vec![LINK_PROD_LABEL.to_owned()]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn stale_loaded_launchd_generation_reloads_owned_plist() {
        let plist = Path::new("/tmp/link-prod.plist");
        let launchd = FakeLaunchd::with_loaded(LINK_PROD_LABEL, plist);

        restart_prod_link_for_observed_generation(&launchd, plist, "rust-new", Some("rust-old"))
            .unwrap();

        assert_eq!(launchd.bootouts(), vec![LINK_PROD_LABEL.to_owned()]);
        assert_eq!(
            launchd.bootstraps(),
            vec![(LINK_PROD_LABEL.to_owned(), plist.to_path_buf())]
        );
        assert!(launchd.kickstarts().is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn aligned_loaded_launchd_generation_uses_bounded_kickstart() {
        let plist = Path::new("/tmp/link-prod.plist");
        let launchd = FakeLaunchd::with_loaded(LINK_PROD_LABEL, plist);

        restart_prod_link_for_observed_generation(&launchd, plist, "rust-new", Some("rust-new"))
            .unwrap();

        assert!(launchd.bootouts().is_empty());
        assert!(launchd.bootstraps().is_empty());
        assert_eq!(launchd.kickstarts(), vec![LINK_PROD_LABEL.to_owned()]);
    }

    #[test]
    fn late_activation_after_kickstart_succeeds_without_rollback() {
        // Regression for the v0.4.3 dev-sync late-convergence race: the Link is
        // still visibly progressing toward the expected generation (pending
        // revision, then a transition) after the hot-switch window. The final
        // bounded convergence phase must let it finish instead of rolling back.
        let mut probes = vec![
            // Poll 1: control bumped to revision 2, Link still on old generation
            // and has not consumed it yet (processed=1 < control=2). Directed.
            ConvergenceEvidence {
                control_desired: Some("rust-new".to_owned()),
                control_revision: Some(2),
                status_processed_revision: Some(1),
                status_outcome: Some("active_unchanged".to_owned()),
                active_generation: Some("rust-old".to_owned()),
                transition_seq: Some(0),
                last_transition_to: None,
                last_transition_outcome: None,
            },
            // Poll 2: Link consumed revision 2 and is retrying a transient
            // health failure while the single-port server finishes coming up.
            ConvergenceEvidence {
                control_desired: Some("rust-new".to_owned()),
                control_revision: Some(2),
                status_processed_revision: Some(2),
                status_outcome: Some("retrying:candidate_rejected:health_http_503".to_owned()),
                active_generation: Some("rust-old".to_owned()),
                transition_seq: Some(0),
                last_transition_to: None,
                last_transition_outcome: None,
            },
            // Poll 3: Link activated the expected generation.
            ConvergenceEvidence {
                control_desired: Some("rust-new".to_owned()),
                control_revision: Some(2),
                status_processed_revision: Some(2),
                status_outcome: Some("activated".to_owned()),
                active_generation: Some("rust-new".to_owned()),
                transition_seq: Some(1),
                last_transition_to: Some("rust-new".to_owned()),
                last_transition_outcome: Some("activated".to_owned()),
            },
        ];
        let mut ownership_checks = 0;
        bounded_convergence_phase(
            "rust-new",
            &mut || {
                ownership_checks += 1;
                Ok(())
            },
            &mut || Ok(probes.remove(0)),
            20,
            3,
            14,
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(ownership_checks, 1);
    }

    #[test]
    fn kickstart_restart_grace_allows_static_prestart_evidence_then_activation() {
        // Real macOS UAT showed `launchctl kickstart -k` returning while the
        // loaded Link job was still inside its 10s ThrottleInterval. The old
        // status therefore remained identical for more than three polls even
        // though the replacement Link later started and consumed the desired
        // revision. That pre-start silence must not trigger service rollback.
        let mut probes = 0usize;
        bounded_convergence_phase(
            "rust-new",
            &mut || Ok(()),
            &mut || {
                probes += 1;
                if probes <= 11 {
                    return Ok(ConvergenceEvidence {
                        control_desired: Some("rust-new".to_owned()),
                        control_revision: Some(2),
                        status_processed_revision: Some(1),
                        status_outcome: Some("active_unchanged".to_owned()),
                        active_generation: Some("rust-old".to_owned()),
                        transition_seq: Some(0),
                        last_transition_to: None,
                        last_transition_outcome: None,
                    });
                }
                Ok(ConvergenceEvidence {
                    control_desired: Some("rust-new".to_owned()),
                    control_revision: Some(2),
                    status_processed_revision: Some(2),
                    status_outcome: Some("activated".to_owned()),
                    active_generation: Some("rust-new".to_owned()),
                    transition_seq: Some(1),
                    last_transition_to: Some("rust-new".to_owned()),
                    last_transition_outcome: Some("activated".to_owned()),
                })
            },
            20,
            3,
            CONVERGENCE_RESTART_GRACE_POLLS,
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(probes, 12);
    }

    #[test]
    fn restart_grace_exceeds_loaded_link_launchd_throttle() {
        let grace = CONVERGENCE_POLL_INTERVAL
            .saturating_mul(CONVERGENCE_RESTART_GRACE_POLLS.saturating_sub(1) as u32);
        assert!(grace > Duration::from_secs(LINK_LAUNCHD_THROTTLE_SECONDS));
    }

    #[test]
    fn pending_retrying_evidence_uses_the_full_bounded_budget() {
        // A pending revision or retrying outcome can be inside the Link's own
        // bounded 30s validation RPC. The outer transaction must not preempt
        // that inner budget after only three identical polls; it still fails at
        // the overall convergence budget if the retry never resolves.
        let mut probes = 0;
        let error = bounded_convergence_phase(
            "rust-new",
            &mut || Ok(()),
            &mut || {
                probes += 1;
                Ok(ConvergenceEvidence {
                    control_desired: Some("rust-new".to_owned()),
                    control_revision: Some(2),
                    status_processed_revision: Some(1),
                    status_outcome: Some("retrying:candidate_rejected:health_http_503".to_owned()),
                    active_generation: Some("rust-old".to_owned()),
                    transition_seq: Some(0),
                    last_transition_to: None,
                    last_transition_outcome: None,
                })
            },
            7,
            3,
            3,
            Duration::ZERO,
        )
        .unwrap_err();
        assert!(error.contains("bounded convergence budget"));
        assert_eq!(probes, 7);
    }

    #[test]
    fn processed_but_static_evidence_stalls_after_tight_budget() {
        // Once the current control revision has been processed, a static
        // active-old state is no longer an in-flight validation. Keep the tight
        // stall detector for this genuinely non-progressing state.
        let mut probes = 0;
        let error = bounded_convergence_phase(
            "rust-new",
            &mut || Ok(()),
            &mut || {
                probes += 1;
                Ok(ConvergenceEvidence {
                    control_desired: Some("rust-new".to_owned()),
                    control_revision: Some(2),
                    status_processed_revision: Some(2),
                    status_outcome: Some("active_unchanged".to_owned()),
                    active_generation: Some("rust-old".to_owned()),
                    transition_seq: Some(0),
                    last_transition_to: None,
                    last_transition_outcome: None,
                })
            },
            20,
            3,
            3,
            Duration::ZERO,
        )
        .unwrap_err();
        assert!(error.contains("stalled"));
        assert_eq!(probes, 3);
    }

    #[test]
    fn ownership_drift_after_kickstart_fails_closed() {
        // Ownership must be re-proved before success. A concurrent replacement
        // between the last progress poll and the success check must fail closed
        // and never report success.
        let mut probes = 0;
        let error = bounded_convergence_phase(
            "rust-new",
            &mut || Err("production Link ownership changed".to_owned()),
            &mut || {
                probes += 1;
                Ok(ConvergenceEvidence {
                    control_desired: Some("rust-new".to_owned()),
                    control_revision: Some(2),
                    status_processed_revision: Some(2),
                    status_outcome: Some("activated".to_owned()),
                    active_generation: Some("rust-new".to_owned()),
                    transition_seq: Some(1),
                    last_transition_to: Some("rust-new".to_owned()),
                    last_transition_outcome: Some("activated".to_owned()),
                })
            },
            20,
            3,
            3,
            Duration::ZERO,
        )
        .unwrap_err();
        assert!(error.contains("ownership changed"));
        assert_eq!(probes, 1);
    }

    #[test]
    fn evidence_changes_forever_but_never_activates_fails_after_max_polls() {
        // The Link keeps making forward progress (revision/transition advance)
        // but never reaches the expected generation. The budget must be bounded
        // and fail after exactly max_polls, not wait forever.
        let mut probes = 0;
        let error = bounded_convergence_phase(
            "rust-new",
            &mut || Ok(()),
            &mut || {
                probes += 1;
                Ok(ConvergenceEvidence {
                    control_desired: Some("rust-new".to_owned()),
                    control_revision: Some(2),
                    status_processed_revision: Some(1),
                    status_outcome: Some("retrying:candidate_rejected:health_http_503".to_owned()),
                    active_generation: Some("rust-old".to_owned()),
                    transition_seq: Some(probes as u64),
                    last_transition_to: Some("rust-new".to_owned()),
                    last_transition_outcome: Some("rolled_back".to_owned()),
                })
            },
            5,
            3,
            3,
            Duration::ZERO,
        )
        .unwrap_err();
        assert!(error.contains("bounded convergence budget"));
        assert_eq!(probes, 5);
    }

    #[test]
    fn non_retryable_outcome_fails_immediately() {
        // A definitive, non-retryable failure of the target candidate must fail
        // immediately rather than waiting out the budget.
        let mut probes = 0;
        let error = bounded_convergence_phase(
            "rust-new",
            &mut || Ok(()),
            &mut || {
                probes += 1;
                Ok(ConvergenceEvidence {
                    control_desired: Some("rust-new".to_owned()),
                    control_revision: Some(2),
                    status_processed_revision: Some(2),
                    status_outcome: Some("candidate_rejected:contract_mismatch".to_owned()),
                    active_generation: Some("rust-old".to_owned()),
                    transition_seq: Some(0),
                    last_transition_to: None,
                    last_transition_outcome: None,
                })
            },
            20,
            3,
            3,
            Duration::ZERO,
        )
        .unwrap_err();
        assert!(error.contains("definitive failure"));
        assert_eq!(probes, 1);
    }

    #[test]
    fn retryable_health_outcome_is_not_a_definitive_failure() {
        let evidence = ConvergenceEvidence {
            control_desired: Some("rust-new".to_owned()),
            control_revision: Some(2),
            status_processed_revision: Some(2),
            status_outcome: Some("retrying:candidate_rejected:health_http_503".to_owned()),
            active_generation: Some("rust-old".to_owned()),
            transition_seq: Some(0),
            last_transition_to: None,
            last_transition_outcome: None,
        };
        assert!(evidence.definitive_failure().is_none());
        assert!(evidence.directed_toward("rust-new"));
    }

    #[test]
    fn named_instance_is_always_a_noop() {
        let _guard = crate::test_env::lock();
        let previous = std::env::var_os("HERDR_MCP_INSTANCE");
        unsafe { std::env::set_var("HERDR_MCP_INSTANCE", "uat") };
        let paths = RuntimePaths::discover().unwrap();
        assert!(paths.instance.is_named());
        reconcile_after_service_generation_change(&paths).unwrap();
        unsafe {
            match previous {
                Some(value) => std::env::set_var("HERDR_MCP_INSTANCE", value),
                None => std::env::remove_var("HERDR_MCP_INSTANCE"),
            }
        }
    }
}
