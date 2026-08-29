use crate::instance::InstanceId;
use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct RuntimePaths {
    pub instance: InstanceId,
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub dev_state_dir: PathBuf,
    pub herdr_socket: Option<PathBuf>,
}

impl RuntimePaths {
    pub fn discover() -> Result<Self, String> {
        let home = home_dir().ok_or_else(|| "cannot determine user home directory".to_owned())?;
        let instance = InstanceId::discover()?;
        let config_dir = env::var_os("HERDR_MCP_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("XDG_CONFIG_HOME")
                    .map(|path| PathBuf::from(path).join(instance.config_leaf()))
            })
            .unwrap_or_else(|| home.join(".config").join(instance.config_leaf()));
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
            instance,
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_instance_uses_isolated_config_leaf_without_config_dir_override() {
        let _guard = crate::test_env::lock();
        let previous_instance = env::var_os("HERDR_MCP_INSTANCE");
        let previous_config = env::var_os("HERDR_MCP_CONFIG_DIR");
        let previous_xdg = env::var_os("XDG_CONFIG_HOME");
        unsafe {
            env::set_var("HERDR_MCP_INSTANCE", "uat");
            env::remove_var("HERDR_MCP_CONFIG_DIR");
            env::remove_var("XDG_CONFIG_HOME");
        }
        let paths = RuntimePaths::discover().unwrap();
        assert!(paths.instance.is_named());
        assert!(
            paths
                .config_dir
                .ends_with(std::path::Path::new(".config/herdr-mcp-uat"))
        );
        unsafe {
            match previous_instance {
                Some(value) => env::set_var("HERDR_MCP_INSTANCE", value),
                None => env::remove_var("HERDR_MCP_INSTANCE"),
            }
            match previous_config {
                Some(value) => env::set_var("HERDR_MCP_CONFIG_DIR", value),
                None => env::remove_var("HERDR_MCP_CONFIG_DIR"),
            }
            match previous_xdg {
                Some(value) => env::set_var("XDG_CONFIG_HOME", value),
                None => env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }
}
