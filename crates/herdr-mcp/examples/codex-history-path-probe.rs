#[path = "../src/codex_paths.rs"]
mod codex_paths;

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let explicit = codex_paths::resolve_codex_home(
        Some(OsString::from("explicit")),
        Some(OsString::from("home")),
        Some(OsString::from("profile")),
    )
    .unwrap();
    assert_eq!(explicit, PathBuf::from("explicit"));

    let fallback = codex_paths::resolve_codex_home(
        None,
        Some(OsString::from("home")),
        Some(OsString::from("profile")),
    )
    .unwrap();
    #[cfg(windows)]
    assert_eq!(fallback, PathBuf::from("profile").join(".codex"));
    #[cfg(not(windows))]
    assert_eq!(fallback, PathBuf::from("home").join(".codex"));

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("herdr-codex-path-{}-{unique}", std::process::id()));
    let project = root.join("ProjectCase");
    std::fs::create_dir_all(&project).unwrap();

    #[cfg(windows)]
    {
        let alias = PathBuf::from(project.to_string_lossy().to_ascii_uppercase());
        assert!(codex_paths::same_directory(&project, &alias));
    }
    #[cfg(not(windows))]
    assert!(codex_paths::same_directory(&project, &project));

    std::fs::remove_dir_all(root).unwrap();
    println!("codex history path probe passed");
}
