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
pub(super) struct StateSchemaMigration {
    pub(super) from: i64,
    pub(super) to: i64,
    pub(super) reversible: bool,
}

impl StateSchemaMigration {
    pub(super) fn warning(&self) -> String {
        format!(
            "⚠️ This update migrates local state schema {} → {} and cannot be downgraded; "
                "older versions will refuse the migrated database. "
                "A state backup is taken before migration; rolling back requires restoring that backup.",
            self.from, self.to
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReleasePlan {
    pub(super) version: Version,
    pub(super) tag: String,
    pub(super) identity: ReleaseIdentity,
    pub(super) contract: contract::ContractIdentity,
    pub(super) state_schema_migration: Option<StateSchemaMigration>,
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
    let local = contract::identity()?;
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    let local = super::scheduled_tests::local_contract().unwrap_or(local);
    let (target_contract, state_schema_migration) =
        admit_release_contract(value, &local, SCHEMA_VERSION)?;
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
        contract: target_contract,
        state_schema_migration,
        asset: ReleaseAsset {
            target: target.to_owned(),
            name,
            size,
            sha256,
            url,
        },
    })
}

fn parse_contract_identity(value: &Value) -> Option<contract::ContractIdentity> {
    let hash = value.get("hash")?.as_str()?;
    if !valid_sha256(hash.strip_prefix("sha256:")?) {
        return None;
    }
    Some(contract::ContractIdentity {
        epoch: u32::try_from(value.get("epoch")?.as_u64()?).ok()?,
        hash: hash.to_owned(),
        tool_count: u32::try_from(value.get("tool_count")?.as_u64()?)
            .ok()
            .filter(|count| *count > 0)?,
    })
}

// Shared by explicit and scheduled updates before any candidate is staged.
fn admit_release_contract(
    value: &Value,
    local: &contract::ContractIdentity,
    state_schema: i64,
) -> Result<(contract::ContractIdentity, Option<StateSchemaMigration>), String> {
    let target_epoch = value
        .get("contract")
        .and_then(|contract| contract.get("epoch"))
        .map_or_else(|| "missing".to_owned(), Value::to_string);
    let refuse = |reason: &str| {
        format!(
            "release manifest contract identity mismatch: local epoch {}, target epoch {target_epoch}: {reason}; update to the intermediate bridge via herdr-mcp update apply --manifest <url>",
            local.epoch
        )
    };
    let target = value
        .get("contract")
        .and_then(parse_contract_identity)
        .ok_or_else(|| refuse("missing or malformed target contract"))?;
    let state_schema_migration = admit_state_schema_migration(value, state_schema, &refuse)?;
    if target == *local {
        return Ok((target, state_schema_migration));
    }
    if local.epoch.checked_add(1) != Some(target.epoch) {
        return Err(refuse("only an adjacent forward epoch is admissible"));
    }
    let migration = value
        .get("contract_migration")
        .and_then(Value::as_object)
        .filter(|object| {
            object.len() == 2
                && object.contains_key("from")
                && object.contains_key("state_schema_from")
        })
        .ok_or_else(|| refuse("missing or malformed contract_migration"))?;
    let from = migration
        .get("from")
        .filter(|from| from.as_object().is_some_and(|object| object.len() == 3))
        .and_then(parse_contract_identity)
        .ok_or_else(|| refuse("malformed contract_migration.from"))?;
    if from != *local {
        return Err(refuse(
            "contract_migration.from does not match the local contract",
        ));
    }
    if migration.get("state_schema_from").and_then(Value::as_i64) != Some(state_schema) {
        return Err(refuse(
            "migration state schema is not rollback-compatible with the local schema",
        ));
    }
    Ok((target, state_schema_migration))
}

fn admit_state_schema_migration(
    value: &Value,
    local_schema: i64,
    refuse: &impl Fn(&str) -> String,
) -> Result<Option<StateSchemaMigration>, String> {
    let target_schema = value
        .get("state_schema")
        .and_then(Value::as_i64)
        .ok_or_else(|| refuse("missing or malformed state_schema"))?;
    if target_schema == local_schema {
        return Ok(None);
    }
    if target_schema < local_schema {
        return Err(refuse(&format!(
            "release state schema {target_schema} is older than local schema {local_schema}; "
                "refusing downgrade"
        )));
    }
    if target_schema > local_schema.saturating_add(1) {
        return Err(refuse(&format!(
            "release state schema {target_schema} is more than one step ahead of local schema "
                "{local_schema}"
        )));
    }

    let declaration = value
        .get("state_schema_migration")
        .and_then(Value::as_object)
        .filter(|object| object.len() == 3)
        .ok_or_else(|| {
            refuse(&format!(
                "release state schema is not rollback-compatible with local schema {local_schema}: "
                    "missing or malformed state_schema_migration declaration"
            ))
        })?;
    let from = declaration
        .get("from")
        .and_then(Value::as_i64)
        .ok_or_else(|| refuse("state_schema_migration.from must be an integer"))?;
    let to = declaration
        .get("to")
        .and_then(Value::as_i64)
        .ok_or_else(|| refuse("state_schema_migration.to must be an integer"))?;
    let reversible = declaration
        .get("reversible")
        .and_then(Value::as_bool)
        .ok_or_else(|| refuse("state_schema_migration.reversible must be a boolean"))?;
    if from != local_schema {
        return Err(refuse(&format!(
            "state_schema_migration.from {from} does not match local schema {local_schema}"
        )));
    }
    if to != target_schema {
        return Err(refuse(&format!(
            "state_schema_migration.to {to} does not match target schema {target_schema}"
        )));
    }
    if reversible {
        return Err(refuse(
            "state_schema_migration.reversible must be false for the forward migration",
        ));
    }
    Ok(Some(StateSchemaMigration {
        from,
        to,
        reversible,
    }))
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
                .contains("state_schema_migration")
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

    fn adjacent_manifest(local: &contract::ContractIdentity) -> Value {
        let mut value = manifest_for(current_target().unwrap(), "9.9.9");
        value["contract"] = json!({
            "epoch": local.epoch + 1,
            "hash": format!("sha256:{}", "c".repeat(64)),
            "tool_count": local.tool_count,
        });
        value["contract_migration"] = json!({
            "from": {"epoch": local.epoch, "hash": local.hash, "tool_count": local.tool_count},
            "state_schema_from": SCHEMA_VERSION,
        });
        value
    }

    #[test]
    fn adjacent_contract_updates_require_exact_declaration_and_unchanged_schema() {
        // Exercise both the frozen bridge and a future epoch without a v5 special case.
        for local in [
            contract::previous_identity().unwrap(),
            contract::identity().unwrap(),
        ] {
            let valid = adjacent_manifest(&local);
            let (admitted, migration) =
                admit_release_contract(&valid, &local, SCHEMA_VERSION).unwrap();
            assert_eq!(admitted.epoch, local.epoch + 1);
            assert_eq!(admitted.hash, format!("sha256:{}", "c".repeat(64)));
            assert_eq!(migration, None);

            let mut cases = Vec::new();
            let mut missing = valid.clone();
            missing
                .as_object_mut()
                .unwrap()
                .remove("contract_migration");
            cases.push(missing);
            for malformed in [Value::Null, json!([]), json!({}), json!("migration")] {
                let mut bad = valid.clone();
                bad["contract_migration"] = malformed;
                cases.push(bad);
            }
            for (pointer, replacement) in [
                ("/contract_migration/extra", json!(true)),
                ("/contract_migration/from/extra", json!(true)),
                ("/contract_migration/from/epoch", json!(local.epoch - 1)),
                (
                    "/contract_migration/from/hash",
                    json!(format!("sha256:{}", "d".repeat(64))),
                ),
                (
                    "/contract_migration/from/tool_count",
                    json!(local.tool_count + 1),
                ),
                ("/contract_migration/from/hash", json!("sha256:malformed")),
                ("/contract_migration/from", json!(null)),
                (
                    "/contract_migration/state_schema_from",
                    json!(SCHEMA_VERSION + 1),
                ),
                ("/contract_migration/state_schema_from", json!("15")),
                ("/state_schema", json!(SCHEMA_VERSION + 1)),
                ("/contract/epoch", json!(local.epoch + 2)),
                ("/contract/epoch", json!(local.epoch - 1)),
                ("/contract/epoch", json!(local.epoch)),
                ("/contract/hash", json!("sha256:malformed")),
                ("/contract/tool_count", json!(0)),
            ] {
                let mut bad = valid.clone();
                if let Some(slot) = bad.pointer_mut(pointer) {
                    *slot = replacement;
                } else if pointer.ends_with("/extra") {
                    let parent = pointer.strip_suffix("/extra").unwrap();
                    bad.pointer_mut(parent).unwrap()["extra"] = replacement;
                }
                cases.push(bad);
            }
            for bad in cases {
                let error = admit_release_contract(&bad, &local, SCHEMA_VERSION).unwrap_err();
                assert!(
                    error.contains(&format!("local epoch {}", local.epoch)),
                    "{error}"
                );
                assert!(error.contains("target epoch"), "{error}");
                assert!(
                    error.contains("herdr-mcp update apply --manifest <url>"),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn one_forward_state_schema_migration_is_admitted_on_both_contract_paths() {
        let local = contract::previous_identity().unwrap();
        let mut adjacent = adjacent_manifest(&local);
        adjacent["state_schema"] = json!(15 + 1);
        adjacent["contract_migration"]["state_schema_from"] = json!(15);
        adjacent["state_schema_migration"] = json!({
            "from": 15,
            "to": 16,
            "reversible": false,
        });
        let (target, migration) = admit_release_contract(&adjacent, &local, 15).unwrap();
        assert_eq!(target.epoch, local.epoch + 1);
        assert_eq!(
            migration,
            Some(StateSchemaMigration {
                from: 15,
                to: 16,
                reversible: false,
            })
        );

        let same = {
            let mut value = manifest_for(current_target().unwrap(), "9.9.9");
            value["state_schema"] = json!(16);
            value["state_schema_migration"] = json!({
                "from": 15,
                "to": 16,
                "reversible": false,
            });
            value
        };
        let local_same = contract::identity().unwrap();
        let (target, migration) = admit_release_contract(&same, &local_same, 15).unwrap();
        assert_eq!(target, local_same);
        let expected = StateSchemaMigration {
            from: 15,
            to: 16,
            reversible: false,
        };
        assert_eq!(migration.unwrap().warning(), expected.warning());
    }

    #[test]
    fn state_schema_migration_rejects_every_non_exact_declaration() {
        let local = contract::identity().unwrap();
        let base = {
            let mut value = manifest_for(current_target().unwrap(), "9.9.9");
            value["state_schema"] = json!(SCHEMA_VERSION + 1);
            value["state_schema_migration"] = json!({
                "from": SCHEMA_VERSION,
                "to": SCHEMA_VERSION + 1,
                "reversible": false,
            });
            value
        };
        let mut cases = Vec::new();
        cases.push(("downgrade", {
            let mut value = base.clone();
            value["state_schema"] = json!(SCHEMA_VERSION - 1);
            value
        }));
        cases.push(("jump", {
            let mut value = base.clone();
            value["state_schema"] = json!(SCHEMA_VERSION + 2);
            value
        }));
        cases.push(("missing", {
            let mut value = base.clone();
            value.as_object_mut().unwrap().remove("state_schema_migration");
            value
        }));
        cases.push(("extra", {
            let mut value = base.clone();
            value["state_schema_migration"]["extra"] = json!(true);
            value
        }));
        cases.push(("reversible", {
            let mut value = base.clone();
            value["state_schema_migration"]["reversible"] = json!(true);
            value
        }));
        cases.push(("from", {
            let mut value = base.clone();
            value["state_schema_migration"]["from"] = json!(SCHEMA_VERSION - 1);
            value
        }));
        cases.push(("to", {
            let mut value = base.clone();
            value["state_schema_migration"]["to"] = json!(SCHEMA_VERSION + 2);
            value
        }));
        cases.push(("malformed", {
            let mut value = base.clone();
            value["state_schema_migration"] = json!({
                "from": "15",
                "to": 16,
                "reversible": false,
            });
            value
        }));
        for (name, value) in cases {
            let error = admit_release_contract(&value, &local, SCHEMA_VERSION).unwrap_err();
            assert!(
                error.contains("state schema") || error.contains("state_schema_migration"),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn release_plan_surfaces_the_forward_migration_warning() {
        let target = current_target().unwrap();
        let mut value = manifest_for(target, "9.9.9");
        value["state_schema"] = json!(SCHEMA_VERSION + 1);
        value["state_schema_migration"] = json!({
            "from": SCHEMA_VERSION,
            "to": SCHEMA_VERSION + 1,
            "reversible": false,
        });
        let plan = parse_release_plan(&value, target).unwrap();
        let migration = plan.state_schema_migration.unwrap();
        assert!(migration.warning().contains("cannot be downgraded"));
        assert!(migration
            .warning()
            .contains("state backup is taken before migration"));
    }

    #[test]
    fn same_contract_remains_admissible_without_a_migration_declaration() {
        let local = contract::identity().unwrap();
        let mut value = manifest_for(current_target().unwrap(), "9.9.9");
        assert_eq!(
            admit_release_contract(&value, &local, SCHEMA_VERSION).unwrap(),
            (local.clone(), None)
        );
        value["contract_migration"] = json!({"ignored_on_same_contract": true});
        assert_eq!(
            admit_release_contract(&value, &local, SCHEMA_VERSION).unwrap(),
            (local, None)
        );
    }

    #[test]
    fn release_plan_carries_the_admitted_target_contract_for_candidate_verification() {
        let local = contract::identity().unwrap();
        let plan =
            parse_release_plan(&adjacent_manifest(&local), current_target().unwrap()).unwrap();
        assert_eq!(plan.contract.epoch, local.epoch + 1);
        assert_ne!(plan.contract, local);
        assert_eq!(plan.state_schema_migration, None);
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
