#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpSection {
    General,
    Worker,
    Connector,
    Automation,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help {
        section: HelpSection,
    },
    Version,
    Status,
    Doctor,
    Uninstall,
    Reinstall,
    DocumentsProbe,
    HerdrSupervisor(HerdrSupervisorCommand),
    Scan {
        json: bool,
        refresh: bool,
        probe: bool,
    },
    Config(ConfigCommand),
    Worker(WorkerCommand),
    Dev(DevCommand),
    Candidate {
        port: u16,
    },
    Service(ServiceCommand),
    Update(UpdateCommand),
    Extension(ExtensionCommand),
    NativeHost(NativeHostCommand),
    ExtensionHost {
        caller_origin: String,
    },
    ArtifactImport(crate::artifact_import::ImportArgs),
    Link(LinkCommand),
    TccBroker(TccBrokerCommand),
    TccBrokerRun,
    CredentialHelperRun,
    Permissions(crate::macos_permissions::PermissionsCommand),
}

#[derive(Debug, PartialEq, Eq)]
pub enum HerdrSupervisorCommand {
    Install,
    Status,
    Enable,
    Disable,
    Start,
    Stop,
    Run,
    RunOnce,
    Uninstall,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LinkCommand {
    Status,
    /// Foreground staged Rust Link candidate. Does not cut over production LaunchAgents.
    Run,
    /// Install candidate LaunchAgent `dev.herdr-mcp.link-rust-candidate` → runtime/current link run.
    Install,
    /// Remove only the Rust Link candidate LaunchAgent. Never touches live Node link/link-prod.
    Uninstall,
    /// Production Link cutover planner/executor/rollback. Default is dry-run.
    Cutover {
        mode: crate::link::CutoverMode,
    },
    /// Auditable production_ready seal (P0-6). Never auto-flipped from LaunchAgent alone.
    Seal {
        mode: crate::link::SealMode,
    },
    /// Prepare / apply Rust-compatible prod runtime-control generation (no LaunchAgent cut).
    MigrateRuntimeControl {
        mode: crate::link::MigrateMode,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigCommand {
    Path,
    Show,
    Init { edge_origin: Option<String> },
    SetEdgeOrigin { edge_origin: String },
}

#[derive(Debug, PartialEq, Eq)]
pub enum WorkerCommand {
    Pair {
        ttl_seconds: u64,
        name: Option<String>,
    },
    Connect {
        pairing_address: String,
        name: Option<String>,
    },
    Rename {
        name: String,
    },
    Revoke {
        device_id: String,
    },
    ConnectorApprove {
        request_id: String,
    },
    ConnectorList,
    ConnectorRevoke {
        connector_id: String,
    },
    AutomationCreate {
        name: String,
        device: String,
    },
    AutomationList,
    AutomationRotate {
        client_id: String,
    },
    AutomationRevoke {
        client_id: String,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum TccBrokerCommand {
    Install { force: bool },
    Status,
    Uninstall,
}

#[derive(Debug, PartialEq, Eq)]
pub enum NativeHostCommand {
    Install,
    Status,
    Uninstall,
    Rollback,
    DevEnable { path: Option<String> },
    DevDisable,
    UseStore,
    UseStandalone,
    UseDev,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ExtensionCommand {
    StandaloneInstall { reference: Option<String> },
    StandaloneStatus,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DevCommand {
    Sync { dry_run: bool, allow_dirty: bool },
    Status,
    Rollback,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ServiceCommand {
    Install {
        adopt_node: bool,
    },
    Status,
    Start,
    Stop,
    Restart,
    Rollback,
    Uninstall,
    Guardian {
        transaction_id: String,
        parent_pid: u32,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum UpdateCommand {
    Check { manifest_url: Option<String> },
    Apply { manifest_url: Option<String> },
    Auto,
    Status,
    Worker { job_id: String },
}

#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    /// Optional `--instance NAME` / `-i NAME` (sugar for `HERDR_MCP_INSTANCE`).
    pub instance: Option<String>,
    pub command: Command,
}

pub fn parse<I>(args: I) -> Result<Parsed, String>
where
    I: IntoIterator<Item = String>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    let (instance, args) = strip_instance_flag(&args)?;
    let command = parse_command(&args)?;
    Ok(Parsed { instance, command })
}

fn strip_instance_flag(args: &[String]) -> Result<(Option<String>, Vec<String>), String> {
    let mut instance = None;
    let mut rest = Vec::with_capacity(args.len());
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--instance" | "-i" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--instance requires a name".to_owned())?;
                crate::instance::InstanceId::parse(value)?;
                if instance.is_some() {
                    return Err("duplicate --instance flag".to_owned());
                }
                instance = Some(value.clone());
                index += 2;
            }
            value if value.starts_with("--instance=") => {
                let value = value
                    .strip_prefix("--instance=")
                    .ok_or_else(|| "invalid --instance value".to_owned())?;
                crate::instance::InstanceId::parse(value)?;
                if instance.is_some() {
                    return Err("duplicate --instance flag".to_owned());
                }
                instance = Some(value.to_owned());
                index += 1;
            }
            _ => {
                rest.push(args[index].clone());
                index += 1;
            }
        }
    }
    Ok((instance, rest))
}

fn parse_command(args: &[String]) -> Result<Command, String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(Command::Help {
            section: HelpSection::General,
        });
    };

    match command {
        "help" | "-h" | "--help" => no_extra(
            args,
            Command::Help {
                section: HelpSection::General,
            },
        ),
        "version" | "-V" | "--version" => no_extra(args, Command::Version),
        "install" => no_extra(
            args,
            Command::Service(ServiceCommand::Install { adopt_node: false }),
        ),
        "status" => no_extra(args, Command::Status),
        "doctor" => no_extra(args, Command::Doctor),
        "__documents-probe" => no_extra(args, Command::DocumentsProbe),
        "__tcc-broker" => no_extra(args, Command::TccBrokerRun),
        "__credential-helper" => no_extra(args, Command::CredentialHelperRun),
        "tcc-broker" => parse_tcc_broker(&args[1..]),
        "permissions" => parse_permissions(&args[1..]),
        "herdr-supervisor" => parse_herdr_supervisor(&args[1..]),
        "scan" => parse_scan(&args[1..]),
        "rollback" => no_extra(args, Command::Service(ServiceCommand::Rollback)),
        "uninstall" => no_extra(args, Command::Uninstall),
        "reinstall" => no_extra(args, Command::Reinstall),
        "config" => parse_config(&args[1..]),
        "worker" => parse_worker(&args[1..]),
        "device" => parse_device_alias(&args[1..]),
        "connector" => parse_connector(&args[1..]),
        "automation" => parse_automation(&args[1..]),
        "dev" => parse_dev(&args[1..]),
        "candidate" => parse_candidate(&args[1..]),
        "service" => parse_service(&args[1..]),
        "update" => parse_update(&args[1..]),
        "extension" => parse_extension(&args[1..]),
        "native-host" => parse_native_host(&args[1..]),
        "extension-host" => parse_extension_host(&args[1..]),
        "artifact" => parse_artifact(&args[1..]),
        "link" => parse_link(&args[1..]),
        value => Err(format!("unknown command '{value}'\n\n{}", help())),
    }
}

fn parse_artifact(args: &[String]) -> Result<Command, String> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        return Err("artifact requires import".to_owned());
    };
    if subcommand != "import" {
        return Err(format!("unknown artifact command '{subcommand}'"));
    }

    let mut url = None;
    let mut path = None;
    let mut expected_sha256 = None;
    let mut capability_env = crate::artifact_import::default_capability_env().to_owned();
    let mut capability_env_explicit = false;
    let mut signed_url = false;
    let mut overwrite = false;
    let mut confirm_dirty = false;
    let mut confirm_busy = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--url" => {
                url = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "--url requires a value".to_owned())?
                        .clone(),
                );
                index += 2;
            }
            "--path" => {
                path = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "--path requires a value".to_owned())?
                        .clone(),
                );
                index += 2;
            }
            "--sha256" => {
                expected_sha256 = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "--sha256 requires a value".to_owned())?
                        .clone(),
                );
                index += 2;
            }
            "--capability-env" => {
                capability_env_explicit = true;
                capability_env = args
                    .get(index + 1)
                    .ok_or_else(|| "--capability-env requires a value".to_owned())?
                    .clone();
                if capability_env.is_empty()
                    || !capability_env.bytes().all(|byte| {
                        byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'
                    })
                {
                    return Err(
                        "--capability-env must be an uppercase environment variable name"
                            .to_owned(),
                    );
                }
                index += 2;
            }
            "--signed-url" => {
                signed_url = true;
                index += 1;
            }
            "--overwrite" => {
                overwrite = true;
                index += 1;
            }
            "--confirm-dirty" => {
                confirm_dirty = true;
                index += 1;
            }
            "--confirm-busy" => {
                confirm_busy = true;
                index += 1;
            }
            value => return Err(format!("unknown artifact import argument '{value}'")),
        }
    }

    if signed_url && capability_env_explicit {
        return Err("--signed-url cannot be combined with --capability-env".to_owned());
    }

    Ok(Command::ArtifactImport(
        crate::artifact_import::ImportArgs {
            url: url.ok_or_else(|| "artifact import requires --url".to_owned())?,
            path: path.ok_or_else(|| "artifact import requires --path".to_owned())?,
            expected_sha256,
            capability_env,
            signed_url,
            overwrite,
            confirm_dirty,
            confirm_busy,
        },
    ))
}

fn parse_herdr_supervisor(args: &[String]) -> Result<Command, String> {
    let command = match args {
        [subcommand] if subcommand == "install" => HerdrSupervisorCommand::Install,
        [subcommand] if subcommand == "status" => HerdrSupervisorCommand::Status,
        [subcommand] if subcommand == "enable" => HerdrSupervisorCommand::Enable,
        [subcommand] if subcommand == "disable" => HerdrSupervisorCommand::Disable,
        [subcommand] if subcommand == "start" => HerdrSupervisorCommand::Start,
        [subcommand] if subcommand == "stop" => HerdrSupervisorCommand::Stop,
        [subcommand] if subcommand == "run" => HerdrSupervisorCommand::Run,
        [subcommand] if subcommand == "run-once" => HerdrSupervisorCommand::RunOnce,
        [subcommand] if subcommand == "uninstall" => HerdrSupervisorCommand::Uninstall,
        [] => return Err("herdr-supervisor requires install, status, enable, disable, start, stop, run-once, or uninstall".to_owned()),
        [subcommand, ..] => return Err(format!("invalid herdr-supervisor command or arguments for '{subcommand}'")),
    };
    Ok(Command::HerdrSupervisor(command))
}

fn parse_link(args: &[String]) -> Result<Command, String> {
    match args {
        [subcommand] if subcommand == "status" => Ok(Command::Link(LinkCommand::Status)),
        [subcommand] if subcommand == "run" => Ok(Command::Link(LinkCommand::Run)),
        [subcommand] if subcommand == "install" => Ok(Command::Link(LinkCommand::Install)),
        [subcommand] if subcommand == "uninstall" => Ok(Command::Link(LinkCommand::Uninstall)),
        [subcommand, ..] if subcommand == "cutover" => parse_link_cutover(&args[1..]),
        [subcommand, ..] if subcommand == "seal" => parse_link_seal(&args[1..]),
        [subcommand, ..] if subcommand == "migrate-runtime-control" => {
            parse_link_migrate_runtime_control(&args[1..])
        }
        [] => Err(
            "link requires status, run, install, uninstall, cutover, seal, or migrate-runtime-control"
                .to_owned(),
        ),
        [subcommand] => Err(format!(
            "unknown link command '{subcommand}' (status|run|install|uninstall|cutover|seal|migrate-runtime-control)"
        )),
        _ => Err(
            "link accepts status, run, install, uninstall, cutover [--dry-run|--execute|--rollback], seal [...], or migrate-runtime-control [--dry-run|--write-staging|--apply]"
                .to_owned(),
        ),
    }
}

fn parse_link_cutover(args: &[String]) -> Result<Command, String> {
    use crate::link::CutoverMode;

    let mut dry_run = false;
    let mut execute = false;
    let mut rollback = false;
    for arg in args {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--execute" => execute = true,
            "--rollback" => rollback = true,
            value => {
                return Err(format!(
                    "unknown link cutover argument '{value}' (expected --dry-run, --execute, or --rollback)"
                ));
            }
        }
    }
    let selected = [dry_run, execute, rollback]
        .into_iter()
        .filter(|value| *value)
        .count();
    if selected > 1 {
        return Err(
            "link cutover accepts only one of --dry-run, --execute, or --rollback (default is dry-run)"
                .to_owned(),
        );
    }
    let mode = if execute {
        CutoverMode::Execute
    } else if rollback {
        CutoverMode::Rollback
    } else {
        CutoverMode::DryRun
    };
    Ok(Command::Link(LinkCommand::Cutover { mode }))
}

fn parse_link_seal(args: &[String]) -> Result<Command, String> {
    use crate::link::SealMode;

    if args.is_empty() {
        return Ok(Command::Link(LinkCommand::Seal {
            mode: SealMode::Status,
        }));
    }
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["status"] => Ok(Command::Link(LinkCommand::Seal {
            mode: SealMode::Status,
        })),
        ["record", "--dual-uat"] => Ok(Command::Link(LinkCommand::Seal {
            mode: SealMode::RecordDualUat,
        })),
        ["record", "--rollback-uat"] => Ok(Command::Link(LinkCommand::Seal {
            mode: SealMode::RecordRollbackUat,
        })),
        ["--dry-run"] => Ok(Command::Link(LinkCommand::Seal {
            mode: SealMode::DryRun,
        })),
        ["--execute"] => Ok(Command::Link(LinkCommand::Seal {
            mode: SealMode::Execute,
        })),
        _ => Err(
            "link seal accepts status | record --dual-uat | record --rollback-uat | --dry-run | --execute"
                .to_owned(),
        ),
    }
}

fn parse_link_migrate_runtime_control(args: &[String]) -> Result<Command, String> {
    use crate::link::MigrateMode;

    let mut dry_run = false;
    let mut write_staging = false;
    let mut apply = false;
    for arg in args {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--write-staging" => write_staging = true,
            "--apply" => apply = true,
            value => {
                return Err(format!(
                    "unknown link migrate-runtime-control argument '{value}' (expected --dry-run, --write-staging, or --apply)"
                ));
            }
        }
    }
    let selected = [dry_run, write_staging, apply]
        .into_iter()
        .filter(|value| *value)
        .count();
    if selected > 1 {
        return Err(
            "link migrate-runtime-control accepts only one of --dry-run, --write-staging, or --apply (default is dry-run)"
                .to_owned(),
        );
    }
    let mode = if apply {
        MigrateMode::Apply
    } else if write_staging {
        MigrateMode::WriteStaging
    } else {
        MigrateMode::DryRun
    };
    Ok(Command::Link(LinkCommand::MigrateRuntimeControl { mode }))
}

fn parse_worker(args: &[String]) -> Result<Command, String> {
    match args.first().map(String::as_str) {
        Some("--help" | "-h") => Ok(Command::Help {
            section: HelpSection::Worker,
        }),
        Some("pair") => parse_worker_pair(&args[1..]),
        Some("connect") => parse_worker_connect(&args[1..]),
        Some("rename") => parse_worker_rename(&args[1..]),
        Some("revoke") => parse_worker_revoke(&args[1..]),
        Some(value) => Err(format!(
            "unknown worker command '{value}' (expected pair, connect, rename, or revoke)"
        )),
        None => Err("worker requires pair, connect, rename, or revoke".to_owned()),
    }
}

fn parse_device_alias(args: &[String]) -> Result<Command, String> {
    match args.first().map(String::as_str) {
        Some("pair") => parse_worker_pair(&args[1..]),
        Some("rename") => parse_worker_rename(&args[1..]),
        Some("revoke") => parse_worker_revoke(&args[1..]),
        Some(value) => Err(format!(
            "device '{value}' is not implemented yet; supported commands are pair, rename, and revoke"
        )),
        None => Err("device requires pair, rename, or revoke".to_owned()),
    }
}

fn parse_connector(args: &[String]) -> Result<Command, String> {
    match args.first().map(String::as_str) {
        Some("--help" | "-h") => Ok(Command::Help {
            section: HelpSection::Connector,
        }),
        Some("list") if args.len() == 1 => Ok(Command::Worker(WorkerCommand::ConnectorList)),
        Some("approve") => {
            let [request_id] = &args[1..] else {
                return Err("connector approve requires exactly one approval request id; the 6-digit code is entered interactively".to_owned());
            };
            if request_id.starts_with('-') || request_id.trim().is_empty() || request_id.len() > 256
            {
                return Err("connector approve requires a valid approval request id".to_owned());
            }
            Ok(Command::Worker(WorkerCommand::ConnectorApprove {
                request_id: request_id.clone(),
            }))
        }
        Some("revoke") => {
            let [connector_id, confirm] = &args[1..] else {
                return Err(
                    "connector revoke requires: <connector-id> --confirm (connector ids begin with conn_)"
                        .to_owned(),
                );
            };
            validate_connector_id(connector_id)?;
            if confirm != "--confirm" {
                return Err("connector revoke requires --confirm".to_owned());
            }
            Ok(Command::Worker(WorkerCommand::ConnectorRevoke {
                connector_id: connector_id.clone(),
            }))
        }
        Some(value) => Err(format!(
            "unknown connector command '{value}' (expected list, approve, or revoke)"
        )),
        None => Err("connector requires list, approve, or revoke".to_owned()),
    }
}

fn parse_automation(args: &[String]) -> Result<Command, String> {
    match args.first().map(String::as_str) {
        Some("--help" | "-h") => Ok(Command::Help {
            section: HelpSection::Automation,
        }),
        Some("create") => parse_automation_create(&args[1..]),
        Some("list") if args.len() == 1 => Ok(Command::Worker(WorkerCommand::AutomationList)),
        Some("rotate") => {
            let [client_id, confirm] = &args[1..] else {
                return Err("automation rotate requires: <client-id> --confirm".to_owned());
            };
            validate_automation_client_id(client_id)?;
            if confirm != "--confirm" {
                return Err("automation rotate requires --confirm because the old secret is invalidated immediately".to_owned());
            }
            Ok(Command::Worker(WorkerCommand::AutomationRotate {
                client_id: client_id.clone(),
            }))
        }
        Some("revoke") => {
            let [client_id, confirm] = &args[1..] else {
                return Err("automation revoke requires: <client-id> --confirm".to_owned());
            };
            validate_automation_client_id(client_id)?;
            if confirm != "--confirm" {
                return Err(
                    "automation revoke requires --confirm because revocation is immediate"
                        .to_owned(),
                );
            }
            Ok(Command::Worker(WorkerCommand::AutomationRevoke {
                client_id: client_id.clone(),
            }))
        }
        Some(value) => Err(format!(
            "unknown automation command '{value}' (expected create, list, rotate, or revoke)"
        )),
        None => Err("automation requires create, list, rotate, or revoke".to_owned()),
    }
}

fn parse_automation_create(args: &[String]) -> Result<Command, String> {
    let mut name = None;
    let mut device = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--name" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "automation create requires --name NAME".to_owned())?
                    .trim()
                    .to_owned();
                if value.is_empty() || value.len() > 256 {
                    return Err("automation --name must contain 1..256 characters".to_owned());
                }
                name = Some(value);
                index += 2;
            }
            "--device" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| {
                        "automation create requires --device <device-id-or-unique-name>".to_owned()
                    })?
                    .trim()
                    .to_owned();
                if value.is_empty() || value.len() > 512 {
                    return Err(
                        "automation --device must name a bound target device (id or unique name)"
                            .to_owned(),
                    );
                }
                device = Some(value);
                index += 2;
            }
            value => {
                return Err(format!(
                    "unknown automation create argument '{value}' (expected --name NAME and --device DEVICE)"
                ));
            }
        }
    }
    let name = name.ok_or_else(|| {
        "automation create requires --name and --device; no silent device selection".to_owned()
    })?;
    let device = device.ok_or_else(|| {
        "automation create requires --device <device-id-or-unique-name>; a target device must be chosen explicitly".to_owned()
    })?;
    Ok(Command::Worker(WorkerCommand::AutomationCreate {
        name,
        device,
    }))
}

fn validate_connector_id(connector_id: &str) -> Result<(), String> {
    // Edge uses a randomBase64Url suffix; we only require a plausible length
    // rather than an exact one (which would break if Edge changes suffix width).
    let valid = connector_id.starts_with("conn_")
        && connector_id.len() >= 12
        && connector_id.len() <= 4096
        && connector_id[5..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err("connector id must be a valid conn_ identifier".to_owned())
    }
}

fn validate_automation_client_id(client_id: &str) -> Result<(), String> {
    let valid = client_id.starts_with("svc_")
        && (12..=132).contains(&client_id.len())
        && client_id[4..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if valid {
        Ok(())
    } else {
        Err("automation client id must be a valid svc_ identifier".to_owned())
    }
}

fn parse_worker_pair(args: &[String]) -> Result<Command, String> {
    let mut ttl_seconds = 600_u64;
    let mut name = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--ttl-seconds" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--ttl-seconds requires 60..600".to_owned())?;
                ttl_seconds = value
                    .parse::<u64>()
                    .map_err(|_| "--ttl-seconds requires an integer".to_owned())?;
                if !(60..=600).contains(&ttl_seconds) {
                    return Err("--ttl-seconds must be between 60 and 600".to_owned());
                }
                index += 2;
            }
            "--name" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--name requires a device name".to_owned())?;
                name = Some(
                    crate::device_name::normalize_device_display_name(value)
                        .ok_or_else(|| "--name must contain 1..128 UTF-16 code units".to_owned())?,
                );
                index += 2;
            }
            "--code" | "--pin" => {
                return Err(
                    "pairing codes are never accepted on argv; the new device enters the code interactively"
                        .to_owned(),
                );
            }
            value if value.starts_with("--code") || value.starts_with("--pin") => {
                return Err(
                    "pairing codes are never accepted on argv; the new device enters the code interactively"
                        .to_owned(),
                );
            }
            value => {
                return Err(format!("unknown worker pair argument '{value}'"));
            }
        }
    }
    Ok(Command::Worker(WorkerCommand::Pair { ttl_seconds, name }))
}

fn parse_worker_connect(args: &[String]) -> Result<Command, String> {
    let mut pairing_address = None;
    let mut name = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--name" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "--name requires a device name".to_owned())?;
                name = Some(
                    crate::device_name::normalize_device_display_name(value)
                        .ok_or_else(|| "--name must contain 1..128 UTF-16 code units".to_owned())?,
                );
                index += 2;
            }
            "--code" | "--pin" => {
                return Err(
                    "pairing codes are never accepted on argv; enter the code interactively"
                        .to_owned(),
                );
            }
            value if value.starts_with("--code") || value.starts_with("--pin") => {
                return Err(
                    "pairing codes are never accepted on argv; enter the code interactively"
                        .to_owned(),
                );
            }
            "--enrollment-file" => {
                return Err(
                    "the enrollment-file flow was replaced by pairing; use `worker connect <pairing-address>`"
                        .to_owned(),
                );
            }
            "--edge-origin" => {
                return Err(
                    "--edge-origin is not needed; the pairing address carries the Worker origin"
                        .to_owned(),
                );
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown worker connect argument '{value}'"));
            }
            value => {
                if pairing_address.is_some() {
                    return Err("worker connect accepts exactly one pairing address".to_owned());
                }
                pairing_address = Some(value.to_owned());
                index += 1;
            }
        }
    }
    Ok(Command::Worker(WorkerCommand::Connect {
        pairing_address: pairing_address
            .ok_or_else(|| "worker connect requires a pairing address".to_owned())?,
        name,
    }))
}

fn parse_worker_rename(args: &[String]) -> Result<Command, String> {
    let [name] = args else {
        return Err("worker rename requires exactly one new device name".to_owned());
    };
    if name.starts_with('-') {
        return Err(format!("unknown worker rename argument '{name}'"));
    }
    let name = crate::device_name::normalize_device_display_name(name)
        .ok_or_else(|| "device name must contain 1..128 UTF-16 code units".to_owned())?;
    Ok(Command::Worker(WorkerCommand::Rename { name }))
}

fn parse_worker_revoke(args: &[String]) -> Result<Command, String> {
    let [device_id, confirm] = args else {
        return Err(
            "worker revoke requires: <device-id> --confirm; revocation is permanent".to_owned(),
        );
    };
    if confirm != "--confirm" {
        return Err("worker revoke requires --confirm because revocation is permanent".to_owned());
    }
    let device_id = crate::config::normalize_device_id(device_id)?;
    Ok(Command::Worker(WorkerCommand::Revoke { device_id }))
}

fn parse_config(args: &[String]) -> Result<Command, String> {
    match args {
        [] => Ok(Command::Config(ConfigCommand::Show)),
        [subcommand] if subcommand == "path" => Ok(Command::Config(ConfigCommand::Path)),
        [subcommand] if subcommand == "show" => Ok(Command::Config(ConfigCommand::Show)),
        [subcommand] if subcommand == "init" => {
            Ok(Command::Config(ConfigCommand::Init { edge_origin: None }))
        }
        [subcommand, flag, origin] if subcommand == "init" && flag == "--edge-origin" => {
            Ok(Command::Config(ConfigCommand::Init {
                edge_origin: Some(origin.clone()),
            }))
        }
        [subcommand, origin] if subcommand == "set-edge-origin" => {
            Ok(Command::Config(ConfigCommand::SetEdgeOrigin {
                edge_origin: origin.clone(),
            }))
        }
        [subcommand] => Err(format!("unknown config command '{subcommand}'")),
        _ => Err(
            "config accepts path, show, init [--edge-origin https://host], or set-edge-origin https://host"
                .to_owned(),
        ),
    }
}

fn no_extra(args: &[String], command: Command) -> Result<Command, String> {
    if args.len() == 1 {
        Ok(command)
    } else {
        Err(format!("unexpected argument '{}'", args[1]))
    }
}

fn parse_scan(args: &[String]) -> Result<Command, String> {
    let mut json = false;
    let mut refresh = false;
    let mut probe = false;
    for arg in args {
        match arg.as_str() {
            "--json" if !json => json = true,
            "--refresh" if !refresh => refresh = true,
            "--probe" if !probe => probe = true,
            "--json" | "--refresh" | "--probe" => {
                return Err(format!("duplicate scan argument '{arg}'"));
            }
            value => {
                return Err(format!(
                    "unknown scan argument '{value}' (expected --json, --refresh, or --probe)"
                ));
            }
        }
    }
    Ok(Command::Scan {
        json,
        refresh,
        probe,
    })
}

fn parse_dev(args: &[String]) -> Result<Command, String> {
    if args.is_empty() {
        return Ok(Command::Dev(DevCommand::Status));
    }
    if args[0] == "status" {
        if args.len() != 1 {
            return Err("dev status does not accept extra arguments".to_owned());
        }
        return Ok(Command::Dev(DevCommand::Status));
    }
    if args[0] == "rollback" {
        if args.len() != 1 {
            return Err("dev rollback does not accept extra arguments".to_owned());
        }
        return Ok(Command::Dev(DevCommand::Rollback));
    }

    // Compatibility: old `herdr-mcp dev --dry-run` means native `dev sync --dry-run`.
    let option_start = if args[0] == "sync" { 1 } else { 0 };
    if option_start == 0 && !args[0].starts_with('-') {
        return Err(format!(
            "unknown dev command '{}' (expected sync | status | rollback)",
            args[0]
        ));
    }
    let mut dry_run = false;
    let mut allow_dirty = false;
    for arg in &args[option_start..] {
        match arg.as_str() {
            "--dry-run" if !dry_run => dry_run = true,
            "--allow-dirty" if !allow_dirty => allow_dirty = true,
            "--dry-run" | "--allow-dirty" => {
                return Err(format!("duplicate dev sync argument '{arg}'"));
            }
            value => return Err(format!("unknown dev sync argument '{value}'")),
        }
    }
    Ok(Command::Dev(DevCommand::Sync {
        dry_run,
        allow_dirty,
    }))
}

fn parse_candidate(args: &[String]) -> Result<Command, String> {
    let mut port = 8873_u16;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--port" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "candidate --port requires a value".to_owned())?;
                port = value
                    .parse::<u16>()
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or_else(|| "candidate --port must be 1..65535".to_owned())?;
                index += 2;
            }
            value => return Err(format!("unknown candidate argument '{value}'")),
        }
    }
    Ok(Command::Candidate { port })
}

fn parse_service(args: &[String]) -> Result<Command, String> {
    match args {
        [subcommand] if subcommand == "install" => Ok(Command::Service(ServiceCommand::Install {
            adopt_node: false,
        })),
        [subcommand, flag] if subcommand == "install" && flag == "--adopt-node" => {
            Ok(Command::Service(ServiceCommand::Install {
                adopt_node: true,
            }))
        }
        [subcommand] if subcommand == "status" => Ok(Command::Service(ServiceCommand::Status)),
        [subcommand] if subcommand == "start" => Ok(Command::Service(ServiceCommand::Start)),
        [subcommand] if subcommand == "stop" => Ok(Command::Service(ServiceCommand::Stop)),
        [subcommand] if subcommand == "restart" => Ok(Command::Service(ServiceCommand::Restart)),
        [subcommand] if subcommand == "rollback" => Ok(Command::Service(ServiceCommand::Rollback)),
        [subcommand] if subcommand == "uninstall" => {
            Ok(Command::Service(ServiceCommand::Uninstall))
        }
        [
            subcommand,
            transaction_flag,
            transaction_id,
            parent_flag,
            parent_pid,
        ] if subcommand == "__guardian"
            && transaction_flag == "--transaction"
            && parent_flag == "--parent-pid" =>
        {
            let parent_pid = parent_pid
                .parse::<u32>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    "service __guardian --parent-pid must be a positive integer".to_owned()
                })?;
            Ok(Command::Service(ServiceCommand::Guardian {
                transaction_id: transaction_id.clone(),
                parent_pid,
            }))
        }
        [] => Err(
            "service requires install, status, start, stop, restart, rollback, or uninstall"
                .to_owned(),
        ),
        [subcommand, ..] => Err(format!(
            "invalid service command or arguments for '{subcommand}'"
        )),
    }
}

fn parse_permissions(args: &[String]) -> Result<Command, String> {
    use crate::macos_permissions::PermissionsCommand;
    match args {
        [subcommand] if subcommand == "status" => {
            Ok(Command::Permissions(PermissionsCommand::Status))
        }
        [subcommand] if subcommand == "setup" => {
            Ok(Command::Permissions(PermissionsCommand::Setup {
                upgrade_broker: false,
            }))
        }
        [subcommand, flag] if subcommand == "setup" && flag == "--upgrade-broker" => {
            Ok(Command::Permissions(PermissionsCommand::Setup {
                upgrade_broker: true,
            }))
        }
        [subcommand] if subcommand == "verify" => {
            Ok(Command::Permissions(PermissionsCommand::Verify))
        }
        [] => Err("permissions requires status, setup [--upgrade-broker], or verify".to_owned()),
        [subcommand, ..] => Err(format!(
            "invalid permissions command or arguments for '{subcommand}'"
        )),
    }
}

fn parse_tcc_broker(args: &[String]) -> Result<Command, String> {
    match args {
        [subcommand] if subcommand == "install" => {
            Ok(Command::TccBroker(TccBrokerCommand::Install {
                force: false,
            }))
        }
        [subcommand, flag] if subcommand == "install" && flag == "--force" => {
            Ok(Command::TccBroker(TccBrokerCommand::Install {
                force: true,
            }))
        }
        [subcommand] if subcommand == "status" => Ok(Command::TccBroker(TccBrokerCommand::Status)),
        [subcommand] if subcommand == "uninstall" => {
            Ok(Command::TccBroker(TccBrokerCommand::Uninstall))
        }
        [] => Err("tcc-broker requires install [--force], status, or uninstall".to_owned()),
        [subcommand, ..] => Err(format!(
            "invalid tcc-broker command or arguments for '{subcommand}'"
        )),
    }
}

fn parse_update(args: &[String]) -> Result<Command, String> {
    match args {
        [subcommand] if subcommand == "check" => {
            Ok(Command::Update(UpdateCommand::Check { manifest_url: None }))
        }
        [subcommand, flag, value] if subcommand == "check" && flag == "--manifest" => {
            Ok(Command::Update(UpdateCommand::Check {
                manifest_url: Some(value.clone()),
            }))
        }
        [subcommand] if subcommand == "apply" => {
            Ok(Command::Update(UpdateCommand::Apply { manifest_url: None }))
        }
        [subcommand, flag, value] if subcommand == "apply" && flag == "--manifest" => {
            Ok(Command::Update(UpdateCommand::Apply {
                manifest_url: Some(value.clone()),
            }))
        }
        [subcommand] if subcommand == "auto" => Ok(Command::Update(UpdateCommand::Auto)),
        [subcommand] if subcommand == "status" => Ok(Command::Update(UpdateCommand::Status)),
        [subcommand, flag, value] if subcommand == "worker" && flag == "--job" => {
            Ok(Command::Update(UpdateCommand::Worker {
                job_id: value.clone(),
            }))
        }
        [] => Ok(Command::Update(UpdateCommand::Apply { manifest_url: None })),
        [subcommand, ..] => Err(format!(
            "invalid update command or arguments for '{subcommand}'"
        )),
    }
}

fn parse_native_host(args: &[String]) -> Result<Command, String> {
    match args {
        [subcommand] if subcommand == "install" => {
            Ok(Command::NativeHost(NativeHostCommand::Install))
        }
        [subcommand] if subcommand == "status" => {
            Ok(Command::NativeHost(NativeHostCommand::Status))
        }
        [subcommand] if subcommand == "uninstall" => {
            Ok(Command::NativeHost(NativeHostCommand::Uninstall))
        }
        [subcommand] if subcommand == "rollback" => {
            Ok(Command::NativeHost(NativeHostCommand::Rollback))
        }
        [group, action] if group == "dev" && action == "enable" => Ok(Command::NativeHost(
            NativeHostCommand::DevEnable { path: None },
        )),
        [group, action, path] if group == "dev" && action == "enable" => Ok(
            Command::NativeHost(NativeHostCommand::DevEnable {
                path: Some(path.clone()),
            }),
        ),
        [group, action] if group == "dev" && action == "disable" => {
            Ok(Command::NativeHost(NativeHostCommand::DevDisable))
        }
        [subcommand, channel] if subcommand == "use" && channel == "store" => {
            Ok(Command::NativeHost(NativeHostCommand::UseStore))
        }
        [subcommand, channel] if subcommand == "use" && channel == "standalone" => {
            Ok(Command::NativeHost(NativeHostCommand::UseStandalone))
        }
        [subcommand, channel] if subcommand == "use" && channel == "dev" => {
            Ok(Command::NativeHost(NativeHostCommand::UseDev))
        }
        [] => Err(
            "native-host requires install, status, uninstall, rollback, dev enable|disable, or use store|standalone|dev"
                .to_owned(),
        ),
        [subcommand] => Err(format!("unknown native-host command '{subcommand}'")),
        _ => Err("invalid native-host command or arguments".to_owned()),
    }
}

fn parse_extension(args: &[String]) -> Result<Command, String> {
    match args {
        [channel, action] if channel == "standalone" && action == "install" => {
            Ok(Command::Extension(ExtensionCommand::StandaloneInstall {
                reference: None,
            }))
        }
        [channel, action, flag, reference]
            if channel == "standalone" && action == "install" && flag == "--ref" =>
        {
            Ok(Command::Extension(ExtensionCommand::StandaloneInstall {
                reference: Some(reference.clone()),
            }))
        }
        [channel, action] if channel == "standalone" && action == "status" => {
            Ok(Command::Extension(ExtensionCommand::StandaloneStatus))
        }
        [] => {
            Err("extension requires standalone install [--ref REF] or standalone status".to_owned())
        }
        _ => Err("invalid extension command or arguments".to_owned()),
    }
}

fn parse_extension_host(args: &[String]) -> Result<Command, String> {
    match args {
        [] => Ok(Command::ExtensionHost {
            caller_origin: String::new(),
        }),
        [caller_origin] if caller_origin.starts_with("chrome-extension://") => {
            Ok(Command::ExtensionHost {
                caller_origin: caller_origin.clone(),
            })
        }
        [value] => Err(format!("invalid extension-host caller origin '{value}'")),
        _ => Err("extension-host accepts at most one Chromium caller origin".to_owned()),
    }
}

pub fn help() -> &'static str {
    "Herdr MCP native runtime\n\n\
User path:\n\
  herdr-mcp install\n\
  herdr-mcp status\n\
  herdr-mcp doctor\n\
  herdr-mcp permissions <status|setup [--upgrade-broker]|verify>\n\
  herdr-mcp scan [--json] [--refresh] [--probe]\n\
  herdr-mcp worker pair [--ttl-seconds 600] [--name NAME]  (macOS enrolled device only; creates pairing for another computer)\n\
  herdr-mcp worker connect <pairing-address> [--name NAME]  (macOS only; requires Keychain, reads the 6-digit code as visible interactive terminal input (or one stdin line), never argv)\n\
  herdr-mcp connector list  (enrolled-device credential; non-secret connector inventory)\n\
  herdr-mcp connector approve <approval-request-id>  (macOS enrolled device; reads the 6-digit code interactively, never argv)\n\
  herdr-mcp connector revoke <connector-id> --confirm  (connector ids begin with conn_)\n\
  herdr-mcp automation create --name NAME --device <device-id-or-unique-name>  (creates one CI/service principal bound to a device; secret is shown once)\n\
  herdr-mcp automation list\n\
  herdr-mcp automation rotate <client-id> --confirm\n\
  herdr-mcp automation revoke <client-id> --confirm\n\
  herdr-mcp update [check [--manifest URL]|apply [--manifest URL]|auto|status]\n\
  herdr-mcp extension standalone <install [--ref REF]|status>\n\
  herdr-mcp rollback\n\
  herdr-mcp reinstall\n\
  herdr-mcp uninstall\n\n\
Same-machine UAT isolation (optional):\n\
  herdr-mcp --instance uat install\n\
  HERDR_MCP_INSTANCE=uat herdr-mcp doctor\n\
  Named instances use distinct LaunchAgent labels, a non-8772 port, and\n\
  ~/.config/herdr-mcp-<name>. They never rewrite ~/.local/bin/herdr-mcp.\n\n\
Advanced / internal:\n\
  herdr-mcp version\n\
  herdr-mcp config [path|show|init [--edge-origin https://host]|set-edge-origin https://host]\n\
  herdr-mcp service <install [--adopt-node]|status|start|stop|restart|rollback|uninstall>\n\
  herdr-mcp herdr-supervisor <install|status|enable|disable|start|stop|uninstall>\n\
  herdr-mcp link status\n\
  herdr-mcp link run\n\
  herdr-mcp link install\n\
  herdr-mcp link uninstall\n\
  herdr-mcp link cutover [--dry-run|--execute|--rollback]\n\
  herdr-mcp link seal [status|record --dual-uat|record --rollback-uat|--dry-run|--execute]\n\
  herdr-mcp link migrate-runtime-control [--dry-run|--write-staging|--apply]\n\
  herdr-mcp tcc-broker <install [--force]|status|uninstall>\n\
  herdr-mcp native-host <install|status|uninstall|rollback>\n\
  herdr-mcp native-host dev <enable [PATH]|disable>\n\
  herdr-mcp native-host use <store|standalone|dev>\n\
  herdr-mcp extension-host [chrome-extension://.../]\n\
  herdr-mcp artifact import --url HTTPS_URL --path PROJECT_PATH [--sha256 HEX] [--capability-env NAME | --signed-url] [--overwrite] [--confirm-dirty] [--confirm-busy]\n\
  (--signed-url imports a safe signed HTTPS URL directly; R2 relay objects use
  HERDR_ARTIFACT_CAPABILITY, which is never a CLI arg)\n\
  herdr-mcp dev [status]\n\
  herdr-mcp dev sync [--dry-run] [--allow-dirty]\n\
  herdr-mcp dev rollback\n\
  herdr-mcp candidate [--port 8873]\n\n\
Prefer the top-level install/status/doctor/permissions/scan/update/rollback/reinstall/uninstall commands\n\
for normal lifecycle. Use service ... only for advanced service control\n\
(for example service install --adopt-node). link status is read-only G5\n\
ownership/gates reporting. link run starts a foreground Rust Link candidate\n\
(Keychain/plist credentials). link install/uninstall manage only the candidate\n\
LaunchAgent dev.herdr-mcp.link-rust-candidate → runtime/current link run; they\n\
never unload or replace live Node link/link-prod. Candidate defaults to an\n\
epoch-2 Edge (edge-prod) and refuses install when Edge /health is still epoch 1.\n\
link cutover defaults to dry-run plan/validate only; --execute / --rollback\n\
require HERDR_LINK_CUTOVER_I_UNDERSTAND=1, mutate only link-prod via\n\
bootout/bootstrap (never the forbidden launchd submission path), and --rollback clears any active\n\
production_ready seal. link seal writes an auditable evidence artifact; it never\n\
auto-flips from LaunchAgent ownership alone (HERDR_LINK_SEAL_I_UNDERSTAND=1 for\n\
--execute). link migrate-runtime-control prepares a\n\
Rust-compatible runtime-control-prod generation (default dry-run; --write-staging\n\
writes a pending sibling; --apply rewrites the live control file only with\n\
HERDR_LINK_MIGRATE_RUNTIME_CONTROL=1) and never mutates LaunchAgents.\n"
}

pub fn worker_help() -> &'static str {
    "Herdr MCP worker (enrolled-device) management\n\n\
All of these require the credential of a device already enrolled in the fleet.\n\n\
  herdr-mcp worker pair [--ttl-seconds 600] [--name NAME]\n      Creates a pairing address for another computer to enroll.\n\n  herdr-mcp worker connect <pairing-address> [--name NAME]\n      Enrolls this machine; requires Keychain, reads the 6-digit code from an\n      interactive or stdin prompt, never argv.\n\n  herdr-mcp worker rename <name>\n      Renames the current enrolled device.\n\n  herdr-mcp worker revoke <device-id> --confirm\n      Revokes the given enrolled device id.\n"
}

pub fn connector_help() -> &'static str {
    "Herdr MCP connector management (OAuth connectors registered against the fleet)\n\n\
All of these require the credential of a device already enrolled in the fleet;\nthere is no WebChat delegated admin path. Secrets are never echoed or written\nto argv.\n\n\
  herdr-mcp connector list\n      Lists the non-secret connector inventory (connector_id, client, source,\n      status, timestamps) as returned by the Edge.\n\n  herdr-mcp connector approve <approval-request-id>\n      Approves a pending owner/approver request. Reads the 6-digit code as visible terminal input\n      (or one stdin line) and never from argv.\n\n  herdr-mcp connector revoke <connector-id> --confirm\n      Revokes a connector by its connector_id (begins with conn_).\n"
}

pub fn automation_help() -> &'static str {
    "Herdr MCP automation (CI/service principal) management\n\n\
All of these require the credential of a device already enrolled in the fleet.\n\n\
  herdr-mcp automation create --name NAME --device <device-id-or-unique-name>\n      Creates one CI/service principal explicitly bound to the given device.\n      --name and --device are both required and may appear in either order; a\n      target device is never auto-chosen. The secret is shown once.\n\n  herdr-mcp automation list\n      Lists the automation clients and their bound devices.\n\n  herdr-mcp automation rotate <client-id> --confirm\n      Rotates the client secret (old secret invalidated immediately).\n\n  herdr-mcp automation revoke <client-id> --confirm\n      Revokes the automation client (immediate).\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parses_core_commands() {
        assert_eq!(
            parse(args(&[])).unwrap().command,
            Command::Help {
                section: HelpSection::General,
            }
        );
        assert_eq!(parse(args(&["version"])).unwrap().command, Command::Version);
        assert_eq!(
            parse(args(&["install"])).unwrap().command,
            Command::Service(ServiceCommand::Install { adopt_node: false })
        );
        assert_eq!(parse(args(&["status"])).unwrap().command, Command::Status);
        assert_eq!(parse(args(&["doctor"])).unwrap().command, Command::Doctor);
        assert_eq!(
            parse(args(&["permissions", "status"])).unwrap().command,
            Command::Permissions(crate::macos_permissions::PermissionsCommand::Status)
        );
        assert_eq!(
            parse(args(&["permissions", "setup"])).unwrap().command,
            Command::Permissions(crate::macos_permissions::PermissionsCommand::Setup {
                upgrade_broker: false
            })
        );
        assert_eq!(
            parse(args(&["permissions", "verify"])).unwrap().command,
            Command::Permissions(crate::macos_permissions::PermissionsCommand::Verify)
        );
        assert_eq!(
            parse(args(&["permissions", "setup", "--upgrade-broker"]))
                .unwrap()
                .command,
            Command::Permissions(crate::macos_permissions::PermissionsCommand::Setup {
                upgrade_broker: true
            })
        );
        assert!(parse(args(&["permissions"])).is_err());
        assert!(parse(args(&["permissions", "grant"])).is_err());
        assert_eq!(
            parse(args(&["herdr-supervisor", "status"]))
                .unwrap()
                .command,
            Command::HerdrSupervisor(HerdrSupervisorCommand::Status)
        );
        assert_eq!(
            parse(args(&["scan", "--json", "--probe"])).unwrap().command,
            Command::Scan {
                json: true,
                refresh: false,
                probe: true
            }
        );
        assert_eq!(
            parse(args(&["rollback"])).unwrap().command,
            Command::Service(ServiceCommand::Rollback)
        );
        assert_eq!(
            parse(args(&["uninstall"])).unwrap().command,
            Command::Uninstall
        );
        assert_eq!(
            parse(args(&["reinstall"])).unwrap().command,
            Command::Reinstall
        );
        assert!(parse(args(&["uninstall", "--force"])).is_err());
        assert!(parse(args(&["reinstall", "--force"])).is_err());
        assert_eq!(
            parse(args(&["config", "show"])).unwrap().command,
            Command::Config(ConfigCommand::Show)
        );
        assert_eq!(
            parse(args(&[
                "config",
                "init",
                "--edge-origin",
                "https://herdr.example.com",
            ]))
            .unwrap()
            .command,
            Command::Config(ConfigCommand::Init {
                edge_origin: Some("https://herdr.example.com".to_owned()),
            })
        );
        assert_eq!(
            parse(args(&[
                "config",
                "set-edge-origin",
                "https://herdr.example.com",
            ]))
            .unwrap()
            .command,
            Command::Config(ConfigCommand::SetEdgeOrigin {
                edge_origin: "https://herdr.example.com".to_owned(),
            })
        );
        assert_eq!(
            parse(args(&[
                "worker",
                "pair",
                "--ttl-seconds",
                "120",
                "--name",
                "mac-b",
            ]))
            .unwrap()
            .command,
            Command::Worker(WorkerCommand::Pair {
                ttl_seconds: 120,
                name: Some("mac-b".to_owned()),
            })
        );
        assert_eq!(
            parse(args(&[
                "worker",
                "connect",
                "https://edge.example/pair#pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "--name",
                "mac-b",
            ]))
            .unwrap()
            .command,
            Command::Worker(WorkerCommand::Connect {
                pairing_address: "https://edge.example/pair#pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                name: Some("mac-b".to_owned()),
            })
        );
        assert_eq!(
            parse(args(&["device", "pair"])).unwrap().command,
            Command::Worker(WorkerCommand::Pair {
                ttl_seconds: 600,
                name: None,
            })
        );
        assert_eq!(
            parse(args(&["worker", "rename", "  qingxian-macbookair  "]))
                .unwrap()
                .command,
            Command::Worker(WorkerCommand::Rename {
                name: "qingxian-macbookair".to_owned(),
            })
        );
        assert_eq!(
            parse(args(&["device", "rename", "青闲的 MacBook Air"]))
                .unwrap()
                .command,
            Command::Worker(WorkerCommand::Rename {
                name: "青闲的 MacBook Air".to_owned(),
            })
        );
        assert!(parse(args(&["worker", "rename"])).is_err());
        assert!(parse(args(&["worker", "rename", "a", "b"])).is_err());
        assert!(parse(args(&["worker", "rename", "--help"])).is_err());
        assert!(parse(args(&["worker", "rename", "-h"])).is_err());
        assert!(parse(args(&["worker", "rename", &"😀".repeat(65)])).is_err());
        assert_eq!(
            parse(args(&[
                "worker",
                "revoke",
                "dev_01m1e4vf6vgxamgd0cn9we8n7m",
                "--confirm",
            ]))
            .unwrap()
            .command,
            Command::Worker(WorkerCommand::Revoke {
                device_id: "dev_01M1E4VF6VGXAMGD0CN9WE8N7M".to_owned(),
            })
        );
        assert!(
            parse(args(&[
                "worker",
                "revoke",
                "dev_01M1E4VF6VGXAMGD0CN9WE8N7M",
            ]))
            .is_err()
        );
        assert!(parse(args(&["worker", "revoke", "not-a-device", "--confirm",])).is_err());

        assert!(parse(args(&["worker", "connect", "--code", "secret"])).is_err());
        assert!(parse(args(&["device", "pair", "--code", "secret"])).is_err());
        assert!(parse(args(&["worker", "pair", "--code", "secret"])).is_err());
        assert!(
            parse(args(&[
                "worker",
                "connect",
                "https://edge.example/pair#pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "--enrollment-file",
                "/tmp/enrollment.json",
            ]))
            .is_err()
        );
        assert!(
            parse(args(&[
                "worker",
                "connect",
                "https://edge.example/pair#pair_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "--edge-origin",
                "https://edge.example",
            ]))
            .is_err()
        );
        assert!(parse(args(&["worker", "connect"])).is_err());
        assert!(parse(args(&["worker", "connect", "a", "b"])).is_err());
        assert!(parse(args(&["worker", "pair", "--ttl-seconds", "900"])).is_err());
        assert!(parse(args(&["worker", "pair", "--ttl-seconds", "59"])).is_err());
        assert_eq!(
            parse(args(&["worker", "pair"])).unwrap().command,
            Command::Worker(WorkerCommand::Pair {
                ttl_seconds: 600,
                name: None,
            })
        );
        assert_eq!(
            parse(args(&["dev", "--dry-run"])).unwrap().command,
            Command::Dev(DevCommand::Sync {
                dry_run: true,
                allow_dirty: false
            })
        );
        assert_eq!(
            parse(args(&["dev"])).unwrap().command,
            Command::Dev(DevCommand::Status)
        );
        assert_eq!(
            parse(args(&["dev", "sync", "--allow-dirty"]))
                .unwrap()
                .command,
            Command::Dev(DevCommand::Sync {
                dry_run: false,
                allow_dirty: true
            })
        );
        assert_eq!(
            parse(args(&["dev", "rollback"])).unwrap().command,
            Command::Dev(DevCommand::Rollback)
        );
        assert_eq!(
            parse(args(&["candidate", "--port", "9000"]))
                .unwrap()
                .command,
            Command::Candidate { port: 9000 }
        );
        assert_eq!(
            parse(args(&["tcc-broker", "install"])).unwrap().command,
            Command::TccBroker(TccBrokerCommand::Install { force: false })
        );
        assert_eq!(
            parse(args(&["tcc-broker", "install", "--force"]))
                .unwrap()
                .command,
            Command::TccBroker(TccBrokerCommand::Install { force: true })
        );
        assert_eq!(
            parse(args(&["tcc-broker", "status"])).unwrap().command,
            Command::TccBroker(TccBrokerCommand::Status)
        );
        assert_eq!(
            parse(args(&["tcc-broker", "uninstall"])).unwrap().command,
            Command::TccBroker(TccBrokerCommand::Uninstall)
        );
        assert_eq!(
            parse(args(&["__tcc-broker"])).unwrap().command,
            Command::TccBrokerRun
        );
        assert!(parse(args(&["tcc-broker", "bogus"])).is_err());
        assert_eq!(
            parse(args(&["service", "install", "--adopt-node"]))
                .unwrap()
                .command,
            Command::Service(ServiceCommand::Install { adopt_node: true })
        );
        assert_eq!(
            parse(args(&["service", "status"])).unwrap().command,
            Command::Service(ServiceCommand::Status)
        );
        assert_eq!(
            parse(args(&["service", "rollback"])).unwrap().command,
            Command::Service(ServiceCommand::Rollback)
        );
        assert_eq!(
            parse(args(&[
                "service",
                "__guardian",
                "--transaction",
                "gtx-1234-abcd",
                "--parent-pid",
                "1234"
            ]))
            .unwrap()
            .command,
            Command::Service(ServiceCommand::Guardian {
                transaction_id: "gtx-1234-abcd".to_owned(),
                parent_pid: 1234,
            })
        );
        assert_eq!(
            parse(args(&[
                "update",
                "check",
                "--manifest",
                "https://example.com/release.json"
            ]))
            .unwrap()
            .command,
            Command::Update(UpdateCommand::Check {
                manifest_url: Some("https://example.com/release.json".to_owned())
            })
        );
        assert_eq!(
            parse(args(&["update", "worker", "--job", "upd-12345678"]))
                .unwrap()
                .command,
            Command::Update(UpdateCommand::Worker {
                job_id: "upd-12345678".to_owned()
            })
        );
        assert_eq!(
            parse(args(&["update", "auto"])).unwrap().command,
            Command::Update(UpdateCommand::Auto)
        );
        assert_eq!(
            parse(args(&["link", "status"])).unwrap().command,
            Command::Link(LinkCommand::Status)
        );
        assert_eq!(
            parse(args(&["link", "run"])).unwrap().command,
            Command::Link(LinkCommand::Run)
        );
        assert_eq!(
            parse(args(&["link", "install"])).unwrap().command,
            Command::Link(LinkCommand::Install)
        );
        assert_eq!(
            parse(args(&["link", "uninstall"])).unwrap().command,
            Command::Link(LinkCommand::Uninstall)
        );
        assert_eq!(
            parse(args(&["link", "cutover"])).unwrap().command,
            Command::Link(LinkCommand::Cutover {
                mode: crate::link::CutoverMode::DryRun
            })
        );
        assert_eq!(
            parse(args(&["link", "cutover", "--dry-run"]))
                .unwrap()
                .command,
            Command::Link(LinkCommand::Cutover {
                mode: crate::link::CutoverMode::DryRun
            })
        );
        assert_eq!(
            parse(args(&["link", "cutover", "--execute"]))
                .unwrap()
                .command,
            Command::Link(LinkCommand::Cutover {
                mode: crate::link::CutoverMode::Execute
            })
        );
        assert_eq!(
            parse(args(&["link", "migrate-runtime-control"]))
                .unwrap()
                .command,
            Command::Link(LinkCommand::MigrateRuntimeControl {
                mode: crate::link::MigrateMode::DryRun
            })
        );
        assert_eq!(
            parse(args(&[
                "link",
                "migrate-runtime-control",
                "--write-staging"
            ]))
            .unwrap()
            .command,
            Command::Link(LinkCommand::MigrateRuntimeControl {
                mode: crate::link::MigrateMode::WriteStaging
            })
        );
        assert_eq!(
            parse(args(&["link", "migrate-runtime-control", "--apply"]))
                .unwrap()
                .command,
            Command::Link(LinkCommand::MigrateRuntimeControl {
                mode: crate::link::MigrateMode::Apply
            })
        );
        assert_eq!(
            parse(args(&["native-host", "status"])).unwrap().command,
            Command::NativeHost(NativeHostCommand::Status)
        );
        assert_eq!(
            parse(args(&["native-host", "uninstall"])).unwrap().command,
            Command::NativeHost(NativeHostCommand::Uninstall)
        );
        assert_eq!(
            parse(args(&["native-host", "rollback"])).unwrap().command,
            Command::NativeHost(NativeHostCommand::Rollback)
        );
        assert_eq!(
            parse(args(&["native-host", "dev", "enable"]))
                .unwrap()
                .command,
            Command::NativeHost(NativeHostCommand::DevEnable { path: None })
        );
        assert_eq!(
            parse(args(&["native-host", "dev", "enable", "./extension"]))
                .unwrap()
                .command,
            Command::NativeHost(NativeHostCommand::DevEnable {
                path: Some("./extension".to_owned())
            })
        );
        assert_eq!(
            parse(args(&["native-host", "dev", "disable"]))
                .unwrap()
                .command,
            Command::NativeHost(NativeHostCommand::DevDisable)
        );
        assert_eq!(
            parse(args(&["native-host", "use", "store"]))
                .unwrap()
                .command,
            Command::NativeHost(NativeHostCommand::UseStore)
        );
        assert_eq!(
            parse(args(&["native-host", "use", "standalone"]))
                .unwrap()
                .command,
            Command::NativeHost(NativeHostCommand::UseStandalone)
        );
        assert_eq!(
            parse(args(&["native-host", "use", "dev"])).unwrap().command,
            Command::NativeHost(NativeHostCommand::UseDev)
        );
        assert_eq!(
            parse(args(&["extension", "standalone", "install"]))
                .unwrap()
                .command,
            Command::Extension(ExtensionCommand::StandaloneInstall { reference: None })
        );
        assert_eq!(
            parse(args(&[
                "extension",
                "standalone",
                "install",
                "--ref",
                "main"
            ]))
            .unwrap()
            .command,
            Command::Extension(ExtensionCommand::StandaloneInstall {
                reference: Some("main".to_owned())
            })
        );
        assert_eq!(
            parse(args(&["extension", "standalone", "status"]))
                .unwrap()
                .command,
            Command::Extension(ExtensionCommand::StandaloneStatus)
        );
        assert_eq!(
            parse(args(&["extension-host"])).unwrap().command,
            Command::ExtensionHost {
                caller_origin: String::new()
            }
        );
        assert_eq!(
            parse(args(&[
                "extension-host",
                "chrome-extension://abcdefghijklmnop/"
            ]))
            .unwrap()
            .command,
            Command::ExtensionHost {
                caller_origin: "chrome-extension://abcdefghijklmnop/".to_owned()
            }
        );
        assert_eq!(
            parse(args(&[
                "artifact",
                "import",
                "--url",
                "https://edge.example/artifacts/abc",
                "--path",
                "/tmp/project/image.png",
                "--sha256",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "--confirm-busy"
            ]))
            .unwrap()
            .command,
            Command::ArtifactImport(crate::artifact_import::ImportArgs {
                url: "https://edge.example/artifacts/abc".to_owned(),
                path: "/tmp/project/image.png".to_owned(),
                expected_sha256: Some(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned()
                ),
                capability_env: "HERDR_ARTIFACT_CAPABILITY".to_owned(),
                signed_url: false,
                overwrite: false,
                confirm_dirty: false,
                confirm_busy: true,
            })
        );
        assert_eq!(
            parse(args(&[
                "artifact",
                "import",
                "--url",
                "https://signed.example/image.png?sig=abc",
                "--path",
                "/tmp/project/image.png",
                "--signed-url"
            ]))
            .unwrap()
            .command,
            Command::ArtifactImport(crate::artifact_import::ImportArgs {
                url: "https://signed.example/image.png?sig=abc".to_owned(),
                path: "/tmp/project/image.png".to_owned(),
                expected_sha256: None,
                capability_env: "HERDR_ARTIFACT_CAPABILITY".to_owned(),
                signed_url: true,
                overwrite: false,
                confirm_dirty: false,
                confirm_busy: false,
            })
        );
        assert!(
            parse(args(&[
                "artifact",
                "import",
                "--url",
                "https://signed.example/image.png?sig=abc",
                "--path",
                "/tmp/project/image.png",
                "--signed-url",
                "--capability-env",
                "CUSTOM_CAPABILITY"
            ]))
            .is_err()
        );
    }

    #[test]
    fn rejects_unknown_arguments() {
        assert!(parse(args(&["dev", "--legacy"])).is_err());
        assert!(parse(args(&["config", "legacy"])).is_err());
        assert!(parse(args(&["candidate", "--port", "0"])).is_err());
        assert!(parse(args(&["service"])).is_err());
        assert!(parse(args(&["service", "install", "--force"])).is_err());
        assert!(parse(args(&["install", "--adopt-node"])).is_err());
        assert_eq!(
            parse(args(&["update"])).unwrap().command,
            Command::Update(UpdateCommand::Apply { manifest_url: None })
        );
        assert!(parse(args(&["update", "apply", "--force"])).is_err());
        assert!(parse(args(&["native-host"])).is_err());
        assert!(parse(args(&["native-host", "legacy"])).is_err());
        assert!(parse(args(&["native-host", "dev", "disable", "extra"])).is_err());
        assert!(parse(args(&["native-host", "use", "preview"])).is_err());
        assert!(parse(args(&["link"])).is_err());
        assert!(parse(args(&["link", "cutover", "--force"])).is_err());
        assert!(parse(args(&["link", "cutover", "--dry-run", "--execute"])).is_err());
        assert!(
            parse(args(&[
                "link",
                "migrate-runtime-control",
                "--dry-run",
                "--apply"
            ]))
            .is_err()
        );
        assert!(parse(args(&["link", "migrate-runtime-control", "--force"])).is_err());
        assert!(parse(args(&["link", "status", "extra"])).is_err());
        assert!(parse(args(&["extension-host", "https://example.com/"])).is_err());
        assert!(parse(args(&["artifact"])).is_err());
        assert!(
            parse(args(&[
                "artifact",
                "import",
                "--url",
                "https://example.com"
            ]))
            .is_err()
        );
        assert!(
            parse(args(&[
                "artifact",
                "import",
                "--url",
                "https://example.com/a",
                "--path",
                "/tmp/a.png",
                "--capability-env",
                "bad-name"
            ]))
            .is_err()
        );
        assert!(parse(args(&["status", "extra"])).is_err());
        assert!(parse(args(&["scan", "--force"])).is_err());
        assert!(parse(args(&["scan", "--json", "--json"])).is_err());
        assert!(parse(args(&["unknown"])).is_err());
    }

    #[test]
    fn parses_connector_owner_approval_without_accepting_code_on_argv() {
        assert_eq!(
            parse(args(&["connector", "approve", "req_abc"]))
                .unwrap()
                .command,
            Command::Worker(WorkerCommand::ConnectorApprove {
                request_id: "req_abc".to_owned(),
            })
        );
        assert_eq!(
            parse(args(&["connector", "list"])).unwrap().command,
            Command::Worker(WorkerCommand::ConnectorList)
        );
        assert_eq!(
            parse(args(&[
                "connector",
                "revoke",
                "conn_abc123XYZ",
                "--confirm"
            ]))
            .unwrap()
            .command,
            Command::Worker(WorkerCommand::ConnectorRevoke {
                connector_id: "conn_abc123XYZ".to_owned(),
            })
        );
        assert!(parse(args(&["connector", "approve", "req_abc", "123456"])).is_err());
        assert!(parse(args(&["connector", "approve", "--code", "123456"])).is_err());
        assert!(parse(args(&["connector", "revoke", "conn_abc", "--confirm"])).is_err());
        assert!(parse(args(&["connector", "revoke", "dcr-client", "--confirm"])).is_err());
        assert!(parse(args(&["connector", "revoke", "conn_abc", "--nope"])).is_err());
        assert!(parse(args(&["connector", "revoke", "conn_abc"])).is_err());
    }

    #[test]
    fn parses_connector_and_worker_help_success() {
        assert_eq!(
            parse(args(&["connector", "--help"])).unwrap().command,
            Command::Help {
                section: HelpSection::Connector,
            }
        );
        assert_eq!(
            parse(args(&["connector", "-h"])).unwrap().command,
            Command::Help {
                section: HelpSection::Connector,
            }
        );
        assert_eq!(
            parse(args(&["worker", "--help"])).unwrap().command,
            Command::Help {
                section: HelpSection::Worker,
            }
        );
        assert_eq!(
            parse(args(&["automation", "--help"])).unwrap().command,
            Command::Help {
                section: HelpSection::Automation,
            }
        );
    }

    #[test]
    fn parses_automation_service_principal_lifecycle() {
        assert_eq!(
            parse(args(&[
                "automation",
                "create",
                "--name",
                "gitlab:group/project:prod",
                "--device",
                "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
            ]))
            .unwrap()
            .command,
            Command::Worker(WorkerCommand::AutomationCreate {
                name: "gitlab:group/project:prod".to_owned(),
                device: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
            })
        );
        // --device may precede --name.
        assert_eq!(
            parse(args(&[
                "automation",
                "create",
                "--device",
                "build-runner-01",
                "--name",
                "gitlab:ci:pipeline",
            ]))
            .unwrap()
            .command,
            Command::Worker(WorkerCommand::AutomationCreate {
                name: "gitlab:ci:pipeline".to_owned(),
                device: "build-runner-01".to_owned(),
            })
        );
        assert_eq!(
            parse(args(&["automation", "list"])).unwrap().command,
            Command::Worker(WorkerCommand::AutomationList)
        );
        assert_eq!(
            parse(args(&[
                "automation",
                "rotate",
                "svc_abcdefgh1234",
                "--confirm",
            ]))
            .unwrap()
            .command,
            Command::Worker(WorkerCommand::AutomationRotate {
                client_id: "svc_abcdefgh1234".to_owned(),
            })
        );
        assert_eq!(
            parse(args(&[
                "automation",
                "revoke",
                "svc_abcdefgh1234",
                "--confirm",
            ]))
            .unwrap()
            .command,
            Command::Worker(WorkerCommand::AutomationRevoke {
                client_id: "svc_abcdefgh1234".to_owned(),
            })
        );
        // --device is required; a device must never be silently chosen.
        assert!(parse(args(&["automation", "create", "gitlab"])).is_err());
        assert!(parse(args(&["automation", "create", "--name", "gitlab:ci"])).is_err());
        assert!(
            parse(args(&[
                "automation",
                "create",
                "--device",
                "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV"
            ]))
            .is_err()
        );
        assert!(parse(args(&["automation", "rotate", "svc_abcdefgh1234"])).is_err());
        assert!(parse(args(&["automation", "revoke", "dcr-client", "--confirm"])).is_err());
    }

    #[test]
    fn parses_instance_flag_before_command() {
        let parsed = parse(args(&["--instance", "uat", "status"])).unwrap();
        assert_eq!(parsed.instance.as_deref(), Some("uat"));
        assert_eq!(parsed.command, Command::Status);
        let parsed = parse(args(&["-i", "clean", "doctor"])).unwrap();
        assert_eq!(parsed.instance.as_deref(), Some("clean"));
        assert_eq!(parsed.command, Command::Doctor);
        assert!(parse(args(&["--instance", "server", "status"])).is_err());
        assert!(
            parse(args(&[
                "--instance",
                "uat",
                "--instance",
                "clean",
                "status"
            ]))
            .is_err()
        );
    }

    #[test]
    fn help_documents_user_path_ahead_of_service() {
        let text = help();
        for needle in [
            "herdr-mcp install",
            "herdr-mcp status",
            "herdr-mcp doctor",
            "herdr-mcp permissions",
            "herdr-mcp scan",
            "herdr-mcp connector list",
            "herdr-mcp connector approve",
            "herdr-mcp connector revoke",
            "herdr-mcp automation create",
            "herdr-mcp automation list",
            "herdr-mcp connector revoke <connector-id> --confirm",
            "herdr-mcp automation create --name NAME --device <device-id-or-unique-name>",
            "herdr-mcp update",
            "herdr-mcp rollback",
            "herdr-mcp uninstall",
        ] {
            assert!(
                text.contains(needle),
                "help missing user-path command: {needle}"
            );
        }
        assert!(text.contains("User path:"));
        assert!(text.contains("Advanced / internal:"));
        assert!(text.contains("herdr-mcp link status"));
        assert!(text.contains("herdr-mcp link run"));
        assert!(text.contains("herdr-mcp link install"));
        assert!(text.contains("herdr-mcp link uninstall"));
        assert!(text.contains("herdr-mcp link cutover"));
        assert!(text.contains("herdr-mcp link migrate-runtime-control"));
        assert!(text.contains("dev.herdr-mcp.link-rust-candidate"));
        assert!(text.contains("dry-run plan/validate only"));
        assert!(text.contains("HERDR_LINK_MIGRATE_RUNTIME_CONTROL=1"));
        let install = text.find("herdr-mcp install").expect("install");
        let service = text.find("herdr-mcp service").expect("service");
        assert!(
            install < service,
            "user-path install must appear before advanced service"
        );
        let user_slice = &text[..service];
        assert!(
            !user_slice.contains("herdr-mcp service install"),
            "user path must not require service install"
        );
    }
}
