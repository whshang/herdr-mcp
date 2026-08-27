//! Named instance isolation for same-uid UAT alongside production dogfood.
//!
//! Default (unset / empty `HERDR_MCP_INSTANCE`) keeps production identities:
//! LaunchAgent `dev.herdr-mcp.server`, port `8772`, config `~/.config/herdr-mcp`.
//! A named instance suffixes labels, picks a stable non-8772 port, and uses an
//! isolated config root. It never owns `~/.local/bin/herdr-mcp`.

use std::env;

/// Production LaunchAgent label (default instance).
pub const DEFAULT_SERVICE_LABEL: &str = "dev.herdr-mcp.server";
/// Legacy Node watchdog label (default instance only; adoption path).
pub const DEFAULT_WATCHDOG_LABEL: &str = "dev.herdr-mcp.watchdog";
/// Rust-era health sidecar label (default instance).
pub const DEFAULT_HEALTH_WATCHDOG_LABEL: &str = "dev.herdr-mcp.health-watchdog";
pub const DEFAULT_RUNTIME_PORT: u16 = 8772;
pub const DEFAULT_CONFIG_LEAF: &str = "herdr-mcp";

const RESERVED_NAMES: &[&str] = &[
    "server",
    "watchdog",
    "health-watchdog",
    "health_watchdog",
    "default",
    "prod",
    "production",
    "current",
    "dev",
    "link",
    "link-prod",
    "link-rust-candidate",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceId {
    /// `None` = production default instance.
    name: Option<String>,
}

impl InstanceId {
    pub fn default_instance() -> Self {
        Self { name: None }
    }

    pub fn discover() -> Result<Self, String> {
        match env::var("HERDR_MCP_INSTANCE") {
            Ok(value) if !value.trim().is_empty() => Self::parse(value.trim()),
            Ok(_) | Err(env::VarError::NotPresent) => Ok(Self::default_instance()),
            Err(error) => Err(format!("invalid HERDR_MCP_INSTANCE: {error}")),
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        let name = raw.trim();
        if name.is_empty() {
            return Ok(Self::default_instance());
        }
        if name.len() > 24 {
            return Err(
                "instance name must be 1..=24 characters (lowercase letter, digit, hyphen)"
                    .to_owned(),
            );
        }
        let mut chars = name.chars();
        let Some(first) = chars.next() else {
            return Ok(Self::default_instance());
        };
        if !first.is_ascii_lowercase() {
            return Err("instance name must start with a lowercase ASCII letter".to_owned());
        }
        if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err(
                "instance name may only contain lowercase ASCII letters, digits, and hyphens"
                    .to_owned(),
            );
        }
        if name.contains("--") || name.ends_with('-') {
            return Err("instance name must not end with '-' or contain '--'".to_owned());
        }
        if RESERVED_NAMES.contains(&name) {
            return Err(format!("instance name '{name}' is reserved"));
        }
        Ok(Self {
            name: Some(name.to_owned()),
        })
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    #[allow(dead_code)]
    pub fn is_default(&self) -> bool {
        self.name.is_none()
    }

    pub fn is_named(&self) -> bool {
        self.name.is_some()
    }

    pub fn config_leaf(&self) -> String {
        match &self.name {
            None => DEFAULT_CONFIG_LEAF.to_owned(),
            Some(name) => format!("{DEFAULT_CONFIG_LEAF}-{name}"),
        }
    }

    pub fn service_label(&self) -> String {
        match &self.name {
            None => DEFAULT_SERVICE_LABEL.to_owned(),
            Some(name) => format!("dev.herdr-mcp.{name}.server"),
        }
    }

    pub fn watchdog_label(&self) -> String {
        match &self.name {
            None => DEFAULT_WATCHDOG_LABEL.to_owned(),
            Some(name) => format!("dev.herdr-mcp.{name}.watchdog"),
        }
    }

    pub fn health_watchdog_label(&self) -> String {
        match &self.name {
            None => DEFAULT_HEALTH_WATCHDOG_LABEL.to_owned(),
            Some(name) => format!("dev.herdr-mcp.{name}.health-watchdog"),
        }
    }

    /// Default loopback port for this instance.
    /// Named instances use a stable port in `8800..=8999` so they never take `8772`.
    pub fn default_port(&self) -> u16 {
        match env::var("HERDR_MCP_PORT") {
            Ok(value) if !value.is_empty() => {
                if let Ok(port) = value.parse::<u16>()
                    && port > 0
                {
                    return port;
                }
            }
            _ => {}
        }
        match &self.name {
            None => DEFAULT_RUNTIME_PORT,
            Some(name) => named_instance_port(name),
        }
    }
}

fn named_instance_port(name: &str) -> u16 {
    let mut hash: u32 = 2166136261;
    for byte in name.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16777619);
    }
    8800 + (hash % 200) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_instance_keeps_production_identities() {
        let id = InstanceId::default_instance();
        assert!(id.is_default());
        assert_eq!(id.service_label(), DEFAULT_SERVICE_LABEL);
        assert_eq!(id.watchdog_label(), DEFAULT_WATCHDOG_LABEL);
        assert_eq!(id.health_watchdog_label(), DEFAULT_HEALTH_WATCHDOG_LABEL);
        assert_eq!(id.config_leaf(), DEFAULT_CONFIG_LEAF);
        assert_eq!(
            named_instance_port("never-default"),
            named_instance_port("never-default")
        );
        // Hash path for named instances never lands on production 8772.
        assert_ne!(named_instance_port("uat"), DEFAULT_RUNTIME_PORT);
    }

    #[test]
    fn named_instance_suffixes_labels_and_avoids_8772() {
        let id = InstanceId::parse("uat").unwrap();
        assert_eq!(id.service_label(), "dev.herdr-mcp.uat.server");
        assert_eq!(id.watchdog_label(), "dev.herdr-mcp.uat.watchdog");
        assert_eq!(
            id.health_watchdog_label(),
            "dev.herdr-mcp.uat.health-watchdog"
        );
        assert_eq!(id.config_leaf(), "herdr-mcp-uat");
        let port = named_instance_port("uat");
        assert_ne!(port, 8772);
        assert!((8800..=8999).contains(&port));
    }

    #[test]
    fn two_named_instances_do_not_share_labels() {
        let a = InstanceId::parse("uat").unwrap();
        let b = InstanceId::parse("clean").unwrap();
        assert_ne!(a.service_label(), b.service_label());
        assert_ne!(a.config_leaf(), b.config_leaf());
    }

    #[test]
    fn rejects_reserved_and_invalid_names() {
        assert!(InstanceId::parse("server").is_err());
        assert!(InstanceId::parse("Default").is_err());
        assert!(InstanceId::parse("1uat").is_err());
        assert!(InstanceId::parse("u_at").is_err());
        assert!(InstanceId::parse("uat-").is_err());
        assert!(InstanceId::parse("u--at").is_err());
    }
}
