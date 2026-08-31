use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const STORE_CONTRACT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/browser-extension-store.json"
));
const STANDALONE_CONTRACT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/browser-extension-standalone.json"
));

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BrowserExtensionIdentity {
    pub extension_id: String,
    pub origin: String,
    pub manifest_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StoreContract {
    schema_version: u32,
    chrome_web_store: ChromeWebStore,
}

#[derive(Debug, Deserialize)]
struct ChromeWebStore {
    extension_id: String,
}

#[derive(Debug, Deserialize)]
struct StandaloneContract {
    schema_version: u32,
    standalone: StandaloneIdentity,
}

#[derive(Debug, Deserialize)]
struct StandaloneIdentity {
    extension_id: String,
    manifest_key: String,
}

pub fn official_store_identity() -> Result<BrowserExtensionIdentity, String> {
    let contract: StoreContract = serde_json::from_str(STORE_CONTRACT)
        .map_err(|error| format!("browser extension store contract is invalid JSON: {error}"))?;
    if contract.schema_version != 1 {
        return Err(format!(
            "unsupported browser extension store contract schema {}",
            contract.schema_version
        ));
    }
    identity_from_id(&contract.chrome_web_store.extension_id, None, "store")
}

pub fn official_standalone_identity() -> Result<BrowserExtensionIdentity, String> {
    let contract: StandaloneContract =
        serde_json::from_str(STANDALONE_CONTRACT).map_err(|error| {
            format!("browser extension standalone contract is invalid JSON: {error}")
        })?;
    if contract.schema_version != 1 {
        return Err(format!(
            "unsupported browser extension standalone contract schema {}",
            contract.schema_version
        ));
    }
    let manifest_key = contract.standalone.manifest_key.trim();
    if manifest_key.is_empty() {
        return Err("browser extension standalone contract manifest_key is empty".to_owned());
    }
    let derived_id = chromium_extension_id_from_manifest_key(manifest_key)?;
    let configured_id = contract.standalone.extension_id.trim();
    validate_chromium_extension_id(configured_id, "standalone")?;
    if derived_id != configured_id {
        return Err(format!(
            "browser extension standalone contract key/id mismatch: derived {derived_id}, configured {configured_id}"
        ));
    }
    identity_from_id(configured_id, Some(manifest_key.to_owned()), "standalone")
}

fn identity_from_id(
    raw_id: &str,
    manifest_key: Option<String>,
    label: &str,
) -> Result<BrowserExtensionIdentity, String> {
    let extension_id = raw_id.trim();
    validate_chromium_extension_id(extension_id, label)?;
    Ok(BrowserExtensionIdentity {
        extension_id: extension_id.to_owned(),
        origin: format!("chrome-extension://{extension_id}/"),
        manifest_key,
    })
}

fn chromium_extension_id_from_manifest_key(manifest_key: &str) -> Result<String, String> {
    let der = BASE64_STANDARD
        .decode(manifest_key.as_bytes())
        .map_err(|error| {
            format!("browser extension standalone manifest_key is invalid base64: {error}")
        })?;
    if der.is_empty() {
        return Err("browser extension standalone manifest_key decodes to empty bytes".to_owned());
    }
    let digest = Sha256::digest(der);
    let mut id = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        id.push((b'a' + (byte >> 4)) as char);
        id.push((b'a' + (byte & 0x0f)) as char);
    }
    Ok(id)
}

fn validate_chromium_extension_id(id: &str, label: &str) -> Result<(), String> {
    if id.len() != 32 || !id.bytes().all(|byte| (b'a'..=b'p').contains(&byte)) {
        return Err(format!(
            "browser extension {label} contract has an invalid Chromium extension id"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_valid_identity(identity: &BrowserExtensionIdentity) {
        assert_eq!(identity.extension_id.len(), 32);
        assert!(
            identity
                .extension_id
                .bytes()
                .all(|byte| (b'a'..=b'p').contains(&byte))
        );
        assert_eq!(
            identity.origin,
            format!("chrome-extension://{}/", identity.extension_id)
        );
    }

    #[test]
    fn store_contract_yields_a_valid_chromium_origin() {
        let identity = official_store_identity().unwrap();
        assert_valid_identity(&identity);
        assert!(identity.manifest_key.is_none());
    }

    #[test]
    fn standalone_contract_key_derives_the_declared_identity() {
        let identity = official_standalone_identity().unwrap();
        assert_valid_identity(&identity);
        assert_eq!(identity.extension_id, "jbcjhnnmhaekdnbgfllpfipennppfedh");
        let key = identity.manifest_key.as_deref().unwrap();
        assert_eq!(
            chromium_extension_id_from_manifest_key(key).unwrap(),
            identity.extension_id
        );
        assert_ne!(
            identity.extension_id,
            official_store_identity().unwrap().extension_id
        );
    }
}
