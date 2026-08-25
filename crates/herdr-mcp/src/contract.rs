const EPOCH2_JSON: &str = include_str!("../../../contracts/epoch2.json");

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ContractIdentity {
    pub epoch: u32,
    pub hash: String,
    pub tool_count: u32,
}

pub fn identity() -> Result<ContractIdentity, String> {
    let identity = ContractIdentity {
        epoch: parse_u32_field("contract_epoch")?,
        hash: parse_string_field("contract_hash")?,
        tool_count: parse_u32_field("tool_count")?,
    };
    let actual_tool_count = tool_names().len() as u32;
    if actual_tool_count != identity.tool_count {
        return Err(format!(
            "contract fixture tool count mismatch: header={} actual={actual_tool_count}",
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

fn parse_u32_field(field: &str) -> Result<u32, String> {
    let prefix = format!("\"{field}\": ");
    let value = EPOCH2_JSON
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(&prefix))
        .ok_or_else(|| format!("contract fixture is missing {field}"))?;
    value
        .trim_end_matches(',')
        .parse::<u32>()
        .map_err(|_| format!("contract fixture has invalid {field}"))
}

fn parse_string_field(field: &str) -> Result<String, String> {
    let prefix = format!("\"{field}\": \"");
    let value = EPOCH2_JSON
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(&prefix))
        .ok_or_else(|| format!("contract fixture is missing {field}"))?;
    value
        .trim_end_matches(',')
        .strip_suffix('"')
        .map(str::to_owned)
        .ok_or_else(|| format!("contract fixture has invalid {field}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_contract_identity_is_epoch2() {
        let identity = identity().unwrap();
        assert_eq!(identity.epoch, 2);
        assert_eq!(identity.tool_count, 18);
        assert_eq!(
            identity.hash,
            "sha256:7da23ad2ec8e7703d6380062126ba797218bde9e7711138c6b3e0ca6592efbf8"
        );
    }

    #[test]
    fn embedded_contract_contains_all_public_tools() {
        let names = tool_names();
        assert_eq!(names.len(), 18);
        assert!(names.contains(&"herdr_inspect"));
        assert!(names.contains(&"herdr_skill"));
        assert!(names.contains(&"herdr_prompt"));
    }
}
