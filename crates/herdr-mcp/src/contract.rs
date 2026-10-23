use serde_json::Value;

/// Frozen runtime execution contract epoch 2. Never mutated in place: the
/// runtime execution contract evolves by shaping a new identity over this
/// baseline (see `RUNTIME_EXEC_V4_JSON`).
const EPOCH2_JSON: &str = include_str!("../../../contracts/epoch2.json");

/// Frozen runtime execution contract epoch 3 (native-default `herdr_exec`
/// description). Retained exactly as shipped: it is the previous rollback
/// baseline, and its identity is pinned by a deterministic hash regression so
/// the predecessor can never drift silently with the descriptor file. The Link
/// boundary owns the production rollback constants.
#[cfg(test)]
const RUNTIME_EXEC_V3_JSON: &str = include_str!("../../../contracts/runtime-exec-v3.json");

/// Frozen runtime execution contract epoch 4 (neutral `herdr_exec` capability
/// description plus the machine-decidable execution-evidence contract).
/// Retained exactly as shipped: it is now the immediately previous rollback
/// baseline, and its identity is pinned by a deterministic hash regression so
/// the predecessor can never drift silently with the descriptor file.
#[cfg(test)]
const RUNTIME_EXEC_V4_JSON: &str = include_str!("../../../contracts/runtime-exec-v4.json");

/// Current runtime execution contract epoch 5: the frozen epoch-2 catalog with
/// the epoch-4 `herdr_exec` execution description plus the two opt-in
/// `inputSchema.properties` entries (`intent`, `idempotency_key`) that make
/// durable idempotent replay discoverable. The descriptor is programmatic
/// (base + single-tool shape) instead of a duplicated 18-tool catalog, so a
/// metadata-only change never forks the tool list or the local method/state
/// machinery.
const RUNTIME_EXEC_V5_JSON: &str = include_str!("../../../contracts/runtime-exec-v5.json");

/// Descriptor of the current runtime execution contract.
const CURRENT_RUNTIME_EXEC_JSON: &str = RUNTIME_EXEC_V5_JSON;
/// Descriptor of the immediately previous runtime execution contract.
#[cfg(test)]
const PREVIOUS_RUNTIME_EXEC_JSON: &str = RUNTIME_EXEC_V4_JSON;
const DESCRIPTOR_NAME: &str = "runtime-exec-v5";

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ContractIdentity {
    pub epoch: u32,
    pub hash: String,
    pub tool_count: u32,
}

fn descriptor() -> Result<Value, String> {
    parse_descriptor(CURRENT_RUNTIME_EXEC_JSON)
}

#[cfg(test)]
fn previous_descriptor() -> Result<Value, String> {
    parse_descriptor(PREVIOUS_RUNTIME_EXEC_JSON)
}

fn parse_descriptor(source: &str) -> Result<Value, String> {
    serde_json::from_str(source)
        .map_err(|error| format!("cannot parse embedded {DESCRIPTOR_NAME} descriptor: {error}"))
}

fn descriptor_u64(descriptor: &Value, field: &str) -> Result<u64, String> {
    descriptor
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{DESCRIPTOR_NAME} descriptor is missing numeric {field}"))
}

fn descriptor_str<'a>(descriptor: &'a Value, field: &str) -> Result<&'a str, String> {
    descriptor
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{DESCRIPTOR_NAME} descriptor is missing string {field}"))
}

pub fn identity() -> Result<ContractIdentity, String> {
    identity_from_descriptor(&descriptor()?, tool_names().len())
}

/// Identity of the immediately previous runtime execution contract (frozen
/// epoch 4). The current binary never serves it; this accessor exists so the
/// frozen predecessor identity stays pinned by regression instead of drifting
/// silently with the descriptor file.
#[cfg(test)]
pub fn previous_identity() -> Result<ContractIdentity, String> {
    identity_from_descriptor(&previous_descriptor()?, tool_names().len())
}

fn identity_from_descriptor(
    descriptor: &Value,
    actual_tool_count: usize,
) -> Result<ContractIdentity, String> {
    let tool_count = u32::try_from(descriptor_u64(descriptor, "tool_count")?)
        .map_err(|_| format!("{DESCRIPTOR_NAME} descriptor tool_count is out of range"))?;
    let identity = ContractIdentity {
        epoch: u32::try_from(descriptor_u64(descriptor, "contract_epoch")?)
            .map_err(|_| format!("{DESCRIPTOR_NAME} descriptor contract_epoch is out of range"))?,
        hash: descriptor_str(descriptor, "contract_hash")?.to_owned(),
        tool_count,
    };
    if actual_tool_count as u32 != identity.tool_count {
        return Err(format!(
            "{DESCRIPTOR_NAME} tool count mismatch: descriptor={} base={actual_tool_count}",
            identity.tool_count
        ));
    }
    Ok(identity)
}

pub fn tool_names() -> Vec<&'static str> {
    EPOCH2_JSON
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let value = line.strip_prefix("\"name\": \"")?;
            value.strip_suffix("\",")
        })
        .collect()
}

/// Frozen runtime execution contract epoch 2 tools, exactly as recorded.
pub fn epoch2_tool_catalog() -> Result<Vec<Value>, String> {
    let document: Value = serde_json::from_str(EPOCH2_JSON)
        .map_err(|error| format!("cannot parse embedded epoch2 contract: {error}"))?;
    document
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| "embedded epoch2 contract is missing tools[]".to_owned())
}

/// Current runtime execution contract tool catalog: the frozen epoch-2 catalog
/// with the current descriptor's single-tool metadata shape applied.
pub fn tool_catalog() -> Result<Vec<Value>, String> {
    shaped_tool_catalog(&descriptor()?)
}

/// Previous runtime execution contract tool catalog (frozen epoch 4), shaped
/// from the same frozen baseline. Used by the identity/immutability regressions.
#[cfg(test)]
pub fn previous_tool_catalog() -> Result<Vec<Value>, String> {
    shaped_tool_catalog(&previous_descriptor()?)
}

fn shaped_tool_catalog(descriptor: &Value) -> Result<Vec<Value>, String> {
    let shape = descriptor
        .get("shape")
        .ok_or_else(|| format!("{DESCRIPTOR_NAME} descriptor is missing shape"))?;
    let tool_name = shape
        .get("tool")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{DESCRIPTOR_NAME} descriptor is missing shape.tool"))?;
    let description = shape
        .get("set")
        .and_then(|set| set.get("description"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{DESCRIPTOR_NAME} descriptor is missing shape.set.description"))?;
    // Optional minimal inputSchema property extension. A descriptor that only
    // reshapes the description stays valid; when it declares
    // `shape.inputSchema_properties`, exactly those named properties are set on
    // the target tool's `inputSchema.properties` and nothing else is touched.
    let property_extensions = shape.get("inputSchema_properties");
    if let Some(extensions) = property_extensions
        && !extensions.is_object()
    {
        return Err(format!(
            "{DESCRIPTOR_NAME} descriptor shape.inputSchema_properties must be an object"
        ));
    }
    let mut tools = epoch2_tool_catalog()?;
    let mut shaped = false;
    for tool in tools.iter_mut() {
        if tool.get("name").and_then(Value::as_str) == Some(tool_name) {
            let object = tool.as_object_mut().ok_or_else(|| {
                format!("{DESCRIPTOR_NAME} base tool {tool_name} is not an object")
            })?;
            object.insert(
                "description".to_owned(),
                Value::String(description.to_owned()),
            );
            if let Some(extensions) = property_extensions.and_then(Value::as_object) {
                let schema = object
                    .get_mut("inputSchema")
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| {
                        format!("{DESCRIPTOR_NAME} base tool {tool_name} has no inputSchema object")
                    })?;
                let properties = schema
                    .get_mut("properties")
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| {
                        format!(
                            "{DESCRIPTOR_NAME} base tool {tool_name} has no inputSchema.properties object"
                        )
                    })?;
                for (property, definition) in extensions {
                    properties.insert(property.clone(), definition.clone());
                }
            }
            shaped = true;
        }
    }
    if !shaped {
        return Err(format!(
            "{DESCRIPTOR_NAME} base contract is missing shape target {tool_name}"
        ));
    }
    Ok(tools)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::relay::contract::compute_contract_hash;
    use serde_json::json;

    #[test]
    fn embedded_contract_identity_is_runtime_exec_v5() {
        let identity = identity().unwrap();
        assert_eq!(identity.epoch, 5);
        assert_eq!(identity.tool_count, 18);
        assert_eq!(
            identity.hash,
            "sha256:fb1925844e873f12b609232890788c6297474433ac4bc7f003d73b1a3a60f030"
        );
        assert_eq!(
            compute_contract_hash(&tool_catalog().unwrap()).unwrap(),
            identity.hash
        );
    }

    #[test]
    fn previous_runtime_execution_contract_epoch_4_is_immutable() {
        let previous = previous_identity().unwrap();
        assert_eq!(previous.epoch, 4);
        assert_eq!(previous.tool_count, 18);
        assert_eq!(
            previous.hash,
            "sha256:1f4d272cedb3334b3e17e08080793f6ed81a03dccffba2f6434f149b10e2e135"
        );
        assert_eq!(
            compute_contract_hash(&previous_tool_catalog().unwrap()).unwrap(),
            previous.hash
        );
        // The epoch-2 baseline catalog itself stays frozen as well.
        assert_eq!(
            compute_contract_hash(&epoch2_tool_catalog().unwrap()).unwrap(),
            "sha256:7da23ad2ec8e7703d6380062126ba797218bde9e7711138c6b3e0ca6592efbf8"
        );
        // The frozen epoch-3 identity stays pinned by regression so the second
        // rollback baseline cannot drift silently either.
        let frozen_v3: Value = serde_json::from_str(RUNTIME_EXEC_V3_JSON).unwrap();
        assert_eq!(frozen_v3["contract_epoch"], 3);
        assert_eq!(
            frozen_v3["contract_hash"],
            "sha256:05350993b3e964ab28c8b586c3fdbffa5fa615025bc7f3e93eb6aa960c901fc5"
        );
    }

    #[test]
    fn current_v5_evolves_v4_by_opt_in_idempotency_metadata_only() {
        let previous = previous_tool_catalog().unwrap();
        let current = tool_catalog().unwrap();
        assert_eq!(previous.len(), current.len());
        assert_eq!(current.len(), 18);

        let mut changed = Vec::new();
        for (previous_tool, current_tool) in previous.iter().zip(current.iter()) {
            let name = previous_tool.get("name").and_then(Value::as_str).unwrap();
            assert_eq!(current_tool.get("name").and_then(Value::as_str), Some(name));
            if previous_tool != current_tool {
                changed.push(name.to_owned());
            }
        }
        assert_eq!(changed, vec!["herdr_exec".to_owned()]);

        let current_exec = current
            .iter()
            .find(|tool| tool["name"] == "herdr_exec")
            .unwrap();
        let previous_exec = previous
            .iter()
            .find(|tool| tool["name"] == "herdr_exec")
            .unwrap();
        // Exactly two additive inputSchema properties may differ.
        let previous_properties = previous_exec["inputSchema"]["properties"]
            .as_object()
            .unwrap();
        let current_properties = current_exec["inputSchema"]["properties"]
            .as_object()
            .unwrap();
        for (key, value) in previous_properties {
            assert_eq!(
                current_properties.get(key),
                Some(value),
                "{key} must stay frozen"
            );
        }
        let added = current_properties
            .keys()
            .filter(|key| !previous_properties.contains_key(*key))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            added,
            vec!["idempotency_key".to_owned(), "intent".to_owned()]
        );
        // The required list and every other tool field stay untouched.
        let mut expected = previous_exec.clone();
        expected["description"] = current_exec["description"].clone();
        expected["inputSchema"]["properties"] = current_exec["inputSchema"]["properties"].clone();
        assert_eq!(&expected, current_exec);
        // The opt-in vocabulary is discoverable and bounded.
        let intent = &current_exec["inputSchema"]["properties"]["intent"];
        assert_eq!(intent["type"], "string");
        assert_eq!(
            intent["enum"],
            json!(["read_only", "idempotent_write", "non_idempotent_write"])
        );
        let key = &current_exec["inputSchema"]["properties"]["idempotency_key"];
        assert_eq!(key["type"], "string");
        assert_eq!(key["minLength"], 1);
        assert_eq!(key["maxLength"], 256);
    }

    #[test]
    fn current_herdr_exec_description_carries_no_classifier_triggering_meta_wording() {
        let catalog = tool_catalog().unwrap();
        let exec = catalog
            .iter()
            .find(|tool| tool["name"] == "herdr_exec")
            .unwrap();
        let description = exec["description"].as_str().unwrap();
        for pattern in [
            "high-capability",
            "arbitrary commands",
            "NOT secret-path gated",
            "secret-path gated",
            "bypass",
            "do not",
            "never",
            "must be",
            "prefer ",
            "planner",
        ] {
            assert!(
                !description.to_lowercase().contains(&pattern.to_lowercase()),
                "herdr_exec description still contains {pattern:?}: {description}"
            );
        }
        // The real constraints stay stated as facts.
        assert!(description.contains("HERDR_MCP_READONLY"));
        assert!(description.contains("HERDR_MCP_WRITE_ROOTS"));
        assert!(description.contains("execution.started"));
        assert!(description.contains("exit_code"));
    }

    #[test]
    fn current_catalog_shapes_only_herdr_exec_description_over_frozen_epoch2() {
        let frozen = epoch2_tool_catalog().unwrap();
        let current = tool_catalog().unwrap();
        assert_eq!(frozen.len(), current.len());
        assert_eq!(current.len(), 18);

        let mut changed = 0usize;
        for (frozen_tool, current_tool) in frozen.iter().zip(current.iter()) {
            let name = frozen_tool.get("name").and_then(Value::as_str).unwrap();
            assert_eq!(current_tool.get("name").and_then(Value::as_str), Some(name));
            if frozen_tool == current_tool {
                continue;
            }
            changed += 1;
            assert_eq!(name, "herdr_exec");
            let mut expected = frozen_tool.clone();
            expected.as_object_mut().unwrap().insert(
                "description".to_owned(),
                current_tool["description"].clone(),
            );
            expected["inputSchema"]["properties"]["intent"] =
                current_tool["inputSchema"]["properties"]["intent"].clone();
            expected["inputSchema"]["properties"]["idempotency_key"] =
                current_tool["inputSchema"]["properties"]["idempotency_key"].clone();
            assert_eq!(
                &expected, current_tool,
                "only herdr_exec description and the two opt-in properties may change"
            );
        }
        assert_eq!(changed, 1, "exactly one tool metadata shape is applied");
        let frozen_exec = frozen
            .iter()
            .find(|tool| tool["name"] == "herdr_exec")
            .unwrap();
        let current_exec = current
            .iter()
            .find(|tool| tool["name"] == "herdr_exec")
            .unwrap();
        assert_ne!(frozen_exec["description"], current_exec["description"]);
    }

    #[test]
    fn embedded_contract_contains_all_public_tools() {
        let names = tool_names();
        assert_eq!(names.len(), 18);
        assert!(names.contains(&"herdr_inspect"));
        assert!(names.contains(&"herdr_skill"));
        assert!(names.contains(&"herdr_prompt"));
        assert_eq!(tool_catalog().unwrap().len(), 18);
    }
}
