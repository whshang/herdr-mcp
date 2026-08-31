#[cfg(target_os = "macos")]
use security_framework::passwords::{
    PasswordOptions, delete_generic_password, generic_password, set_generic_password,
};

#[cfg(any(target_os = "macos", test))]
pub fn store_generic_secret(service: &str, account: &str, secret: &str) -> Result<(), String> {
    validate_label(service, "service")?;
    validate_label(account, "account")?;
    if secret.is_empty() || secret.len() > 4096 {
        return Err("Keychain secret length is invalid".to_owned());
    }

    #[cfg(target_os = "macos")]
    {
        set_generic_password(service, account, secret.as_bytes()).map_err(|error| {
            format!("cannot store workstation credential in macOS Keychain: {error}")
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (service, account, secret);
        Err("workstation credential persistence requires macOS Keychain".to_owned())
    }
}

#[cfg(any(target_os = "macos", test))]
pub fn load_generic_secret(service: &str, account: &str) -> Result<String, String> {
    validate_label(service, "service")?;
    validate_label(account, "account")?;

    #[cfg(target_os = "macos")]
    {
        let bytes = generic_password(PasswordOptions::new_generic_password(service, account))
            .map_err(|error| {
                format!("cannot load workstation credential from macOS Keychain: {error}")
            })?;
        let secret = String::from_utf8(bytes)
            .map_err(|_| "workstation credential in macOS Keychain is not UTF-8".to_owned())?;
        if secret.is_empty() || secret.len() > 4096 {
            return Err("workstation credential in macOS Keychain has invalid length".to_owned());
        }
        Ok(secret)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (service, account);
        Err("workstation credential loading requires macOS Keychain".to_owned())
    }
}

#[cfg(any(target_os = "macos", test))]
fn validate_label(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 255 || value.chars().any(char::is_control) {
        return Err(format!("Keychain {label} is invalid"));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
pub fn delete_generic_secret(service: &str, account: &str) -> Result<(), String> {
    validate_label(service, "service")?;
    validate_label(account, "account")?;
    #[cfg(target_os = "macos")]
    {
        delete_generic_password(service, account).map_err(|error| {
            format!("cannot delete workstation credential from macOS Keychain: {error}")
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (service, account);
        Err("workstation credential deletion requires macOS Keychain".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_labels_before_platform_access() {
        assert!(store_generic_secret("", "user", "secret").is_err());
        assert!(load_generic_secret("service", "bad\naccount").is_err());
    }
}
