#![cfg(target_os = "macos")]

use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const GUARDIAN_PARENT_FD: i32 = 198;
const GUARDIAN_LOCK_FD: i32 = 199;

fn test_root() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "herdr-mcp-guardian-process-{}-{nonce}",
        std::process::id()
    ))
}

fn atomic_json(path: &Path, value: &Value) {
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent).unwrap();
    let temp = parent.join(format!(".transaction-{}.tmp", std::process::id()));
    fs::write(&temp, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    fs::set_permissions(&temp, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(temp, path).unwrap();
}

fn read_state(path: &Path) -> String {
    serde_json::from_slice::<Value>(&fs::read(path).unwrap())
        .unwrap()
        .get("state")
        .and_then(Value::as_str)
        .unwrap()
        .to_owned()
}

fn wait_for_state(path: &Path, expected: &str, budget: Duration) {
    let deadline = Instant::now() + budget;
    loop {
        if path.exists() && read_state(path) == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "guardian transaction did not reach {expected}; current={}",
            if path.exists() {
                read_state(path)
            } else {
                "<missing>".to_owned()
            }
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn pipe() -> io::Result<(File, File)> {
    let mut fds = [0_i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
}

fn wait_child(child: &mut Child, budget: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("guardian child did not exit within {budget:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn guardian_real_exec_inherits_pipe_and_lock_until_committed_hup() {
    let root = test_root();
    let home = root.join("home");
    let config = root.join("config");
    let transaction_id = format!("gtx-{}-process-test", std::process::id());
    let guardian_dir = config.join("guardians").join(&transaction_id);
    fs::create_dir_all(&guardian_dir).unwrap();
    fs::set_permissions(&guardian_dir, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir_all(&home).unwrap();

    let transaction = guardian_dir.join("transaction.json");
    let parent_pid = std::process::id();
    let mut record = json!({
        "schema_version": 1,
        "transaction_id": transaction_id,
        "mode": "install",
        "state": "armed",
        "parent_pid": parent_pid,
        "created_at": 1,
        "rollback_id": null,
        "candidate_generation_id": null,
        "server_plist_backup": null,
        "watchdog_plist_backup": null,
        "previous_current_target": null,
        "server_was_loaded": false,
        "watchdog_was_loaded": false,
        "detail": null
    });
    atomic_json(&transaction, &record);

    let lock_path = config.join("service-mutation.lock");
    fs::create_dir_all(&config).unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );

    let (read_signal, write_signal) = pipe().unwrap();
    let read_fd = read_signal.as_raw_fd();
    let write_fd = write_signal.as_raw_fd();
    let lock_fd = lock.as_raw_fd();
    assert!(![read_fd, write_fd, lock_fd].contains(&GUARDIAN_PARENT_FD));
    assert!(![read_fd, write_fd, lock_fd].contains(&GUARDIAN_LOCK_FD));

    let binary = env!("CARGO_BIN_EXE_herdr-mcp");
    let mut command = Command::new(binary);
    command
        .args([
            "service",
            "__guardian",
            "--transaction",
            &transaction_id,
            "--parent-pid",
            &parent_pid.to_string(),
        ])
        .env_clear()
        .env("HOME", &home)
        .env("HERDR_MCP_CONFIG_DIR", &config)
        .env("HERDR_SOCKET_PATH", root.join("herdr.sock"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::dup2(read_fd, GUARDIAN_PARENT_FD) == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::dup2(lock_fd, GUARDIAN_LOCK_FD) == -1 {
                return Err(io::Error::last_os_error());
            }
            libc::close(write_fd);
            libc::close(read_fd);
            libc::close(lock_fd);
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    drop(read_signal);
    drop(lock);

    wait_for_state(&transaction, "watching", Duration::from_secs(3));
    assert!(
        child.try_wait().unwrap().is_none(),
        "guardian exited before parent settlement"
    );

    let probe = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    let blocked = unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_ne!(
        blocked, 0,
        "guardian must retain the inherited service mutation lock after exec"
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::EWOULDBLOCK)
    );
    drop(probe);

    record["state"] = json!("committed");
    record["detail"] = json!("integration test durable commit");
    atomic_json(&transaction, &record);
    drop(write_signal);

    let status = wait_child(&mut child, Duration::from_secs(5));
    assert!(status.success(), "guardian exited with {status}");
    wait_for_state(&transaction, "observed_committed", Duration::from_secs(1));

    let unlocked = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(unlocked.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "service mutation lock must be released when guardian exits"
    );
    unsafe { libc::flock(unlocked.as_raw_fd(), libc::LOCK_UN) };
    drop(unlocked);

    fs::remove_dir_all(root).unwrap();
}
