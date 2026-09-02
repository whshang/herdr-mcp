use crate::cli::ServiceCommand;
#[cfg(target_os = "macos")]
use crate::native_host_install;
use crate::{herdr_supervisor, link, paths::RuntimePaths, service_manager};
use serde_json::Value;
use std::path::Path;
use std::process::ExitCode;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServiceSnapshot {
    implementation: String,
    current_target: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallRecovery {
    None,
    Rollback,
    Uninstall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RollbackSupervisorStrategy {
    Preserve,
    Remove,
}

fn retry_sidecar_once<F>(label: &str, mut operation: F) -> Result<(), String>
where
    F: FnMut() -> Result<(), String>,
{
    match operation() {
        Ok(()) => Ok(()),
        Err(first) => operation().map_err(|second| {
            format!("{label} failed: {first}; bounded retry also failed: {second}")
        }),
    }
}

pub(crate) fn run(command: ServiceCommand) -> Result<ExitCode, String> {
    match command {
        ServiceCommand::Install { adopt_node } => run_install(adopt_node),
        ServiceCommand::Rollback => run_rollback(),
        ServiceCommand::Uninstall => run_uninstall(),
        other => service_manager::run(other),
    }
}

/// Shared install lifecycle for the public `service install` path. The
/// orchestrator is the executing binary; the installed payload is the same
/// executable (`current_exe` inside `service_manager`).
fn run_install(adopt_node: bool) -> Result<ExitCode, String> {
    run_install_lifecycle(|mutation_lock| {
        service_manager::run_with_mutation_lock(
            ServiceCommand::Install { adopt_node },
            mutation_lock,
        )
    })
}

/// Crate-internal install-from-payload path used by `dev rollback`: the
/// current orchestrator owns the entire service lifecycle transaction (mutation
/// lock, Herdr supervisor, product identity/update fence, production Link
/// generation reconcile, native-host sync, compensation) while the installed
/// generation bytes come from the pinned payload path. The payload is data
/// only; it is never executed as the orchestrator.
pub(crate) fn run_install_from_payload(
    adopt_node: bool,
    payload_binary: &Path,
) -> Result<ExitCode, String> {
    refuse_sidecar_mutation_inside_managed_exec()?;
    run_install_lifecycle(|mutation_lock| {
        service_manager::run_install_from_payload(adopt_node, payload_binary, mutation_lock)
    })
}

/// One shared install lifecycle. The mutation lock is acquired, the pre-commit
/// supervisor/service snapshot is captured, the install commit itself runs via
/// `install` (either `current_exe` or an explicit payload), and then the
/// post-commit sidecar orchestration (Herdr supervisor, product identity/update
/// fence, production Link generation reconcile, native-host sync, compensation)
/// completes inside `finish_install_lifecycle`.
fn run_install_lifecycle<Install>(install: Install) -> Result<ExitCode, String>
where
    Install: FnOnce(&service_manager::ServiceMutationLease) -> Result<ExitCode, String>,
{
    let mutation_lock = service_manager::acquire_mutation_lock()?;
    let before_service = service_snapshot()?;
    let before_supervisor = herdr_supervisor::capture_install_state_for_service()?;
    herdr_supervisor::preflight_install_for_service()?;

    #[cfg(target_os = "macos")]
    {
        let paths = RuntimePaths::discover()?;
        crate::macos_permissions::preserve_or_install_broker(&paths.config_dir)
            .map_err(|error| format!("macOS TCC broker install preflight failed: {error}"))?;
        crate::macos_credential_helper::preserve_or_install(&paths.config_dir).map_err(
            |error| format!("macOS credential helper install preflight failed: {error}"),
        )?;
        crate::macos_credential_helper::prewarm_existing_default(&paths.config_dir)
            .map_err(|error| format!("macOS credential helper preflight failed: {error}"))?;
    }

    let result = install(&mutation_lock)?;
    finish_install_lifecycle(before_service, before_supervisor, &mutation_lock, result)
}

/// The lifecycle after the service commit itself, shared by the public install
/// and the internal install-from-payload path so the sidecar orchestration
/// (guardian, Herdr supervisor, product identity/update fence, production Link
/// generation reconcile, native-host sync, compensation) never diverges.
fn finish_install_lifecycle(
    before_service: ServiceSnapshot,
    before_supervisor: herdr_supervisor::InstallState,
    mutation_lock: &service_manager::ServiceMutationLease,
    result: ExitCode,
) -> Result<ExitCode, String> {
    if result != ExitCode::SUCCESS {
        return Ok(result);
    }

    if let Err(supervisor_error) = herdr_supervisor::ensure_installed_for_service() {
        let after_service = service_snapshot()?;
        let recovery = install_recovery(&before_service, &after_service);
        let cleanup_error = herdr_supervisor::remove_for_service().err();
        let service_recovery = match recovery {
            InstallRecovery::None => Ok(ExitCode::SUCCESS),
            InstallRecovery::Rollback => {
                service_manager::run_with_mutation_lock(ServiceCommand::Rollback, mutation_lock)
            }
            InstallRecovery::Uninstall => {
                service_manager::run_with_mutation_lock(ServiceCommand::Uninstall, mutation_lock)
            }
        };

        let service_recovered = matches!(service_recovery, Ok(code) if code == ExitCode::SUCCESS);
        let supervisor_restore =
            herdr_supervisor::restore_install_state_for_service(before_supervisor);

        if service_recovered && supervisor_restore.is_ok() && cleanup_error.is_none() {
            return Err(format!(
                "service install post-commit Herdr supervisor activation failed and the service change was recovered ({recovery:?}): {supervisor_error}"
            ));
        }

        let service_detail = match service_recovery {
            Ok(code) => format!("service recovery exited with {code:?}"),
            Err(error) => format!("service recovery failed: {error}"),
        };
        let cleanup_detail = cleanup_error
            .map(|error| format!("; supervisor cleanup failed: {error}"))
            .unwrap_or_default();
        let restore_detail = supervisor_restore
            .err()
            .map(|error| format!("; previous supervisor state restore failed: {error}"))
            .unwrap_or_default();
        if recovery != InstallRecovery::None && service_recovered {
            return Err(format!(
                "service install was rolled back but supervisor recovery is incomplete after Herdr supervisor activation failed: {supervisor_error}; {service_detail}{cleanup_detail}{restore_detail}"
            ));
        }
        return Err(format!(
            "service install committed but Herdr supervisor activation failed: {supervisor_error}; {service_detail}{cleanup_detail}{restore_detail}"
        ));
    }

    let paths = RuntimePaths::discover()?;
    if let Err(link_error) = link::reconcile_after_service_generation_change(&paths) {
        let after_service = service_snapshot()?;
        let recovery = install_recovery(&before_service, &after_service);
        let service_recovery = match recovery {
            InstallRecovery::None => Ok(ExitCode::SUCCESS),
            InstallRecovery::Rollback => {
                service_manager::run_with_mutation_lock(ServiceCommand::Rollback, mutation_lock)
            }
            InstallRecovery::Uninstall => {
                service_manager::run_with_mutation_lock(ServiceCommand::Uninstall, mutation_lock)
            }
        };
        let service_recovered = matches!(service_recovery, Ok(code) if code == ExitCode::SUCCESS);
        let supervisor_restore = if service_recovered {
            herdr_supervisor::restore_install_state_for_service(before_supervisor)
        } else {
            Ok(())
        };
        let link_restore = if service_recovered && recovery != InstallRecovery::None {
            retry_sidecar_once("production Link restore", || {
                RuntimePaths::discover()
                    .and_then(|paths| link::reconcile_after_service_generation_change(&paths))
            })
        } else {
            Ok(())
        };

        if service_recovered && supervisor_restore.is_ok() && link_restore.is_ok() {
            return Err(format!(
                "service install post-commit production Link generation reconcile failed and the service change was recovered ({recovery:?}): {link_error}"
            ));
        }

        let service_detail = match service_recovery {
            Ok(code) => format!("service recovery exited with {code:?}"),
            Err(error) => format!("service recovery failed: {error}"),
        };
        let supervisor_detail = supervisor_restore
            .err()
            .map(|error| format!("; previous supervisor state restore failed: {error}"))
            .unwrap_or_default();
        let link_detail = link_restore
            .err()
            .map(|error| format!("; production Link restore failed: {error}"))
            .unwrap_or_default();
        if recovery != InstallRecovery::None && service_recovered {
            return Err(format!(
                "service install was rolled back but sidecar recovery is incomplete after production Link generation reconcile failed: {link_error}; {service_detail}{supervisor_detail}{link_detail}"
            ));
        }
        return Err(format!(
            "service install committed but production Link generation reconcile failed: {link_error}; {service_detail}{supervisor_detail}{link_detail}"
        ));
    }

    #[cfg(target_os = "macos")]
    {
        if !paths.instance.is_named()
            && let Err(native_host_error) = native_host_install::sync_owned_runtime_from_active()
        {
            let after_service = service_snapshot()?;
            let recovery = install_recovery(&before_service, &after_service);
            let service_recovery = match recovery {
                InstallRecovery::None => Ok(ExitCode::SUCCESS),
                InstallRecovery::Rollback => {
                    service_manager::run_with_mutation_lock(ServiceCommand::Rollback, mutation_lock)
                }
                InstallRecovery::Uninstall => service_manager::run_with_mutation_lock(
                    ServiceCommand::Uninstall,
                    mutation_lock,
                ),
            };
            let service_recovered =
                matches!(service_recovery, Ok(code) if code == ExitCode::SUCCESS);
            let supervisor_restore = if service_recovered && recovery != InstallRecovery::None {
                herdr_supervisor::restore_install_state_for_service(before_supervisor)
            } else {
                Ok(())
            };
            let link_restore = if service_recovered && recovery != InstallRecovery::None {
                retry_sidecar_once("production Link restore", || {
                    RuntimePaths::discover()
                        .and_then(|paths| link::reconcile_after_service_generation_change(&paths))
                })
            } else {
                Ok(())
            };
            let native_host_restore = if service_recovered && recovery != InstallRecovery::None {
                retry_sidecar_once("native-host restore", || {
                    native_host_install::sync_owned_runtime_from_active().map(|_| ())
                })
            } else {
                Ok(())
            };

            if recovery != InstallRecovery::None
                && service_recovered
                && supervisor_restore.is_ok()
                && link_restore.is_ok()
                && native_host_restore.is_ok()
            {
                return Err(format!(
                    "service install post-commit owned native-host runtime sync failed and the service change was recovered ({recovery:?}): {native_host_error}"
                ));
            }

            let service_detail = match service_recovery {
                Ok(code) => format!("service recovery exited with {code:?}"),
                Err(error) => format!("service recovery failed: {error}"),
            };
            let supervisor_detail = supervisor_restore
                .err()
                .map(|error| format!("; previous supervisor state restore failed: {error}"))
                .unwrap_or_default();
            let link_detail = link_restore
                .err()
                .map(|error| format!("; production Link restore failed: {error}"))
                .unwrap_or_default();
            let native_host_detail = native_host_restore
                .err()
                .map(|error| format!("; native-host restore failed: {error}"))
                .unwrap_or_default();
            if recovery != InstallRecovery::None && service_recovered {
                return Err(format!(
                    "service install was rolled back but sidecar recovery is incomplete after owned native-host runtime sync failed: {native_host_error}; {service_detail}{supervisor_detail}{link_detail}{native_host_detail}"
                ));
            }
            return Err(format!(
                "service install committed but owned native-host runtime sync failed: {native_host_error}; {service_detail}{supervisor_detail}{link_detail}{native_host_detail}"
            ));
        }

        crate::macos_permissions::post_service_install_onboarding(paths.instance.is_named());
    }

    Ok(result)
}

fn run_rollback() -> Result<ExitCode, String> {
    refuse_sidecar_mutation_inside_managed_exec()?;
    let mutation_lock = service_manager::acquire_mutation_lock()?;
    let before_supervisor = herdr_supervisor::capture_install_state_for_service()?;
    let target_supports_supervisor = match service_manager::rollback_target_runtime_binary()? {
        Some(binary) => herdr_supervisor::runtime_binary_supports_supervisor(&binary)?,
        None => false,
    };
    let strategy = rollback_supervisor_strategy(before_supervisor, target_supports_supervisor);
    if strategy == RollbackSupervisorStrategy::Remove {
        herdr_supervisor::remove_for_service()?;
    }

    match service_manager::run_with_mutation_lock(ServiceCommand::Rollback, &mutation_lock) {
        Ok(code) if code == ExitCode::SUCCESS => {
            if strategy == RollbackSupervisorStrategy::Preserve {
                // The existing daemon stays alive in memory and retains its
                // owned Herdr Child. Its launchd command points at runtime/current,
                // so any later daemon restart naturally uses the rolled-back binary.
                herdr_supervisor::ensure_installed_for_service().map_err(|error| {
                    format!(
                        "service rollback committed but preserved Herdr supervisor validation failed: {error}"
                    )
                })?;
            } else {
                herdr_supervisor::reconcile_after_service_rollback().map_err(|error| {
                    format!(
                        "service rollback committed but Herdr supervisor reconciliation failed: {error}"
                    )
                })?;
            }
            let paths = RuntimePaths::discover()?;
            retry_sidecar_once("production Link reconcile after service rollback", || {
                link::reconcile_after_service_generation_change(&paths)
            })
            .map_err(|error| {
                format!("service rollback completed but sidecars are incomplete: {error}")
            })?;
            #[cfg(target_os = "macos")]
            if !paths.instance.is_named() {
                retry_sidecar_once(
                    "owned native-host runtime sync after service rollback",
                    || native_host_install::sync_owned_runtime_from_active().map(|_| ()),
                )
                .map_err(|error| {
                    format!("service rollback completed but sidecars are incomplete: {error}")
                })?;
            }
            Ok(code)
        }
        Ok(code) => {
            if strategy == RollbackSupervisorStrategy::Remove {
                restore_after_failed_service_mutation(before_supervisor, None)?;
            }
            Ok(code)
        }
        Err(error) => {
            if strategy == RollbackSupervisorStrategy::Remove {
                restore_after_failed_service_mutation(before_supervisor, Some(&error))?;
            }
            Err(error)
        }
    }
}

fn rollback_supervisor_strategy(
    current: herdr_supervisor::InstallState,
    target_supports_supervisor: bool,
) -> RollbackSupervisorStrategy {
    if current.loaded && current.present && target_supports_supervisor {
        RollbackSupervisorStrategy::Preserve
    } else {
        RollbackSupervisorStrategy::Remove
    }
}

fn run_uninstall() -> Result<ExitCode, String> {
    refuse_sidecar_mutation_inside_managed_exec()?;
    let mutation_lock = service_manager::acquire_mutation_lock()?;
    let before_supervisor = herdr_supervisor::capture_install_state_for_service()?;
    herdr_supervisor::remove_for_service()?;

    match service_manager::run_with_mutation_lock(ServiceCommand::Uninstall, &mutation_lock) {
        Ok(code) if code == ExitCode::SUCCESS => Ok(code),
        Ok(code) => {
            restore_after_failed_service_mutation(before_supervisor, None)?;
            Ok(code)
        }
        Err(error) => {
            restore_after_failed_service_mutation(before_supervisor, Some(&error))?;
            Err(error)
        }
    }
}

fn restore_after_failed_service_mutation(
    before_supervisor: herdr_supervisor::InstallState,
    original_error: Option<&str>,
) -> Result<(), String> {
    herdr_supervisor::restore_install_state_for_service(before_supervisor).map_err(|restore_error| {
        match original_error {
            Some(original_error) => format!(
                "service mutation failed: {original_error}; restoring the previous Herdr supervisor state also failed: {restore_error}"
            ),
            None => format!(
                "service mutation returned failure and restoring the previous Herdr supervisor state also failed: {restore_error}"
            ),
        }
    })
}

fn refuse_sidecar_mutation_inside_managed_exec() -> Result<(), String> {
    if std::env::var_os("HERDR_MCP_EXEC_ID").is_some() {
        return Err(
            "service mutations cannot run inside a managed herdr_exec session; run the command from an independent terminal so supervisor/service lifecycle changes cannot sever their own control path"
                .to_owned(),
        );
    }
    Ok(())
}

fn service_snapshot() -> Result<ServiceSnapshot, String> {
    snapshot_from_status(&service_manager::doctor_status()?)
}

fn snapshot_from_status(status: &Value) -> Result<ServiceSnapshot, String> {
    let implementation = status
        .get("implementation")
        .and_then(Value::as_str)
        .ok_or_else(|| "service status is missing implementation".to_owned())?;
    let current_target = status
        .get("current_target")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(ServiceSnapshot {
        implementation: implementation.to_owned(),
        current_target,
    })
}

fn install_recovery(before: &ServiceSnapshot, after: &ServiceSnapshot) -> InstallRecovery {
    if before == after {
        return InstallRecovery::None;
    }
    if before.implementation == "missing" {
        InstallRecovery::Uninstall
    } else {
        InstallRecovery::Rollback
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn install_recovery_is_transactional_by_previous_service_state() {
        let missing = ServiceSnapshot {
            implementation: "missing".to_owned(),
            current_target: None,
        };
        let v1 = ServiceSnapshot {
            implementation: "rust".to_owned(),
            current_target: Some("generations/rust-v1".to_owned()),
        };
        let v2 = ServiceSnapshot {
            implementation: "rust".to_owned(),
            current_target: Some("generations/rust-v2".to_owned()),
        };
        assert_eq!(install_recovery(&v1, &v1), InstallRecovery::None);
        assert_eq!(install_recovery(&v1, &v2), InstallRecovery::Rollback);
        assert_eq!(install_recovery(&missing, &v2), InstallRecovery::Uninstall);
    }

    #[test]
    fn compatible_loaded_supervisor_is_preserved_across_rollback() {
        let loaded = herdr_supervisor::InstallState {
            present: true,
            loaded: true,
        };
        assert_eq!(
            rollback_supervisor_strategy(loaded, true),
            RollbackSupervisorStrategy::Preserve
        );
        assert_eq!(
            rollback_supervisor_strategy(loaded, false),
            RollbackSupervisorStrategy::Remove
        );
        assert_eq!(
            rollback_supervisor_strategy(
                herdr_supervisor::InstallState {
                    present: true,
                    loaded: false,
                },
                true,
            ),
            RollbackSupervisorStrategy::Remove
        );
    }

    #[test]
    fn sidecar_retry_is_bounded_to_one_retry() {
        let mut attempts = 0;
        retry_sidecar_once("synthetic", || {
            attempts += 1;
            if attempts == 1 {
                Err("first".to_owned())
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(attempts, 2);

        let mut failed_attempts = 0;
        let error = retry_sidecar_once("synthetic", || {
            failed_attempts += 1;
            Err(format!("failure-{failed_attempts}"))
        })
        .unwrap_err();
        assert_eq!(failed_attempts, 2);
        assert!(error.contains("failure-1"));
        assert!(error.contains("failure-2"));
    }

    #[test]
    fn service_snapshot_uses_only_stable_status_fields() {
        let snapshot = snapshot_from_status(&json!({
            "implementation": "rust",
            "current_target": "generations/rust-abcd",
            "healthy": true,
        }))
        .unwrap();
        assert_eq!(snapshot.implementation, "rust");
        assert_eq!(
            snapshot.current_target.as_deref(),
            Some("generations/rust-abcd")
        );
    }
}
