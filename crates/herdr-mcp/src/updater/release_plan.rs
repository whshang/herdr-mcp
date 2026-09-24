//! Release discovery and manifest trust planning for the native updater.
//!
//! This module owns one boundary: turning "which release should this workstation
//! run, and what exactly does the publisher claim about it" into a validated
//! [`ReleasePlan`]. That covers the release index, the tag allowed by the local
//! update channel, and the release manifest contract (schema, product,
//! rollback-compatible state schema, contract identity, repository/tag/asset URL
//! trust) before any artifact is fetched.
//!
//! It deliberately does not own activation. The shared update transport and URL
//! policy, artifact attestation, staging and download, the detached update
//! worker, service lifecycle, major migration, status, and rollback stay in the
//! parent `updater` module; this module consumes those primitives through
//! `super::` instead of duplicating them.

use super::{
    BINARY_MAX_BYTES, current_target, fetch_bounded, parse_update_url, sha256_bytes, update_client,
    valid_asset_name, valid_sha256, verify_artifact_attestation,
};
use crate::config::UpdateChannel;
use crate::contract;
use crate::release_trust::{self, ReleaseIdentity};
use crate::state_store::SCHEMA_VERSION;
use reqwest::blocking::Client;
use semver::Version;
use serde_json::Value;
use std::env;
use url::Url;

const DEFAULT_RELEASES_API_URL: &str =
    "https://api.github.com/repos/whshang/herdr-mcp/releases?per_page=20";
const RUNTIME_MANIFEST_NAME: &str = "runtime-manifest.json";
const LEGACY_MANIFEST_NAME: &str = "release-manifest.json";
const RELEASES_MAX_BYTES: usize = 1024 * 1024;
const MANIFEST_MAX_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReleaseAsset {
    pub(super) target: String,
    pub(super) name: String,
    pub(super) size: u64,
    pub(super) sha256: String,
    pub(super) url: Url,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReleasePlan {
    pub(super) version: Version,
    pub(super) tag: String,
    pub(super) identity: ReleaseIdentity,
    pub(super) asset: ReleaseAsset,
}

pub(super) fn fetch_release_plan(
    manifest_override: Option<&str>,
    channel: UpdateChannel,
) -> Result<ReleasePlan, String> {
    let client = update_client()?;
    let (manifest_url, manifest_name) = match manifest_override
        .map(str::to_owned)
        .or_else(|| env::var("HERDR_MCP_UPDATE_MANIFEST_URL").ok())
    {
        Some(raw) => {
            let url = parse_update_url(&raw)?;
            let name = release_manifest_name(&url)?;
            (url, name)
        }
        None => discover_default_manifest_url(&client, channel)?,
    };
    let bytes = fetch_bounded(
        &client,
        manifest_url,
        MANIFEST_MAX_BYTES,
        "release manifest",
    )?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("release manifest is invalid JSON: {error}"))?;
    let plan = parse_release_plan(&value, current_target()?)?;
    if manifest_override.is_none()
        && env::var("HERDR_MCP_UPDATE_MANIFEST_URL").is_err()
        && !channel.accepts_version(&plan.version)
    {
        return Err(format!(
            "discovered release {} is outside update channel {}",
            plan.version,
            channel.as_str()
        ));
    }
    let manifest_sha256 = sha256_bytes(&bytes);
    verify_artifact_attestation(&client, manifest_name, &manifest_sha256, &plan.identity)?;
    Ok(plan)
}

fn release_manifest_name(url: &Url) -> Result<&'static str, String> {
    match url.path_segments().and_then(Iterator::last) {
        Some(RUNTIME_MANIFEST_NAME) => Ok(RUNTIME_MANIFEST_NAME),
        Some(LEGACY_MANIFEST_NAME) => Ok(LEGACY_MANIFEST_NAME),
        _ => Err(format!(
            "release manifest URL must end in {RUNTIME_MANIFEST_NAME} or {LEGACY_MANIFEST_NAME}"
        )),
    }
}

fn discover_default_manifest_url(
    client: &Client,
    channel: UpdateChannel,
) -> Result<(Url, &'static str), String> {
    let releases_url = Url::parse(DEFAULT_RELEASES_API_URL)
        .map_err(|_| "default GitHub releases API URL is invalid".to_owned())?;
    let bytes = fetch_bounded(
        client,
        releases_url,
        RELEASES_MAX_BYTES,
        "GitHub releases index",
    )?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("GitHub releases index is invalid JSON: {error}"))?;
    for manifest_name in [RUNTIME_MANIFEST_NAME, LEGACY_MANIFEST_NAME] {
        if let Some(tag) = select_release_tag(&value, channel, manifest_name)? {
            let url = Url::parse(&format!(
                "https://github.com/{}/releases/download/{tag}/{manifest_name}",
                release_trust::RELEASE_REPOSITORY
            ))
            .map_err(|_| "cannot construct discovered release manifest URL".to_owned())?;
            return Ok((url, manifest_name));
        }
    }
    Err(format!(
        "GitHub releases index contains no non-draft semver release with {RUNTIME_MANIFEST_NAME} or {LEGACY_MANIFEST_NAME} for update channel {}",
        channel.as_str()
    ))
}

fn select_release_tag(
    value: &Value,
    channel: UpdateChannel,
    manifest_name: &str,
) -> Result<Option<String>, String> {
    let releases = value
        .as_array()
        .ok_or_else(|| "GitHub releases index must be a JSON array".to_owned())?;
    let mut best: Option<(Version, String)> = None;
    for release in releases {
        let Some(object) = release.as_object() else {
            continue;
        };
        if object.get("draft").and_then(Value::as_bool) != Some(false) {
            continue;
        }
        let Some(tag) = object.get("tag_name").and_then(Value::as_str) else {
            continue;
        };
        let Some(version_text) = tag.strip_prefix('v') else {
            continue;
        };
        let Ok(version) = Version::parse(version_text) else {
            continue;
        };
        if tag != format!("v{version}") {
            continue;
        }
        if !channel.accepts_version(&version) {
            continue;
        }
        let has_manifest = object
            .get("assets")
            .and_then(Value::as_array)
            .is_some_and(|assets| {
                assets
                    .iter()
                    .any(|asset| asset.get("name").and_then(Value::as_str) == Some(manifest_name))
            });
        if !has_manifest {
            continue;
        }
        if best
            .as_ref()
            .is_none_or(|(best_version, _)| version > *best_version)
        {
            best = Some((version, tag.to_owned()));
        }
    }
    Ok(best.map(|(_, tag)| tag))
}

fn parse_release_plan(value: &Value, target: &str) -> Result<ReleasePlan, String> {
    if value.get("schema_version").and_then(Value::as_u64)
        != Some(release_trust::MANIFEST_SCHEMA_VERSION)
    {
        return Err("unsupported release manifest schema".to_owned());
    }
    if value.get("product").and_then(Value::as_str) != Some("herdr-mcp") {
        return Err("release manifest product mismatch".to_owned());
    }
    if value.get("state_schema").and_then(Value::as_i64) != Some(SCHEMA_VERSION) {
        return Err(format!(
            "release state schema is not rollback-compatible with local schema {SCHEMA_VERSION}"
        ));
    }
    let version_text = value
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| "release manifest is missing version".to_owned())?;
    let version = Version::parse(version_text)
        .map_err(|_| "release manifest version is not semver".to_owned())?;
    let tag = value
        .get("tag")
        .and_then(Value::as_str)
        .filter(|tag| *tag == format!("v{version}"))
        .ok_or_else(|| "release manifest tag/version mismatch".to_owned())?
        .to_owned();
    let identity = release_trust::parse_manifest_identity(value, &tag)?;

    let expected = contract::identity()?;
    let contract = value
        .get("contract")
        .and_then(Value::as_object)
        .ok_or_else(|| "release manifest is missing contract identity".to_owned())?;
    if contract.get("epoch").and_then(Value::as_u64) != Some(u64::from(expected.epoch))
        || contract.get("hash").and_then(Value::as_str) != Some(expected.hash.as_str())
        || contract.get("tool_count").and_then(Value::as_u64)
            != Some(u64::from(expected.tool_count))
    {
        return Err("release manifest contract identity mismatch".to_owned());
    }

    let assets = value
        .get("assets")
        .and_then(Value::as_array)
        .ok_or_else(|| "release manifest is missing assets".to_owned())?;
    let matches = assets
        .iter()
        .filter(|asset| asset.get("target").and_then(Value::as_str) == Some(target))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(format!(
            "release manifest must contain exactly one asset for target {target}"
        ));
    }
    let asset = matches[0];
    let name = asset
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| valid_asset_name(name))
        .ok_or_else(|| "release asset name is invalid".to_owned())?
        .to_owned();
    let size = asset
        .get("size")
        .and_then(Value::as_u64)
        .filter(|size| *size > 0 && *size <= BINARY_MAX_BYTES)
        .ok_or_else(|| "release asset size is invalid or too large".to_owned())?;
    let sha256 = asset
        .get("sha256")
        .and_then(Value::as_str)
        .filter(|hash| valid_sha256(hash))
        .ok_or_else(|| "release asset sha256 is invalid".to_owned())?
        .to_owned();
    let asset_url = asset
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| "release asset URL is missing".to_owned())?;
    let url = parse_update_url(asset_url)?;
    let expected_url = Url::parse(&format!(
        "https://github.com/{}/releases/download/{}/{}",
        identity.repository, tag, name
    ))
    .map_err(|_| "cannot construct expected release asset URL".to_owned())?;
    if url != expected_url {
        return Err("release asset URL does not match trusted repository/tag/name".to_owned());
    }
    Ok(ReleasePlan {
        version,
        tag,
        identity,
        asset: ReleaseAsset {
            target: target.to_owned(),
            name,
            size,
            sha256,
            url,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest_for(target: &str, version: &str) -> Value {
        let identity = contract::identity().unwrap();
        let name = format!("herdr-mcp-{version}-{target}");
        let tag = format!("v{version}");
        json!({
            "schema_version": release_trust::MANIFEST_SCHEMA_VERSION,
            "product": "herdr-mcp",
            "state_schema": SCHEMA_VERSION,
            "version": version,
            "tag": tag,
            "release_identity": {
                "tag": tag,
                "source_commit": "b".repeat(40),
                "source_ref": format!("refs/tags/v{version}"),
            },
            "repository_identity": {
                "repository": release_trust::RELEASE_REPOSITORY,
                "repository_id": release_trust::RELEASE_REPOSITORY_ID,
            },
            "provenance": {
                "predicate_type": release_trust::SLSA_PROVENANCE_V1,
                "attestation": release_trust::GITHUB_ARTIFACT_ATTESTATION,
                "bundle_media_type": release_trust::SIGSTORE_BUNDLE_V03,
                "workflow": release_trust::RELEASE_WORKFLOW,
                "workflow_name": release_trust::RELEASE_WORKFLOW_NAME,
                "issuer": release_trust::RELEASE_ISSUER,
                "runner_environment": release_trust::RELEASE_RUNNER_ENVIRONMENT,
            },
            "contract": {
                "epoch": identity.epoch,
                "hash": identity.hash,
                "tool_count": identity.tool_count,
            },
            "assets": [{
                "target": target,
                "name": name,
                "size": 1234,
                "sha256": "a".repeat(64),
                "url": format!("https://github.com/whshang/herdr-mcp/releases/download/v{version}/herdr-mcp-{version}-{target}")
            }]
        })
    }

    #[test]
    fn manifest_validation_pins_contract_target_and_semver() {
        let target = current_target().unwrap();
        let plan = parse_release_plan(&manifest_for(target, "9.9.9"), target).unwrap();
        assert_eq!(plan.version, Version::parse("9.9.9").unwrap());
        assert_eq!(plan.asset.target, target);

        let mut bad = manifest_for(target, "9.9.9");
        bad["contract"]["hash"] = json!("sha256:wrong");
        assert!(
            parse_release_plan(&bad, target)
                .unwrap_err()
                .contains("contract")
        );
        let mut future_schema = manifest_for(target, "9.9.9");
        future_schema["state_schema"] = json!(SCHEMA_VERSION + 1);
        assert!(
            parse_release_plan(&future_schema, target)
                .unwrap_err()
                .contains("rollback-compatible")
        );
        let mut wrong_repo = manifest_for(target, "9.9.9");
        wrong_repo["repository_identity"]["repository"] = json!("attacker/fork");
        assert!(
            parse_release_plan(&wrong_repo, target)
                .unwrap_err()
                .contains("repository")
        );
        let mut wrong_url = manifest_for(target, "9.9.9");
        wrong_url["assets"][0]["url"] = json!("https://example.com/herdr-mcp");
        assert!(
            parse_release_plan(&wrong_url, target)
                .unwrap_err()
                .contains("trusted repository")
        );
        assert!(parse_release_plan(&manifest_for(target, "9.9.9"), "other-target").is_err());
    }

    #[test]
    fn release_discovery_respects_update_channel() {
        let releases = json!([
            {
                "draft": false,
                "prerelease": true,
                "tag_name": "v0.4.0-alpha.5",
                "assets": [{"name": "release-manifest.json"}]
            },
            {
                "draft": false,
                "prerelease": true,
                "tag_name": "v0.4.0-alpha.6",
                "assets": [{"name": "release-manifest.json"}]
            },
            {
                "draft": true,
                "prerelease": true,
                "tag_name": "v9.0.0-alpha.1",
                "assets": [{"name": "release-manifest.json"}]
            },
            {
                "draft": false,
                "prerelease": false,
                "tag_name": "v0.3.0",
                "assets": [{"name": "release-manifest.json"}]
            },
            {
                "draft": false,
                "prerelease": false,
                "tag_name": "v8.0.0",
                "assets": [{"name": "other.bin"}]
            },
            {
                "draft": false,
                "prerelease": true,
                "tag_name": "not-semver",
                "assets": [{"name": "release-manifest.json"}]
            }
        ]);
        assert_eq!(
            select_release_tag(&releases, UpdateChannel::Preview, LEGACY_MANIFEST_NAME).unwrap(),
            Some("v0.4.0-alpha.6".to_owned())
        );
        assert_eq!(
            select_release_tag(&releases, UpdateChannel::Stable, LEGACY_MANIFEST_NAME).unwrap(),
            Some("v0.3.0".to_owned())
        );

        assert!(
            select_release_tag(
                &json!({"tag_name": "v1.0.0"}),
                UpdateChannel::Preview,
                RUNTIME_MANIFEST_NAME
            )
            .is_err()
        );
        assert_eq!(
            select_release_tag(
                &json!([{
                    "draft": false,
                    "tag_name": "v1.0.0-alpha.1",
                    "assets": [{"name": "release-manifest.json"}]
                }]),
                UpdateChannel::Stable,
                LEGACY_MANIFEST_NAME
            )
            .unwrap(),
            None
        );
        assert_eq!(
            select_release_tag(
                &json!([{
                    "draft": false,
                    "tag_name": "v1.0.0",
                    "assets": []
                }]),
                UpdateChannel::Preview,
                RUNTIME_MANIFEST_NAME
            )
            .unwrap(),
            None
        );
        let runtime_release = json!([{
            "draft": false,
            "tag_name": "v1.0.0",
            "assets": [{"name": "runtime-manifest.json"}]
        }]);
        assert_eq!(
            select_release_tag(
                &runtime_release,
                UpdateChannel::Stable,
                RUNTIME_MANIFEST_NAME
            )
            .unwrap(),
            Some("v1.0.0".to_owned())
        );
    }
}
