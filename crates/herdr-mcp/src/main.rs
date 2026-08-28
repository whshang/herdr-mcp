mod agent_visibility;
mod browser_extension_identity;
mod capability_inventory;
mod capability_probe;
mod capability_resolver;
mod capability_scan;
mod cli;
mod config;
mod contract;
mod dev;
pub mod development_orchestration;
mod events;
mod exec_compact;
mod exec_sessions;
mod exec_tools;
mod extension_ipc;
mod fs_mutation;
mod fs_patch;
mod fs_security;
mod fs_tools;
mod git_tools;
mod herdr;
mod inspect;
mod instance;
mod link;
mod mcp;
mod mcp_http;
mod mutation;
mod native_host;
mod native_host_install;
mod native_tools;
mod patch;
mod paths;
mod progressive_skills;
mod projects;
mod prompt;
mod relay;
mod release_trust;
mod runtime_meta;
mod schema;
mod service_manager;
mod skill;
pub mod skill_dispatch;
mod snapshot;
mod state_cache;
mod state_store;
mod status;
mod updater;
mod updater_store;
// Wired only from the macOS service manager; keep unit tests compiling on Linux CI.
#[cfg(any(target_os = "macos", test))]
mod user_cli;
mod utility_exec;
mod workstation;

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
    let parsed = cli::parse(std::env::args().skip(1))?;
    if let Some(name) = parsed.instance.as_deref() {
        // SSOT for path/label/port discovery in this process.
        unsafe { std::env::set_var("HERDR_MCP_INSTANCE", name) };
    }
    match parsed.command {
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
            println!("state schema {}", state_store::SCHEMA_VERSION);
            Ok(ExitCode::SUCCESS)
        }
        cli::Command::Status => {
            let paths = paths::RuntimePaths::discover()?;
            let config = config::Config::load_for_instance(&paths.config_file, &paths.instance)?;
            status::print_status(&paths, &config);
            Ok(ExitCode::SUCCESS)
        }
        cli::Command::Doctor => {
            let paths = paths::RuntimePaths::discover()?;
            let config = config::Config::load_for_instance(&paths.config_file, &paths.instance)?;
            Ok(if status::print_doctor(&paths, &config) {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            })
        }
        cli::Command::Scan {
            json,
            refresh,
            probe,
        } => capability_scan::run(capability_scan::ScanOptions {
            json,
            refresh,
            probe,
        }),
        cli::Command::Config(command) => {
            let paths = paths::RuntimePaths::discover()?;
            match command {
                cli::ConfigCommand::Path => println!("{}", paths.config_file.display()),
                cli::ConfigCommand::Show => {
                    let config =
                        config::Config::load_for_instance(&paths.config_file, &paths.instance)?;
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
                    std::fs::write(
                        &paths.config_file,
                        config::Config::missing_file_default_for_instance(&paths.instance).render(),
                    )
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
        cli::Command::Candidate { port } => mcp_http::serve_candidate(port),
        cli::Command::Service(command) => service_manager::run(command),
        cli::Command::Update(command) => updater::run(command),
        cli::Command::NativeHost(command) => native_host_install::run(command),
        cli::Command::ExtensionHost { caller_origin } => native_host::run(&caller_origin),
        cli::Command::Link(command) => match command {
            cli::LinkCommand::Status => link::run_link_status(),
            cli::LinkCommand::Run => link::run_link(),
            cli::LinkCommand::Install => link::run_link_install(),
            cli::LinkCommand::Uninstall => link::run_link_uninstall(),
            cli::LinkCommand::Cutover { mode } => link::run_link_cutover(mode),
            cli::LinkCommand::Seal { mode } => link::run_link_seal(mode),
            cli::LinkCommand::MigrateRuntimeControl { mode } => {
                link::run_link_migrate_runtime_control(mode)
            }
        },
    }
}
