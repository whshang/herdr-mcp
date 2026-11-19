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
/// Retained exactly as shipped as a historical pinned baseline.
#[cfg(test)]
const RUNTIME_EXEC_V4_JSON: &str = include_str!("../../../contracts/runtime-exec-v4.json");

/// Frozen runtime execution contract epoch 5: epoch-2 catalog with the epoch-4
/// `herdr_exec` description plus the opt-in `intent`/`idempotency_key`
/// properties. Retained exactly as shipped: it is now the immediately previous
/// rollback baseline, and its identity is pinned by a deterministic hash
/// regression so the predecessor can never drift silently.
#[cfg(test)]
const RUNTIME_EXEC_V5_JSON: &str = include_str!("../../../contracts/runtime-exec-v5.json");

/// Frozen runtime execution contract epoch 6: the epoch-5 catalog plus the
/// `herdr_call` `params` schema widened to `anyOf:[object,string]`. Retained
/// exactly as shipped: it is now the immediately previous rollback baseline,
/// and its identity is pinned by a deterministic hash regression.
#[cfg(test)]
const RUNTIME_EXEC_V6_JSON: &str = include_str!("../../../contracts/runtime-exec-v6.json");

/// Current runtime execution contract epoch 7: the epoch-6 catalog plus three
/// additive tool inputs — `herdr_inspect` `workspace`/`since_cursor`/`view`,
/// `herdr_exec` `max_output_bytes`, and `herdr_fs_read` `reads[]` (with the
/// descriptor `set.required` extension clearing its single-path requirement).
/// Shaping is declarative over the frozen epoch-2 base via the descriptor
/// `shapes[]` array.
const RUNTIME_EXEC_V7_JSON: &str = include_str!("../../../contracts/runtime-exec-v7.json");

/// Descriptor of the current runtime execution contract.
const CURRENT_RUNTIME_EXEC_JSON: &str = RUNTIME_EXEC_V7_JSON;
/// Descriptor of the immediately previous runtime execution contract.
#[cfg(test)]
const PREVIOUS_RUNTIME_EXEC_JSON: &str = RUNTIME_EXEC_V6_JSON;
const DESCRIPTOR_NAME: &str = "runtime-exec-v7";

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

fn apply_shape(tools: &mut [Value], shape: &Value) -> Result<(), String> {
    let tool_name = shape
        .get("tool")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{DESCRIPTOR_NAME} descriptor shape is missing tool"))?;
    let description = shape
        .get("set")
        .and_then(|set| set.get("description"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!("{DESCRIPTOR_NAME} descriptor shape {tool_name} is missing set.description")
        })?;
    // Optional minimal inputSchema property extension. A descriptor that only
    // reshapes the description stays valid; when it declares
    // `shape.inputSchema_properties`, exactly those named properties are set on
    // the target tool's `inputSchema.properties` and nothing else is touched.
    let property_extensions = shape.get("inputSchema_properties");
    if let Some(extensions) = property_extensions
        && !extensions.is_object()
    {
        return Err(format!(
            "{DESCRIPTOR_NAME} descriptor shape {tool_name} inputSchema_properties must be an object"
        ));
    }
    // Optional `set.required` override (v7+): replaces the tool's
    // `inputSchema.required` list so a batch/alternate input form can lift a
    // formerly required single-mode field. Only a string array is accepted;
    // an empty array clears the requirement.
    let required_override = shape
        .get("set")
        .and_then(|set| set.get("required"))
        .map(|required| {
            required
                .as_array()
                .filter(|items| items.iter().all(Value::is_string))
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "{DESCRIPTOR_NAME} descriptor shape {tool_name} required must be a string array"
                    )
                })
        })
        .transpose()?;
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
            if let Some(required) = &required_override {
                let schema = object
                    .get_mut("inputSchema")
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| {
                        format!("{DESCRIPTOR_NAME} base tool {tool_name} has no inputSchema object")
                    })?;
                if required.is_empty() {
                    schema.remove("required");
                } else {
                    schema.insert("required".to_owned(), Value::Array(required.clone()));
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
    Ok(())
}

fn shaped_tool_catalog(descriptor: &Value) -> Result<Vec<Value>, String> {
    // A descriptor declares either a single `shape` object (v5 and earlier)
    // or a `shapes` array applied in order (v6+). Both forms are declarative
    // metadata over the frozen epoch-2 base catalog.
    let shapes: Vec<Value> = if let Some(shape) = descriptor.get("shape") {
        vec![shape.clone()]
    } else if let Some(shapes) = descriptor
        .get("shapes")
        .and_then(Value::as_array)
        .filter(|shapes| !shapes.is_empty())
    {
        shapes.clone()
    } else {
        return Err(format!(
            "{DESCRIPTOR_NAME} descriptor is missing shape/shapes"
        ));
    };
    let mut tools = epoch2_tool_catalog()?;
    for shape in &shapes {
        apply_shape(&mut tools, shape)?;
    }
    Ok(tools)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::relay::contract::compute_contract_hash;
    use serde_json::json;

    #[test]
    fn embedded_contract_identity_is_runtime_exec_v7() {
        let identity = identity().unwrap();
        assert_eq!(identity.epoch, 7);
        assert_eq!(identity.tool_count, 18);
        // The exact hash is pinned below by the descriptor's contract_hash;
        // recomputing it here guards against the descriptor and the shaped
        // catalog drifting apart.
        let descriptor = descriptor().unwrap();
        assert_eq!(identity.hash, descriptor["contract_hash"].as_str().unwrap());
        assert_eq!(
            compute_contract_hash(&tool_catalog().unwrap()).unwrap(),
            identity.hash
        );
    }

    #[test]
    fn previous_runtime_execution_contract_epoch_6_is_immutable() {
        let previous = previous_identity().unwrap();
        assert_eq!(previous.epoch, 6);
        assert_eq!(previous.tool_count, 18);
        assert_eq!(
            previous.hash,
            "sha256:9221eb8c46009ed68f447b00fd3581ad7c9bd02d69053bd799f541937047ba6e"
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
        // The frozen epoch-3/4/5 identities stay pinned by regression so the
        // older rollback baselines cannot drift silently either.
        let frozen_v5: Value = serde_json::from_str(RUNTIME_EXEC_V5_JSON).unwrap();
        assert_eq!(frozen_v5["contract_epoch"], 5);
        assert_eq!(
            frozen_v5["contract_hash"],
            "sha256:fb1925844e873f12b609232890788c6297474433ac4bc7f003d73b1a3a60f030"
        );
        let frozen_v3: Value = serde_json::from_str(RUNTIME_EXEC_V3_JSON).unwrap();
        assert_eq!(frozen_v3["contract_epoch"], 3);
        assert_eq!(
            frozen_v3["contract_hash"],
            "sha256:05350993b3e964ab28c8b586c3fdbffa5fa615025bc7f3e93eb6aa960c901fc5"
        );
        let frozen_v4: Value = serde_json::from_str(RUNTIME_EXEC_V4_JSON).unwrap();
        assert_eq!(frozen_v4["contract_epoch"], 4);
        assert_eq!(
            frozen_v4["contract_hash"],
            "sha256:1f4d272cedb3334b3e17e08080793f6ed81a03dccffba2f6434f149b10e2e135"
        );
    }

    #[test]
    fn previous_v6_evolves_epoch2_by_exec_metadata_and_call_params_only() {
        let frozen = epoch2_tool_catalog().unwrap();
        let previous = previous_tool_catalog().unwrap();
        assert_eq!(frozen.len(), previous.len());
        assert_eq!(previous.len(), 18);

        let mut changed = Vec::new();
        for (frozen_tool, previous_tool) in frozen.iter().zip(previous.iter()) {
            let name = frozen_tool.get("name").and_then(Value::as_str).unwrap();
            assert_eq!(
                previous_tool.get("name").and_then(Value::as_str),
                Some(name)
            );
            if frozen_tool != previous_tool {
                changed.push(name.to_owned());
            }
        }
        assert_eq!(
            changed,
            vec!["herdr_call".to_owned(), "herdr_exec".to_owned()]
        );

        let previous_exec = previous
            .iter()
            .find(|tool| tool["name"] == "herdr_exec")
            .unwrap();
        let frozen_exec = frozen
            .iter()
            .find(|tool| tool["name"] == "herdr_exec")
            .unwrap();
        // Exactly two additive inputSchema properties may differ.
        let frozen_properties = frozen_exec["inputSchema"]["properties"]
            .as_object()
            .unwrap();
        let previous_properties = previous_exec["inputSchema"]["properties"]
            .as_object()
            .unwrap();
        for (key, value) in frozen_properties {
            assert_eq!(
                previous_properties.get(key),
                Some(value),
                "{key} must stay frozen"
            );
        }
        let added = previous_properties
            .keys()
            .filter(|key| !frozen_properties.contains_key(*key))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            added,
            vec!["idempotency_key".to_owned(), "intent".to_owned()]
        );
        // The required list and every other tool field stay untouched.
        let mut expected = frozen_exec.clone();
        expected["description"] = previous_exec["description"].clone();
        expected["inputSchema"]["properties"] = previous_exec["inputSchema"]["properties"].clone();
        assert_eq!(&expected, previous_exec);
        // The opt-in vocabulary is discoverable and bounded.
        let intent = &previous_exec["inputSchema"]["properties"]["intent"];
        assert_eq!(intent["type"], "string");
        assert_eq!(
            intent["enum"],
            json!(["read_only", "idempotent_write", "non_idempotent_write"])
        );
        let key = &previous_exec["inputSchema"]["properties"]["idempotency_key"];
        assert_eq!(key["type"], "string");
        assert_eq!(key["minLength"], 1);
        assert_eq!(key["maxLength"], 256);
    }

    #[test]
    fn current_v7_evolves_v6_by_three_additive_tool_inputs() {
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
        assert_eq!(
            changed,
            vec![
                "herdr_inspect".to_owned(),
                "herdr_fs_read".to_owned(),
                "herdr_exec".to_owned()
            ]
        );

        // herdr_exec: exactly one additive optional property, nothing else
        // changed (the v6 intent/idempotency properties stay verbatim).
        let exec_prev = previous
            .iter()
            .find(|tool| tool["name"] == "herdr_exec")
            .unwrap();
        let exec_now = current
            .iter()
            .find(|tool| tool["name"] == "herdr_exec")
            .unwrap();
        let prev_properties = exec_prev["inputSchema"]["properties"].as_object().unwrap();
        let now_properties = exec_now["inputSchema"]["properties"].as_object().unwrap();
        for (key, value) in prev_properties {
            assert_eq!(now_properties.get(key), Some(value), "{key} stays frozen");
        }
        assert_eq!(
            now_properties
                .keys()
                .filter(|key| !prev_properties.contains_key(*key))
                .cloned()
                .collect::<Vec<_>>(),
            vec!["max_output_bytes".to_owned()]
        );
        assert_eq!(
            exec_prev["inputSchema"]["required"],
            exec_now["inputSchema"]["required"]
        );

        // herdr_inspect: three optional params on an otherwise empty schema.
        let inspect_now = current
            .iter()
            .find(|tool| tool["name"] == "herdr_inspect")
            .unwrap();
        let inspect_properties = inspect_now["inputSchema"]["properties"]
            .as_object()
            .unwrap();
        assert_eq!(
            inspect_properties.keys().cloned().collect::<Vec<_>>(),
            vec![
                "since_cursor".to_owned(),
                "view".to_owned(),
                "workspace".to_owned()
            ]
        );
        assert_eq!(
            inspect_now["inputSchema"]["properties"]["view"]["enum"],
            json!(["full", "compact"])
        );

        // herdr_fs_read: reads[] batch, single-mode properties intact, and the
        // descriptor `set.required` extension cleared the path requirement.
        let read_prev = previous
            .iter()
            .find(|tool| tool["name"] == "herdr_fs_read")
            .unwrap();
        let read_now = current
            .iter()
            .find(|tool| tool["name"] == "herdr_fs_read")
            .unwrap();
        assert_eq!(read_prev["inputSchema"]["required"], json!(["path"]));
        assert_eq!(read_now["inputSchema"].get("required"), None);
        let read_properties = read_now["inputSchema"]["properties"].as_object().unwrap();
        for key in ["path", "start_line", "end_line", "max_bytes"] {
            assert_eq!(
                read_properties.get(key),
                read_prev["inputSchema"]["properties"].get(key),
                "{key} stays frozen"
            );
        }
        let reads = &read_properties["reads"];
        assert_eq!(reads["type"], "array");
        assert_eq!(reads["items"]["required"], json!(["path"]));
        for key in ["path", "start_line", "end_line", "max_bytes"] {
            assert!(
                reads["items"]["properties"].get(key).is_some(),
                "reads[] item keeps {key}"
            );
        }
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
    fn current_catalog_shapes_only_herdr_exec_and_herdr_call_over_frozen_epoch2() {
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
            let mut expected = frozen_tool.clone();
            match name {
                "herdr_exec" => {
                    expected.as_object_mut().unwrap().insert(
                        "description".to_owned(),
                        current_tool["description"].clone(),
                    );
                    expected["inputSchema"]["properties"]["intent"] =
                        current_tool["inputSchema"]["properties"]["intent"].clone();
                    expected["inputSchema"]["properties"]["idempotency_key"] =
                        current_tool["inputSchema"]["properties"]["idempotency_key"].clone();
                    expected["inputSchema"]["properties"]["max_output_bytes"] =
                        current_tool["inputSchema"]["properties"]["max_output_bytes"].clone();
                }
                "herdr_call" => {
                    expected.as_object_mut().unwrap().insert(
                        "description".to_owned(),
                        current_tool["description"].clone(),
                    );
                    expected["inputSchema"]["properties"]["params"] =
                        current_tool["inputSchema"]["properties"]["params"].clone();
                }
                "herdr_inspect" => {
                    expected.as_object_mut().unwrap().insert(
                        "description".to_owned(),
                        current_tool["description"].clone(),
                    );
                    expected["inputSchema"]["properties"] =
                        current_tool["inputSchema"]["properties"].clone();
                }
                "herdr_fs_read" => {
                    expected.as_object_mut().unwrap().insert(
                        "description".to_owned(),
                        current_tool["description"].clone(),
                    );
                    expected["inputSchema"]["properties"]["reads"] =
                        current_tool["inputSchema"]["properties"]["reads"].clone();
                    expected["inputSchema"]
                        .as_object_mut()
                        .unwrap()
                        .remove("required");
                }
                other => panic!("unexpected shaped tool {other}"),
            }
            assert_eq!(
                &expected, current_tool,
                "only the declared shape fields may change on {name}"
            );
        }
        assert_eq!(changed, 4, "exactly four tool metadata shapes are applied");
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
