//! Cryptographic trust policy for Rust release artifacts.
//!
//! A GitHub URL is transport, not identity. The updater accepts an artifact only
//! when GitHub's Sigstore bundle verifies against the public-good production
//! trust root and the certificate-bound repository/workflow/source identity
//! agrees with the release manifest.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;
use sigstore_trust_root::{SIGSTORE_PRODUCTION_TRUSTED_ROOT, TrustedRoot};
use sigstore_types::{Bundle, Sha256Hash};
use sigstore_verify::{VerificationPolicy, verify};
use x509_cert::{
    Certificate,
    der::{Decode, asn1::Utf8StringRef},
};

pub const MANIFEST_SCHEMA_VERSION: u64 = 2;
pub const RELEASE_REPOSITORY: &str = "whshang/herdr-mcp";
pub const RELEASE_REPOSITORY_ID: u64 = 1_340_180_695;
pub const RELEASE_WORKFLOW: &str = ".github/workflows/rust-release.yml";
pub const RELEASE_WORKFLOW_NAME: &str = "Rust Release";
pub const RELEASE_ISSUER: &str = "https://token.actions.githubusercontent.com";
pub const RELEASE_RUNNER_ENVIRONMENT: &str = "github-hosted";
pub const SLSA_PROVENANCE_V1: &str = "https://slsa.dev/provenance/v1";
pub const GITHUB_ARTIFACT_ATTESTATION: &str = "github-artifact-attestation";
pub const SIGSTORE_BUNDLE_V03: &str = "application/vnd.dev.sigstore.bundle.v0.3+json";
const IN_TOTO_STATEMENT_V1: &str = "https://in-toto.io/Statement/v1";
const GITHUB_WORKFLOW_BUILD_TYPE: &str = "https://actions.github.io/buildtypes/workflow/v1";
const DSSE_PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";

const OID_WORKFLOW_TRIGGER: &str = "1.3.6.1.4.1.57264.1.2";
const OID_WORKFLOW_SHA: &str = "1.3.6.1.4.1.57264.1.3";
const OID_WORKFLOW_NAME: &str = "1.3.6.1.4.1.57264.1.4";
const OID_WORKFLOW_REPOSITORY: &str = "1.3.6.1.4.1.57264.1.5";
const OID_WORKFLOW_REF: &str = "1.3.6.1.4.1.57264.1.6";
const OID_AUTHENTICATED_ISSUER: &str = "1.3.6.1.4.1.57264.1.8";
const OID_BUILD_SIGNER_URI: &str = "1.3.6.1.4.1.57264.1.9";
const OID_BUILD_SIGNER_DIGEST: &str = "1.3.6.1.4.1.57264.1.10";
const OID_RUNNER_ENVIRONMENT: &str = "1.3.6.1.4.1.57264.1.11";
const OID_SOURCE_REPOSITORY_URI: &str = "1.3.6.1.4.1.57264.1.12";
const OID_SOURCE_REPOSITORY_DIGEST: &str = "1.3.6.1.4.1.57264.1.13";
const OID_SOURCE_REPOSITORY_REF: &str = "1.3.6.1.4.1.57264.1.14";
const OID_SOURCE_REPOSITORY_ID: &str = "1.3.6.1.4.1.57264.1.15";
const OID_BUILD_CONFIG_URI: &str = "1.3.6.1.4.1.57264.1.18";
const OID_BUILD_CONFIG_DIGEST: &str = "1.3.6.1.4.1.57264.1.19";
const OID_BUILD_TRIGGER: &str = "1.3.6.1.4.1.57264.1.20";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseIdentity {
    pub tag: String,
    pub source_commit: String,
    pub source_ref: String,
    pub repository: String,
    pub repository_id: u64,
    pub workflow: String,
    pub workflow_name: String,
    pub issuer: String,
    pub runner_environment: String,
}

impl ReleaseIdentity {
    fn repository_uri(&self) -> String {
        format!("https://github.com/{}", self.repository)
    }

    fn workflow_uri(&self) -> String {
        format!(
            "https://github.com/{}/{}@{}",
            self.repository, self.workflow, self.source_ref
        )
    }

    fn source_dependency_uri(&self) -> String {
        format!(
            "git+https://github.com/{}@{}",
            self.repository, self.source_ref
        )
    }
}

pub fn parse_manifest_identity(value: &Value, tag: &str) -> Result<ReleaseIdentity, String> {
    let release = value
        .get("release_identity")
        .and_then(Value::as_object)
        .ok_or_else(|| "release manifest is missing release identity".to_owned())?;
    let repository = value
        .get("repository_identity")
        .and_then(Value::as_object)
        .ok_or_else(|| "release manifest is missing repository identity".to_owned())?;
    let provenance = value
        .get("provenance")
        .and_then(Value::as_object)
        .ok_or_else(|| "release manifest is missing provenance identity".to_owned())?;

    if release.get("tag").and_then(Value::as_str) != Some(tag) {
        return Err("release manifest release identity tag mismatch".to_owned());
    }
    let source_commit = release
        .get("source_commit")
        .and_then(Value::as_str)
        .filter(|value| valid_git_sha(value))
        .ok_or_else(|| "release manifest source commit is invalid".to_owned())?
        .to_owned();
    let expected_ref = format!("refs/tags/{tag}");
    let source_ref = release
        .get("source_ref")
        .and_then(Value::as_str)
        .filter(|value| *value == expected_ref)
        .ok_or_else(|| "release manifest source ref/tag mismatch".to_owned())?
        .to_owned();

    if repository.get("repository").and_then(Value::as_str) != Some(RELEASE_REPOSITORY)
        || repository.get("repository_id").and_then(Value::as_u64) != Some(RELEASE_REPOSITORY_ID)
    {
        return Err("release manifest repository identity mismatch".to_owned());
    }
    if provenance.get("predicate_type").and_then(Value::as_str) != Some(SLSA_PROVENANCE_V1)
        || provenance.get("attestation").and_then(Value::as_str)
            != Some(GITHUB_ARTIFACT_ATTESTATION)
        || provenance.get("bundle_media_type").and_then(Value::as_str) != Some(SIGSTORE_BUNDLE_V03)
        || provenance.get("workflow").and_then(Value::as_str) != Some(RELEASE_WORKFLOW)
        || provenance.get("workflow_name").and_then(Value::as_str) != Some(RELEASE_WORKFLOW_NAME)
        || provenance.get("issuer").and_then(Value::as_str) != Some(RELEASE_ISSUER)
        || provenance.get("runner_environment").and_then(Value::as_str)
            != Some(RELEASE_RUNNER_ENVIRONMENT)
    {
        return Err("release manifest provenance identity mismatch".to_owned());
    }

    Ok(ReleaseIdentity {
        tag: tag.to_owned(),
        source_commit,
        source_ref,
        repository: RELEASE_REPOSITORY.to_owned(),
        repository_id: RELEASE_REPOSITORY_ID,
        workflow: RELEASE_WORKFLOW.to_owned(),
        workflow_name: RELEASE_WORKFLOW_NAME.to_owned(),
        issuer: RELEASE_ISSUER.to_owned(),
        runner_environment: RELEASE_RUNNER_ENVIRONMENT.to_owned(),
    })
}

pub fn attestation_api_url(sha256: &str) -> Result<url::Url, String> {
    if !valid_sha256(sha256) {
        return Err("release attestation digest is invalid".to_owned());
    }
    url::Url::parse(&format!(
        "https://api.github.com/repos/{RELEASE_REPOSITORY}/attestations/sha256:{sha256}"
    ))
    .map_err(|_| "cannot construct GitHub attestation URL".to_owned())
}

pub fn verify_github_attestations(
    response: &[u8],
    artifact_name: &str,
    artifact_sha256: &str,
    identity: &ReleaseIdentity,
) -> Result<(), String> {
    if artifact_name.is_empty() || !valid_sha256(artifact_sha256) {
        return Err("release attestation subject is invalid".to_owned());
    }
    let document: Value = serde_json::from_slice(response)
        .map_err(|_| "GitHub attestation response is invalid JSON".to_owned())?;
    let attestations = document
        .get("attestations")
        .and_then(Value::as_array)
        .ok_or_else(|| "GitHub attestation response is missing attestations".to_owned())?;
    if attestations.is_empty() {
        return Err("GitHub returned no attestation for release artifact".to_owned());
    }

    let trusted_root = TrustedRoot::from_json(SIGSTORE_PRODUCTION_TRUSTED_ROOT)
        .map_err(|_| "cannot load embedded Sigstore production trust root".to_owned())?;
    let mut last_error = "no matching attestation".to_owned();
    for record in attestations.iter().take(32) {
        match verify_attestation_record(
            record,
            artifact_name,
            artifact_sha256,
            identity,
            &trusted_root,
        ) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = error,
        }
    }
    Err(format!(
        "release attestation verification failed: {last_error}"
    ))
}

fn verify_attestation_record(
    record: &Value,
    artifact_name: &str,
    artifact_sha256: &str,
    identity: &ReleaseIdentity,
    trusted_root: &TrustedRoot,
) -> Result<(), String> {
    if record.get("repository_id").and_then(Value::as_u64) != Some(identity.repository_id) {
        return Err("attestation repository id mismatch".to_owned());
    }
    let bundle_value = record
        .get("bundle")
        .ok_or_else(|| "attestation bundle is missing".to_owned())?;
    if bundle_value.get("mediaType").and_then(Value::as_str) != Some(SIGSTORE_BUNDLE_V03) {
        return Err("attestation bundle media type mismatch".to_owned());
    }
    let bundle_json = serde_json::to_string(bundle_value)
        .map_err(|_| "cannot encode attestation bundle".to_owned())?;
    let bundle = Bundle::from_json(&bundle_json)
        .map_err(|_| "attestation bundle structure is invalid".to_owned())?;
    let digest = Sha256Hash::from_hex(artifact_sha256)
        .map_err(|_| "release attestation digest is invalid".to_owned())?;
    let policy = VerificationPolicy::default()
        .require_identity(identity.workflow_uri())
        .require_issuer(identity.issuer.clone());
    verify(digest, &bundle, &policy, trusted_root).map_err(|_| {
        "Sigstore signature, certificate, SCT, or transparency-log verification failed".to_owned()
    })?;

    verify_certificate_claims(bundle_value, identity)?;
    verify_statement(bundle_value, artifact_name, artifact_sha256, identity)
}

fn verify_certificate_claims(bundle: &Value, identity: &ReleaseIdentity) -> Result<(), String> {
    let raw = bundle
        .pointer("/verificationMaterial/certificate/rawBytes")
        .and_then(Value::as_str)
        .ok_or_else(|| "attestation signing certificate is missing".to_owned())?;
    let der = STANDARD
        .decode(raw)
        .map_err(|_| "attestation signing certificate is invalid base64".to_owned())?;
    let certificate = Certificate::from_der(&der)
        .map_err(|_| "attestation signing certificate is invalid".to_owned())?;
    let repository_uri = identity.repository_uri();
    let workflow_uri = identity.workflow_uri();
    let repository_id = identity.repository_id.to_string();
    let checks = [
        (OID_WORKFLOW_TRIGGER, "push"),
        (OID_WORKFLOW_SHA, identity.source_commit.as_str()),
        (OID_WORKFLOW_NAME, identity.workflow_name.as_str()),
        (OID_WORKFLOW_REPOSITORY, identity.repository.as_str()),
        (OID_WORKFLOW_REF, identity.source_ref.as_str()),
        (OID_AUTHENTICATED_ISSUER, identity.issuer.as_str()),
        (OID_BUILD_SIGNER_URI, workflow_uri.as_str()),
        (OID_BUILD_SIGNER_DIGEST, identity.source_commit.as_str()),
        (OID_RUNNER_ENVIRONMENT, identity.runner_environment.as_str()),
        (OID_SOURCE_REPOSITORY_URI, repository_uri.as_str()),
        (
            OID_SOURCE_REPOSITORY_DIGEST,
            identity.source_commit.as_str(),
        ),
        (OID_SOURCE_REPOSITORY_REF, identity.source_ref.as_str()),
        (OID_SOURCE_REPOSITORY_ID, repository_id.as_str()),
        (OID_BUILD_CONFIG_URI, workflow_uri.as_str()),
        (OID_BUILD_CONFIG_DIGEST, identity.source_commit.as_str()),
        (OID_BUILD_TRIGGER, "push"),
    ];
    for (oid, expected) in checks {
        if certificate_extension(&certificate, oid)? != expected {
            return Err("attestation certificate release identity mismatch".to_owned());
        }
    }
    Ok(())
}

fn certificate_extension(certificate: &Certificate, oid: &str) -> Result<String, String> {
    let extensions = certificate
        .tbs_certificate
        .extensions
        .as_deref()
        .unwrap_or(&[]);
    let mut matches = extensions
        .iter()
        .filter(|ext| ext.extn_id.to_string() == oid);
    let Some(extension) = matches.next() else {
        return Err("attestation certificate is missing required release identity".to_owned());
    };
    if matches.next().is_some() {
        return Err("attestation certificate has duplicate release identity".to_owned());
    }
    let bytes = extension.extn_value.as_bytes();
    if let Ok(value) = Utf8StringRef::from_der(bytes) {
        return Ok(value.as_str().to_owned());
    }
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| "attestation certificate release identity is not UTF-8".to_owned())
}

fn verify_statement(
    bundle: &Value,
    artifact_name: &str,
    artifact_sha256: &str,
    identity: &ReleaseIdentity,
) -> Result<(), String> {
    let envelope = bundle
        .get("dsseEnvelope")
        .and_then(Value::as_object)
        .ok_or_else(|| "attestation DSSE envelope is missing".to_owned())?;
    if envelope.get("payloadType").and_then(Value::as_str) != Some(DSSE_PAYLOAD_TYPE) {
        return Err("attestation DSSE payload type mismatch".to_owned());
    }
    let payload = envelope
        .get("payload")
        .and_then(Value::as_str)
        .ok_or_else(|| "attestation DSSE payload is missing".to_owned())?;
    let statement_bytes = STANDARD
        .decode(payload)
        .map_err(|_| "attestation DSSE payload is invalid base64".to_owned())?;
    let statement: Value = serde_json::from_slice(&statement_bytes)
        .map_err(|_| "attestation statement is invalid JSON".to_owned())?;
    if statement.get("_type").and_then(Value::as_str) != Some(IN_TOTO_STATEMENT_V1)
        || statement.get("predicateType").and_then(Value::as_str) != Some(SLSA_PROVENANCE_V1)
    {
        return Err("attestation statement provenance type mismatch".to_owned());
    }
    let subjects = statement
        .get("subject")
        .and_then(Value::as_array)
        .ok_or_else(|| "attestation statement has no subjects".to_owned())?;
    let matching_subjects = subjects
        .iter()
        .filter(|subject| {
            subject.get("name").and_then(Value::as_str) == Some(artifact_name)
                && subject.pointer("/digest/sha256").and_then(Value::as_str)
                    == Some(artifact_sha256)
        })
        .count();
    if matching_subjects != 1 {
        return Err("attestation subject name/digest mismatch".to_owned());
    }

    let predicate = statement
        .get("predicate")
        .ok_or_else(|| "attestation SLSA predicate is missing".to_owned())?;
    let build = predicate
        .get("buildDefinition")
        .ok_or_else(|| "attestation SLSA build definition is missing".to_owned())?;
    if build.get("buildType").and_then(Value::as_str) != Some(GITHUB_WORKFLOW_BUILD_TYPE) {
        return Err("attestation build type mismatch".to_owned());
    }
    let workflow = build
        .pointer("/externalParameters/workflow")
        .ok_or_else(|| "attestation workflow parameters are missing".to_owned())?;
    if workflow.get("repository").and_then(Value::as_str)
        != Some(identity.repository_uri().as_str())
        || workflow.get("path").and_then(Value::as_str) != Some(identity.workflow.as_str())
        || workflow.get("ref").and_then(Value::as_str) != Some(identity.source_ref.as_str())
    {
        return Err("attestation workflow provenance mismatch".to_owned());
    }
    let github = build
        .pointer("/internalParameters/github")
        .ok_or_else(|| "attestation GitHub build parameters are missing".to_owned())?;
    if github.get("event_name").and_then(Value::as_str) != Some("push")
        || !value_matches_u64(github.get("repository_id"), identity.repository_id)
        || github.get("runner_environment").and_then(Value::as_str)
            != Some(identity.runner_environment.as_str())
    {
        return Err("attestation GitHub build identity mismatch".to_owned());
    }
    let expected_dependency = identity.source_dependency_uri();
    let dependency_matches = build
        .get("resolvedDependencies")
        .and_then(Value::as_array)
        .is_some_and(|dependencies| {
            dependencies.iter().any(|dependency| {
                dependency.get("uri").and_then(Value::as_str) == Some(expected_dependency.as_str())
                    && dependency
                        .pointer("/digest/gitCommit")
                        .and_then(Value::as_str)
                        == Some(identity.source_commit.as_str())
            })
        });
    if !dependency_matches {
        return Err("attestation source revision mismatch".to_owned());
    }
    if predicate
        .pointer("/runDetails/builder/id")
        .and_then(Value::as_str)
        != Some(identity.workflow_uri().as_str())
    {
        return Err("attestation builder identity mismatch".to_owned());
    }
    Ok(())
}

fn value_matches_u64(value: Option<&Value>, expected: u64) -> bool {
    value.is_some_and(|value| {
        value.as_u64() == Some(expected)
            || value.as_str().and_then(|text| text.parse::<u64>().ok()) == Some(expected)
    })
}

fn valid_git_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ALPHA3_ATTESTATION: &str = include_str!("../testdata/github-attestation-alpha3.json");
    const ALPHA3_COMMIT: &str = "ab9eebb1af8e28cdf6d2f915b518a9a6fc9761e9";

    fn alpha3_identity() -> ReleaseIdentity {
        ReleaseIdentity {
            tag: "v0.4.0-alpha.3".to_owned(),
            source_commit: ALPHA3_COMMIT.to_owned(),
            source_ref: "refs/tags/v0.4.0-alpha.3".to_owned(),
            repository: RELEASE_REPOSITORY.to_owned(),
            repository_id: RELEASE_REPOSITORY_ID,
            workflow: RELEASE_WORKFLOW.to_owned(),
            workflow_name: RELEASE_WORKFLOW_NAME.to_owned(),
            issuer: RELEASE_ISSUER.to_owned(),
            runner_environment: RELEASE_RUNNER_ENVIRONMENT.to_owned(),
        }
    }

    #[test]
    fn real_github_alpha3_bundle_verifies_manifest_and_binary_subjects() {
        let identity = alpha3_identity();
        verify_github_attestations(
            ALPHA3_ATTESTATION.as_bytes(),
            "release-manifest.json",
            "8e79eb45fc28e052c5e4236f2ac7fa76e9e97797a94aa36700e8f7765a1e9c7c",
            &identity,
        )
        .unwrap();
        verify_github_attestations(
            ALPHA3_ATTESTATION.as_bytes(),
            "herdr-mcp-0.4.0-alpha.3-aarch64-apple-darwin",
            "4b8d8af421755f5a8e0230629f252b58b30ef6c1c546df7a57029b843f8ad867",
            &identity,
        )
        .unwrap();
    }

    #[test]
    fn real_bundle_fails_closed_on_source_repository_and_subject_mismatch() {
        let mut wrong_commit = alpha3_identity();
        wrong_commit.source_commit = "0".repeat(40);
        assert!(
            verify_github_attestations(
                ALPHA3_ATTESTATION.as_bytes(),
                "release-manifest.json",
                "8e79eb45fc28e052c5e4236f2ac7fa76e9e97797a94aa36700e8f7765a1e9c7c",
                &wrong_commit,
            )
            .is_err()
        );
        assert!(
            verify_github_attestations(
                ALPHA3_ATTESTATION.as_bytes(),
                "release-manifest.json",
                &"0".repeat(64),
                &alpha3_identity(),
            )
            .is_err()
        );
        assert!(
            verify_github_attestations(
                ALPHA3_ATTESTATION.as_bytes(),
                "release-manifest.json",
                "4b8d8af421755f5a8e0230629f252b58b30ef6c1c546df7a57029b843f8ad867",
                &alpha3_identity(),
            )
            .is_err()
        );
    }

    #[test]
    fn real_bundle_rejects_missing_transparency_log_and_tampered_dsse() {
        let mut missing_tlog: Value = serde_json::from_str(ALPHA3_ATTESTATION).unwrap();
        missing_tlog["attestations"][0]["bundle"]["verificationMaterial"]["tlogEntries"] =
            json!([]);
        assert!(
            verify_github_attestations(
                serde_json::to_string(&missing_tlog).unwrap().as_bytes(),
                "release-manifest.json",
                "8e79eb45fc28e052c5e4236f2ac7fa76e9e97797a94aa36700e8f7765a1e9c7c",
                &alpha3_identity(),
            )
            .is_err()
        );

        let mut tampered: Value = serde_json::from_str(ALPHA3_ATTESTATION).unwrap();
        let payload = tampered["attestations"][0]["bundle"]["dsseEnvelope"]["payload"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut bytes = payload.into_bytes();
        let index = bytes.len() / 2;
        bytes[index] = if bytes[index] == b'A' { b'B' } else { b'A' };
        tampered["attestations"][0]["bundle"]["dsseEnvelope"]["payload"] =
            json!(String::from_utf8(bytes).unwrap());
        assert!(
            verify_github_attestations(
                serde_json::to_string(&tampered).unwrap().as_bytes(),
                "release-manifest.json",
                "8e79eb45fc28e052c5e4236f2ac7fa76e9e97797a94aa36700e8f7765a1e9c7c",
                &alpha3_identity(),
            )
            .is_err()
        );
    }

    #[test]
    fn manifest_identity_is_exact_and_schema_bound() {
        let manifest = json!({
            "release_identity": {
                "tag": "v1.2.3",
                "source_commit": "a".repeat(40),
                "source_ref": "refs/tags/v1.2.3",
            },
            "repository_identity": {
                "repository": RELEASE_REPOSITORY,
                "repository_id": RELEASE_REPOSITORY_ID,
            },
            "provenance": {
                "predicate_type": SLSA_PROVENANCE_V1,
                "attestation": GITHUB_ARTIFACT_ATTESTATION,
                "bundle_media_type": SIGSTORE_BUNDLE_V03,
                "workflow": RELEASE_WORKFLOW,
                "workflow_name": RELEASE_WORKFLOW_NAME,
                "issuer": RELEASE_ISSUER,
                "runner_environment": RELEASE_RUNNER_ENVIRONMENT,
            },
        });
        let identity = parse_manifest_identity(&manifest, "v1.2.3").unwrap();
        assert_eq!(identity.source_commit, "a".repeat(40));
        let mut wrong = manifest.clone();
        wrong["repository_identity"]["repository"] = json!("attacker/fork");
        assert!(parse_manifest_identity(&wrong, "v1.2.3").is_err());
    }
}
