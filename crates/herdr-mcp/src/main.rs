mod cli;
mod config;
mod contract;
mod dev;
mod herdr;
mod native_tools;
mod paths;
mod schema;
mod status;

use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("herdr-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, String> {
    match cli::parse(std::env::args().skip(1))? {
        cli::Command::Help => {
            print!("{}", cli::help());
            Ok(ExitCode::SUCCESS)
        }
        cli::Command::Version => {
            let contract = contract::identity()?;
            println!("herdr-mcp {}", env!("CARGO_PKG_VERSION"));
            println!(
                "contract epoch {} / {} tools",
                contract.epoch, contract.tool_count
            );
            Ok(ExitCode::SUCCESS)
        }
        cli::Command::Status => {
            let paths = paths::RuntimePaths::discover()?;
            let config = config::Config::load(&paths.config_file)?;
            status::print_status(&paths, &config);
            Ok(ExitCode::SUCCESS)
        }
        cli::Command::Doctor => {
            let paths = paths::RuntimePaths::discover()?;
            let config = config::Config::load(&paths.config_file)?;
            Ok(if status::print_doctor(&paths, &config) {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            })
        }
        cli::Command::Config(command) => {
            let paths = paths::RuntimePaths::discover()?;
            match command {
                cli::ConfigCommand::Path => println!("{}", paths.config_file.display()),
                cli::ConfigCommand::Show => {
                    let config = config::Config::load(&paths.config_file)?;
                    print!("{}", config.render());
                }
                cli::ConfigCommand::Init => {
                    if paths.config_file.exists() {
                        return Err(format!(
                            "config already exists: {}",
                            paths.config_file.display()
                        ));
                    }
                    std::fs::create_dir_all(&paths.config_dir).map_err(|error| {
                        format!(
                            "cannot create config directory {}: {error}",
                            paths.config_dir.display()
                        )
                    })?;
                    std::fs::write(&paths.config_file, config::Config::default().render())
                        .map_err(|error| {
                            format!(
                                "cannot write config {}: {error}",
                                paths.config_file.display()
                            )
                        })?;
                    println!("created {}", paths.config_file.display());
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        cli::Command::Dev { dry_run } => dev::run(dry_run),
    }
}
