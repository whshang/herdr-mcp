use crate::cli::ServiceCommand;
#[cfg(target_os = "macos")]
use crate::native_host_install;
use crate::{herdr_supervisor, link, paths::RuntimePaths, service_manager};
use serde_json::Value;
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

pub(crate) fn run(command: ServiceCommand) -> Result<ExitCode, String> {
    match command {
        ServiceCommand::Install { adopt_node } => run_install(adopt_node),
        ServiceCommand::Rollback => run_rollback(),
        ServiceCommand::Uninstall => run_uninstall(),
        other => service_manager::run(other),
    }
}

fn run_install(adopt_node: bool) -> Result<ExitCode, String> {
    let before_service = service_snapshot()?;
    let before_supervisor = herdr_supervisor::capture_install_state_for_service()?;
    herdr_supervisor::preflight_install_for_service()?;

    let result = service_manager::run(ServiceCommand::Install { adopt_node })?;
    if result != ExitCode::SUCCESS {
        return Ok(result);
    }

    if let Err(supervisor_error) = herdr_supervisor::ensure_installed_for_service() {
        let after_service = service_snapshot()?;
        let recovery = install_recovery(&before_service, &after_service);
        let cleanup_error = herdr_supervisor::remove_for_service().err();
        let service_recovery = match recovery {
            InstallRecovery::None => Ok(ExitCode::SUCCESS),
            InstallRecovery::Rollback => service_manager::run(ServiceCommand::Rollback),
            InstallRecovery::Uninstall => service_manager::run(ServiceCommand::Uninstall),
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
            InstallRecovery::Rollback => service_manager::run(ServiceCommand::Rollback),
            InstallRecovery::Uninstall => service_manager::run(ServiceCommand::Uninstall),
        };
        let service_recovered = matches!(service_recovery, Ok(code) if code == ExitCode::SUCCESS);
        let supervisor_restore = if service_recovered {
            herdr_supervisor::restore_install_state_for_service(before_supervisor)
        } else {
            Ok(())
        };
        let link_restore = if service_recovered && recovery != InstallRecovery::None {
            RuntimePaths::discover()
                .and_then(|paths| link::reconcile_after_service_generation_change(&paths))
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
        return Err(format!(
            "service install committed but production Link generation reconcile failed: {link_error}; {service_detail}{supervisor_detail}{link_detail}"
        ));
    }

    #[cfg(target_os = "macos")]
    {
        if !paths.instance.is_named()
            && let Err(native_host_error) = native_host_install::sync_owned_runtime_from_active()
        {
            return Err(format!(
                "service install committed but owned native-host runtime sync failed: {native_host_error}"
            ));
        }
    }

    Ok(result)
}

fn run_rollback() -> Result<ExitCode, String> {
    refuse_sidecar_mutation_inside_managed_exec()?;
    let before_supervisor = herdr_supervisor::capture_install_state_for_service()?;
    let target_supports_supervisor = match service_manager::rollback_target_runtime_binary()? {
        Some(binary) => herdr_supervisor::runtime_binary_supports_supervisor(&binary)?,
        None => false,
    };
    let strategy = rollback_supervisor_strategy(before_supervisor, target_supports_supervisor);
    if strategy == RollbackSupervisorStrategy::Remove {
        herdr_supervisor::remove_for_service()?;
    }

    match service_manager::run(ServiceCommand::Rollback) {
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
    let before_supervisor = herdr_supervisor::capture_install_state_for_service()?;
    herdr_supervisor::remove_for_service()?;

    match service_manager::run(ServiceCommand::Uninstall) {
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
