use crate::instance::InstanceId;
use semver::Version;
use std::fs;
use std::path::Path;
use url::Url;

pub const DEFAULT_RUNTIME_PORT: u16 = 8772;
pub const DEFAULT_DEV_PORT: u16 = 8872;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum UpdateChannel {
    Stable,
    Preview,
}

impl UpdateChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Preview => "preview",
        }
    }

    /// Stable discovers non-prerelease tags only. Preview discovers prerelease
    /// and stable tags, then selects the highest semver.
    pub fn accepts_version(self, version: &Version) -> bool {
        match self {
            Self::Stable => version.pre.is_empty(),
            Self::Preview => true,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Config {
    pub runtime_port: u16,
    pub dev_port: u16,
    pub update_channel: UpdateChannel,
    pub update_check: bool,
    pub edge_public_origin: Option<String>,
    pub edge_device_id: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            runtime_port: DEFAULT_RUNTIME_PORT,
            dev_port: DEFAULT_DEV_PORT,
            update_channel: UpdateChannel::Stable,
            update_check: true,
            edge_public_origin: None,
            edge_device_id: None,
        }
    }
}

impl Config {
    /// Defaults used when config.toml is absent. Alpha/prerelease binaries keep
    /// dogfood on `preview` so discovery still sees current GitHub alphas.
    #[allow(dead_code)]
    pub fn missing_file_default() -> Self {
        Self::missing_file_default_for_instance(&InstanceId::default_instance())
    }

    pub fn missing_file_default_for_instance(instance: &InstanceId) -> Self {
        let mut config = Self {
            runtime_port: instance.default_port(),
            ..Self::default()
        };
        if binary_is_prerelease() {
            config.update_channel = UpdateChannel::Preview;
        }
        config
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        Self::load_for_instance(path, &InstanceId::default_instance())
    }

    pub fn load_for_instance(path: &Path, instance: &InstanceId) -> Result<Self, String> {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::missing_file_default_for_instance(instance));
            }
            Err(error) => return Err(format!("cannot read config {}: {error}", path.display())),
        };
        parse(&content).map_err(|error| format!("invalid config {}: {error}", path.display()))
    }

    pub fn render(&self) -> String {
        let mut rendered = format!(
            "[runtime]\nport = {}\n\n[dev]\nport = {}\n\n[update]\nchannel = \"{}\"\ncheck = {}\n",
            self.runtime_port,
            self.dev_port,
            self.update_channel.as_str(),
            self.update_check
        );
        if self.edge_public_origin.is_some() || self.edge_device_id.is_some() {
            rendered.push_str("\n[edge]\n");
            if let Some(origin) = &self.edge_public_origin {
                rendered.push_str(&format!("public_origin = \"{origin}\"\n"));
            }
            if let Some(device_id) = &self.edge_device_id {
                rendered.push_str(&format!("device_id = \"{device_id}\"\n"));
            }
        }
        rendered
    }

    #[cfg(any(target_os = "macos", test))]
    pub fn set_edge_public_origin(&mut self, origin: &str) -> Result<(), String> {
        self.edge_public_origin = Some(normalize_edge_public_origin(origin)?);
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", test)))]
    pub fn set_edge_public_origin(&mut self, _origin: &str) -> Result<(), String> {
        Err("edge configuration requires macOS".to_owned())
    }

    #[cfg(any(target_os = "macos", test))]
    pub fn set_edge_device_id(&mut self, device_id: &str) -> Result<(), String> {
        self.edge_device_id = Some(normalize_device_id(device_id)?);
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", test)))]
    pub fn set_edge_device_id(&mut self, _device_id: &str) -> Result<(), String> {
        Err("edge configuration requires macOS".to_owned())
    }

    #[cfg(any(target_os = "macos", test))]
    pub fn edge_link_keychain_service(&self) -> Option<String> {
        self.edge_device_id
            .as_ref()
            .map(|device_id| format!("herdr-edge-link-{device_id}"))
    }

    #[cfg(not(any(target_os = "macos", test)))]
    pub fn edge_link_keychain_service(&self) -> Option<String> {
        None
    }

    pub fn edge_ws_url(&self) -> Result<Option<String>, String> {
        let Some(origin) = &self.edge_public_origin else {
            return Ok(None);
        };
        let mut url =
            Url::parse(origin).map_err(|error| format!("invalid edge public origin: {error}"))?;
        url.set_scheme("wss")
            .map_err(|_| "edge public origin must use https://".to_owned())?;
        url.set_path("/ws");
        url.set_query(None);
        url.set_fragment(None);
        Ok(Some(url.to_string()))
    }
}

fn parse(content: &str) -> Result<Config, String> {
    let mut config = Config::default();
    let mut section = "";

    for (index, raw_line) in content.lines().enumerate() {
        let line_number = index + 1;
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim();
            match section {
                "runtime" | "dev" | "update" | "edge" => continue,
                _ => return Err(format!("line {line_number}: unknown section [{section}]")),
            }
        }

        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("line {line_number}: expected key = value"))?;
        let key = key.trim();
        let value = value.trim();

        match (section, key) {
            ("runtime", "port") => config.runtime_port = parse_port(value, line_number)?,
            ("dev", "port") => config.dev_port = parse_port(value, line_number)?,
            ("update", "channel") => {
                config.update_channel = match unquote(value) {
                    "stable" => UpdateChannel::Stable,
                    "preview" => UpdateChannel::Preview,
                    other => {
                        return Err(format!(
                            "line {line_number}: update.channel must be stable or preview, got '{other}'"
                        ));
                    }
                }
            }
            ("update", "check") => {
                config.update_check = match value {
                    "true" => true,
                    "false" => false,
                    _ => {
                        return Err(format!(
                            "line {line_number}: update.check must be true or false"
                        ));
                    }
                }
            }
            ("edge", "public_origin") => {
                config.edge_public_origin = Some(normalize_edge_public_origin(unquote(value))?)
            }
            ("edge", "device_id") => {
                config.edge_device_id = Some(normalize_device_id(unquote(value))?)
            }
            ("", _) => return Err(format!("line {line_number}: keys must be inside a section")),
            _ => return Err(format!("line {line_number}: unknown key {section}.{key}")),
        }
    }

    Ok(config)
}

fn normalize_edge_public_origin(value: &str) -> Result<String, String> {
    let mut url =
        Url::parse(value).map_err(|error| format!("edge.public_origin is invalid: {error}"))?;
    if url.scheme() != "https" {
        return Err("edge.public_origin must use https://".to_owned());
    }
    if url.host_str().is_none() {
        return Err("edge.public_origin must include a host".to_owned());
    }
    if url.username() != "" || url.password().is_some() {
        return Err("edge.public_origin must not include credentials".to_owned());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("edge.public_origin must not include query or fragment".to_owned());
    }
    if url.path() != "/" && !url.path().is_empty() {
        return Err("edge.public_origin must not include a path".to_owned());
    }
    url.set_path("");
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

pub fn normalize_device_id(value: &str) -> Result<String, String> {
    let value = value.trim();
    let suffix = value
        .strip_prefix("dev_")
        .or_else(|| value.strip_prefix("DEV_"))
        .ok_or_else(|| "edge.device_id must start with dev_".to_owned())?
        .to_ascii_uppercase();
    if suffix.len() != 26 {
        return Err("edge.device_id must contain one canonical 26-character ULID".to_owned());
    }
    let first = suffix
        .chars()
        .next()
        .ok_or_else(|| "edge.device_id is empty".to_owned())?;
    if !(('0'..='7').contains(&first)) {
        return Err("edge.device_id ULID must begin with 0-7".to_owned());
    }
    const CROCKFORD: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    if !suffix.chars().all(|ch| CROCKFORD.contains(ch)) {
        return Err("edge.device_id contains invalid ULID characters".to_owned());
    }
    Ok(format!("dev_{suffix}"))
}

fn parse_port(value: &str, line_number: usize) -> Result<u16, String> {
    let port = value
        .parse::<u16>()
        .map_err(|_| format!("line {line_number}: port must be an integer from 1 to 65535"))?;
    if port == 0 {
        return Err(format!(
            "line {line_number}: port must be greater than zero"
        ));
    }
    Ok(port)
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
}

fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    for (index, byte) in line.as_bytes().iter().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b'#' if !quoted => return &line[..index],
            _ => {}
        }
    }
    line
}

fn binary_is_prerelease() -> bool {
    Version::parse(env!("CARGO_PKG_VERSION"))
        .map(|version| !version.pre.is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_product_defaults() {
        assert_eq!(Config::default().runtime_port, 8772);
        assert_eq!(Config::default().dev_port, 8872);
        assert_eq!(Config::default().update_channel, UpdateChannel::Stable);
        assert!(Config::default().update_check);
        assert_eq!(Config::default().edge_public_origin, None);
        assert_eq!(Config::default().edge_device_id, None);
    }

    #[test]
    fn channel_accepts_versions_per_policy() {
        let stable = Version::parse("1.0.0").unwrap();
        let alpha = Version::parse("1.0.0-alpha.1").unwrap();
        assert!(UpdateChannel::Stable.accepts_version(&stable));
        assert!(!UpdateChannel::Stable.accepts_version(&alpha));
        assert!(UpdateChannel::Preview.accepts_version(&stable));
        assert!(UpdateChannel::Preview.accepts_version(&alpha));
    }

    #[test]
    fn missing_file_default_follows_binary_prerelease() {
        let missing = Config::missing_file_default();
        if binary_is_prerelease() {
            assert_eq!(missing.update_channel, UpdateChannel::Preview);
        } else {
            assert_eq!(missing.update_channel, UpdateChannel::Stable);
        }
    }

    #[test]
    fn parses_supported_config() {
        let config = parse(
            r#"
            [runtime]
            port = 9000

            [dev]
            port = 9001

            [update]
            channel = "preview"
            check = false

            [edge]
            public_origin = "https://herdr.example.com"
            device_id = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV"
            "#,
        )
        .unwrap();

        assert_eq!(config.runtime_port, 9000);
        assert_eq!(config.dev_port, 9001);
        assert_eq!(config.update_channel, UpdateChannel::Preview);
        assert!(!config.update_check);
        assert_eq!(
            config.edge_public_origin.as_deref(),
            Some("https://herdr.example.com")
        );
        assert_eq!(
            config.edge_device_id.as_deref(),
            Some("dev_01ARZ3NDEKTSV4RRFFQ69G5FAV")
        );
        assert_eq!(
            config.edge_link_keychain_service().as_deref(),
            Some("herdr-edge-link-dev_01ARZ3NDEKTSV4RRFFQ69G5FAV")
        );
        assert_eq!(
            config.edge_ws_url().unwrap().as_deref(),
            Some("wss://herdr.example.com/ws")
        );
    }

    #[test]
    fn rejects_unknown_or_invalid_config() {
        assert!(parse("port = 1").is_err());
        assert!(parse("[runtime]\nport = 0").is_err());
        assert!(parse("[update]\nchannel = \"nightly\"").is_err());
        assert!(parse("[edge]\npublic_origin = \"http://example.com\"").is_err());
        assert!(parse("[edge]\ndevice_id = \"dev_bad\"").is_err());
        assert!(parse("[unknown]\nvalue = 1").is_err());
    }

    #[test]
    fn rendered_config_round_trips() {
        let config = Config {
            runtime_port: 9000,
            dev_port: 9001,
            update_channel: UpdateChannel::Preview,
            update_check: false,
            edge_public_origin: Some("https://herdr.example.com".to_owned()),
            edge_device_id: Some("dev_01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()),
        };
        assert_eq!(parse(&config.render()).unwrap(), config);
    }

    #[test]
    fn normalizes_device_id_case_without_accepting_ambiguous_ulid_chars() {
        assert_eq!(
            normalize_device_id("dev_01arz3ndektsv4rrffq69g5fav").unwrap(),
            "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV"
        );
        assert!(normalize_device_id("dev_01ARZ3NDEKTSV4RRFFQ69G5FAI").is_err());
        assert!(normalize_device_id("dev_81ARZ3NDEKTSV4RRFFQ69G5FAV").is_err());
    }
}
