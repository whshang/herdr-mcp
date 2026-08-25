use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct RuntimePaths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub dev_state_dir: PathBuf,
    pub herdr_socket: Option<PathBuf>,
}

impl RuntimePaths {
    pub fn discover() -> Result<Self, String> {
        let home = home_dir().ok_or_else(|| "cannot determine user home directory".to_owned())?;
        let config_dir = env::var_os("HERDR_MCP_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("XDG_CONFIG_HOME").map(|path| PathBuf::from(path).join("herdr-mcp"))
            })
            .unwrap_or_else(|| home.join(".config").join("herdr-mcp"));
        let config_file = config_dir.join("config.toml");
        let dev_state_dir = env::var_os("HERDR_MCP_DEV_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config").join("herdr-mcp-dev"));

        #[cfg(unix)]
        let herdr_socket = Some(
            env::var_os("HERDR_SOCKET_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config").join("herdr").join("herdr.sock")),
        );

        #[cfg(windows)]
        let herdr_socket = env::var_os("HERDR_SOCKET_PATH").map(PathBuf::from);

        Ok(Self {
            config_dir,
            config_file,
            dev_state_dir,
            herdr_socket,
        })
    }
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}
