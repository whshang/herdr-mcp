use crate::config::Config;
use crate::paths::RuntimePaths;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

pub fn run(dry_run: bool) -> Result<ExitCode, String> {
    let cwd =
        env::current_dir().map_err(|error| format!("cannot read current directory: {error}"))?;
    let repo = find_repo_root(&cwd).ok_or_else(|| {
        "dev mode must run inside a herdr-mcp source checkout containing package.json and src/server.ts".to_owned()
    })?;
    let paths = RuntimePaths::discover()?;
    let config = Config::load(&paths.config_file)?;

    if dry_run {
        println!("mode: dev-transition");
        println!("repo: {}", repo.display());
        println!("state: {}", paths.dev_state_dir.display());
        println!("port: {}", config.dev_port);
        println!("command: npm run dev");
        println!("note: TypeScript runtime is temporary migration reference code");
        return Ok(ExitCode::SUCCESS);
    }

    std::fs::create_dir_all(&paths.dev_state_dir)
        .map_err(|error| format!("cannot create dev state directory: {error}"))?;

    let status = Command::new("npm")
        .args(["run", "dev"])
        .current_dir(&repo)
        .env("HERDR_MCP_PORT", config.dev_port.to_string())
        .env("HERDR_MCP_STATE_DIR", &paths.dev_state_dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| {
            format!("failed to start transitional TypeScript development runtime: {error}")
        })?;

    Ok(match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(code) if (1..=255).contains(&code) => ExitCode::from(code as u8),
        _ => ExitCode::FAILURE,
    })
}

fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(path) = current {
        if path.join("package.json").is_file() && path.join("src").join("server.ts").is_file() {
            return Some(path.to_path_buf());
        }
        current = path.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn finds_checkout_from_nested_directory() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!("herdr-mcp-dev-test-{unique}"));
        let nested = root.join("a").join("b");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("package.json"), "{}").unwrap();
        fs::write(root.join("src").join("server.ts"), "").unwrap();

        assert_eq!(find_repo_root(&nested), Some(root.clone()));

        fs::remove_dir_all(root).unwrap();
    }
}
