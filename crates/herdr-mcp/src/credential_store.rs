#[cfg(any(target_os = "linux", target_os = "windows", test))]
use sha2::{Digest, Sha256};

pub fn load(service: &str, account: &str) -> Result<String, String> {
    validate_key(service, account)?;
    #[cfg(target_os = "macos")]
    {
        crate::macos_credential_helper::load(service, account)
    }
    #[cfg(target_os = "linux")]
    {
        linux::load(service, account)
    }
    #[cfg(target_os = "windows")]
    {
        windows::load(service, account)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (service, account);
        Err("secure device credential storage is unsupported on this platform".to_owned())
    }
}

pub fn store(service: &str, account: &str, secret: &str) -> Result<(), String> {
    validate_key(service, account)?;
    validate_secret(secret)?;
    #[cfg(target_os = "macos")]
    {
        crate::macos_credential_helper::store(service, account, secret)
    }
    #[cfg(target_os = "linux")]
    {
        linux::store(service, account, secret)
    }
    #[cfg(target_os = "windows")]
    {
        windows::store(service, account, secret)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (service, account, secret);
        Err("secure device credential storage is unsupported on this platform".to_owned())
    }
}

pub fn delete(service: &str, account: &str) -> Result<(), String> {
    validate_key(service, account)?;
    #[cfg(target_os = "macos")]
    {
        crate::macos_credential_helper::delete(service, account)
    }
    #[cfg(target_os = "linux")]
    {
        linux::delete(service, account)
    }
    #[cfg(target_os = "windows")]
    {
        windows::delete(service, account)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (service, account);
        Err("secure device credential storage is unsupported on this platform".to_owned())
    }
}

fn validate_key(service: &str, account: &str) -> Result<(), String> {
    for (label, value) in [("service", service), ("account", account)] {
        if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
            return Err(format!("credential {label} is invalid"));
        }
    }
    Ok(())
}

fn validate_secret(secret: &str) -> Result<(), String> {
    if secret.is_empty() || secret.len() > 4096 || secret.chars().any(char::is_control) {
        return Err("credential secret is invalid".to_owned());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "windows", test))]
fn key_id(service: &str, account: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(service.as_bytes());
    hash.update([0]);
    hash.update(account.as_bytes());
    format!("{:x}", hash.finalize())
}

#[cfg(target_os = "windows")]
mod windows {
    use super::{key_id, validate_secret};
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
        CredReadW, CredWriteW,
    };

    struct CredentialGuard(*mut CREDENTIALW);

    impl Drop for CredentialGuard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CredFree(self.0.cast()) };
            }
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn target_name(service: &str, account: &str) -> Vec<u16> {
        wide(&format!("Herdr-MCP/{}", key_id(service, account)))
    }

    pub(super) fn load(service: &str, account: &str) -> Result<String, String> {
        let target = target_name(service, account);
        let mut raw = null_mut();
        let ok = unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut raw) };
        if ok == 0 {
            return Err(format!(
                "cannot load Windows device credential: {}",
                std::io::Error::last_os_error()
            ));
        }
        if raw.is_null() {
            return Err(
                "Windows Credential Manager returned an empty credential handle".to_owned(),
            );
        }
        let guard = CredentialGuard(raw);
        let credential = unsafe { &*guard.0 };
        let size = credential.CredentialBlobSize as usize;
        if size == 0 || size > 4096 || credential.CredentialBlob.is_null() {
            return Err("Windows device credential has an invalid size".to_owned());
        }
        let bytes = unsafe { std::slice::from_raw_parts(credential.CredentialBlob, size) };
        let secret = String::from_utf8(bytes.to_vec())
            .map_err(|_| "Windows device credential is not valid UTF-8".to_owned())?;
        validate_secret(&secret)?;
        Ok(secret)
    }

    pub(super) fn store(service: &str, account: &str, secret: &str) -> Result<(), String> {
        validate_secret(secret)?;
        let mut target = target_name(service, account);
        let mut username = wide(account);
        let mut blob = secret.as_bytes().to_vec();
        let blob_size = u32::try_from(blob.len())
            .map_err(|_| "Windows device credential is too large".to_owned())?;
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            CredentialBlobSize: blob_size,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            UserName: username.as_mut_ptr(),
            ..CREDENTIALW::default()
        };
        let ok = unsafe { CredWriteW(&credential, 0) };
        if ok == 0 {
            return Err(format!(
                "cannot store Windows device credential: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    }

    pub(super) fn delete(service: &str, account: &str) -> Result<(), String> {
        let target = target_name(service, account);
        let ok = unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) };
        if ok != 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
            return Ok(());
        }
        Err(format!("cannot delete Windows device credential: {error}"))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{key_id, validate_secret};
    use std::fs::{self, File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::{Path, PathBuf};

    fn paths(service: &str, account: &str) -> Result<(PathBuf, PathBuf), String> {
        let runtime = crate::paths::RuntimePaths::discover()?;
        let dir = runtime.config_dir.join("credentials");
        let path = dir.join(format!("{}.cred", key_id(service, account)));
        Ok((dir, path))
    }

    fn ensure_dir(dir: &Path) -> Result<(), String> {
        fs::create_dir_all(dir).map_err(|error| {
            format!(
                "cannot create Linux credential directory {}: {error}",
                dir.display()
            )
        })?;
        let meta = fs::symlink_metadata(dir).map_err(|error| {
            format!(
                "cannot inspect Linux credential directory {}: {error}",
                dir.display()
            )
        })?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(format!(
                "Linux credential directory {} must be a real directory",
                dir.display()
            ));
        }
        let mode = meta.permissions().mode() & 0o777;
        if mode != 0o700 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|error| {
                format!(
                    "cannot secure Linux credential directory {}: {error}",
                    dir.display()
                )
            })?;
        }
        Ok(())
    }

    pub(super) fn ensure_private_file_mode(path: &Path, meta: &fs::Metadata) -> Result<(), String> {
        if meta.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
                format!(
                    "cannot repair Linux device credential permissions on {}: {error}",
                    path.display()
                )
            })?;
        }
        Ok(())
    }

    pub(super) fn load(service: &str, account: &str) -> Result<String, String> {
        let (dir, path) = paths(service, account)?;
        ensure_dir(&dir)?;
        let meta = fs::symlink_metadata(&path).map_err(|error| {
            format!(
                "cannot inspect Linux device credential {}: {error}",
                path.display()
            )
        })?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err("Linux device credential must be a regular non-symlink file".to_owned());
        }
        ensure_private_file_mode(&path, &meta)?;
        if meta.len() == 0 || meta.len() > 4096 {
            return Err("Linux device credential has an invalid size".to_owned());
        }
        let file = File::open(&path).map_err(|error| {
            format!(
                "cannot open Linux device credential {}: {error}",
                path.display()
            )
        })?;
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        file.take(4097).read_to_end(&mut bytes).map_err(|error| {
            format!(
                "cannot read Linux device credential {}: {error}",
                path.display()
            )
        })?;
        let secret = String::from_utf8(bytes)
            .map_err(|_| "Linux device credential is not valid UTF-8".to_owned())?;
        validate_secret(&secret)?;
        Ok(secret)
    }

    pub(super) fn store(service: &str, account: &str, secret: &str) -> Result<(), String> {
        validate_secret(secret)?;
        let (dir, path) = paths(service, account)?;
        ensure_dir(&dir)?;
        if let Ok(meta) = fs::symlink_metadata(&path)
            && (meta.file_type().is_symlink() || !meta.is_file())
        {
            return Err("refusing to replace a non-regular Linux device credential".to_owned());
        }
        let temp = dir.join(format!(
            ".credential-{}-{}",
            std::process::id(),
            key_id(service, account)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        let mut file = options.open(&temp).map_err(|error| {
            format!(
                "cannot create temporary Linux device credential {}: {error}",
                temp.display()
            )
        })?;
        let result = file
            .write_all(secret.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("cannot persist Linux device credential: {error}"));
        if let Err(error) = result {
            let _ = fs::remove_file(&temp);
            return Err(error);
        }
        fs::rename(&temp, &path).map_err(|error| {
            let _ = fs::remove_file(&temp);
            format!(
                "cannot activate Linux device credential {}: {error}",
                path.display()
            )
        })?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            format!(
                "cannot secure Linux device credential {}: {error}",
                path.display()
            )
        })?;
        Ok(())
    }

    pub(super) fn delete(service: &str, account: &str) -> Result<(), String> {
        let (dir, path) = paths(service, account)?;
        ensure_dir(&dir)?;
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
                Err("refusing to delete a non-regular Linux device credential".to_owned())
            }
            Ok(_) => fs::remove_file(&path).map_err(|error| {
                format!(
                    "cannot delete Linux device credential {}: {error}",
                    path.display()
                )
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "cannot inspect Linux device credential {}: {error}",
                path.display()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_id_is_stable_and_separates_fields() {
        assert_eq!(key_id("service", "account"), key_id("service", "account"));
        assert_ne!(key_id("service-a", "b"), key_id("service", "a-b"));
    }

    #[test]
    fn rejects_control_characters_and_empty_secrets() {
        assert!(validate_key("service", "account").is_ok());
        assert!(validate_key("service\n", "account").is_err());
        assert!(validate_secret("").is_err());
        assert!(validate_secret("secret").is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_credential_permissions_are_repaired_to_private_mode() {
        use std::os::unix::fs::PermissionsExt;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "herdr-credential-mode-test-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("device.cred");
        std::fs::write(&path, b"secret").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();

        linux::ensure_private_file_mode(&path, &meta).unwrap();

        let mode = std::fs::symlink_metadata(&path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
