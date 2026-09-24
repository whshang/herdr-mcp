//! Platform service manager for the Rust local runtime.
//!
//! macOS uses launchd; Linux uses `systemd --user` when available and an
//! ownership-checked detached user-process fallback in init-less environments.
//! Windows uses a current-user Startup-folder shortcut plus ownership-checked detached user processes.
//! On macOS, the default production launchd label stays `dev.herdr-mcp.server`
//! so callers and the browser extension keep a single primary service identity.
//! Named instances (`HERDR_MCP_INSTANCE` / `--instance`) suffix labels and ports
//! for same-uid UAT and never rewrite `~/.local/bin/herdr-mcp`.
//! A Rust install is content-addressed under `runtime/generations/` and launchd
//! points at the stable `runtime/current/herdr-mcp` path. Replacing an existing
//! Node service is explicit (`service install --adopt-node`) and transactional:
//! the old server/watchdog plists and generation pointer are restored if the
//! Rust service cannot bootstrap and pass `/health`.

use crate::cli::ServiceCommand;
use serde_json::Value;
use std::process::ExitCode;

pub fn run(command: ServiceCommand) -> Result<ExitCode, String> {
    #[cfg(target_os = "linux")]
    {
        crate::linux_service_manager::run(command)
    }

    #[cfg(target_os = "windows")]
    {
        crate::windows_service_manager::run(command)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = command;
        Err("service manager is unsupported on this platform".to_owned())
    }

    #[cfg(target_os = "macos")]
    {
        macos::run(command)
    }
}

pub(crate) struct ServiceMutationLease {
    #[cfg(target_os = "macos")]
    inner: macos::ServiceMutationLock,
}

pub(crate) fn acquire_mutation_lock() -> Result<ServiceMutationLease, String> {
    #[cfg(not(target_os = "macos"))]
    {
        Err("service_manager_currently_requires_macos".to_owned())
    }
    #[cfg(target_os = "macos")]
    {
        Ok(ServiceMutationLease {
            inner: macos::acquire_mutation_lock()?,
        })
    }
}

pub(crate) fn run_with_mutation_lock(
    command: ServiceCommand,
    mutation_lock: &ServiceMutationLease,
) -> Result<ExitCode, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (command, mutation_lock);
        Err("service_manager_currently_requires_macos".to_owned())
    }
    #[cfg(target_os = "macos")]
    {
        macos::run_with_mutation_lock(command, &mutation_lock.inner)
    }
}

/// Crate-internal install-from-payload seam: the current orchestrator owns the
/// entire service lifecycle transaction while the installed generation bytes
/// come from an explicit payload path (used by `dev rollback` to restore a
/// pinned older PROD binary without executing that older binary as the
/// orchestrator). Normal public `service install` never routes through this
/// seam; it keeps `current_exe` as both orchestrator and payload.
pub(crate) fn run_install_from_payload(
    adopt_node: bool,
    payload_binary: &std::path::Path,
    mutation_lock: &ServiceMutationLease,
) -> Result<ExitCode, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (adopt_node, payload_binary, mutation_lock);
        Err("service_manager_currently_requires_macos".to_owned())
    }
    #[cfg(target_os = "macos")]
    {
        macos::run_install_from_payload(adopt_node, payload_binary, &mutation_lock.inner)
    }
}

/// Return the managed Rust binary targeted by the current ready rollback.
/// This is read-only preflight data; `rollback()` independently revalidates the
/// ledger and target immediately before mutation.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub fn rollback_target_runtime_binary() -> Result<Option<std::path::PathBuf>, String> {
    #[cfg(not(target_os = "macos"))]
    {
        Ok(None)
    }

    #[cfg(target_os = "macos")]
    {
        macos::rollback_target_runtime_binary()
    }
}

/// Read-only service ownership snapshot for `doctor`. Never mutates launchd.
pub fn doctor_status() -> Result<serde_json::Value, String> {
    #[cfg(target_os = "linux")]
    {
        crate::linux_service_manager::doctor_status()
    }

    #[cfg(target_os = "windows")]
    {
        crate::windows_service_manager::doctor_status()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Ok(serde_json::json!({
            "ok": false,
            "implementation": "unsupported",
            "detail": "service manager is unsupported on this platform",
        }))
    }

    #[cfg(target_os = "macos")]
    {
        macos::doctor_status()
    }
}

/// Return the active local runtime bearer only for an in-process doctor probe.
/// The caller must never print, serialize, or otherwise expose this value.
pub(crate) fn doctor_runtime_token() -> Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    let managed_token = macos::doctor_runtime_token();

    #[cfg(target_os = "linux")]
    let managed_token = crate::linux_service_manager::doctor_runtime_token();

    #[cfg(target_os = "windows")]
    let managed_token = crate::windows_service_manager::doctor_runtime_token();

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    if let Ok(Some(token)) = &managed_token {
        return Ok(Some(token.clone()));
    }

    if let Ok(token) = std::env::var("HERDR_MCP_TOKEN")
        && !token.trim().is_empty()
    {
        return Ok(Some(token));
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Ok(None)
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        managed_token
    }
}

/// How to compensate a committed service install when a post-commit step fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
enum InstallPostCommitRecovery {
    /// The install was a no-op (already active); nothing to compensate.
    None,
    /// The install replaced a prior service; roll back to the previous generation.
    Rollback,
    /// The install created a fresh service; uninstall it.
    Uninstall,
}

/// Decide the compensation for a committed install from its result alone.
///
/// A no-op install (`already_active`) changed nothing and needs no recovery. A
/// real install that produced a `rollback_id` replaced a prior service and is
/// recovered by rollback; a real install with no `rollback_id` was a fresh
/// install and is recovered by uninstall.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn install_post_commit_recovery(result: &Value) -> InstallPostCommitRecovery {
    if result.get("already_active").and_then(Value::as_bool) == Some(true) {
        return InstallPostCommitRecovery::None;
    }
    if result.get("rollback_id").is_some_and(Value::is_null) {
        InstallPostCommitRecovery::Uninstall
    } else {
        InstallPostCommitRecovery::Rollback
    }
}

/// Run the fail-fast post-commit sequence for a committed service install.
///
/// Order is fixed: product identity persistence, then update-uninstall fence
/// clearing, then checked auto-update scheduler setup. Any failure is
/// propagated (never silently swallowed). When the install actually changed the
/// service, the committed service change is compensated via rollback or
/// uninstall first; only after that compensation succeeds are the pre-install
/// integration snapshots (identity marker, service-uninstall fence, auto-update
/// scheduler) restored, so a failed post-commit step can never leave an
/// unfenced scheduler able to resurrect a service that compensation just
/// uninstalled. Any compensation or restore failure is reported alongside the
/// original error.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn install_post_commit<Identity, Fence, Scheduler, Rollback, Uninstall, Restore>(
    result: &Value,
    identity: Identity,
    fence: Fence,
    scheduler: Scheduler,
    rollback: Rollback,
    uninstall: Uninstall,
    restore_integrations: Restore,
) -> Result<Value, String>
where
    Identity: FnOnce() -> Result<(), String>,
    Fence: FnOnce() -> Result<(), String>,
    Scheduler: FnOnce() -> Result<Value, String>,
    Rollback: FnOnce() -> Result<(), String>,
    Uninstall: FnOnce() -> Result<(), String>,
    Restore: FnOnce() -> Result<(), String>,
{
    let recovery = install_post_commit_recovery(result);
    let post_commit = (|| -> Result<Value, String> {
        if result.get("ok").and_then(Value::as_bool) == Some(true) {
            identity()?;
        }
        fence()?;
        scheduler()
    })();
    match post_commit {
        Ok(scheduler) => Ok(scheduler),
        Err(error) => {
            let compensation = match recovery {
                InstallPostCommitRecovery::None => Ok(()),
                InstallPostCommitRecovery::Rollback => rollback(),
                InstallPostCommitRecovery::Uninstall => uninstall(),
            };
            // Restore integrations only after service compensation succeeded.
            let restore = match compensation {
                Ok(()) => restore_integrations(),
                Err(compensation_error) => Err(format!(
                    "compensating service recovery failed: {compensation_error}"
                )),
            };
            Err(match restore {
                Ok(()) => format!(
                    "service install committed but a post-commit step failed and the service change was recovered ({recovery:?}): {error}"
                ),
                Err(restore_error) => format!(
                    "service install committed but a post-commit step failed: {error}; compensating service recovery and integration restore also failed: {restore_error}"
                ),
            })
        }
    }
}

#[cfg(test)]
mod post_commit_tests {
    use super::*;
    use serde_json::json;

    fn fresh_install_result() -> Value {
        json!({
            "ok": true,
            "implementation": "rust",
            "generation": "rust-abc",
            "rollback_id": Value::Null,
            "already_active": false,
        })
    }

    fn upgrade_install_result() -> Value {
        json!({
            "ok": true,
            "implementation": "rust",
            "generation": "rust-abc",
            "rollback_id": "rb-123-rust-abc",
        })
    }

    fn noop_install_result() -> Value {
        json!({
            "ok": true,
            "implementation": "rust",
            "already_active": true,
            "rollback_id": Value::Null,
        })
    }

    #[test]
    fn recovery_decision_matches_install_shape() {
        assert_eq!(
            install_post_commit_recovery(&noop_install_result()),
            InstallPostCommitRecovery::None
        );
        assert_eq!(
            install_post_commit_recovery(&fresh_install_result()),
            InstallPostCommitRecovery::Uninstall
        );
        assert_eq!(
            install_post_commit_recovery(&upgrade_install_result()),
            InstallPostCommitRecovery::Rollback
        );
    }

    #[test]
    fn identity_failure_propagates_and_compensates_upgrade_with_rollback() {
        let mut rolled_back = false;
        let mut uninstalled = false;
        let mut restored = false;
        let err = install_post_commit(
            &upgrade_install_result(),
            || Err("identity boom".to_owned()),
            || Ok(()),
            || Ok(json!({ "ok": true })),
            || {
                rolled_back = true;
                Ok(())
            },
            || {
                uninstalled = true;
                Ok(())
            },
            || {
                restored = true;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(err.contains("identity boom"));
        assert!(err.contains("Rollback"));
        assert!(rolled_back);
        assert!(!uninstalled);
        assert!(restored, "integrations must be restored after compensation");
    }

    #[test]
    fn identity_failure_propagates_and_compensates_fresh_with_uninstall() {
        let mut rolled_back = false;
        let mut uninstalled = false;
        let mut restored = false;
        let err = install_post_commit(
            &fresh_install_result(),
            || Err("identity boom".to_owned()),
            || Ok(()),
            || Ok(json!({ "ok": true })),
            || {
                rolled_back = true;
                Ok(())
            },
            || {
                uninstalled = true;
                Ok(())
            },
            || {
                restored = true;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(err.contains("identity boom"));
        assert!(err.contains("Uninstall"));
        assert!(!rolled_back);
        assert!(uninstalled);
        assert!(restored);
    }

    #[test]
    fn fence_failure_propagates_and_compensates() {
        let mut rolled_back = false;
        let mut restored = false;
        let err = install_post_commit(
            &upgrade_install_result(),
            || Ok(()),
            || Err("fence boom".to_owned()),
            || Ok(json!({ "ok": true })),
            || {
                rolled_back = true;
                Ok(())
            },
            || unreachable!("fresh install must not be chosen for upgrade"),
            || {
                restored = true;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(err.contains("fence boom"));
        assert!(rolled_back);
        assert!(restored);
    }

    #[test]
    fn scheduler_failure_propagates_and_compensates() {
        let mut uninstalled = false;
        let mut restored = false;
        let err = install_post_commit(
            &fresh_install_result(),
            || Ok(()),
            || Ok(()),
            || Err("scheduler boom".to_owned()),
            || unreachable!("upgrade must not be chosen for fresh install"),
            || {
                uninstalled = true;
                Ok(())
            },
            || {
                restored = true;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(err.contains("scheduler boom"));
        assert!(uninstalled);
        assert!(restored);
    }

    #[test]
    fn noop_failure_propagates_without_service_compensation_but_restores_integrations() {
        let compensated = std::cell::Cell::new(false);
        let restored = std::cell::Cell::new(false);
        let err = install_post_commit(
            &noop_install_result(),
            || Err("identity boom".to_owned()),
            || Ok(()),
            || Ok(json!({ "ok": true })),
            || {
                compensated.set(true);
                Ok(())
            },
            || {
                compensated.set(true);
                Ok(())
            },
            || {
                restored.set(true);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(err.contains("identity boom"));
        assert!(
            !compensated.get(),
            "a no-op install must not compensate the service"
        );
        assert!(
            restored.get(),
            "a no-op install must still restore prior integration state"
        );
    }

    #[test]
    fn success_returns_scheduler_without_compensation_or_restore() {
        let compensated = std::cell::Cell::new(false);
        let restored = std::cell::Cell::new(false);
        let scheduler = install_post_commit(
            &upgrade_install_result(),
            || Ok(()),
            || Ok(()),
            || Ok(json!({ "ok": true, "installed": true })),
            || {
                compensated.set(true);
                Ok(())
            },
            || {
                compensated.set(true);
                Ok(())
            },
            || {
                restored.set(true);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            scheduler.get("installed").and_then(Value::as_bool),
            Some(true)
        );
        assert!(!compensated.get());
        assert!(!restored.get());
    }

    #[test]
    fn compensation_failure_is_reported_alongside_original_error() {
        let err = install_post_commit(
            &upgrade_install_result(),
            || Ok(()),
            || Err("fence boom".to_owned()),
            || Ok(json!({ "ok": true })),
            || Err("rollback boom".to_owned()),
            || unreachable!(),
            || unreachable!("restore must not run when compensation failed"),
        )
        .unwrap_err();
        assert!(err.contains("fence boom"));
        assert!(err.contains("rollback boom"));
    }

    #[test]
    fn restore_failure_is_reported_alongside_original_error() {
        let err = install_post_commit(
            &upgrade_install_result(),
            || Ok(()),
            || Err("fence boom".to_owned()),
            || Ok(json!({ "ok": true })),
            || Ok(()),
            || unreachable!(),
            || Err("restore boom".to_owned()),
        )
        .unwrap_err();
        assert!(err.contains("fence boom"));
        assert!(err.contains("restore boom"));
    }

    #[test]
    fn identity_success_then_fence_failure_restores_integrations() {
        let mut identity_called = false;
        let mut restored = false;
        let err = install_post_commit(
            &upgrade_install_result(),
            || {
                identity_called = true;
                Ok(())
            },
            || Err("fence boom".to_owned()),
            || Ok(json!({ "ok": true })),
            || Ok(()),
            || unreachable!(),
            || {
                restored = true;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(identity_called, "identity must run before the fence step");
        assert!(err.contains("fence boom"));
        assert!(restored);
    }

    #[test]
    fn identity_and_fence_success_then_scheduler_failure_restores_integrations() {
        let mut identity_called = false;
        let mut fence_called = false;
        let mut restored = false;
        let err = install_post_commit(
            &fresh_install_result(),
            || {
                identity_called = true;
                Ok(())
            },
            || {
                fence_called = true;
                Ok(())
            },
            || Err("scheduler boom".to_owned()),
            || unreachable!(),
            || Ok(()),
            || {
                restored = true;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(identity_called);
        assert!(fence_called);
        assert!(err.contains("scheduler boom"));
        assert!(restored);
    }
}

#[cfg(target_os = "macos")]
mod macos;
