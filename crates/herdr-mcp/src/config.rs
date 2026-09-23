use crate::instance::InstanceId;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use url::Url;

pub const DEFAULT_RUNTIME_PORT: u16 = 8772;
pub const DEFAULT_DEV_PORT: u16 = 8872;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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
    pub edge_link_upstream_origin: Option<String>,
    pub edge_device_id: Option<String>,
    pub semantic: SemanticConfig,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SemanticConfig {
    pub routes: Vec<SemanticRouteConfig>,
}

#[derive(Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRouteConfig {
    pub name: String,
    pub protocol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct JsonConfigInput {
    runtime: JsonRuntimeInput,
    dev: JsonDevInput,
    update: JsonUpdateInput,
    edge: JsonEdgeInput,
    semantic: SemanticConfig,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct JsonRuntimeInput {
    port: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct JsonDevInput {
    port: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct JsonUpdateInput {
    channel: Option<UpdateChannel>,
    check: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct JsonEdgeInput {
    public_origin: Option<String>,
    link_upstream_origin: Option<String>,
    device_id: Option<String>,
}

#[derive(Serialize)]
struct JsonConfigOutput<'a> {
    runtime: JsonRuntimeOutput,
    dev: JsonDevOutput,
    update: JsonUpdateOutput,
    edge: JsonEdgeOutput<'a>,
    semantic: &'a SemanticConfig,
}

#[derive(Serialize)]
struct JsonRuntimeOutput {
    port: u16,
}

#[derive(Serialize)]
struct JsonDevOutput {
    port: u16,
}

#[derive(Serialize)]
struct JsonUpdateOutput {
    channel: UpdateChannel,
    check: bool,
}

#[derive(Serialize)]
struct JsonEdgeOutput<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    public_origin: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    link_upstream_origin: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_id: Option<&'a str>,
}

impl std::fmt::Debug for SemanticRouteConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SemanticRouteConfig")
            .field("name", &self.name)
            .field("protocol", &self.protocol)
            .field("url", &self.url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            runtime_port: DEFAULT_RUNTIME_PORT,
            dev_port: DEFAULT_DEV_PORT,
            update_channel: UpdateChannel::Stable,
            update_check: true,
            edge_public_origin: None,
            edge_link_upstream_origin: None,
            edge_device_id: None,
            semantic: SemanticConfig::default(),
        }
    }
}

impl Config {
    /// Defaults used when config.json is absent. Alpha/prerelease binaries keep
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
                let legacy_path = path.with_file_name("config.toml");
                let legacy = match fs::read_to_string(&legacy_path) {
                    Ok(content) => Some(content),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => {
                        return Err(format!(
                            "cannot read legacy config {}: {error}",
                            legacy_path.display()
                        ));
                    }
                };
                let Some(legacy) = legacy else {
                    return Ok(Self::missing_file_default_for_instance(instance));
                };
                let config = parse_legacy_toml(&legacy, instance).map_err(|error| {
                    format!("invalid legacy config {}: {error}", legacy_path.display())
                })?;
                write_json_config(path, &config)?;
                let backup = path.with_file_name("config.toml.migrated");
                if !backup.exists() {
                    let _ = fs::rename(&legacy_path, &backup);
                }
                return Ok(config);
            }
            Err(error) => return Err(format!("cannot read config {}: {error}", path.display())),
        };
        parse_json(&content, instance)
            .map_err(|error| format!("invalid config {}: {error}", path.display()))
    }

    pub fn render(&self) -> String {
        let output = JsonConfigOutput {
            runtime: JsonRuntimeOutput {
                port: self.runtime_port,
            },
            dev: JsonDevOutput {
                port: self.dev_port,
            },
            update: JsonUpdateOutput {
                channel: self.update_channel,
                check: self.update_check,
            },
            edge: JsonEdgeOutput {
                public_origin: self.edge_public_origin.as_deref(),
                link_upstream_origin: self.edge_link_upstream_origin.as_deref(),
                device_id: self.edge_device_id.as_deref(),
            },
            semantic: &self.semantic,
        };
        let mut rendered = serde_json::to_string_pretty(&output)
            .expect("serializing herdr-mcp config cannot fail");
        rendered.push('\n');
        rendered
    }

    pub fn render_redacted(&self) -> String {
        let mut redacted = self.clone();
        for route in &mut redacted.semantic.routes {
            if route.api_key.is_some() {
                route.api_key = Some("[REDACTED]".to_owned());
            }
        }
        redacted.render()
    }

    pub(crate) fn save(&self, path: &Path) -> Result<(), String> {
        validate_config(self)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "cannot create config directory {}: {error}",
                    parent.display()
                )
            })?;
        }
        fs::write(path, self.render())
            .map_err(|error| format!("cannot write config {}: {error}", path.display()))?;
        #[cfg(unix)]
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("cannot secure config {}: {error}", path.display()))?;
        Ok(())
    }

    pub fn set_edge_public_origin(&mut self, origin: &str) -> Result<(), String> {
        self.edge_public_origin = Some(normalize_edge_public_origin(origin)?);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn set_edge_link_upstream_origin(&mut self, origin: &str) -> Result<(), String> {
        self.edge_link_upstream_origin = Some(normalize_edge_origin_field(
            origin,
            "edge.link_upstream_origin",
        )?);
        Ok(())
    }

    pub fn set_edge_device_id(&mut self, device_id: &str) -> Result<(), String> {
        self.edge_device_id = Some(normalize_device_id(device_id)?);
        Ok(())
    }

    pub fn edge_link_keychain_service(&self) -> Option<String> {
        self.edge_device_id
            .as_ref()
            .map(|device_id| format!("herdr-edge-link-{device_id}"))
    }

    /// The effective origin used by the Link transport (prefers explicit
    /// `link_upstream_origin`, falling back to `public_origin`).
    pub fn link_upstream_origin(&self) -> Option<&str> {
        self.edge_link_upstream_origin
            .as_deref()
            .or(self.edge_public_origin.as_deref())
    }

    pub fn edge_ws_url(&self) -> Result<Option<String>, String> {
        let Some(origin) = self.link_upstream_origin() else {
            return Ok(None);
        };
        let mut url =
            Url::parse(origin).map_err(|error| format!("invalid edge upstream origin: {error}"))?;
        url.set_scheme("wss")
            .map_err(|_| "edge upstream origin must use https://".to_owned())?;
        url.set_path("/ws");
        url.set_query(None);
        url.set_fragment(None);
        Ok(Some(url.to_string()))
    }
}

fn parse_json(content: &str, instance: &InstanceId) -> Result<Config, String> {
    let input: JsonConfigInput =
        serde_json::from_str(content).map_err(|error| format!("invalid JSON: {error}"))?;
    let mut config = Config::missing_file_default_for_instance(instance);
    if let Some(port) = input.runtime.port {
        if port == 0 {
            return Err("runtime.port must be greater than zero".to_owned());
        }
        config.runtime_port = port;
    }
    if let Some(port) = input.dev.port {
        if port == 0 {
            return Err("dev.port must be greater than zero".to_owned());
        }
        config.dev_port = port;
    }
    if let Some(channel) = input.update.channel {
        config.update_channel = channel;
    }
    if let Some(check) = input.update.check {
        config.update_check = check;
    }
    if let Some(origin) = input.edge.public_origin {
        config.edge_public_origin = Some(normalize_edge_public_origin(&origin)?);
    }
    if let Some(origin) = input.edge.link_upstream_origin {
        config.edge_link_upstream_origin = Some(normalize_edge_origin_field(
            &origin,
            "edge.link_upstream_origin",
        )?);
    }
    if let Some(device_id) = input.edge.device_id {
        config.edge_device_id = Some(normalize_device_id(&device_id)?);
    }
    config.semantic = input.semantic;
    validate_config(&config)?;
    Ok(config)
}

fn write_json_config(path: &Path, config: &Config) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "cannot create config directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, config.render())
        .map_err(|error| format!("cannot write config {}: {error}", temp.display()))?;
    #[cfg(unix)]
    fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot secure config {}: {error}", temp.display()))?;
    fs::rename(&temp, path)
        .map_err(|error| format!("cannot activate config {}: {error}", path.display()))?;
    Ok(())
}

fn parse_legacy_toml(content: &str, instance: &InstanceId) -> Result<Config, String> {
    let mut config = Config::missing_file_default_for_instance(instance);
    let mut section = "";

    for (index, raw_line) in content.lines().enumerate() {
        let line_number = index + 1;
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("[[") && line.ends_with("]]") {
            let section = line[2..line.len() - 2].trim();
            return Err(format!(
                "line {line_number}: unknown array section [[{section}]]"
            ));
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
            ("edge", "link_upstream_origin") => {
                config.edge_link_upstream_origin = Some(normalize_edge_origin_field(
                    unquote(value),
                    "edge.link_upstream_origin",
                )?)
            }
            ("edge", "device_id") => {
                config.edge_device_id = Some(normalize_device_id(unquote(value))?)
            }
            ("", _) => return Err(format!("line {line_number}: keys must be inside a section")),
            _ => return Err(format!("line {line_number}: unknown key {section}.{key}")),
        }
    }

    validate_config(&config)?;
    Ok(config)
}

fn validate_config(config: &Config) -> Result<(), String> {
    let mut route_names = std::collections::BTreeSet::new();
    for route in &config.semantic.routes {
        validate_semantic_route_name(&route.name, 0)?;
        if !route_names.insert(route.name.as_str()) {
            return Err(format!("duplicate semantic route '{}'", route.name));
        }
        if !matches!(
            route.protocol.as_str(),
            "decision" | "decision-vercel" | "openai-chat"
        ) || route.url.is_none()
            || route.model.is_none()
            || route.api_key.is_none()
        {
            return Err(format!(
                "semantic.route.{} must define valid protocol, url, model, and api_key",
                route.name
            ));
        }
    }
    Ok(())
}

fn validate_semantic_route_name(value: &str, line_number: usize) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(if line_number == 0 {
            "semantic route name must use only letters, digits, '_' or '-'".to_owned()
        } else {
            format!(
                "line {line_number}: semantic route name must use only letters, digits, '_' or '-'"
            )
        });
    }
    Ok(())
}

fn normalize_edge_origin_field(value: &str, field_name: &str) -> Result<String, String> {
    let mut url = Url::parse(value).map_err(|error| format!("{field_name} is invalid: {error}"))?;
    if url.scheme() != "https" {
        return Err(format!("{field_name} must use https://"));
    }
    if url.host_str().is_none() {
        return Err(format!("{field_name} must include a host"));
    }
    if url.username() != "" || url.password().is_some() {
        return Err(format!("{field_name} must not include credentials"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(format!("{field_name} must not include query or fragment"));
    }
    if url.path() != "/" && !url.path().is_empty() {
        return Err(format!("{field_name} must not include a path"));
    }
    url.set_path("");
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

fn normalize_edge_public_origin(value: &str) -> Result<String, String> {
    normalize_edge_origin_field(value, "edge.public_origin")
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

    fn parse(content: &str) -> Result<Config, String> {
        parse_legacy_toml(content, &InstanceId::default_instance())
    }

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
    fn parses_supported_legacy_config() {
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
        assert_eq!(config.edge_link_upstream_origin, None);
        assert_eq!(
            config.link_upstream_origin(),
            Some("https://herdr.example.com")
        );
        assert_eq!(
            config.edge_device_id.as_deref(),
            Some("dev_01ARZ3NDEKTSV4RRFFQ69G5FAV")
        );
        assert!(config.semantic.routes.is_empty());
    }

    #[test]
    fn parses_distinct_link_upstream_origin() {
        let config = parse(
            r#"
            [edge]
            public_origin = "https://custom.example.com"
            link_upstream_origin = "https://backend.workers.dev"
            device_id = "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV"
            "#,
        )
        .unwrap();

        assert_eq!(
            config.edge_public_origin.as_deref(),
            Some("https://custom.example.com")
        );
        assert_eq!(
            config.edge_link_upstream_origin.as_deref(),
            Some("https://backend.workers.dev")
        );
        // Link upstream origin overrides public origin for Link socket target
        assert_eq!(
            config.link_upstream_origin(),
            Some("https://backend.workers.dev")
        );
        assert_eq!(
            config.edge_ws_url().unwrap().as_deref(),
            Some("wss://backend.workers.dev/ws")
        );
    }

    #[test]
    fn rejects_unknown_or_invalid_legacy_config() {
        assert!(parse("port = 1").is_err());
        assert!(parse("[runtime]\nport = 0").is_err());
        assert!(parse("[update]\nchannel = \"nightly\"").is_err());
        let public_err = parse("[edge]\npublic_origin = \"http://example.com\"").unwrap_err();
        assert!(public_err.contains("edge.public_origin must use https://"));
        let upstream_err =
            parse("[edge]\nlink_upstream_origin = \"http://example.com\"").unwrap_err();
        assert!(upstream_err.contains("edge.link_upstream_origin must use https://"));
        assert!(parse("[edge]\ndevice_id = \"dev_bad\"").is_err());
        assert!(parse("[unknown]\nvalue = 1").is_err());
        assert!(parse("[[semantic.route]]\nname = \"fast\"").is_err());
    }

    #[test]
    fn rendered_config_round_trips() {
        let config = Config {
            runtime_port: 9000,
            dev_port: 9001,
            update_channel: UpdateChannel::Preview,
            update_check: false,
            edge_public_origin: Some("https://herdr.example.com".to_owned()),
            edge_link_upstream_origin: Some("https://backend.workers.dev".to_owned()),
            edge_device_id: Some("dev_01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()),
            semantic: SemanticConfig {
                routes: vec![SemanticRouteConfig {
                    name: "fast_a".to_owned(),
                    protocol: "decision".to_owned(),
                    url: Some("https://api.typesafe.ai/v1/systemone".to_owned()),
                    model: Some("jev-latest".to_owned()),
                    api_key: Some("test-key".to_owned()),
                }],
            },
        };
        assert_eq!(
            parse_json(&config.render(), &InstanceId::default_instance()).unwrap(),
            config
        );
    }

    #[test]
    fn semantic_secret_is_redacted_from_human_output_and_debug() {
        let config = Config {
            semantic: SemanticConfig {
                routes: vec![SemanticRouteConfig {
                    name: "fast_a".to_owned(),
                    protocol: "decision".to_owned(),
                    url: Some("https://api.typesafe.ai/v1/systemone".to_owned()),
                    model: Some("jev-latest".to_owned()),
                    api_key: Some("super-secret-key".to_owned()),
                }],
            },
            ..Config::default()
        };

        let rendered = config.render_redacted();
        assert!(rendered.contains("\"api_key\": \"[REDACTED]\""));
        assert!(!rendered.contains("super-secret-key"));

        let debug = format!("{config:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("super-secret-key"));
    }

    #[test]
    fn json_rejects_unknown_fields_and_invalid_route_protocols() {
        assert!(
            parse_json(
                r#"{"runtime":{"port":8772,"unknown":true}}"#,
                &InstanceId::default_instance()
            )
            .is_err()
        );
        assert!(parse_json(
            r#"{"semantic":{"routes":[{"name":"fast","protocol":"invalid","url":"https://example.com","model":"m","api_key":"k"}]}}"#,
            &InstanceId::default_instance()
        )
        .is_err());
        assert!(parse_json(
            r#"{"semantic":{"routes":[{"name":"fast","capability":"evaluate","protocol":"decision","url":"https://example.com","model":"m","api_key":"k"}]}}"#,
            &InstanceId::default_instance()
        )
        .is_err());
    }

    #[test]
    fn load_migrates_legacy_toml_once_to_json() {
        let root = std::env::temp_dir().join(format!(
            "herdr-config-json-migration-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let json_path = root.join("config.json");
        let legacy_path = root.join("config.toml");
        fs::write(
            &legacy_path,
            r#"[runtime]
port = 9000

[edge]
public_origin = "https://herdr.example.com"
"#,
        )
        .unwrap();

        let config =
            Config::load_for_instance(&json_path, &InstanceId::default_instance()).unwrap();
        assert_eq!(config.runtime_port, 9000);
        assert!(json_path.is_file());
        assert!(root.join("config.toml.migrated").is_file());
        let migrated = fs::read_to_string(&json_path).unwrap();
        assert!(migrated.starts_with("{\n"));
        assert!(config.semantic.routes.is_empty());
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&json_path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let _ = fs::remove_dir_all(&root);
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
