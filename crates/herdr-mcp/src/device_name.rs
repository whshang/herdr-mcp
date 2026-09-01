use std::process::Command;

pub const MAX_DEVICE_DISPLAY_NAME_UTF16: usize = 128;

pub fn normalize_device_display_name(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.encode_utf16().count() > MAX_DEVICE_DISPLAY_NAME_UTF16 {
        return None;
    }
    Some(value.to_owned())
}

pub fn system_device_display_name() -> Option<String> {
    #[cfg(target_os = "macos")]
    if let Some(name) = command_name("/usr/sbin/scutil", &["--get", "ComputerName"]) {
        return Some(name);
    }

    #[cfg(target_os = "windows")]
    if let Ok(name) = std::env::var("COMPUTERNAME")
        && let Some(name) = normalize_device_display_name(&name)
    {
        return Some(name);
    }

    #[cfg(unix)]
    if let Some(name) = command_name("/bin/hostname", &[]) {
        return Some(name);
    }

    std::env::var("HOSTNAME")
        .ok()
        .and_then(|name| normalize_device_display_name(&name))
}

fn command_name(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    normalize_device_display_name(&stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_normalization_trims_and_uses_utf16_bound() {
        assert_eq!(
            normalize_device_display_name("  Qingxian MacBook Air  ").as_deref(),
            Some("Qingxian MacBook Air")
        );
        assert_eq!(normalize_device_display_name("   "), None);
        assert!(normalize_device_display_name(&"a".repeat(128)).is_some());
        assert!(normalize_device_display_name(&"a".repeat(129)).is_none());
        assert!(normalize_device_display_name(&"测".repeat(128)).is_some());
        assert!(normalize_device_display_name(&"😀".repeat(64)).is_some());
        assert!(normalize_device_display_name(&"😀".repeat(65)).is_none());
    }
}
