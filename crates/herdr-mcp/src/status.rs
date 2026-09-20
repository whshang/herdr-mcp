use crate::config::Config;
use crate::herdr::HerdrClient;
use crate::herdr_native;
use crate::herdr_supervisor;
use crate::macos_privacy;
use crate::native_host_install;
use crate::native_tools;
use crate::paths::RuntimePaths;
use crate::service_manager;
use crate::snapshot;
use crate::state_cache::{EventCache, EventCacheHealth};
use crate::updater_store::UpdateStore;
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, PermissionsExt};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum RuntimeHealth {
    Healthy(u16),
    UnexpectedHttp(u16),
    Unreachable,
}

#[derive(Debug)]
struct StatusReport {
    runtime: RuntimeHealth,
    herdr_transport_reachable: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum DiagnosticState {
    Pass,
    Fail,
    NotProbed,
}

impl DiagnosticState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::NotProbed => "not_probed",
        }
    }
}

#[derive(Debug, Clone)]
struct AuthenticatedMcpProbe {
    state: DiagnosticState,
    detail: String,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum OverallReadiness {
    Fail,
    NotProven,
}

impl OverallReadiness {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fail => "fail",
            Self::NotProven => "not_proven",
        }
    }
}

#[derive(Debug)]
struct EventCacheProbe {
    healthy: bool,
    mode: &'static str,
    cursor: u64,
    digest_events: usize,
    agents: usize,
    workspaces: usize,
    snapshot_panes: usize,
    stream_events: u64,
    last_event_at: Option<String>,
    needs_reconcile: bool,
    error: Option<String>,
}

fn collect(paths: &RuntimePaths, config: &Config) -> StatusReport {
    StatusReport {
        runtime: probe_runtime(config.runtime_port),
        herdr_transport_reachable: probe_herdr_transport(paths),
    }
}

pub fn print_status(
    paths: &RuntimePaths,
    config: &Config,
    language: crate::locale::Locale,
    verbose: bool,
) {
    if verbose {
        print_status_verbose(paths, config, language);
        return;
    }

    let report = collect(paths, config);
    let cli_probe = herdr_native::probe_cli();
    let server_version = paths
        .herdr_socket
        .as_ref()
        .and_then(|socket| HerdrClient::new(socket).ping().ok())
        .and_then(|pong| {
            pong.get("version")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let native_runtime = herdr_native::project_runtime(&cli_probe, server_version.as_deref(), 0);
    let tcc = crate::tcc_broker::upgrade_status(&paths.config_dir);
    let tcc_summary =
        if crate::tcc_broker::status_line(&paths.config_dir).starts_with("not installed") {
            language
                .text("not installed", "未安装", "未インストール")
                .to_owned()
        } else if tcc.metadata_invalid.is_some() {
            language
                .text("needs repair", "需要修复", "修復が必要")
                .to_owned()
        } else if tcc.update_available {
            if tcc.update_requires_reauthorization {
                language
                    .text(
                        "update available · macOS authorization required",
                        "可更新 · 需要重新授权 macOS 权限",
                        "更新あり · macOS の再認証が必要",
                    )
                    .to_owned()
            } else {
                language
                    .text("update available", "可更新", "更新あり")
                    .to_owned()
            }
        } else {
            "OK".to_owned()
        };
    let scheduler = crate::update_scheduler::status_line();
    let scheduler = if scheduler.starts_with("daily,") {
        language.text("daily background", "每日后台", "毎日バックグラウンド")
    } else {
        scheduler.as_str()
    };

    println!(
        "Herdr MCP {} · {}",
        crate::runtime_meta::runtime_version(),
        crate::runtime_meta::runtime_channel()
    );
    println!(
        "{}: {} · 127.0.0.1:{}",
        language.text("Runtime", "运行时", "ランタイム"),
        if matches!(report.runtime, RuntimeHealth::Healthy(_)) {
            "OK"
        } else {
            "FAIL"
        },
        config.runtime_port
    );
    println!(
        "Herdr: {} · CLI {} · server {}",
        if report.herdr_transport_reachable {
            "OK"
        } else {
            "FAIL"
        },
        native_runtime["cli_version"].as_str().unwrap_or("unknown"),
        native_runtime["server_version"]
            .as_str()
            .unwrap_or("unknown")
    );
    println!(
        "{}: {} · {scheduler}",
        language.text("Updates", "更新", "更新"),
        if config.update_check {
            config.update_channel.as_str()
        } else {
            language.text("disabled", "已关闭", "無効")
        }
    );
    println!("TCC: {tcc_summary}");
    println!();
    println!(
        "{}: herdr-mcp status --verbose",
        language.text("Details", "详细信息", "詳細")
    );
}

fn print_status_verbose(paths: &RuntimePaths, config: &Config, language: crate::locale::Locale) {
    let report = collect(paths, config);
    let cli_probe = herdr_native::probe_cli();
    let server_version = paths
        .herdr_socket
        .as_ref()
        .and_then(|socket| HerdrClient::new(socket).ping().ok())
        .and_then(|pong| {
            pong.get("version")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let native_runtime = herdr_native::project_runtime(&cli_probe, server_version.as_deref(), 0);
    println!("Herdr MCP {}", crate::runtime_meta::runtime_version());
    println!(
        "{}: {}",
        language.text("runtime channel", "运行通道", "ランタイムチャネル"),
        crate::runtime_meta::runtime_channel()
    );
    println!(
        "{}: {}{}",
        language.text("runtime source", "运行源码", "ランタイムソース"),
        crate::runtime_meta::compiled_source_commit().unwrap_or("release"),
        if crate::runtime_meta::compiled_source_dirty() {
            " (dirty)"
        } else {
            ""
        }
    );
    println!(
        "{}: {}",
        language.text("config", "配置", "設定"),
        paths.config_file.display()
    );
    println!(
        "{}: {}",
        language.text("runtime", "运行时", "ランタイム"),
        runtime_label(report.runtime, config.runtime_port)
    );
    println!(
        "{}: {}",
        language.text("herdr transport", "Herdr 本地传输", "Herdr ローカル通信"),
        if report.herdr_transport_reachable {
            language.text("reachable", "可达", "到達可能")
        } else {
            language.text("unreachable", "不可达", "到達不可")
        }
    );
    println!(
        "{}: cli={} server={} state={} machine_forwarding={} saved_machines={}",
        language.text("herdr native", "Herdr 原生运行时", "Herdr ネイティブ"),
        native_runtime["cli_version"].as_str().unwrap_or("unknown"),
        native_runtime["server_version"]
            .as_str()
            .unwrap_or("unknown"),
        native_runtime["version_state"]
            .as_str()
            .unwrap_or("unknown"),
        native_runtime["machine_forwarding"]
            .as_bool()
            .map(|value| if value { "true" } else { "false" })
            .unwrap_or("unknown"),
        native_runtime["saved_machine_count"]
            .as_u64()
            .map(|value| value.to_string())
            .as_deref()
            .unwrap_or("unknown")
    );
    println!(
        "{}: {}",
        language.text("tcc broker", "TCC broker", "TCC broker"),
        crate::tcc_broker::status_line(&paths.config_dir)
    );
    println!(
        "{}: {}",
        language.text("update channel", "更新通道", "更新チャネル"),
        config.update_channel.as_str()
    );
    println!(
        "{}: {}",
        language.text("update checks", "更新检查", "更新チェック"),
        if config.update_check {
            language.text("enabled", "已启用", "有効")
        } else {
            language.text("disabled", "已关闭", "無効")
        }
    );
    println!(
        "{}: {}",
        language.text(
            "auto update scheduler",
            "自动更新调度",
            "自動更新スケジューラ"
        ),
        crate::update_scheduler::status_line()
    );
    println!(
        "{}: {}",
        language.text("lifecycle residue", "生命周期残留", "ライフサイクル残留"),
        crate::residue::status_line()
    );
    println!(
        "{}: {}",
        language.text("relay pool", "Relay 池", "Relay プール"),
        crate::link::relay_manifest::status_line(paths, unix_now_seconds())
    );
    println!(
        "{}: {}",
        language.text("relay use", "Relay 用途", "Relay 用途"),
        crate::link::RELAY_POLICY_DESCRIPTION
    );
    println!(
        "{}: {}",
        language.text(
            "local agent skill",
            "本地 Agent Skill",
            "ローカル Agent Skill"
        ),
        crate::local_agent_skill::status_line()
    );
}

pub fn print_doctor(
    paths: &RuntimePaths,
    config: &Config,
    language: crate::locale::Locale,
    verbose: bool,
    json_only: bool,
) -> bool {
    macro_rules! println {
        ($($arg:tt)*) => {
            if verbose {
                std::println!($($arg)*);
            }
        };
    }
    let print_check = |label: &str, pass: bool| {
        if verbose {
            std::println!("{} {label}", if pass { "PASS" } else { "FAIL" });
        }
    };

    let report = collect(paths, config);
    let runtime_healthy = matches!(report.runtime, RuntimeHealth::Healthy(_));
    let methods_result = native_tools::methods("");
    // `native_tools::methods` can still return the local progressive-method
    // registry when live Herdr schema reflection is unavailable. Doctor must
    // prove the live schema itself rather than letting that local fallback
    // mask a broken Herdr executable lookup.
    let schema_healthy = crate::schema::list_methods("").is_ok();
    let native_call_result = paths
        .herdr_socket
        .as_ref()
        .map(|socket| native_tools::call(&HerdrClient::new(socket), "ping", json!({})))
        .unwrap_or_else(|| json!({"ok": false}));
    let native_call_healthy = native_call_result["ok"].as_bool() == Some(true);
    let snapshot_result = match paths.herdr_socket.as_ref() {
        Some(socket) => snapshot::fetch(&HerdrClient::new(socket)),
        None => Err("Herdr local transport is unavailable".to_owned()),
    };
    let snapshot_healthy = snapshot_result.is_ok();
    let inspect_result = paths
        .herdr_socket
        .as_ref()
        .map(|socket| native_tools::inspect(&HerdrClient::new(socket), None, None))
        .unwrap_or_else(|| json!({"ok": false}));
    let inspect_healthy = inspect_result["ok"].as_bool() == Some(true);
    let cli_probe = herdr_native::probe_cli();
    let server_version = paths
        .herdr_socket
        .as_ref()
        .and_then(|socket| HerdrClient::new(socket).ping().ok())
        .and_then(|pong| {
            pong.get("version")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let pane_count = snapshot_result
        .as_ref()
        .ok()
        .map(|snapshot| snapshot::collection_count(&snapshot.value, "panes"))
        .unwrap_or_default();
    let native_runtime =
        herdr_native::project_runtime(&cli_probe, server_version.as_deref(), pane_count);
    let event_cache = probe_event_cache(paths);
    let documents_permission = macos_privacy::probe_documents_permission(&paths.config_dir);
    let code_identity = macos_privacy::probe_code_identity();
    let authenticated_local_mcp = probe_authenticated_local_mcp(config.runtime_port);
    let standalone_browser = crate::standalone_extension::doctor_report();
    let local_agent_skill = crate::local_agent_skill::status_summary();
    let windows_service_healthy = if cfg!(target_os = "windows") {
        service_manager::doctor_status()
            .ok()
            .and_then(|status| status.get("ok").and_then(Value::as_bool))
            == Some(true)
    } else {
        true
    };
    println!("Herdr MCP {}", language.text("doctor", "诊断", "診断"));
    println!(
        "{}: channel={} version={} source={}{}",
        language.text("runtime provenance", "运行来源", "ランタイム由来"),
        crate::runtime_meta::runtime_channel(),
        crate::runtime_meta::runtime_version(),
        crate::runtime_meta::compiled_source_commit().unwrap_or("release"),
        if crate::runtime_meta::compiled_source_dirty() {
            " dirty"
        } else {
            ""
        }
    );
    print_check(
        language.text("runtime endpoint", "运行端点", "ランタイムエンドポイント"),
        runtime_healthy,
    );
    print_check(
        language.text(
            "Herdr local transport",
            "Herdr 本地传输",
            "Herdr ローカル通信",
        ),
        report.herdr_transport_reachable,
    );
    print_check(
        language.text("Herdr API schema", "Herdr API schema", "Herdr API schema"),
        schema_healthy,
    );
    print_check(
        language.text(
            "validated Herdr RPC",
            "已验证 Herdr RPC",
            "検証済み Herdr RPC",
        ),
        native_call_healthy,
    );
    print_check(
        language.text(
            "Herdr snapshot state",
            "Herdr 快照状态",
            "Herdr スナップショット状態",
        ),
        snapshot_healthy,
    );
    print_check(
        language.text(
            "Herdr inspect projection",
            "Herdr inspect 投影",
            "Herdr inspect 投影",
        ),
        inspect_healthy,
    );
    print_check(
        language.text(
            "Herdr event cache",
            "Herdr 事件缓存",
            "Herdr イベントキャッシュ",
        ),
        event_cache.healthy,
    );
    let macos_permissions = crate::macos_permissions::collect_status();
    println!("{}", documents_permission.doctor_line());
    println!(
        "{}",
        crate::macos_permissions::doctor_layer_from(&macos_permissions)
    );
    println!("{}", crate::tcc_broker::doctor_line(&paths.config_dir));
    println!("{}", code_identity.doctor_line());
    println!("{}", herdr_supervisor::doctor_line());
    println!(
        "LAYER herdr-native cli={} server={} state={} machine_forwarding={} saved_machines={} handoff_blocked_reason={}",
        native_runtime["cli_version"].as_str().unwrap_or("unknown"),
        native_runtime["server_version"]
            .as_str()
            .unwrap_or("unknown"),
        native_runtime["version_state"]
            .as_str()
            .unwrap_or("unknown"),
        native_runtime["machine_forwarding"]
            .as_bool()
            .map(|value| if value { "true" } else { "false" })
            .unwrap_or("unknown"),
        native_runtime["saved_machine_count"]
            .as_u64()
            .map(|value| value.to_string())
            .as_deref()
            .unwrap_or("unknown"),
        native_runtime["handoff_blocked_reason"]
            .as_str()
            .unwrap_or("none")
    );
    println!("{}", crate::child_process::doctor_line());
    println!("{}", standalone_browser.doctor_line());
    print_check(
        language.text(
            "local agent Skill",
            "本地 Agent Skill",
            "ローカル Agent Skill",
        ),
        local_agent_skill.0,
    );
    println!("LAYER local-agent-skill {}", local_agent_skill.1);
    let remote = print_layer_ownership(paths, config, &report, verbose);
    println!(
        "LAYER authenticated-local-mcp {}",
        authenticated_local_mcp.detail
    );
    println!("LAYER authenticated-remote-mcp not_probed reason=no-connector-oauth-credential");
    let service_health = windows_service_healthy
        && runtime_healthy
        && report.herdr_transport_reachable
        && schema_healthy
        && native_call_healthy
        && snapshot_healthy
        && inspect_healthy
        && event_cache.healthy
        && documents_permission.doctor_pass()
        && macos_permissions
            .as_ref()
            .map(crate::macos_permissions::report_doctor_pass)
            .unwrap_or(true);
    let readiness = overall_readiness(service_health, authenticated_local_mcp.state, &remote);
    println!(
        "READINESS service_health={} authenticated_local_mcp={} authenticated_remote_mcp=not_probed overall={}",
        if service_health { "pass" } else { "fail" },
        authenticated_local_mcp.state.as_str(),
        readiness.as_str()
    );
    let doctor_json = json!({
        "service_health": if service_health { "pass" } else { "fail" },
        "authenticated_local_mcp": authenticated_local_mcp.state.as_str(),
        "authenticated_remote_mcp": "not_probed",
        "edge_reachable": remote.edge_state.as_str(),
        "oauth_metadata": remote.oauth_state.as_str(),
        "mcp_surface": remote.mcp_surface_state.as_str(),
        "standalone_extension": standalone_browser.as_json(),
        "herdr_native": native_runtime,
        "overall": readiness.as_str(),
    });
    println!("DOCTOR_JSON {doctor_json}");
    println!("INFO config {}", paths.config_file.display());
    println!("INFO state {}", paths.config_dir.display());
    println!("INFO dev-state {}", paths.dev_state_dir.display());
    if let Some(socket) = &paths.herdr_socket {
        println!("INFO herdr-socket {}", socket.display());
    }
    println!("INFO update-channel {}", config.update_channel.as_str());
    if let Some(count) = methods_result["count"].as_u64() {
        println!("INFO herdr-methods {count}");
    }
    if let Ok(snapshot_result) = &snapshot_result {
        println!("INFO snapshot-source {}", snapshot_result.source.as_str());
        println!(
            "INFO snapshot-counts workspaces={} panes={} agents={}",
            snapshot::collection_count(&snapshot_result.value, "workspaces"),
            snapshot::collection_count(&snapshot_result.value, "panes"),
            snapshot::collection_count(&snapshot_result.value, "agents")
        );
    }
    println!(
        "INFO event-cache cursor={} events={} agents={} workspaces={} panes={} stream-events={} reconcile={} mode={}",
        event_cache.cursor,
        event_cache.digest_events,
        event_cache.agents,
        event_cache.workspaces,
        event_cache.snapshot_panes,
        event_cache.stream_events,
        event_cache.needs_reconcile,
        event_cache.mode
    );
    if let Some(last_event_at) = &event_cache.last_event_at {
        println!("INFO event-cache-last-event {last_event_at}");
    }
    if let Some(error) = &event_cache.error {
        println!("WARN event-cache {error}");
    }

    if json_only {
        std::println!("{doctor_json}");
    } else if !verbose {
        let remote_summary = if remote.edge_state == DiagnosticState::Fail
            || remote.oauth_state == DiagnosticState::Fail
            || remote.mcp_surface_state == DiagnosticState::Fail
        {
            DiagnosticState::Fail
        } else if remote.edge_state == DiagnosticState::Pass
            && remote.oauth_state == DiagnosticState::Pass
            && remote.mcp_surface_state == DiagnosticState::Pass
        {
            DiagnosticState::Pass
        } else {
            DiagnosticState::NotProbed
        };
        let edge_host = resolve_edge_config(config)
            .map(|edge| edge.host)
            .unwrap_or_else(|| {
                language
                    .text("not configured", "未配置", "未設定")
                    .to_owned()
            });

        std::println!("Herdr MCP {}", language.text("doctor", "诊断", "診断"));
        std::println!(
            "{}: {} · {}",
            language.text("Local", "本机", "ローカル"),
            diagnostic_label(if service_health {
                DiagnosticState::Pass
            } else {
                DiagnosticState::Fail
            }),
            language.text(
                "runtime, Herdr and local control checks",
                "运行时、Herdr 与本地控制检查",
                "runtime、Herdr、ローカル制御チェック"
            )
        );
        std::println!(
            "{}: {}",
            language.text("Local MCP", "本地 MCP", "ローカル MCP"),
            diagnostic_label(authenticated_local_mcp.state)
        );
        std::println!("Edge: {} · {edge_host}", diagnostic_label(remote_summary));
        std::println!(
            "{}: SKIP · {}",
            language.text("Remote MCP auth", "远程 MCP 认证", "リモート MCP 認証"),
            language.text(
                "doctor does not use a Connector OAuth credential",
                "doctor 不使用 Connector OAuth 凭据",
                "doctor は Connector OAuth credential を使用しません"
            )
        );

        let mut attention = Vec::new();
        if !service_health {
            attention.push(
                language
                    .text(
                        "A local health check failed; use --verbose for the failing layer",
                        "本机健康检查失败；使用 --verbose 查看失败层",
                        "ローカルの健全性チェックに失敗しました。--verbose で失敗したレイヤーを確認してください",
                    )
                    .to_owned(),
            );
        }
        if authenticated_local_mcp.state == DiagnosticState::Fail {
            attention.push(
                language
                    .text(
                        "Authenticated local MCP check failed",
                        "本地 MCP 认证检查失败",
                        "ローカル MCP 認証チェックに失敗しました",
                    )
                    .to_owned(),
            );
        }
        if remote_summary == DiagnosticState::Fail {
            attention.push(
                language
                    .text(
                        "Edge or public MCP endpoint check failed",
                        "Edge 或公开 MCP 端点检查失败",
                        "Edge または公開 MCP endpoint のチェックに失敗しました",
                    )
                    .to_owned(),
            );
        }
        if cfg!(target_os = "macos") {
            let tcc = crate::tcc_broker::upgrade_status(&paths.config_dir);
            if tcc.metadata_invalid.is_some() {
                attention.push(
                    language
                        .text(
                            "TCC broker metadata needs repair",
                            "TCC broker 元数据需要修复",
                            "TCC broker metadata の修復が必要です",
                        )
                        .to_owned(),
                );
            } else if tcc.update_available {
                attention.push(
                    language
                        .text(
                            "TCC broker update available",
                            "TCC broker 有可用更新",
                            "TCC broker の更新があります",
                        )
                        .to_owned(),
                );
            }
        }
        if doctor_json
            .pointer("/standalone_extension/state")
            .and_then(Value::as_str)
            == Some("drift")
        {
            attention.push(
                language
                    .text(
                        "Chrome standalone extension path has drifted",
                        "Chrome standalone 扩展加载路径已漂移",
                        "Chrome standalone 拡張の読み込みパスがずれています",
                    )
                    .to_owned(),
            );
        }
        if !local_agent_skill.0 {
            attention.push(format!(
                "{}: {}",
                language.text(
                    "Local Agent Skill needs attention",
                    "本地 Agent Skill 需要处理",
                    "ローカル Agent Skill の確認が必要です",
                ),
                local_agent_skill.1
            ));
        }
        if !attention.is_empty() {
            std::println!();
            std::println!("{}:", language.text("Attention", "需要处理", "要確認"));
            for item in attention {
                std::println!("  - {item}");
            }
        }
        std::println!();
        std::println!(
            "{}: {} · {}",
            language.text("Result", "结果", "結果"),
            if readiness == OverallReadiness::Fail {
                "FAIL"
            } else {
                "PASS"
            },
            if readiness == OverallReadiness::Fail {
                language.text(
                    "one or more checked layers failed",
                    "至少一项已检查层失败",
                    "チェック済みレイヤーの一部が失敗しました",
                )
            } else {
                language.text(
                    "all checked layers are healthy",
                    "已检查层均健康",
                    "チェック済みレイヤーはすべて正常です",
                )
            }
        );
        std::println!(
            "{}: herdr-mcp doctor --verbose",
            language.text("Details", "详细信息", "詳細")
        );
        std::println!("JSON: herdr-mcp doctor --json");
    }

    service_health && readiness != OverallReadiness::Fail
}

/// Product-layer ownership map. Local probes always run. When Edge is
/// configured locally, doctor also runs bounded credential-free HTTPS probes.
fn print_layer_ownership(
    paths: &RuntimePaths,
    config: &Config,
    report: &StatusReport,
    verbose: bool,
) -> RemoteProbeReport {
    if verbose {
        println!("LAYER herdr {}", format_herdr_layer(paths, report));
        println!(
            "LAYER local-runtime {}",
            format_local_runtime_layer(paths, config, report.runtime)
        );
        println!("LAYER service {}", format_service_layer());
        println!("LAYER local-ipc {}", format_local_ipc_layer(paths));
        println!("LAYER native-messaging {}", format_native_messaging_layer());
        println!("LAYER link {}", format_link_layer(paths));
        println!(
            "LAYER link-transport {}",
            format_link_transport_layer(paths, config)
        );
        println!(
            "LAYER relay-pool {}",
            crate::link::relay_manifest::status_line(paths, unix_now_seconds())
        );
    }
    let edge = resolve_edge_config(config);
    if verbose {
        println!("LAYER edge {}", format_edge_configured_layer(&edge, config));
    }
    let remote = edge
        .as_ref()
        .map(probe_edge_remote)
        .unwrap_or(RemoteProbeReport::absent());
    if verbose {
        println!("LAYER edge-reachable {}", remote.edge_reachable);
        println!("LAYER oauth-metadata {}", remote.oauth_metadata);
        println!("LAYER mcp-endpoint {}", remote.mcp_endpoint);
        println!("LAYER update-state {}", format_update_state_layer(paths));
        println!("{}", crate::residue::doctor_line());
    }
    remote
}

fn unix_now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

fn format_herdr_layer(paths: &RuntimePaths, report: &StatusReport) -> String {
    let sock = paths
        .herdr_socket
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "unset".to_owned());
    if report.herdr_transport_reachable {
        format!("owned reachable sock={sock}")
    } else {
        format!("unowned unreachable sock={sock}")
    }
}

fn format_local_runtime_layer(
    paths: &RuntimePaths,
    config: &Config,
    health: RuntimeHealth,
) -> String {
    let current = paths.config_dir.join("runtime").join("current");
    let generation = read_runtime_generation(&current);
    let health_label = match health {
        RuntimeHealth::Healthy(code) => format!("healthy http={code}"),
        RuntimeHealth::UnexpectedHttp(code) => format!("unexpected http={code}"),
        RuntimeHealth::Unreachable => "unreachable".to_owned(),
    };
    match generation {
        Ok(Some(generation)) => format!(
            "owned {health_label} port={} generation={generation}",
            config.runtime_port
        ),
        Ok(None) => format!(
            "unowned {health_label} port={} generation=missing",
            config.runtime_port
        ),
        Err(detail) => format!(
            "unowned {health_label} port={} generation=invalid detail={detail}",
            config.runtime_port
        ),
    }
}

fn format_service_layer() -> String {
    match service_manager::doctor_status() {
        Ok(value) => {
            let implementation = value
                .get("implementation")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let loaded = value
                .get("loaded")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let healthy = value
                .get("healthy")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let label = value
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or("dev.herdr-mcp.server");
            let generation = value
                .get("generation")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let ownership = if implementation.starts_with("rust") && loaded {
                "owned"
            } else if implementation == "missing" {
                "absent"
            } else {
                "unowned"
            };
            format!(
                "{ownership} implementation={implementation} loaded={loaded} healthy={healthy} label={label} generation={generation}"
            )
        }
        Err(error) => format!("error detail={}", compact_detail(&error)),
    }
}

fn format_local_ipc_layer(paths: &RuntimePaths) -> String {
    let path = paths.config_dir.join("extension.sock");
    match inspect_unix_socket(&path) {
        SocketView::Present { mode } => {
            format!("owned present mode={mode:04o} path={}", path.display())
        }
        SocketView::Absent => format!("absent path={}", path.display()),
        SocketView::Invalid { detail } => {
            format!("unowned invalid path={} detail={detail}", path.display())
        }
    }
}

fn format_native_messaging_layer() -> String {
    match native_host_install::doctor_status() {
        Ok(value) => {
            let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
            let owned = value
                .get("owned_manifest_count")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let wrapper_ok = value
                .get("wrapper_ok")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let runtime_ok = value
                .get("runtime_binary_ok")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let runtime_matches = value
                .get("runtime_matches_current")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let version_consistent = value
                .get("version_consistent")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let stale_runtime = value
                .get("stale_runtime")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let ownership = if ok {
                "owned"
            } else if owned == 0 && !wrapper_ok && !runtime_ok {
                "absent"
            } else {
                "unowned"
            };
            let stale = if stale_runtime { " stale-runtime" } else { "" };
            format!(
                "{ownership}{stale} manifests={owned} wrapper_ok={wrapper_ok} runtime_binary_ok={runtime_ok} runtime_matches_current={runtime_matches} version_consistent={version_consistent}"
            )
        }
        Err(error) => format!("error detail={}", compact_detail(&error)),
    }
}

fn format_link_layer(_paths: &RuntimePaths) -> String {
    #[cfg(target_os = "linux")]
    {
        match crate::linux_service_manager::link_status_report() {
            Ok(report) => format_linux_link_layer_report(&report),
            Err(error) => format!("error detail={}", compact_detail(&error)),
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let home = home_dir().unwrap_or_else(|| PathBuf::from("."));
        crate::link::doctor_layer_summary(&home, &_paths.config_dir)
    }
}

#[cfg(any(target_os = "linux", test))]
fn format_linux_link_layer_report(report: &Value) -> String {
    let owner = report
        .get("production_owner")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let implementation = report
        .get("implementation")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let link_loaded = report
        .get("link_loaded")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let eligible = report
        .get("production_ready_eligible")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ownership = if owner == "rust" {
        "owned"
    } else if owner == "absent" {
        "absent"
    } else {
        "unowned"
    };
    format!(
        "{ownership} production_owner={owner} prod_impl=not-applicable prod_loaded=false link_impl={implementation} link_loaded={link_loaded} candidate_label=not-applicable production_ready_eligible={eligible} remote-probe=edge-layer"
    )
}

fn format_link_transport_layer(paths: &RuntimePaths, config: &Config) -> String {
    let pool = crate::link::relay_manifest::load_cached_pool(paths, unix_now_seconds());
    let evidence = crate::link::collect_transport_evidence_with_pool(
        config.edge_public_origin.as_deref(),
        config.edge_link_upstream_origin.as_deref(),
        &pool.relays,
        pool.source,
    );
    format!(
        "mcp_origin={} link_upstream={} live_transport={} configured_preferred_transport={} proxy_source={} relay={} relay_policy={} relay_selection={} pool_source={} failover_ready={}",
        evidence.mcp_origin,
        evidence.link_upstream,
        evidence.live_transport,
        evidence.configured_preferred_transport,
        evidence.proxy_source,
        evidence.relay,
        evidence.relay_policy,
        evidence.relay_selection,
        evidence.pool_source,
        evidence.failover_ready,
    )
}

fn format_edge_configured_layer(edge: &Option<EdgeConfigView>, config: &Config) -> String {
    let upstream_info = match config.edge_link_upstream_origin.as_deref() {
        Some(upstream) => format!(" upstream={upstream}"),
        None => String::new(),
    };
    match edge {
        Some(edge) => format!(
            "configured-local source={} label={} host={} origin={}{} plist={}",
            edge.source.as_str(),
            edge.label.as_deref().unwrap_or("-"),
            edge.host,
            edge.origin,
            upstream_info,
            edge.plist
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "unset".to_owned())
        ),
        None => "unconfigured reason=no-link-plist-or-HERDR_EDGE_URL".to_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeConfigSource {
    ConfigFile,
    LinkProdPlist,
    LinkPlist,
    LinkCandidatePlist,
    ProcessEnv,
}

impl EdgeConfigSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::ConfigFile => "config-file",
            Self::LinkProdPlist => "link-prod-plist",
            Self::LinkPlist => "link-plist",
            Self::LinkCandidatePlist => "link-candidate-plist",
            Self::ProcessEnv => "link-env",
        }
    }
}

#[derive(Debug, Clone)]
struct EdgeConfigView {
    host: String,
    origin: String,
    plist: Option<PathBuf>,
    source: EdgeConfigSource,
    label: Option<String>,
}

#[derive(Debug, Clone)]
struct RemoteProbeReport {
    edge_reachable: String,
    oauth_metadata: String,
    mcp_endpoint: String,
    edge_state: DiagnosticState,
    oauth_state: DiagnosticState,
    mcp_surface_state: DiagnosticState,
}

impl RemoteProbeReport {
    fn absent() -> Self {
        Self {
            edge_reachable: "skipped reason=edge-unconfigured".to_owned(),
            oauth_metadata: "skipped reason=edge-unconfigured".to_owned(),
            mcp_endpoint: "skipped reason=edge-unconfigured".to_owned(),
            edge_state: DiagnosticState::NotProbed,
            oauth_state: DiagnosticState::NotProbed,
            mcp_surface_state: DiagnosticState::NotProbed,
        }
    }
}

fn resolve_edge_config(config: &Config) -> Option<EdgeConfigView> {
    let home = home_dir();

    // Check if a real link LaunchAgent plist exists
    let plist_info = home.as_ref().and_then(|h| {
        let plist_candidates = [
            ("dev.herdr-mcp.link-prod", EdgeConfigSource::LinkProdPlist),
            ("dev.herdr-mcp.link", EdgeConfigSource::LinkPlist),
            (
                "dev.herdr-mcp.link-rust-candidate",
                EdgeConfigSource::LinkCandidatePlist,
            ),
        ];
        for (label, source) in plist_candidates {
            let path = h
                .join("Library")
                .join("LaunchAgents")
                .join(format!("{label}.plist"));
            if path.is_file() {
                let host = edge_host_from_plist(&path);
                return Some((path, label.to_owned(), source, host));
            }
        }
        None
    });

    // If [edge].public_origin is configured in config.json, it is the authoritative public identity
    if let Some(public_origin) = config.edge_public_origin.as_deref()
        && let Ok(parsed) = url::Url::parse(public_origin)
        && let Some(host) = parsed.host_str()
    {
        return Some(EdgeConfigView {
            host: host.to_owned(),
            origin: public_origin.to_owned(),
            plist: plist_info.as_ref().map(|(p, _, _, _)| p.clone()),
            source: EdgeConfigSource::ConfigFile,
            label: plist_info.as_ref().map(|(_, l, _, _)| l.clone()),
        });
    }

    if let Some((path, label, source, Some(host))) = plist_info
        && let Some(origin) = https_origin_for_host(&host)
    {
        return Some(EdgeConfigView {
            host,
            origin,
            plist: Some(path),
            source,
            label: Some(label),
        });
    }

    let edge_url = std::env::var("HERDR_EDGE_URL")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())?;
    let host = edge_host(&edge_url)?;
    let origin = https_origin_for_host(&host)?;
    Some(EdgeConfigView {
        host,
        origin,
        plist: None,
        source: EdgeConfigSource::ProcessEnv,
        label: None,
    })
}

fn https_origin_for_host(host: &str) -> Option<String> {
    let host = host.trim();
    if host.is_empty() || host.contains('/') || host.contains('@') || host.contains(' ') {
        return None;
    }
    // Refuse credential-shaped hosts and keep output host-only.
    if host.contains(':') && !host.starts_with('[') {
        // allow host:port
        let (name, port) = host.split_once(':')?;
        if name.is_empty() || port.parse::<u16>().is_err() {
            return None;
        }
    }
    Some(format!("https://{host}"))
}

fn probe_edge_remote(edge: &EdgeConfigView) -> RemoteProbeReport {
    let client = match remote_probe_client() {
        Ok(client) => client,
        Err(detail) => {
            let failed = format!("error detail={}", compact_detail(&detail));
            return RemoteProbeReport {
                edge_reachable: failed.clone(),
                oauth_metadata: failed.clone(),
                mcp_endpoint: failed,
                edge_state: DiagnosticState::Fail,
                oauth_state: DiagnosticState::Fail,
                mcp_surface_state: DiagnosticState::Fail,
            };
        }
    };

    let health_url = format!("{}/health", edge.origin);
    let oauth_url = format!("{}/.well-known/oauth-authorization-server", edge.origin);
    let mcp_url = format!("{}/mcp", edge.origin);

    let (edge_reachable, edge_state) =
        match probe_https_get(&client, &health_url, RemoteExpect::Health) {
            Ok(summary) => (format!("reachable {summary}"), DiagnosticState::Pass),
            Err(detail) => (
                format!("unreachable detail={}", compact_detail(&detail)),
                DiagnosticState::Fail,
            ),
        };
    let (oauth_metadata, oauth_state) =
        match probe_https_get(&client, &oauth_url, RemoteExpect::OauthMetadata) {
            Ok(summary) => (format!("reachable {summary}"), DiagnosticState::Pass),
            Err(detail) => (
                format!("unreachable detail={}", compact_detail(&detail)),
                DiagnosticState::Fail,
            ),
        };
    let (mcp_endpoint, mcp_surface_state) =
        match probe_https_get(&client, &mcp_url, RemoteExpect::McpEndpoint) {
            Ok(summary) => (format!("reachable {summary}"), DiagnosticState::Pass),
            Err(detail) => (
                format!("unreachable detail={}", compact_detail(&detail)),
                DiagnosticState::Fail,
            ),
        };

    RemoteProbeReport {
        edge_reachable,
        oauth_metadata,
        mcp_endpoint,
        edge_state,
        oauth_state,
        mcp_surface_state,
    }
}

#[derive(Debug, Clone, Copy)]
enum RemoteExpect {
    Health,
    OauthMetadata,
    McpEndpoint,
}

const REMOTE_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const REMOTE_PROBE_MAX_BYTES: usize = 64 * 1024;

fn remote_probe_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(REMOTE_PROBE_TIMEOUT)
        .connect_timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|error| format!("cannot build remote probe client: {error}"))
}

fn probe_https_get(
    client: &reqwest::blocking::Client,
    url: &str,
    expect: RemoteExpect,
) -> Result<String, String> {
    let parsed = url::Url::parse(url).map_err(|_| "invalid probe URL".to_owned())?;
    if parsed.scheme() != "https" {
        return Err("remote probe requires https".to_owned());
    }
    if parsed.username() != "" || parsed.password().is_some() {
        return Err("probe URL must not carry credentials".to_owned());
    }
    if parsed.query().is_some() {
        return Err("probe URL must not carry query credentials".to_owned());
    }

    let response = client
        .get(parsed)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .map_err(|error| format!("request failed: {error}"))?;
    let status = response.status().as_u16();
    let bytes = response
        .bytes()
        .map_err(|error| format!("read failed: {error}"))?;
    if bytes.len() > REMOTE_PROBE_MAX_BYTES {
        return Err("response exceeds probe byte budget".to_owned());
    }
    let body = String::from_utf8_lossy(&bytes);

    match expect {
        RemoteExpect::Health => {
            if status != 200 {
                return Err(format!("unexpected http={status}"));
            }
            let service =
                json_string_field(&body, "service").unwrap_or_else(|| "unknown".to_owned());
            let epoch = json_u64_field(&body, "contractEpoch")
                .map(|epoch| epoch.to_string())
                .unwrap_or_else(|| "unknown".to_owned());
            Ok(format!(
                "http={status} service={} contract_epoch={}",
                sanitize_probe_token(&service),
                sanitize_probe_token(&epoch)
            ))
        }
        RemoteExpect::OauthMetadata => {
            if status != 200 {
                return Err(format!("unexpected http={status}"));
            }
            let issuer = json_string_field(&body, "issuer")
                .and_then(|issuer| issuer_host(&issuer))
                .unwrap_or_else(|| "unknown".to_owned());
            Ok(format!(
                "http={status} issuer_host={}",
                sanitize_probe_token(&issuer)
            ))
        }
        RemoteExpect::McpEndpoint => {
            // Never send Authorization. 401 proves the public MCP surface exists.
            if matches!(status, 200 | 401) {
                Ok(format!("http={status} auth=not-sent"))
            } else {
                Err(format!("unexpected http={status}"))
            }
        }
    }
}

fn json_string_field(body: &str, key: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn json_u64_field(body: &str, key: &str) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value.get(key).and_then(Value::as_u64)
}

fn issuer_host(issuer: &str) -> Option<String> {
    let parsed = url::Url::parse(issuer).ok()?;
    if parsed.username() != "" || parsed.password().is_some() || parsed.query().is_some() {
        return None;
    }
    parsed.host_str().map(str::to_owned)
}

pub(crate) fn sanitize_probe_token(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.contains("token")
        || lower.contains("secret")
        || lower.contains("bearer")
        || lower.contains("authorization")
        || value.contains('=')
        || value.len() > 96
    {
        return "redacted".to_owned();
    }
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ':') {
                ch
            } else {
                '-'
            }
        })
        .take(64)
        .collect()
}

fn format_update_state_layer(paths: &RuntimePaths) -> String {
    let db = paths.config_dir.join("update").join("state.db");
    if !db.is_file() {
        return "absent db=missing".to_owned();
    }
    match UpdateStore::open(paths).and_then(|store| store.latest_update_job()) {
        Ok(Some(job)) => format!(
            "owned job={} version={} state={}",
            job.job_id, job.version, job.state
        ),
        Ok(None) => "owned job=none".to_owned(),
        Err(error) => format!("error detail={}", compact_detail(&error)),
    }
}

fn read_runtime_generation(current: &Path) -> Result<Option<String>, String> {
    match fs::symlink_metadata(current) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot stat runtime/current: {error}")),
        Ok(metadata) => {
            #[cfg(target_os = "windows")]
            if metadata.is_dir() {
                let marker = current.join("generation");
                let generation = fs::read_to_string(&marker).map_err(|error| {
                    format!("cannot read runtime/current generation marker: {error}")
                })?;
                let generation = generation.trim();
                if !generation.starts_with("rust-") {
                    return Err(format!("unmanaged generation marker {generation}"));
                }
                return Ok(Some(generation.to_owned()));
            }
            if !metadata.file_type().is_symlink() {
                return Err("runtime/current is not a symlink".to_owned());
            }
            let target = fs::read_link(current)
                .map_err(|error| format!("cannot read runtime/current: {error}"))?;
            let name = target
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or_else(|| "runtime/current target is not a generation id".to_owned())?;
            if !name.starts_with("rust-") {
                return Err(format!("unmanaged generation target {name}"));
            }
            Ok(Some(name.to_owned()))
        }
    }
}

#[derive(Debug)]
enum SocketView {
    Present { mode: u32 },
    Absent,
    Invalid { detail: String },
}

fn inspect_unix_socket(path: &Path) -> SocketView {
    #[cfg(unix)]
    {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => SocketView::Absent,
            Err(error) => SocketView::Invalid {
                detail: format!("stat-failed:{error}"),
            },
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return SocketView::Invalid {
                        detail: "symlink-refused".to_owned(),
                    };
                }
                if !metadata.file_type().is_socket() {
                    return SocketView::Invalid {
                        detail: "not-a-socket".to_owned(),
                    };
                }
                SocketView::Present {
                    mode: metadata.permissions().mode() & 0o777,
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        SocketView::Invalid {
            detail: "unix-socket-unsupported".to_owned(),
        }
    }
}

fn edge_host_from_plist(path: &Path) -> Option<String> {
    let value = plist::Value::from_file(path).ok()?;
    let env = value
        .as_dictionary()?
        .get("EnvironmentVariables")?
        .as_dictionary()?;
    let edge_url = env.get("HERDR_EDGE_URL")?.as_string()?;
    edge_host(edge_url)
}

fn edge_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if !(trimmed.starts_with("wss://") || trimmed.starts_with("ws://")) {
        return None;
    }
    let rest = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let host = rest.split(['/', '?', '#']).next()?.trim();
    if host.is_empty() {
        None
    } else {
        Some(host.to_owned())
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn compact_detail(detail: &str) -> String {
    detail
        .chars()
        .map(|ch| if ch.is_whitespace() { '-' } else { ch })
        .take(120)
        .collect()
}

fn probe_event_cache(paths: &RuntimePaths) -> EventCacheProbe {
    let Some(socket) = paths.herdr_socket.as_ref() else {
        return EventCacheProbe {
            healthy: false,
            mode: "failed",
            cursor: 0,
            digest_events: 0,
            agents: 0,
            workspaces: 0,
            snapshot_panes: 0,
            stream_events: 0,
            last_event_at: None,
            needs_reconcile: false,
            error: Some("Herdr local transport is unavailable".to_owned()),
        };
    };

    let mut cache = EventCache::start(HerdrClient::new(socket));
    // Keep the initial ready/live waits unchanged. The extra reconcile budget only
    // covers an already-observed resubscribe / needs_reconcile window so doctor
    // does not randomly FAIL while the cache is mid-cycle.
    let health = cache.wait_for_doctor_probe(
        Duration::from_secs(2),
        Duration::from_secs(1),
        Duration::from_secs(2),
    );
    let since_result = native_tools::since(&cache, 0, None, Ok(None));
    let snapshot_state = cache.snapshot();
    let diagnostics = cache.diagnostics();
    let since_ok = since_result["ok"].as_bool() == Some(true);
    let error = health
        .error_message()
        .map(str::to_owned)
        .or_else(|| cache.last_error());
    cache.shutdown();

    let cursor = since_result["cursor"].as_u64().unwrap_or(0);
    let digest_events = since_result["events"].as_array().map(Vec::len).unwrap_or(0);
    let agents = since_result["agents"].as_array().map(Vec::len).unwrap_or(0);
    let workspaces = since_result["workspaces"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);

    EventCacheProbe {
        healthy: event_cache_doctor_pass(&health, since_ok),
        mode: health.mode(),
        cursor,
        digest_events,
        agents,
        workspaces,
        snapshot_panes: snapshot::collection_count(&snapshot_state, "panes"),
        stream_events: diagnostics.event_count,
        last_event_at: diagnostics.last_event_at,
        needs_reconcile: diagnostics.needs_reconcile,
        error,
    }
}

fn event_cache_doctor_pass(health: &EventCacheHealth, since_ok: bool) -> bool {
    health.doctor_pass() && since_ok
}

fn probe_runtime(port: u16) -> RuntimeHealth {
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(500)) else {
        return RuntimeHealth::Unreachable;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(750)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(750)));

    let body = r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{}}"#;
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return RuntimeHealth::Unreachable;
    }

    let mut buffer = [0_u8; 512];
    let Ok(count) = stream.read(&mut buffer) else {
        return RuntimeHealth::Unreachable;
    };
    let response = String::from_utf8_lossy(&buffer[..count]);
    match parse_http_status(&response) {
        Some(code @ (200 | 401)) => RuntimeHealth::Healthy(code),
        Some(code) => RuntimeHealth::UnexpectedHttp(code),
        None => RuntimeHealth::Unreachable,
    }
}

fn probe_herdr_transport(paths: &RuntimePaths) -> bool {
    paths
        .herdr_socket
        .as_ref()
        .is_some_and(|socket| HerdrClient::new(socket).ping().is_ok())
}

fn probe_authenticated_local_mcp(port: u16) -> AuthenticatedMcpProbe {
    let token = match service_manager::doctor_runtime_token() {
        Ok(Some(token)) => token,
        Ok(None) => {
            return AuthenticatedMcpProbe {
                state: DiagnosticState::NotProbed,
                detail: "not_probed reason=runtime-bearer-unavailable".to_owned(),
            };
        }
        Err(error) => {
            return AuthenticatedMcpProbe {
                state: DiagnosticState::NotProbed,
                detail: format!(
                    "not_probed reason=runtime-bearer-unavailable detail={}",
                    compact_detail(&error)
                ),
            };
        }
    };
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .connect_timeout(Duration::from_secs(1))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return AuthenticatedMcpProbe {
                state: DiagnosticState::Fail,
                detail: format!(
                    "fail phase=client detail={}",
                    compact_detail(&error.to_string())
                ),
            };
        }
    };
    let url = format!("http://127.0.0.1:{port}/mcp");
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": "doctor-init",
        "method": "initialize",
        "params": {
            "protocolVersion": crate::mcp::SDK_WIRE_PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": "herdr-doctor", "version": crate::runtime_meta::runtime_version()}
        }
    });
    let response = match client
        .post(&url)
        .bearer_auth(&token)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&initialize)
        .send()
    {
        Ok(response) => response,
        Err(error) => {
            return AuthenticatedMcpProbe {
                state: DiagnosticState::Fail,
                detail: format!(
                    "fail phase=initialize detail={}",
                    compact_detail(&error.to_string())
                ),
            };
        }
    };
    if response.status().as_u16() != 200 {
        return AuthenticatedMcpProbe {
            state: DiagnosticState::Fail,
            detail: format!("fail phase=initialize http={}", response.status().as_u16()),
        };
    }
    let session_id = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let initialize_bytes = match response.bytes() {
        Ok(bytes) => bytes,
        Err(error) => {
            return AuthenticatedMcpProbe {
                state: DiagnosticState::Fail,
                detail: format!(
                    "fail phase=initialize-read detail={}",
                    compact_detail(&error.to_string())
                ),
            };
        }
    };
    let initialize_payload: Value = match serde_json::from_slice(&initialize_bytes) {
        Ok(value) => value,
        Err(_) => {
            return AuthenticatedMcpProbe {
                state: DiagnosticState::Fail,
                detail: "fail phase=initialize-decode".to_owned(),
            };
        }
    };
    let Some(protocol) = initialize_payload
        .pointer("/result/protocolVersion")
        .and_then(Value::as_str)
    else {
        return AuthenticatedMcpProbe {
            state: DiagnosticState::Fail,
            detail: "fail phase=initialize-result".to_owned(),
        };
    };
    let Some(session_id) = session_id else {
        return AuthenticatedMcpProbe {
            state: DiagnosticState::Fail,
            detail: "fail phase=initialize-session".to_owned(),
        };
    };

    let list =
        json!({"jsonrpc": "2.0", "id": "doctor-tools", "method": "tools/list", "params": {}});
    let response = match client
        .post(&url)
        .bearer_auth(&token)
        .header(reqwest::header::ACCEPT, "application/json")
        .header("mcp-session-id", &session_id)
        .json(&list)
        .send()
    {
        Ok(response) => response,
        Err(error) => {
            let _ = client
                .delete(&url)
                .bearer_auth(&token)
                .header("mcp-session-id", &session_id)
                .send();
            return AuthenticatedMcpProbe {
                state: DiagnosticState::Fail,
                detail: format!(
                    "fail phase=tools-list detail={}",
                    compact_detail(&error.to_string())
                ),
            };
        }
    };
    let status = response.status().as_u16();
    let list_bytes = response.bytes();
    let _ = client
        .delete(&url)
        .bearer_auth(&token)
        .header("mcp-session-id", &session_id)
        .send();
    if status != 200 {
        return AuthenticatedMcpProbe {
            state: DiagnosticState::Fail,
            detail: format!("fail phase=tools-list http={status}"),
        };
    }
    let list_payload: Value = match list_bytes
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    {
        Some(value) => value,
        None => {
            return AuthenticatedMcpProbe {
                state: DiagnosticState::Fail,
                detail: "fail phase=tools-list-decode".to_owned(),
            };
        }
    };
    let Some(tools) = list_payload
        .pointer("/result/tools")
        .and_then(Value::as_array)
    else {
        return AuthenticatedMcpProbe {
            state: DiagnosticState::Fail,
            detail: "fail phase=tools-list-result".to_owned(),
        };
    };
    AuthenticatedMcpProbe {
        state: DiagnosticState::Pass,
        detail: format!(
            "pass protocol={} tools={}",
            sanitize_probe_token(protocol),
            tools.len()
        ),
    }
}

fn overall_readiness(
    service_health: bool,
    local_authenticated: DiagnosticState,
    remote: &RemoteProbeReport,
) -> OverallReadiness {
    if !service_health
        || local_authenticated == DiagnosticState::Fail
        || remote.edge_state == DiagnosticState::Fail
        || remote.oauth_state == DiagnosticState::Fail
        || remote.mcp_surface_state == DiagnosticState::Fail
    {
        OverallReadiness::Fail
    } else {
        // Doctor intentionally does not possess or mint a user's Connector
        // OAuth credential. A healthy public MCP surface therefore proves
        // reachability, not authenticated end-to-end Connector usability.
        OverallReadiness::NotProven
    }
}

fn parse_http_status(response: &str) -> Option<u16> {
    let first_line = response.lines().next()?;
    let mut parts = first_line.split_whitespace();
    let protocol = parts.next()?;
    if !protocol.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

fn runtime_label(health: RuntimeHealth, port: u16) -> String {
    match health {
        RuntimeHealth::Healthy(code) => format!("127.0.0.1:{port} healthy (HTTP {code})"),
        RuntimeHealth::UnexpectedHttp(code) => {
            format!("127.0.0.1:{port} unexpected response (HTTP {code})")
        }
        RuntimeHealth::Unreachable => format!("127.0.0.1:{port} unreachable"),
    }
}

fn diagnostic_label(state: DiagnosticState) -> &'static str {
    match state {
        DiagnosticState::Pass => "PASS",
        DiagnosticState::Fail => "FAIL",
        DiagnosticState::NotProbed => "SKIP",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_link_doctor_layer_uses_native_linux_status_report() {
        let layer = format_linux_link_layer_report(&serde_json::json!({
            "implementation": "rust-systemd-user",
            "production_owner": "rust",
            "production_ready_eligible": true,
            "link_loaded": true,
        }));
        assert!(layer.starts_with("owned "));
        assert!(layer.contains("production_owner=rust"));
        assert!(layer.contains("link_impl=rust-systemd-user"));
        assert!(layer.contains("link_loaded=true"));
        assert!(layer.contains("production_ready_eligible=true"));
        assert!(!layer.contains("production_owner=absent"));
    }

    #[test]
    fn parses_http_status_line() {
        assert_eq!(
            parse_http_status("HTTP/1.1 401 Unauthorized\r\n"),
            Some(401)
        );
        assert_eq!(parse_http_status("HTTP/1.1 200 OK\r\n"), Some(200));
        assert_eq!(parse_http_status("not-http"), None);
    }

    #[test]
    fn labels_runtime_state() {
        assert!(runtime_label(RuntimeHealth::Healthy(401), 8772).contains("healthy"));
        assert!(runtime_label(RuntimeHealth::Unreachable, 8772).contains("unreachable"));
    }

    #[test]
    fn extracts_edge_host_without_credentials() {
        assert_eq!(
            edge_host("wss://herdr-edge-prod.example/ws?link_token=secret"),
            Some("herdr-edge-prod.example".to_owned())
        );
        assert_eq!(edge_host("https://example"), None);
    }

    #[test]
    fn remote_probe_helpers_never_echo_secrets() {
        assert_eq!(
            https_origin_for_host("herdr-edge-prod.example").as_deref(),
            Some("https://herdr-edge-prod.example")
        );
        assert_eq!(https_origin_for_host("user:pass@host"), None);
        assert_eq!(
            issuer_host("https://issuer.example/oauth").as_deref(),
            Some("issuer.example".to_owned()).as_deref()
        );
        assert_eq!(
            issuer_host("https://issuer.example/oauth?token=secret"),
            None
        );
        assert_eq!(sanitize_probe_token("herdr-edge-prod"), "herdr-edge-prod");
        assert_eq!(sanitize_probe_token("Bearer abc"), "redacted");
        assert_eq!(sanitize_probe_token("link_token=secret"), "redacted");
    }

    #[test]
    fn mcp_endpoint_accepts_unauthorized_without_sending_auth() {
        assert!(matches!(
            RemoteExpect::McpEndpoint,
            RemoteExpect::McpEndpoint
        ));
        let body = r#"{"service":"herdr-edge-prod","contractEpoch":2}"#;
        assert_eq!(
            json_string_field(body, "service").as_deref(),
            Some("herdr-edge-prod")
        );
        assert_eq!(json_u64_field(body, "contractEpoch"), Some(2));
    }

    #[test]
    fn readiness_never_promotes_unauthenticated_remote_surface_to_ready() {
        let mut remote = RemoteProbeReport::absent();
        remote.edge_state = DiagnosticState::Pass;
        remote.oauth_state = DiagnosticState::Pass;
        remote.mcp_surface_state = DiagnosticState::Pass;
        assert_eq!(
            overall_readiness(true, DiagnosticState::Pass, &remote),
            OverallReadiness::NotProven
        );
    }

    #[test]
    fn readiness_fails_on_known_authenticated_or_remote_breakage() {
        let mut remote = RemoteProbeReport::absent();
        remote.edge_state = DiagnosticState::Pass;
        remote.oauth_state = DiagnosticState::Pass;
        remote.mcp_surface_state = DiagnosticState::Pass;
        assert_eq!(
            overall_readiness(true, DiagnosticState::Fail, &remote),
            OverallReadiness::Fail
        );
        remote.oauth_state = DiagnosticState::Fail;
        assert_eq!(
            overall_readiness(true, DiagnosticState::Pass, &remote),
            OverallReadiness::Fail
        );
    }

    #[test]
    fn reads_managed_runtime_generation_symlink() {
        let root = std::env::temp_dir().join(format!(
            "herdr-doctor-gen-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("generations").join("rust-abc123")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink("generations/rust-abc123", root.join("current")).unwrap();
            assert_eq!(
                read_runtime_generation(&root.join("current")).unwrap(),
                Some("rust-abc123".to_owned())
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn edge_config_source_labels_candidate_and_env_paths() {
        assert_eq!(
            EdgeConfigSource::LinkCandidatePlist.as_str(),
            "link-candidate-plist"
        );
        assert_eq!(EdgeConfigSource::ProcessEnv.as_str(), "link-env");
        let edge = EdgeConfigView {
            host: "herdr-edge-device.username.workers.dev".to_owned(),
            origin: "https://herdr-edge-device.username.workers.dev".to_owned(),
            plist: None,
            source: EdgeConfigSource::ProcessEnv,
            label: None,
        };
        let formatted = format_edge_configured_layer(&Some(edge), &Config::default());
        assert!(formatted.contains("source=link-env"));
        assert!(!formatted.contains("unconfigured"));
    }

    #[test]
    fn split_edge_layer_keeps_public_origin_upstream_and_plist_evidence_distinct() {
        let edge = EdgeConfigView {
            host: "custom.example".to_owned(),
            origin: "https://custom.example".to_owned(),
            plist: Some(PathBuf::from(
                "/Users/test/Library/LaunchAgents/dev.herdr-mcp.link-prod.plist",
            )),
            source: EdgeConfigSource::ConfigFile,
            label: Some("dev.herdr-mcp.link-prod".to_owned()),
        };
        let config = Config {
            edge_public_origin: Some("https://custom.example".to_owned()),
            edge_link_upstream_origin: Some("https://backend.workers.dev".to_owned()),
            ..Config::default()
        };

        let formatted = format_edge_configured_layer(&Some(edge), &config);
        assert!(formatted.contains("origin=https://custom.example"));
        assert!(formatted.contains("upstream=https://backend.workers.dev"));
        assert!(formatted.contains("dev.herdr-mcp.link-prod.plist"));
        assert!(formatted.contains("label=dev.herdr-mcp.link-prod"));
    }

    #[test]
    fn unconfigured_edge_layer_names_missing_link_and_env() {
        let formatted = format_edge_configured_layer(&None, &Config::default());
        assert!(formatted.contains("unconfigured"));
        assert!(formatted.contains("HERDR_EDGE_URL"));
    }

    #[test]
    fn event_cache_doctor_pass_accepts_healthy_and_reconciling() {
        assert!(event_cache_doctor_pass(&EventCacheHealth::Healthy, true));
        assert!(event_cache_doctor_pass(
            &EventCacheHealth::Reconciling,
            true
        ));
        assert!(!event_cache_doctor_pass(
            &EventCacheHealth::Failed("boom".to_owned()),
            true
        ));
        assert!(!event_cache_doctor_pass(&EventCacheHealth::Healthy, false));
        assert!(!event_cache_doctor_pass(
            &EventCacheHealth::Reconciling,
            false
        ));
    }
}
