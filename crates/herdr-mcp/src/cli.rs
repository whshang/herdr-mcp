#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Status,
    Doctor,
    Config(ConfigCommand),
    Dev { dry_run: bool },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigCommand {
    Path,
    Show,
    Init,
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

pub fn help() -> &'static str {
    "Herdr MCP native runtime\n\n\
Usage:\n\
  herdr-mcp version\n\
  herdr-mcp status\n\
  herdr-mcp doctor\n\
  herdr-mcp config [path|show|init]\n\
  herdr-mcp dev [--dry-run]\n\n\
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
    }

    #[test]
    fn rejects_unknown_arguments() {
        assert!(parse(args(&["dev", "--legacy"])).is_err());
        assert!(parse(args(&["config", "legacy"])).is_err());
        assert!(parse(args(&["status", "extra"])).is_err());
        assert!(parse(args(&["unknown"])).is_err());
    }
}
