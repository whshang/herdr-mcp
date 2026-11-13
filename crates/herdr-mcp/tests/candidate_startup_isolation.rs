#![cfg(unix)]

use std::{
    ffi::OsString,
    fs,
    os::unix::{ffi::OsStringExt, net::UnixListener},
    path::Path,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn candidate_rejects_core_or_invalid_config_before_tui_startup() {
    let root = std::env::temp_dir().join(format!(
        "h936-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();

    for name in [
        "core-json",
        "core-legacy",
        "bad-json",
        "bad-env",
        "dangling",
        "nonunicode-backend",
        "nonunicode-session",
    ] {
        let case = root.join(name);
        for dir in ["home", "config", "state", "xdg", "runtime"] {
            fs::create_dir_all(case.join(dir)).unwrap();
        }
        let config = case.join("config/config.json");
        let legacy = case.join("config/config.toml");
        match name {
            "core-json" => fs::write(&config, r#"{"runtime":{"backend":"herdr-core"}}"#).unwrap(),
            "core-legacy" => fs::write(&legacy, "[runtime]\nport = 9000\n").unwrap(),
            "bad-json" => fs::write(&config, r#"{"runtime":{"backend":"invalid"}}"#).unwrap(),
            "dangling" => std::os::unix::fs::symlink(case.join("missing.json"), &config).unwrap(),
            _ => {}
        }
        // Keep the Unix socket path short enough for macOS temporary directories.
        let socket = root.join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_herdr-mcp"));
        command
            .args(["candidate", "--port", "65534"])
            .current_dir(case.join("home"))
            .env_clear()
            .env("HOME", case.join("home"))
            .env("USERPROFILE", case.join("home"))
            .env("XDG_CONFIG_HOME", case.join("xdg"))
            .env("XDG_STATE_HOME", case.join("state"))
            .env("XDG_RUNTIME_DIR", case.join("runtime"))
            .env("TMPDIR", case.join("runtime"))
            .env("HERDR_MCP_CONFIG_DIR", case.join("config"))
            .env("HERDR_MCP_STATE_DIR", case.join("state"))
            .env("HERDR_MCP_DEV_STATE_DIR", case.join("state"))
            .env("HERDR_SOCKET_PATH", &socket)
            .env("HC_SOCKET", case.join("hc.sock"))
            .env("HERDR_MCP_CHILD_REGISTRY", "0")
            .env("HERDR_MCP_LANG", "en")
            .env("LANG", "C")
            .env("PATH", "/usr/bin:/bin");
        match name {
            "core-legacy" => {
                command.env("HERDR_MCP_RUNTIME_BACKEND", "herdr-core");
            }
            "bad-env" => {
                command.env("HERDR_MCP_RUNTIME_BACKEND", "bogus");
            }
            "nonunicode-backend" => {
                command.env("HERDR_MCP_RUNTIME_BACKEND", OsString::from_vec(vec![0xff]));
            }
            "nonunicode-session" => {
                command.env("HERDR_MCP_RUNTIME_BACKEND", "herdr-core");
                command.env_remove("HC_SOCKET");
                command.env("HC_SESSION", OsString::from_vec(vec![0xff]));
            }
            _ => {}
        }
        // No bearer token is provided: even an accidental TUI fallback cannot
        // start an HTTP listener; the diagnostic below detects that fallback.
        let output = command.output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        let expected = match name {
            "core-json" | "core-legacy" => "candidate startup for herdr-core is unavailable",
            "bad-json" => "invalid config",
            "bad-env" => "invalid runtime backend",
            "nonunicode-backend" => "HERDR_MCP_RUNTIME_BACKEND must be valid UTF-8",
            "nonunicode-session" => "HC_SESSION must be valid UTF-8",
            _ => "cannot read config",
        };
        assert!(!output.status.success(), "{name}: unexpected success");
        assert!(stderr.contains(expected), "{name}: {stderr}");
        assert!(!stderr.contains("LAYER child-process"), "{name}: {stderr}");
        assert!(
            !stderr.contains("requires HERDR_MCP_TOKEN"),
            "{name}: {stderr}"
        );
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(
            fs::read_dir(case.join("state")).unwrap().count(),
            0,
            "{name}"
        );
        drop(listener);
        fs::remove_file(&socket).unwrap();
        if name == "core-legacy" {
            assert_eq!(
                fs::read_to_string(&legacy).unwrap(),
                "[runtime]\nport = 9000\n"
            );
            assert!(
                !Path::new(&config).exists(),
                "legacy config must not migrate"
            );
        }
    }
    fs::remove_dir_all(root).unwrap();
}
