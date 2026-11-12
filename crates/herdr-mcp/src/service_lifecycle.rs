#![cfg_attr(any(target_os = "linux", target_os = "windows"), allow(dead_code))]

use crate::cli::ServiceCommand;
use crate::locale::Locale;
#[cfg(target_os = "macos")]
use crate::native_host_install;
use crate::{herdr_dependency, herdr_supervisor, link, paths::RuntimePaths, service_manager};
#[cfg(any(target_os = "macos", test))]
use plist::{Dictionary, Value as PlistValue};
use serde_json::Value;
#[cfg(any(target_os = "macos", test))]
use std::collections::BTreeMap;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::process::Command;
use std::process::ExitCode;
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) const GUI_DOMAIN_WORKER_ENV: &str = "HERDR_MCP_GUI_DOMAIN_WORKER";
pub(crate) const GUI_WORKER_PAYLOAD_ENV: &str = "HERDR_MCP_INSTALL_PAYLOAD_BINARY";
pub(crate) const GUI_WORKER_RESULT_ENV: &str = "HERDR_MCP_GUI_WORKER_RESULT_FILE";
pub(crate) const GUI_DOMAIN_INACTIVE_CODE: &str = "macos_gui_domain_inactive";
#[cfg(target_os = "macos")]
const GUI_WORKER_WAIT_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(target_os = "macos")]
const GUI_WORKER_POLL_INTERVAL: Duration = Duration::from_millis(200);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HerdrReadinessPolicy {
    Required,
    BestEffort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostCommitSidecarPolicy {
    Required,
    BestEffort,
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
    run_with_locale(command, crate::locale::resolve(None))
}

pub(crate) fn run_with_locale(
    command: ServiceCommand,
    language: Locale,
) -> Result<ExitCode, String> {
    match command {
        ServiceCommand::Install { adopt_node } => run_install_command(
            adopt_node,
            language,
            HerdrReadinessPolicy::Required,
            PostCommitSidecarPolicy::Required,
        ),
        ServiceCommand::Rollback => {
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            {
                let _ = language;
                service_manager::run(ServiceCommand::Rollback)
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            {
                run_rollback()
            }
        }
        ServiceCommand::Uninstall => {
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            {
                let _ = language;
                service_manager::run(ServiceCommand::Uninstall)
            }
            #[cfg(not(any(target_os = "linux", target_os = "windows")))]
            {
                run_uninstall()
            }
        }
        other => service_manager::run(other),
    }
}

pub(crate) fn run_major_upgrade_install() -> Result<ExitCode, String> {
    run_install_command(
        false,
        crate::locale::resolve(None),
        HerdrReadinessPolicy::BestEffort,
        PostCommitSidecarPolicy::BestEffort,
    )
}

fn run_install_command(
    adopt_node: bool,
    language: Locale,
    herdr_readiness: HerdrReadinessPolicy,
    sidecars: PostCommitSidecarPolicy,
) -> Result<ExitCode, String> {
    let outcome = run_install_command_inner(adopt_node, language, herdr_readiness, sidecars);
    #[cfg(target_os = "macos")]
    record_gui_worker_result(&outcome);
    outcome
}

fn run_install_command_inner(
    adopt_node: bool,
    language: Locale,
    herdr_readiness: HerdrReadinessPolicy,
    sidecars: PostCommitSidecarPolicy,
) -> Result<ExitCode, String> {
    #[cfg(target_os = "macos")]
    if caller_is_gui_domain_worker()
        && let Some(payload) =
            std::env::var_os(GUI_WORKER_PAYLOAD_ENV).filter(|value| !value.is_empty())
    {
        return run_install_from_payload(adopt_node, Path::new(&payload));
    }

    herdr_dependency::prepare_for_service_install()?;
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let _ = (language, sidecars);
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    let result = service_manager::run(ServiceCommand::Install { adopt_node })?;
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    let result = run_install(adopt_node, language, sidecars)?;
    if result == ExitCode::SUCCESS {
        apply_herdr_readiness_policy(
            herdr_readiness,
            herdr_dependency::ensure_server_ready_after_install(),
        )?;
    }
    Ok(result)
}

fn apply_herdr_readiness_policy(
    policy: HerdrReadinessPolicy,
    readiness: Result<(), String>,
) -> Result<(), String> {
    match (policy, readiness) {
        (_, Ok(())) => Ok(()),
        (HerdrReadinessPolicy::Required, Err(error)) => Err(error),
        (HerdrReadinessPolicy::BestEffort, Err(error)) => {
            eprintln!(
                "warning: major Runtime migration committed, but Herdr Server readiness still needs repair: {error}"
            );
            Ok(())
        }
    }
}

fn keep_committed_runtime_on_sidecar_failure(
    policy: PostCommitSidecarPolicy,
    label: &str,
    error: &str,
) -> bool {
    if policy != PostCommitSidecarPolicy::BestEffort {
        return false;
    }
    eprintln!(
        "warning: major Runtime migration committed, but {label} still needs repair: {error}"
    );
    true
}

/// Shared install lifecycle for the public `service install` path. The
/// orchestrator is the executing binary; the installed payload is the same
/// executable (`current_exe` inside `service_manager`).
fn run_install(
    adopt_node: bool,
    language: Locale,
    sidecars: PostCommitSidecarPolicy,
) -> Result<ExitCode, String> {
    #[cfg(target_os = "macos")]
    if let Some(code) = maybe_delegate_ssh_service_mutation_to_gui_domain(
        &GuiWorkerJob::Install {
            adopt_node,
            payload_binary: None,
        },
        "herdr-mcp service install",
    )? {
        return Ok(code);
    }

    let result = run_install_lifecycle(language, sidecars, |mutation_lock| {
        service_manager::run_with_mutation_lock(
            ServiceCommand::Install { adopt_node },
            mutation_lock,
        )
    })?;
    if result == ExitCode::SUCCESS
        && let Err(error) = crate::dev::reconcile_after_public_prod_install()
        && !keep_committed_runtime_on_sidecar_failure(
            sidecars,
            "DEV/PROD channel state reconcile",
            &error,
        )
    {
        return Err(error);
    }
    Ok(result)
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
    #[cfg(target_os = "macos")]
    if let Some(code) = maybe_delegate_ssh_service_mutation_to_gui_domain(
        &GuiWorkerJob::Install {
            adopt_node,
            payload_binary: Some(payload_binary.to_path_buf()),
        },
        "herdr-mcp dev rollback",
    )? {
        return Ok(code);
    }
    run_install_lifecycle(
        crate::locale::resolve(None),
        PostCommitSidecarPolicy::Required,
        |mutation_lock| {
            service_manager::run_install_from_payload(adopt_node, payload_binary, mutation_lock)
        },
    )
}

/// One shared install lifecycle. The mutation lock is acquired, the pre-commit
/// supervisor/service snapshot is captured, the install commit itself runs via
/// `install` (either `current_exe` or an explicit payload), and then the
/// post-commit sidecar orchestration (Herdr supervisor, product identity/update
/// fence, production Link generation reconcile, native-host sync, compensation)
/// completes inside `finish_install_lifecycle`.
fn run_install_lifecycle<Install>(
    language: Locale,
    sidecars: PostCommitSidecarPolicy,
    install: Install,
) -> Result<ExitCode, String>
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
    finish_install_lifecycle(
        before_service,
        before_supervisor,
        &mutation_lock,
        result,
        language,
        sidecars,
    )
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
    language: Locale,
    sidecars: PostCommitSidecarPolicy,
) -> Result<ExitCode, String> {
    #[cfg(not(target_os = "macos"))]
    let _ = language;

    if result != ExitCode::SUCCESS {
        return Ok(result);
    }

    if let Err(supervisor_error) = herdr_supervisor::ensure_installed_for_service()
        && !keep_committed_runtime_on_sidecar_failure(
            sidecars,
            "Herdr supervisor activation",
            &supervisor_error,
        )
    {
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
    if let Err(link_error) = link::reconcile_after_service_generation_change(&paths)
        && !keep_committed_runtime_on_sidecar_failure(
            sidecars,
            "production Link generation reconcile",
            &link_error,
        )
    {
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
            && !keep_committed_runtime_on_sidecar_failure(
                sidecars,
                "owned native-host runtime sync",
                &native_host_error,
            )
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

        crate::macos_permissions::post_service_install_onboarding(
            paths.instance.is_named(),
            language,
        );
    }

    crate::semantic_cli::print_install_hint(&paths, language);

    Ok(result)
}

fn run_rollback() -> Result<ExitCode, String> {
    let outcome = run_rollback_inner();
    #[cfg(target_os = "macos")]
    record_gui_worker_result(&outcome);
    outcome
}

fn run_rollback_inner() -> Result<ExitCode, String> {
    refuse_sidecar_mutation_inside_managed_exec()?;
    // #930 (P2-8): route macOS non-GUI SSH `service rollback` through the
    // same one-shot gui/<uid> worker as install (fail closed with
    // `macos_gui_domain_inactive` when gui/<uid> is inactive). Inside the
    // worker, `HERDR_MCP_GUI_DOMAIN_WORKER=1` keeps `caller_is_non_gui_ssh_session`
    // false so the mutation itself never re-delegates.
    #[cfg(target_os = "macos")]
    if let Some(code) = maybe_delegate_ssh_service_mutation_to_gui_domain(
        &GuiWorkerJob::Rollback,
        "herdr-mcp service rollback",
    )? {
        return Ok(code);
    }
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

pub(crate) fn caller_is_gui_domain_worker() -> bool {
    std::env::var_os(GUI_DOMAIN_WORKER_ENV).is_some_and(|value| !value.is_empty())
}

pub(crate) fn caller_is_non_gui_ssh_session_from_vars(
    gui_worker: Option<&str>,
    ssh_connection: Option<&str>,
    ssh_client: Option<&str>,
    ssh_tty: Option<&str>,
) -> bool {
    if gui_worker.is_some_and(|value| !value.trim().is_empty()) {
        return false;
    }
    [ssh_connection, ssh_client, ssh_tty]
        .into_iter()
        .flatten()
        .any(|value| !value.trim().is_empty())
}

pub(crate) fn caller_is_non_gui_ssh_session() -> bool {
    #[cfg(test)]
    if std::env::var_os("HERDR_MCP_TEST_ALLOW_SSH_GUI_ROUTING").is_none_or(|value| value.is_empty())
    {
        return false;
    }
    let gui_worker = std::env::var(GUI_DOMAIN_WORKER_ENV).ok();
    let ssh_connection = std::env::var("SSH_CONNECTION").ok();
    let ssh_client = std::env::var("SSH_CLIENT").ok();
    let ssh_tty = std::env::var("SSH_TTY").ok();
    caller_is_non_gui_ssh_session_from_vars(
        gui_worker.as_deref(),
        ssh_connection.as_deref(),
        ssh_client.as_deref(),
        ssh_tty.as_deref(),
    )
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn gui_domain_target() -> String {
    #[cfg(unix)]
    let uid = unsafe { libc::geteuid() };
    #[cfg(not(unix))]
    let uid = 0_u32;
    format!("gui/{uid}")
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn format_gui_domain_inactive_error(command_label: &str, detail: &str) -> String {
    let domain = gui_domain_target();
    let detail_suffix = if detail.trim().is_empty() {
        String::new()
    } else {
        format!(" ({})", detail.trim())
    };
    format!(
        "{GUI_DOMAIN_INACTIVE_CODE}: cannot run `{command_label}` from a non-GUI SSH session because the macOS LaunchAgent domain `{domain}` is not active{detail_suffix}; log in to the macOS graphical session for this user so `{domain}` is active (preserving Keychain ACL and credential helper checks), or run `{command_label}` from an interactive Mac Terminal"
    )
}

/// #930 (P2-9): the fixed system security PATH every one-shot gui/<uid>
/// worker runs with. Production always uses this constant; unit tests may
/// prepend a fake-bin directory via `HERDR_MCP_TEST_GUI_WORKER_PATH_PREFIX`
/// so `fake_launchctl` remains reachable inside the pinned PATH.
#[cfg(any(target_os = "macos", test))]
pub(crate) const GUI_WORKER_SYSTEM_PATH: &str =
    "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

#[cfg(any(target_os = "macos", test))]
pub(crate) fn deterministic_gui_worker_path() -> String {
    #[cfg(test)]
    if let Some(prefix) = std::env::var_os("HERDR_MCP_TEST_GUI_WORKER_PATH_PREFIX")
        .filter(|value| !value.to_string_lossy().trim().is_empty())
    {
        return format!("{}:{GUI_WORKER_SYSTEM_PATH}", prefix.to_string_lossy());
    }
    GUI_WORKER_SYSTEM_PATH.to_owned()
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn encode_one_shot_gui_worker_plist(
    label: &str,
    program_arguments: &[String],
    environment: &BTreeMap<String, String>,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<Vec<u8>, String> {
    if label.trim().is_empty() {
        return Err("one-shot GUI worker LaunchAgent label must not be empty".to_owned());
    }
    if program_arguments.is_empty() {
        return Err(
            "one-shot GUI worker LaunchAgent ProgramArguments must not be empty".to_owned(),
        );
    }
    let mut root = Dictionary::new();
    root.insert("Label".to_owned(), PlistValue::String(label.to_owned()));
    root.insert(
        "ProgramArguments".to_owned(),
        PlistValue::Array(
            program_arguments
                .iter()
                .cloned()
                .map(PlistValue::String)
                .collect(),
        ),
    );
    // #930 (P2-9): pin a deterministic system security PATH inside the plist
    // itself so an arbitrary non-GUI SSH session PATH can never propagate into
    // gui/<uid>. This overrides any PATH the caller supplied and runs last.
    let mut env_dict = Dictionary::new();
    for (key, value) in environment {
        env_dict.insert(key.clone(), PlistValue::String(value.clone()));
    }
    env_dict.insert(
        "PATH".to_owned(),
        PlistValue::String(deterministic_gui_worker_path()),
    );
    env_dict.insert(
        GUI_DOMAIN_WORKER_ENV.to_owned(),
        PlistValue::String("1".to_owned()),
    );
    root.insert(
        "EnvironmentVariables".to_owned(),
        PlistValue::Dictionary(env_dict),
    );
    root.insert("RunAtLoad".to_owned(), PlistValue::Boolean(true));
    root.insert("KeepAlive".to_owned(), PlistValue::Boolean(false));
    root.insert(
        "StandardOutPath".to_owned(),
        PlistValue::String(stdout_path.to_string_lossy().into_owned()),
    );
    root.insert(
        "StandardErrorPath".to_owned(),
        PlistValue::String(stderr_path.to_string_lossy().into_owned()),
    );
    let mut bytes = Vec::new();
    PlistValue::Dictionary(root)
        .to_writer_xml(&mut bytes)
        .map_err(|error| format!("cannot encode one-shot GUI worker LaunchAgent: {error}"))?;
    Ok(bytes)
}

#[cfg(target_os = "macos")]
pub(crate) fn probe_gui_domain_active() -> Result<(bool, String), String> {
    if let Ok(override_state) = std::env::var("HERDR_MCP_GUI_DOMAIN_PROBE_OVERRIDE") {
        let trimmed = override_state.trim();
        if trimmed.eq_ignore_ascii_case("active") {
            return Ok((true, "override=active".to_owned()));
        }
        if trimmed.eq_ignore_ascii_case("inactive") {
            return Ok((
                false,
                "launchd gui domain is not loaded (override=inactive)".to_owned(),
            ));
        }
    }
    let domain = gui_domain_target();
    let mut command = Command::new("launchctl");
    command.args(["print", &domain]);
    let output =
        crate::child_process::run_bounded_output(&mut command, Duration::from_secs(5), 64 * 1024)
            .map_err(|error| format!("cannot query launchd domain {domain}: {error}"))?
            .ok_or_else(|| format!("timed out querying launchd domain {domain}"))?;
    if output.status.success() {
        return Ok((true, "active".to_owned()));
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("launchctl print {domain} exited with {}", output.status)
    };
    Ok((false, detail))
}

#[cfg(target_os = "macos")]
pub(crate) fn bootstrap_gui_worker_plist(
    plist_path: &Path,
    label: &str,
) -> Result<Option<u32>, String> {
    let domain = gui_domain_target();
    let _ = bootout_gui_worker_label(label);
    let mut command = Command::new("launchctl");
    command.args(["bootstrap", &domain, plist_path.to_string_lossy().as_ref()]);
    let output =
        crate::child_process::run_bounded_output(&mut command, Duration::from_secs(10), 32 * 1024)
            .map_err(|error| format!("cannot bootstrap one-shot GUI worker {label}: {error}"))?
            .ok_or_else(|| format!("timed out bootstrapping one-shot GUI worker {label}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(format!(
            "cannot bootstrap one-shot GUI worker {label} in {domain}: {detail}"
        ));
    }
    Ok(query_gui_worker_pid(label))
}

#[cfg(target_os = "macos")]
pub(crate) fn bootout_gui_worker_label(label: &str) -> Result<(), String> {
    let target = format!("{}/{label}", gui_domain_target());
    let mut command = Command::new("launchctl");
    command.args(["bootout", &target]);
    let output =
        crate::child_process::run_bounded_output(&mut command, Duration::from_secs(5), 16 * 1024)
            .map_err(|error| format!("cannot bootout one-shot GUI worker {label}: {error}"))?
            .ok_or_else(|| format!("timed out booting out one-shot GUI worker {label}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn query_gui_worker_pid(label: &str) -> Option<u32> {
    let target = format!("{}/{label}", gui_domain_target());
    let mut command = Command::new("launchctl");
    command.args(["print", &target]);
    let output =
        crate::child_process::run_bounded_output(&mut command, Duration::from_secs(3), 32 * 1024)
            .ok()??;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_launchctl_print_pid(&text)
}

#[cfg(any(target_os = "macos", test))]
fn parse_launchctl_print_pid(text: &str) -> Option<u32> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("pid = ")
            && let Ok(pid) = rest.trim().parse::<u32>()
            && pid > 0
        {
            return Some(pid);
        }
    }
    None
}

#[cfg(any(target_os = "macos", test))]
fn parse_launchctl_print_exit_status(text: &str) -> Option<i32> {
    let state_not_running = text
        .lines()
        .any(|line| line.trim() == "state = not running");
    if !state_not_running {
        return None;
    }
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("last exit code = ")
            && let Some(token) = rest.split_whitespace().next()
            && let Ok(code) = token.parse::<i32>()
        {
            return Some(code);
        }
    }
    None
}

#[cfg(target_os = "macos")]
pub(crate) fn gui_worker_has_exited(label: &str) -> bool {
    let target = format!("{}/{label}", gui_domain_target());
    let mut command = Command::new("launchctl");
    command.args(["print", &target]);
    let Ok(Some(output)) =
        crate::child_process::run_bounded_output(&mut command, Duration::from_secs(3), 32 * 1024)
    else {
        return false;
    };
    if !output.status.success() {
        return true;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_launchctl_print_exit_status(&text).is_some()
}

#[cfg(target_os = "macos")]
fn write_private_file(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("worker");
    let temp = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temp)
        .map_err(|error| format!("cannot create {}: {error}", temp.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            format!("cannot write {}: {error}", temp.display())
        })?;
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("cannot commit {}: {error}", path.display())
    })
}

#[cfg(target_os = "macos")]
fn record_gui_worker_result(outcome: &Result<ExitCode, String>) {
    let Some(result_path) = std::env::var_os(GUI_WORKER_RESULT_ENV).filter(|v| !v.is_empty())
    else {
        return;
    };
    let path = PathBuf::from(result_path);
    let payload = match outcome {
        Ok(code) if *code == ExitCode::SUCCESS => serde_json::json!({
            "ok": true,
            "exit_code": 0,
        }),
        Ok(code) => serde_json::json!({
            "ok": false,
            "exit_code": 1,
            "error": format!("GUI domain lifecycle worker exited with {code:?}"),
        }),
        Err(error) => serde_json::json!({
            "ok": false,
            "exit_code": 1,
            "error": error,
        }),
    };
    if let Ok(bytes) = serde_json::to_vec(&payload) {
        let _ = write_private_file(&path, &bytes, 0o600);
    }
}

/// Which service mutation a one-shot gui/<uid> worker executes (#930).
#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
enum GuiWorkerJob {
    Install {
        adopt_node: bool,
        payload_binary: Option<PathBuf>,
    },
    Rollback,
}

#[cfg(target_os = "macos")]
impl GuiWorkerJob {
    fn worker_arguments(&self, exe: &Path) -> Vec<String> {
        let mut args = vec![exe.to_string_lossy().into_owned(), "service".to_owned()];
        match self {
            Self::Install { adopt_node, .. } => {
                args.push("install".to_owned());
                if *adopt_node {
                    args.push("--adopt-node".to_owned());
                }
            }
            Self::Rollback => args.push("rollback".to_owned()),
        }
        args
    }

    fn payload_binary(&self) -> Option<&Path> {
        match self {
            Self::Install { payload_binary, .. } => payload_binary.as_deref(),
            Self::Rollback => None,
        }
    }
}

#[cfg(target_os = "macos")]
fn maybe_delegate_ssh_service_mutation_to_gui_domain(
    job: &GuiWorkerJob,
    command_label: &str,
) -> Result<Option<ExitCode>, String> {
    if !caller_is_non_gui_ssh_session() {
        return Ok(None);
    }
    // #930 (P2-7): a non-GUI SSH session must never run Keychain-creating
    // service setup inline. `prewarm_existing_default` returns Ok(()) when no
    // default credential exists yet, so its former success could not
    // distinguish "boundary already proven" from "nothing to lose yet" and
    // let a first install execute inside the SSH session. Delegate
    // unconditionally instead; fail closed below when gui/<uid> is inactive.
    let paths = RuntimePaths::discover()?;
    crate::macos_permissions::preserve_or_install_broker(&paths.config_dir)
        .map_err(|error| format!("macOS TCC broker install preflight failed: {error}"))?;
    crate::macos_credential_helper::preserve_or_install(&paths.config_dir)
        .map_err(|error| format!("macOS credential helper install preflight failed: {error}"))?;
    let (active, detail) = probe_gui_domain_active()?;
    if !active {
        return Err(format_gui_domain_inactive_error(command_label, &detail));
    }
    delegate_service_mutation_via_gui_domain(&paths, job, command_label).map(Some)
}

#[cfg(target_os = "macos")]
fn delegate_service_mutation_via_gui_domain(
    paths: &RuntimePaths,
    job: &GuiWorkerJob,
    command_label: &str,
) -> Result<ExitCode, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("cannot resolve current executable for GUI worker: {error}"))?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let worker_id = format!("lifecycle-{now_ms}-{}", std::process::id());
    let label = format!("dev.herdr-mcp.lifecycle-worker.{worker_id}");
    let worker_dir = paths.config_dir.join("update").join("gui-workers");
    let plist_path = worker_dir.join(format!("{worker_id}.plist"));
    let stdout_path = worker_dir.join(format!("{worker_id}.out.log"));
    let stderr_path = worker_dir.join(format!("{worker_id}.err.log"));
    let result_path = worker_dir.join(format!("{worker_id}.result.json"));

    // #930 (P2-9): no caller-supplied PATH enters the worker environment.
    // `encode_one_shot_gui_worker_plist` pins the deterministic system PATH.
    let args = job.worker_arguments(&exe);
    let mut env = BTreeMap::new();
    env.insert(
        "HERDR_MCP_CONFIG_DIR".to_owned(),
        paths.config_dir.to_string_lossy().into_owned(),
    );
    env.insert(
        GUI_WORKER_RESULT_ENV.to_owned(),
        result_path.to_string_lossy().into_owned(),
    );
    if let Some(payload) = job.payload_binary() {
        env.insert(
            GUI_WORKER_PAYLOAD_ENV.to_owned(),
            payload.to_string_lossy().into_owned(),
        );
    }
    if let Ok(trigger) = std::env::var("HERDR_MCP_GENERATION_TRIGGER")
        && !trigger.trim().is_empty()
    {
        env.insert("HERDR_MCP_GENERATION_TRIGGER".to_owned(), trigger);
    }
    if let Some(name) = paths.instance.name() {
        env.insert("HERDR_MCP_INSTANCE".to_owned(), name.to_owned());
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.trim().is_empty()
    {
        env.insert("HOME".to_owned(), home);
    }

    let plist_bytes =
        encode_one_shot_gui_worker_plist(&label, &args, &env, &stdout_path, &stderr_path)?;
    write_private_file(&plist_path, &plist_bytes, 0o600)?;
    if let Err(error) = bootstrap_gui_worker_plist(&plist_path, &label) {
        let _ = std::fs::remove_file(&plist_path);
        return Err(error);
    }

    let outcome = wait_for_gui_lifecycle_worker(
        &label,
        &result_path,
        &stdout_path,
        &stderr_path,
        command_label,
    );
    let _ = bootout_gui_worker_label(&label);
    let _ = std::fs::remove_file(&plist_path);
    let _ = std::fs::remove_file(&stdout_path);
    let _ = std::fs::remove_file(&stderr_path);
    let _ = std::fs::remove_file(&result_path);
    outcome
}

#[cfg(target_os = "macos")]
fn wait_for_gui_lifecycle_worker(
    label: &str,
    result_path: &Path,
    stdout_path: &Path,
    stderr_path: &Path,
    command_label: &str,
) -> Result<ExitCode, String> {
    let started = Instant::now();
    loop {
        if let Ok(bytes) = std::fs::read(result_path)
            && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
        {
            if value.get("ok").and_then(Value::as_bool) == Some(true) {
                return Ok(ExitCode::SUCCESS);
            }
            let error = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("GUI domain lifecycle worker failed");
            return Err(error.to_owned());
        }
        if gui_worker_has_exited(label) {
            std::thread::sleep(GUI_WORKER_POLL_INTERVAL);
            if let Ok(bytes) = std::fs::read(result_path)
                && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
            {
                if value.get("ok").and_then(Value::as_bool) == Some(true) {
                    return Ok(ExitCode::SUCCESS);
                }
                let error = value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("GUI domain lifecycle worker failed");
                return Err(error.to_owned());
            }
            let stderr = std::fs::read_to_string(stderr_path).unwrap_or_default();
            let stdout = std::fs::read_to_string(stdout_path).unwrap_or_default();
            let detail = if !stderr.trim().is_empty() {
                stderr.trim().to_owned()
            } else if !stdout.trim().is_empty() {
                stdout.trim().to_owned()
            } else {
                format!("one-shot GUI worker {label} exited without writing result")
            };
            return Err(format!("`{command_label}` GUI worker failed: {detail}"));
        }
        if started.elapsed() >= GUI_WORKER_WAIT_TIMEOUT {
            return Err(format!(
                "`{command_label}` timed out waiting for one-shot GUI worker {label} in {}",
                gui_domain_target()
            ));
        }
        std::thread::sleep(GUI_WORKER_POLL_INTERVAL);
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn spawn_gui_domain_update_worker(
    paths: &RuntimePaths,
    binary: &Path,
    job_id: &str,
) -> Result<(String, PathBuf, Option<u32>), String> {
    let (active, detail) = probe_gui_domain_active()?;
    if !active {
        return Err(format_gui_domain_inactive_error(
            "herdr-mcp update apply",
            &detail,
        ));
    }
    let job_dir = binary
        .parent()
        .ok_or_else(|| "staged update binary has no parent directory".to_owned())?;
    let label = format!("dev.herdr-mcp.update-worker.{job_id}");
    let plist_path = job_dir.join("gui-worker.plist");
    let stdout_path = job_dir.join("gui-worker.out.log");
    let stderr_path = job_dir.join("gui-worker.err.log");
    let args = vec![
        binary.to_string_lossy().into_owned(),
        "update".to_owned(),
        "worker".to_owned(),
        "--job".to_owned(),
        job_id.to_owned(),
    ];
    let mut env = BTreeMap::new();
    env.insert(
        "HERDR_MCP_CONFIG_DIR".to_owned(),
        paths.config_dir.to_string_lossy().into_owned(),
    );
    if let Some(name) = paths.instance.name() {
        env.insert("HERDR_MCP_INSTANCE".to_owned(), name.to_owned());
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.trim().is_empty()
    {
        env.insert("HOME".to_owned(), home);
    }
    // #930 (P2-9): the caller PATH never enters the worker; the plist pins
    // the deterministic system PATH inside encode_one_shot_gui_worker_plist.
    let plist_bytes =
        encode_one_shot_gui_worker_plist(&label, &args, &env, &stdout_path, &stderr_path)?;
    write_private_file(&plist_path, &plist_bytes, 0o600)?;
    match bootstrap_gui_worker_plist(&plist_path, &label) {
        Ok(pid) => Ok((label, plist_path, pid)),
        Err(error) => {
            let _ = std::fs::remove_file(&plist_path);
            Err(error)
        }
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
    fn major_upgrade_keeps_committed_runtime_when_only_herdr_readiness_is_pending() {
        let error = "Herdr server did not become reachable".to_owned();
        assert!(
            apply_herdr_readiness_policy(HerdrReadinessPolicy::BestEffort, Err(error.clone()),)
                .is_ok()
        );
        assert_eq!(
            apply_herdr_readiness_policy(HerdrReadinessPolicy::Required, Err(error)).unwrap_err(),
            "Herdr server did not become reachable"
        );
    }

    #[test]
    fn major_upgrade_keeps_committed_runtime_when_repairable_sidecar_is_pending() {
        for label in [
            "Herdr supervisor activation",
            "production Link generation reconcile",
            "owned native-host runtime sync",
            "DEV/PROD channel state reconcile",
        ] {
            assert!(keep_committed_runtime_on_sidecar_failure(
                PostCommitSidecarPolicy::BestEffort,
                label,
                "synthetic sidecar failure",
            ));
            assert!(!keep_committed_runtime_on_sidecar_failure(
                PostCommitSidecarPolicy::Required,
                label,
                "synthetic sidecar failure",
            ));
        }
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

    #[test]
    fn gui_domain_worker_plist_enforces_one_shot_run_at_load_without_keep_alive() {
        let mut env = BTreeMap::new();
        env.insert(
            "HERDR_MCP_CONFIG_DIR".to_owned(),
            "/tmp/herdr-config".to_owned(),
        );
        env.insert(
            GUI_WORKER_PAYLOAD_ENV.to_owned(),
            "/tmp/herdr-config/dev/prod-herdr-mcp".to_owned(),
        );
        let bytes = encode_one_shot_gui_worker_plist(
            "dev.herdr-mcp.lifecycle-worker.test-913",
            &[
                "/tmp/herdr-config/runtime/current/herdr-mcp".to_owned(),
                "service".to_owned(),
                "install".to_owned(),
            ],
            &env,
            Path::new("/tmp/herdr-config/worker.out.log"),
            Path::new("/tmp/herdr-config/worker.err.log"),
        )
        .expect("one-shot GUI worker plist must encode cleanly");
        let parsed = PlistValue::from_reader(std::io::Cursor::new(&bytes))
            .expect("encoded plist must parse");
        let dict = parsed.as_dictionary().expect("root must be dictionary");
        assert_eq!(
            dict.get("Label").and_then(PlistValue::as_string),
            Some("dev.herdr-mcp.lifecycle-worker.test-913")
        );
        assert_eq!(
            dict.get("RunAtLoad").and_then(PlistValue::as_boolean),
            Some(true),
            "one-shot GUI worker must set RunAtLoad=true"
        );
        assert_eq!(
            dict.get("KeepAlive").and_then(PlistValue::as_boolean),
            Some(false),
            "one-shot GUI worker must set KeepAlive=false so launchd never replays it"
        );
        let env_dict = dict
            .get("EnvironmentVariables")
            .and_then(PlistValue::as_dictionary)
            .expect("EnvironmentVariables must be present");
        assert_eq!(
            env_dict
                .get(GUI_DOMAIN_WORKER_ENV)
                .and_then(PlistValue::as_string),
            Some("1")
        );
        assert_eq!(
            env_dict
                .get(GUI_WORKER_PAYLOAD_ENV)
                .and_then(PlistValue::as_string),
            Some("/tmp/herdr-config/dev/prod-herdr-mcp")
        );
    }

    #[test]
    fn non_gui_ssh_session_detection_and_inactive_gui_domain_fail_closed_contract() {
        assert!(!caller_is_non_gui_ssh_session_from_vars(
            None, None, None, None
        ));
        assert!(caller_is_non_gui_ssh_session_from_vars(
            None,
            Some("10.0.0.1 55112 10.0.0.2 22"),
            None,
            None
        ));
        assert!(caller_is_non_gui_ssh_session_from_vars(
            None,
            None,
            None,
            Some("/dev/ttys004")
        ));
        assert!(
            !caller_is_non_gui_ssh_session_from_vars(
                Some("1"),
                Some("10.0.0.1 55112 10.0.0.2 22"),
                Some("10.0.0.1 55112 22"),
                Some("/dev/ttys004")
            ),
            "a worker already running inside gui/<uid> must not re-delegate recursively"
        );

        let error =
            format_gui_domain_inactive_error("herdr-mcp dev rollback", "Could not find domain");
        assert!(error.contains(GUI_DOMAIN_INACTIVE_CODE));
        assert!(error.contains("herdr-mcp dev rollback"));
        assert!(error.contains("Could not find domain"));
        assert!(error.contains(&gui_domain_target()));

        assert_eq!(
            parse_launchctl_print_pid("{\n\tpid = 4242\n\tstate = running\n}"),
            Some(4242)
        );
        assert_eq!(
            parse_launchctl_print_exit_status("{\n\tstate = not running\n\tlast exit code = 0\n}"),
            Some(0)
        );
        assert_eq!(
            parse_launchctl_print_exit_status("{\n\tstate = running\n\tlast exit code = 0\n}"),
            None
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn non_gui_ssh_service_mutations_route_through_active_gui_domain_or_fail_closed_when_inactive()
    {
        use std::os::unix::fs::PermissionsExt;

        let _guard = crate::test_env::lock();
        let root = std::env::temp_dir().join(format!(
            "herdr-gui-lifecycle-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let config_dir = root.join("config");
        let bin_dir = root.join("bin");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&bin_dir).unwrap();

        let fake_launchctl = bin_dir.join("launchctl");
        let log_file = root.join("launchctl-calls.log");
        let captured_plist = root.join("captured-worker.plist");
        let script = format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> \"{}\"\n\
             cmd=\"$1\"\n\
             if [ \"$cmd\" = \"print\" ]; then\n\
               printf 'state = not running\\nlast exit code = 0\\n'\n\
               exit 0\n\
             fi\n\
             if [ \"$cmd\" = \"bootstrap\" ]; then\n\
               plist=\"$3\"\n\
               cp \"$plist\" \"{}\"\n\
               res=$(/usr/bin/plutil -extract EnvironmentVariables.{GUI_WORKER_RESULT_ENV} raw -o - \"$plist\" 2>/dev/null || true)\n\
               if [ -n \"$res\" ]; then\n\
                 if [ \"${{FAKE_GUI_WORKER_FAIL:-0}}\" = \"1\" ]; then\n\
                   printf '{{\"ok\":false,\"exit_code\":1,\"error\":\"macOS credential helper preflight failed inside gui domain\"}}\\n' > \"$res\"\n\
                 else\n\
                   printf '{{\"ok\":true,\"exit_code\":0}}\\n' > \"$res\"\n\
                 fi\n\
               fi\n\
               exit 0\n\
             fi\n\
             exit 0\n",
            log_file.display(),
            captured_plist.display()
        );
        std::fs::write(&fake_launchctl, script.as_bytes()).unwrap();
        std::fs::set_permissions(&fake_launchctl, std::fs::Permissions::from_mode(0o755)).unwrap();

        let original_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{original_path}", bin_dir.display());
        let vars = [
            (
                "HERDR_MCP_CONFIG_DIR",
                Some(config_dir.to_string_lossy().into_owned()),
            ),
            ("HERDR_MCP_INSTANCE", None),
            ("HERDR_MCP_TEST_ALLOW_SSH_GUI_ROUTING", Some("1".to_owned())),
            (
                "SSH_CONNECTION",
                Some("10.0.0.1 54321 10.0.0.2 22".to_owned()),
            ),
            ("HERDR_MCP_GUI_DOMAIN_WORKER", None),
            (
                "HERDR_MCP_GENERATION_TRIGGER",
                Some("dev_rollback".to_owned()),
            ),
            (
                "HERDR_MCP_TEST_GUI_WORKER_PATH_PREFIX",
                Some(bin_dir.to_string_lossy().into_owned()),
            ),
            ("PATH", Some(new_path.clone())),
        ];
        let previous: Vec<_> = vars
            .iter()
            .map(|(k, v)| {
                let prev = std::env::var_os(k);
                unsafe {
                    match v {
                        Some(val) => std::env::set_var(k, val),
                        None => std::env::remove_var(k),
                    }
                }
                (*k, prev)
            })
            .collect();

        let install_job = GuiWorkerJob::Install {
            adopt_node: false,
            payload_binary: Some(config_dir.join("dev/prod-herdr-mcp")),
        };

        // 1. Fail closed when gui/<uid> is inactive.
        unsafe { std::env::set_var("HERDR_MCP_GUI_DOMAIN_PROBE_OVERRIDE", "inactive") };
        let err = maybe_delegate_ssh_service_mutation_to_gui_domain(
            &install_job,
            "herdr-mcp dev rollback",
        )
        .expect_err("inactive gui/<uid> domain must fail closed");
        assert!(
            err.contains(GUI_DOMAIN_INACTIVE_CODE),
            "unexpected error: {err}"
        );
        assert!(err.contains("herdr-mcp dev rollback"));
        assert!(!captured_plist.exists());

        // 1b. #930 (P2-8): rollback also fails closed when gui/<uid> is
        // inactive, before any service mutation.
        let rollback_err = maybe_delegate_ssh_service_mutation_to_gui_domain(
            &GuiWorkerJob::Rollback,
            "herdr-mcp service rollback",
        )
        .expect_err("rollback over non-GUI SSH with inactive gui/<uid> must fail closed");
        assert!(
            rollback_err.contains(GUI_DOMAIN_INACTIVE_CODE),
            "unexpected rollback error: {rollback_err}"
        );
        assert!(rollback_err.contains("herdr-mcp service rollback"));

        // 2. Route through one-shot LaunchAgent in gui/<uid> when active.
        unsafe { std::env::set_var("HERDR_MCP_GUI_DOMAIN_PROBE_OVERRIDE", "active") };
        let code = maybe_delegate_ssh_service_mutation_to_gui_domain(
            &install_job,
            "herdr-mcp dev rollback",
        )
        .expect("active gui/<uid> worker delegation must succeed")
        .expect("must delegate when in non-GUI SSH session");
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(
            captured_plist.is_file(),
            "bootstrap must receive one-shot plist"
        );
        let calls = std::fs::read_to_string(&log_file).unwrap_or_default();
        assert!(calls.contains(&format!("bootstrap {}", gui_domain_target())));
        assert!(calls.contains(&format!(
            "bootout {}/dev.herdr-mcp.lifecycle-worker.",
            gui_domain_target()
        )));

        // 2b. #930 (P2-9): the worker plist PATH is the pinned system PATH
        // (plus the explicit test prefix), never the SSH session PATH.
        let parsed = PlistValue::from_file(&captured_plist).unwrap();
        let env_dict = parsed
            .as_dictionary()
            .unwrap()
            .get("EnvironmentVariables")
            .and_then(PlistValue::as_dictionary)
            .unwrap();
        assert_eq!(
            env_dict.get("PATH").and_then(PlistValue::as_string),
            Some(deterministic_gui_worker_path().as_str()),
            "worker PATH must be the deterministic system PATH"
        );
        assert_ne!(
            env_dict.get("PATH").and_then(PlistValue::as_string),
            Some(new_path.as_str()),
            "the arbitrary SSH session PATH must never reach gui/<uid>"
        );
        assert!(
            env_dict
                .get("PATH")
                .and_then(PlistValue::as_string)
                .unwrap()
                .ends_with(GUI_WORKER_SYSTEM_PATH),
            "the test prefix may only prepend to the pinned system PATH"
        );

        // 2c. #930 (P2-8): rollback delegates through the same one-shot
        // gui/<uid> worker and its plist runs `service rollback`.
        std::fs::remove_file(&captured_plist).ok();
        let rollback_code = maybe_delegate_ssh_service_mutation_to_gui_domain(
            &GuiWorkerJob::Rollback,
            "herdr-mcp service rollback",
        )
        .expect("rollback worker delegation must succeed")
        .expect("must delegate when in non-GUI SSH session");
        assert_eq!(rollback_code, ExitCode::SUCCESS);
        let rollback_plist = PlistValue::from_file(&captured_plist).unwrap();
        let rollback_dict = rollback_plist.as_dictionary().unwrap();
        let program_args: Vec<&str> = rollback_dict
            .get("ProgramArguments")
            .and_then(PlistValue::as_array)
            .unwrap()
            .iter()
            .filter_map(PlistValue::as_string)
            .collect();
        assert!(program_args.contains(&"service".to_owned().as_str()));
        assert!(program_args.contains(&"rollback".to_owned().as_str()));
        assert!(
            !program_args.contains(&"install".to_owned().as_str()),
            "rollback worker must not run install"
        );

        // 3. Propagate credential guard failure from inside gui/<uid> (never bypass Keychain checks).
        unsafe { std::env::set_var("FAKE_GUI_WORKER_FAIL", "1") };
        let guard_err = maybe_delegate_ssh_service_mutation_to_gui_domain(
            &install_job,
            "herdr-mcp dev rollback",
        )
        .expect_err("Keychain guard failure inside gui/<uid> must fail closed");
        assert!(guard_err.contains("macOS credential helper preflight failed inside gui domain"));

        unsafe {
            std::env::remove_var("HERDR_MCP_GUI_DOMAIN_PROBE_OVERRIDE");
            std::env::remove_var("FAKE_GUI_WORKER_FAIL");
            for (k, prev) in previous {
                match prev {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// user cannot run Keychain-creating install inline over non-GUI SSH |
    /// Given a first install with no default Keychain credential (prewarm
    /// returns empty success) | When `service install` checks delegation |
    /// Then it still delegates to gui/<uid> instead of running inline.
    #[cfg(target_os = "macos")]
    #[test]
    fn first_install_with_empty_keychain_still_delegates_to_gui_domain() {
        let _guard = crate::test_env::lock();
        // Empty-keychain first install means `prewarm_existing_default`
        // returns Ok(()) (no prewarm service configured). Delegation must not
        // depend on that result: with no production Link environment at all
        // (fresh HOME), prewarm succeeds vacuously, and delegation still has
        // to happen. Drive the decision helper directly with a fresh HOME so
        // the empty-keychain precondition is real.
        let root = std::env::temp_dir().join(format!(
            "herdr-gui-empty-keychain-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let config_dir = root.join("config");
        std::fs::create_dir_all(&config_dir).unwrap();

        let vars = [
            (
                "HERDR_MCP_CONFIG_DIR",
                Some(config_dir.to_string_lossy().into_owned()),
            ),
            ("HERDR_MCP_INSTANCE", None),
            ("HERDR_MCP_TEST_ALLOW_SSH_GUI_ROUTING", Some("1".to_owned())),
            (
                "SSH_CONNECTION",
                Some("10.0.0.1 54321 10.0.0.2 22".to_owned()),
            ),
            ("HERDR_MCP_GUI_DOMAIN_WORKER", None),
            ("HOME", Some(root.to_string_lossy().into_owned())),
        ];
        let previous: Vec<_> = vars
            .iter()
            .map(|(k, v)| {
                let prev = std::env::var_os(k);
                unsafe {
                    match v {
                        Some(val) => std::env::set_var(k, val),
                        None => std::env::remove_var(k),
                    }
                }
                (*k, prev)
            })
            .collect();

        // Empty Keychain precondition: no production Link env, so prewarm
        // returns Ok(()) without touching any credential.
        crate::macos_credential_helper::prewarm_existing_default(&config_dir)
            .expect("prewarm must succeed vacuously with no default credential");
        // The decision to delegate can no longer consult prewarm at all: the
        // boundary check must fire before gui/<uid> probing, proving empty
        // Keychain never keeps the mutation inline.
        unsafe { std::env::set_var("HERDR_MCP_GUI_DOMAIN_PROBE_OVERRIDE", "inactive") };
        let err = maybe_delegate_ssh_service_mutation_to_gui_domain(
            &GuiWorkerJob::Install {
                adopt_node: false,
                payload_binary: None,
            },
            "herdr-mcp service install",
        )
        .expect_err("empty-keychain first install must still fail closed, not run inline");
        assert!(err.contains(GUI_DOMAIN_INACTIVE_CODE));

        unsafe {
            std::env::remove_var("HERDR_MCP_GUI_DOMAIN_PROBE_OVERRIDE");
            for (k, prev) in previous {
                match prev {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
