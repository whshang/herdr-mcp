use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub fn resolve_codex_home(
    codex_home: Option<OsString>,
    home: Option<OsString>,
    user_profile: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(value) = codex_home.filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(value));
    }
    default_codex_home(home, user_profile)
}

fn default_codex_home(home: Option<OsString>, user_profile: Option<OsString>) -> Option<PathBuf> {
    #[cfg(windows)]
    let home = user_profile.or(home);
    #[cfg(not(windows))]
    let home = home.or(user_profile);
    home.map(|home| PathBuf::from(home).join(".codex"))
}

pub fn same_directory(left: &Path, right: &Path) -> bool {
    let real = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    real(left) == real(right)
}
