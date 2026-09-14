use serde_json::Value;

/// Frozen runtime execution contract epoch 2. Never mutated in place: the
/// runtime execution contract evolves by shaping a new identity over this
/// baseline (see `RUNTIME_EXEC_V3_JSON`).
const EPOCH2_JSON: &str = include_str!("../../../contracts/epoch2.json");

/// Current runtime execution contract epoch 3: the frozen epoch-2 catalog with
/// only the `herdr_exec` execution description shaped to the native-default
/// semantics. The descriptor is programmatic (base + single-tool shape) instead
/// of a duplicated 18-tool catalog; the resulting identity is pinned by a
/// deterministic hash regression in `relay::contract`.
const RUNTIME_EXEC_V3_JSON: &str = include_str!("../../../contracts/runtime-exec-v3.json");

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ContractIdentity {
    pub epoch: u32,
    pub hash: String,
    pub tool_count: u32,
}

fn descriptor() -> Result<Value, String> {
    serde_json::from_str(RUNTIME_EXEC_V3_JSON)
        .map_err(|error| format!("cannot parse embedded runtime-exec-v3 descriptor: {error}"))
}

fn descriptor_u64(descriptor: &Value, field: &str) -> Result<u64, String> {
    descriptor
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("runtime-exec-v3 descriptor is missing numeric {field}"))
}

fn descriptor_str<'a>(descriptor: &'a Value, field: &str) -> Result<&'a str, String> {
    descriptor
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("runtime-exec-v3 descriptor is missing string {field}"))
}

pub fn identity() -> Result<ContractIdentity, String> {
    let descriptor = descriptor()?;
    let tool_count = u32::try_from(descriptor_u64(&descriptor, "tool_count")?)
        .map_err(|_| "runtime-exec-v3 descriptor tool_count is out of range".to_owned())?;
    let identity = ContractIdentity {
        epoch: u32::try_from(descriptor_u64(&descriptor, "contract_epoch")?)
            .map_err(|_| "runtime-exec-v3 descriptor contract_epoch is out of range".to_owned())?,
        hash: descriptor_str(&descriptor, "contract_hash")?.to_owned(),
        tool_count,
    };
    let actual_tool_count = tool_names().len() as u32;
    if actual_tool_count != identity.tool_count {
        return Err(format!(
            "runtime-exec-v3 tool count mismatch: descriptor={} base={actual_tool_count}",
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
/// with the descriptor's single-tool metadata shape applied.
pub fn tool_catalog() -> Result<Vec<Value>, String> {
    let descriptor = descriptor()?;
    let shape = descriptor
        .get("shape")
        .ok_or_else(|| "runtime-exec-v3 descriptor is missing shape".to_owned())?;
    let tool_name = shape
        .get("tool")
        .and_then(Value::as_str)
        .ok_or_else(|| "runtime-exec-v3 descriptor is missing shape.tool".to_owned())?;
    let description = shape
        .get("set")
        .and_then(|set| set.get("description"))
        .and_then(Value::as_str)
        .ok_or_else(|| "runtime-exec-v3 descriptor is missing shape.set.description".to_owned())?;
    let mut tools = epoch2_tool_catalog()?;
    let mut shaped = false;
    for tool in tools.iter_mut() {
        if tool.get("name").and_then(Value::as_str) == Some(tool_name) {
            let object = tool
                .as_object_mut()
                .ok_or_else(|| format!("runtime-exec-v3 base tool {tool_name} is not an object"))?;
            object.insert(
                "description".to_owned(),
                Value::String(description.to_owned()),
            );
            shaped = true;
        }
    }
    if !shaped {
        return Err(format!(
            "runtime-exec-v3 base contract is missing shape target {tool_name}"
        ));
    }
    Ok(tools)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_contract_identity_is_runtime_exec_v3() {
        let identity = identity().unwrap();
        assert_eq!(identity.epoch, 3);
        assert_eq!(identity.tool_count, 18);
        assert!(identity.hash.starts_with("sha256:"));
        assert_ne!(
            identity.hash,
            "sha256:7da23ad2ec8e7703d6380062126ba797218bde9e7711138c6b3e0ca6592efbf8"
        );
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
            assert_eq!(
                &expected, current_tool,
                "only herdr_exec.description may change"
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
