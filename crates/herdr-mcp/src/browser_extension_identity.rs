use serde::Deserialize;

const STORE_CONTRACT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/browser-extension-store.json"
));

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct BrowserExtensionIdentity {
    pub extension_id: String,
    pub origin: String,
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

pub fn official_store_identity() -> Result<BrowserExtensionIdentity, String> {
    let contract: StoreContract = serde_json::from_str(STORE_CONTRACT)
        .map_err(|error| format!("browser extension store contract is invalid JSON: {error}"))?;
    if contract.schema_version != 1 {
        return Err(format!(
            "unsupported browser extension store contract schema {}",
            contract.schema_version
        ));
    }
    let extension_id = contract.chrome_web_store.extension_id.trim();
    validate_chromium_extension_id(extension_id)?;
    Ok(BrowserExtensionIdentity {
        extension_id: extension_id.to_owned(),
        origin: format!("chrome-extension://{extension_id}/"),
    })
}

fn validate_chromium_extension_id(id: &str) -> Result<(), String> {
    if id.len() != 32 || !id.bytes().all(|byte| (b'a'..=b'p').contains(&byte)) {
        return Err(
            "browser extension store contract has an invalid Chromium extension id".to_owned(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_contract_yields_a_valid_chromium_origin() {
        let identity = official_store_identity().unwrap();
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
}
