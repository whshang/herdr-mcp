#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Status,
    Doctor,
    Config(ConfigCommand),
    Dev { dry_run: bool },
    Candidate { port: u16 },
    Service(ServiceCommand),
    Update(UpdateCommand),
    NativeHost(NativeHostCommand),
    ExtensionHost { caller_origin: String },
    Link(LinkCommand),
}

#[derive(Debug, PartialEq, Eq)]
pub enum LinkCommand {
    Status,
    /// Foreground staged Rust Link candidate. Does not cut over production LaunchAgents.
    Run,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigCommand {
    Path,
    Show,
    Init,
}

#[derive(Debug, PartialEq, Eq)]
pub enum NativeHostCommand {
    Install,
    Status,
    Uninstall,
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
    Status,
    Worker { job_id: String },
}

pub fn parse<I>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = String>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    let Some(command) = args.first().map(String::as_str) else {
        return Ok(Command::Help);
    };

    match command {
        "help" | "-h" | "--help" => no_extra(&args, Command::Help),
        "version" | "-V" | "--version" => no_extra(&args, Command::Version),
        "install" => no_extra(
            &args,
            Command::Service(ServiceCommand::Install { adopt_node: false }),
        ),
        "status" => no_extra(&args, Command::Status),
        "doctor" => no_extra(&args, Command::Doctor),
        "rollback" => no_extra(&args, Command::Service(ServiceCommand::Rollback)),
        "uninstall" => no_extra(&args, Command::Service(ServiceCommand::Uninstall)),
        "config" => parse_config(&args[1..]),
        "dev" => parse_dev(&args[1..]),
        "candidate" => parse_candidate(&args[1..]),
        "service" => parse_service(&args[1..]),
        "update" => parse_update(&args[1..]),
        "native-host" => parse_native_host(&args[1..]),
        "extension-host" => parse_extension_host(&args[1..]),
        "link" => parse_link(&args[1..]),
        value => Err(format!("unknown command '{value}'\n\n{}", help())),
    }
}

fn parse_link(args: &[String]) -> Result<Command, String> {
    match args {
        [subcommand] if subcommand == "status" => Ok(Command::Link(LinkCommand::Status)),
        [subcommand] if subcommand == "run" => Ok(Command::Link(LinkCommand::Run)),
        [] => Err("link requires status or run".to_owned()),
        [subcommand] => Err(format!(
            "unknown link command '{subcommand}' (status|run; install/cutover land in a later G5 slice)"
        )),
        _ => Err("link accepts exactly one subcommand: status or run".to_owned()),
    }
}

fn parse_config(args: &[String]) -> Result<Command, String> {
    match args {
        [] => Ok(Command::Config(ConfigCommand::Show)),
        [subcommand] if subcommand == "path" => Ok(Command::Config(ConfigCommand::Path)),
        [subcommand] if subcommand == "show" => Ok(Command::Config(ConfigCommand::Show)),
        [subcommand] if subcommand == "init" => Ok(Command::Config(ConfigCommand::Init)),
        [subcommand] => Err(format!("unknown config command '{subcommand}'")),
        _ => Err("config accepts exactly one subcommand: path, show, or init".to_owned()),
    }
}

fn no_extra(args: &[String], command: Command) -> Result<Command, String> {
    if args.len() == 1 {
        Ok(command)
    } else {
        Err(format!("unexpected argument '{}'", args[1]))
    }
}

fn parse_dev(args: &[String]) -> Result<Command, String> {
    let mut dry_run = false;
    for arg in args {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            value => return Err(format!("unknown dev argument '{value}'")),
        }
    }
    Ok(Command::Dev { dry_run })
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
        [subcommand] if subcommand == "status" => Ok(Command::Update(UpdateCommand::Status)),
        [subcommand, flag, value] if subcommand == "worker" && flag == "--job" => {
            Ok(Command::Update(UpdateCommand::Worker {
                job_id: value.clone(),
            }))
        }
        [] => Err("update requires check, apply, or status".to_owned()),
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
        [] => Err("native-host requires install, status, uninstall, or rollback".to_owned()),
        [subcommand] => Err(format!("unknown native-host command '{subcommand}'")),
        _ => Err(
            "native-host accepts exactly one subcommand: install, status, uninstall, or rollback"
                .to_owned(),
        ),
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
  herdr-mcp update <check [--manifest URL]|apply [--manifest URL]|status>\n\
  herdr-mcp rollback\n\
  herdr-mcp uninstall\n\n\
Advanced / internal:\n\
  herdr-mcp version\n\
  herdr-mcp config [path|show|init]\n\
  herdr-mcp service <install [--adopt-node]|status|start|stop|restart|rollback|uninstall>\n\
  herdr-mcp link status\n\
  herdr-mcp link run\n\
  herdr-mcp native-host <install|status|uninstall|rollback>\n\
  herdr-mcp extension-host [chrome-extension://.../]\n\
  herdr-mcp dev [--dry-run]\n\
  herdr-mcp candidate [--port 8873]\n\n\
Prefer the top-level install/status/doctor/update/rollback/uninstall commands\n\
for normal lifecycle. Use service ... only for advanced service control\n\
(for example service install --adopt-node). link status is read-only G5\n\
ownership/gates reporting. link run starts a foreground Rust Link candidate\n\
(Keychain/plist credentials); it does not install or cut over production Link.\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn parses_core_commands() {
        assert_eq!(parse(args(&[])).unwrap(), Command::Help);
        assert_eq!(parse(args(&["version"])).unwrap(), Command::Version);
        assert_eq!(
            parse(args(&["install"])).unwrap(),
            Command::Service(ServiceCommand::Install { adopt_node: false })
        );
        assert_eq!(parse(args(&["status"])).unwrap(), Command::Status);
        assert_eq!(parse(args(&["doctor"])).unwrap(), Command::Doctor);
        assert_eq!(
            parse(args(&["rollback"])).unwrap(),
            Command::Service(ServiceCommand::Rollback)
        );
        assert_eq!(
            parse(args(&["uninstall"])).unwrap(),
            Command::Service(ServiceCommand::Uninstall)
        );
        assert_eq!(
            parse(args(&["config", "show"])).unwrap(),
            Command::Config(ConfigCommand::Show)
        );
        assert_eq!(
            parse(args(&["dev", "--dry-run"])).unwrap(),
            Command::Dev { dry_run: true }
        );
        assert_eq!(
            parse(args(&["candidate", "--port", "9000"])).unwrap(),
            Command::Candidate { port: 9000 }
        );
        assert_eq!(
            parse(args(&["service", "install", "--adopt-node"])).unwrap(),
            Command::Service(ServiceCommand::Install { adopt_node: true })
        );
        assert_eq!(
            parse(args(&["service", "status"])).unwrap(),
            Command::Service(ServiceCommand::Status)
        );
        assert_eq!(
            parse(args(&["service", "rollback"])).unwrap(),
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
            .unwrap(),
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
            .unwrap(),
            Command::Update(UpdateCommand::Check {
                manifest_url: Some("https://example.com/release.json".to_owned())
            })
        );
        assert_eq!(
            parse(args(&["update", "worker", "--job", "upd-12345678"])).unwrap(),
            Command::Update(UpdateCommand::Worker {
                job_id: "upd-12345678".to_owned()
            })
        );
        assert_eq!(
            parse(args(&["link", "status"])).unwrap(),
            Command::Link(LinkCommand::Status)
        );
        assert_eq!(
            parse(args(&["link", "run"])).unwrap(),
            Command::Link(LinkCommand::Run)
        );
        assert_eq!(
            parse(args(&["native-host", "status"])).unwrap(),
            Command::NativeHost(NativeHostCommand::Status)
        );
        assert_eq!(
            parse(args(&["native-host", "uninstall"])).unwrap(),
            Command::NativeHost(NativeHostCommand::Uninstall)
        );
        assert_eq!(
            parse(args(&["native-host", "rollback"])).unwrap(),
            Command::NativeHost(NativeHostCommand::Rollback)
        );
        assert_eq!(
            parse(args(&["extension-host"])).unwrap(),
            Command::ExtensionHost {
                caller_origin: String::new()
            }
        );
        assert_eq!(
            parse(args(&[
                "extension-host",
                "chrome-extension://abcdefghijklmnop/"
            ]))
            .unwrap(),
            Command::ExtensionHost {
                caller_origin: "chrome-extension://abcdefghijklmnop/".to_owned()
            }
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
        assert!(parse(args(&["update"])).is_err());
        assert!(parse(args(&["update", "apply", "--force"])).is_err());
        assert!(parse(args(&["native-host"])).is_err());
        assert!(parse(args(&["native-host", "legacy"])).is_err());
        assert!(parse(args(&["link"])).is_err());
        assert!(parse(args(&["link", "install"])).is_err());
        assert!(parse(args(&["link", "status", "extra"])).is_err());
        assert!(parse(args(&["extension-host", "https://example.com/"])).is_err());
        assert!(parse(args(&["status", "extra"])).is_err());
        assert!(parse(args(&["unknown"])).is_err());
    }

    #[test]
    fn help_documents_user_path_ahead_of_service() {
        let text = help();
        for needle in [
            "herdr-mcp install",
            "herdr-mcp status",
            "herdr-mcp doctor",
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
