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
    NativeHost(NativeHostCommand),
    ExtensionHost { caller_origin: String },
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
}

#[derive(Debug, PartialEq, Eq)]
pub enum ServiceCommand {
    Install { adopt_node: bool },
    Status,
    Start,
    Stop,
    Restart,
    Uninstall,
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
        "status" => no_extra(&args, Command::Status),
        "doctor" => no_extra(&args, Command::Doctor),
        "config" => parse_config(&args[1..]),
        "dev" => parse_dev(&args[1..]),
        "candidate" => parse_candidate(&args[1..]),
        "service" => parse_service(&args[1..]),
        "native-host" => parse_native_host(&args[1..]),
        "extension-host" => parse_extension_host(&args[1..]),
        value => Err(format!("unknown command '{value}'\n\n{}", help())),
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
        [subcommand] if subcommand == "uninstall" => {
            Ok(Command::Service(ServiceCommand::Uninstall))
        }
        [] => {
            Err("service requires install, status, start, stop, restart, or uninstall".to_owned())
        }
        [subcommand, ..] => Err(format!(
            "invalid service command or arguments for '{subcommand}'"
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
        [] => Err("native-host requires install, status, or uninstall".to_owned()),
        [subcommand] => Err(format!("unknown native-host command '{subcommand}'")),
        _ => Err(
            "native-host accepts exactly one subcommand: install, status, or uninstall".to_owned(),
        ),
    }
}

fn parse_extension_host(args: &[String]) -> Result<Command, String> {
    match args {
        [caller_origin] if caller_origin.starts_with("chrome-extension://") => {
            Ok(Command::ExtensionHost {
                caller_origin: caller_origin.clone(),
            })
        }
        [] => Err("extension-host requires the Chromium caller origin".to_owned()),
        [value] => Err(format!("invalid extension-host caller origin '{value}'")),
        _ => Err("extension-host accepts exactly one Chromium caller origin".to_owned()),
    }
}

pub fn help() -> &'static str {
    "Herdr MCP native runtime\n\n\
Usage:\n\
  herdr-mcp version\n\
  herdr-mcp status\n\
  herdr-mcp doctor\n\
  herdr-mcp config [path|show|init]\n\
  herdr-mcp dev [--dry-run]\n\
  herdr-mcp candidate [--port 8873]\n\
  herdr-mcp service <install [--adopt-node]|status|start|stop|restart|uninstall>\n\
  herdr-mcp native-host <install|status|uninstall>\n\
  herdr-mcp extension-host <chrome-extension://.../>\n\n\
The Rust binary is the new local product boundary. Service, update, runtime,\n\
link, and native-host commands are added as their native implementations land.\n"
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
        assert_eq!(parse(args(&["status"])).unwrap(), Command::Status);
        assert_eq!(parse(args(&["doctor"])).unwrap(), Command::Doctor);
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
            parse(args(&["native-host", "status"])).unwrap(),
            Command::NativeHost(NativeHostCommand::Status)
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
        assert!(parse(args(&["native-host"])).is_err());
        assert!(parse(args(&["native-host", "legacy"])).is_err());
        assert!(parse(args(&["extension-host"])).is_err());
        assert!(parse(args(&["extension-host", "https://example.com/"])).is_err());
        assert!(parse(args(&["status", "extra"])).is_err());
        assert!(parse(args(&["unknown"])).is_err());
    }
}
