use crate::fs_tools;
use serde_json::{Value, json};

const MODEL_VISIBLE_ADVISORY_KEYS: &[&str] = &[
    "hint",
    "retry_hint",
    "idempotency_hint",
    "task_hint",
    "pairing_hint",
    "revoke_hint",
    "next_action",
    "next_surface",
    "recovery",
    "instructions",
];

const MODEL_VISIBLE_OPAQUE_KEYS: &[&str] = &[
    "output",
    "partial_output",
    "stdout",
    "stderr",
    "structured_output",
    "prompt",
    "command",
];

pub(super) fn model_visible_tool_output(name: &str, arguments: &Value, output: Value) -> Value {
    let method = (name == "herdr_call")
        .then(|| arguments.get("method").and_then(Value::as_str))
        .flatten();
    let skill_surface = name == "herdr_skill"
        || method.is_some_and(|method| method.starts_with("herdr_mcp.skill."));
    let mut output = neutralize_model_visible_metadata(output, None);
    // Skill surfaces keep the policy bodies the caller explicitly requested
    // (`content`, including nested per-skill bodies); the generic advisory
    // neutralization above still applies to them. The marker stays for
    // consumers, now reporting that the requested text is model-visible.
    if skill_surface && let Some(object) = output.as_object_mut() {
        object.insert("reference_text_exposed".to_owned(), json!(true));
    }
    output
}

fn neutralize_model_visible_metadata(value: Value, parent_key: Option<&str>) -> Value {
    if parent_key.is_some_and(|key| MODEL_VISIBLE_OPAQUE_KEYS.contains(&key)) {
        return value;
    }
    match value {
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| neutralize_model_visible_metadata(value, None))
                .collect(),
        ),
        Value::Object(object) => {
            let mut visible = serde_json::Map::new();
            for (key, value) in object {
                if MODEL_VISIBLE_ADVISORY_KEYS.contains(&key.as_str()) || key.ends_with("_hint") {
                    continue;
                }
                let value = if MODEL_VISIBLE_OPAQUE_KEYS.contains(&key.as_str()) {
                    value
                } else {
                    neutralize_model_visible_metadata(value, Some(&key))
                };
                visible.insert(key, value);
            }
            Value::Object(visible)
        }
        other => other,
    }
}

pub(super) fn image_tool_result(image: fs_tools::ImageData) -> Value {
    let text = serde_json::to_string(&image.meta).unwrap_or_else(|_| "{}".to_owned());
    json!({
        "content": [
            {"type": "text", "text": text},
            {"type": "image", "data": image.data, "mimeType": image.mime_type}
        ]
    })
}

pub(super) fn tool_result(value: Value, is_error: bool) -> Value {
    let visible = neutralize_model_visible_metadata(value, None);
    let text = serde_json::to_string(&visible).unwrap_or_else(|_| "{}".to_owned());
    if is_error {
        json!({"content": [{"type": "text", "text": text}], "isError": true})
    } else {
        json!({"content": [{"type": "text", "text": text}]})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_visible_results_remove_advisory_prose_without_touching_process_output() {
        let visible = model_visible_tool_output(
            "herdr_exec",
            &json!({"workspace": "w1"}),
            json!({
                "ok": true,
                "hint": "do something next",
                "instructions": "use another tool",
                "nested": {
                    "retry_hint": "retry this way",
                    "next_surface": "herdr_call",
                    "recovery": {"action": "retry"},
                    "fact": "kept"
                },
                "output": "user stdout: do not rewrite me",
                "structured_output": {"hint": "user-owned-json", "value": 7}
            }),
        );
        assert!(visible.get("hint").is_none());
        assert!(visible.get("instructions").is_none());
        assert!(visible["nested"].get("retry_hint").is_none());
        assert!(visible["nested"].get("next_surface").is_none());
        assert!(visible["nested"].get("recovery").is_none());
        assert_eq!(visible["nested"]["fact"], "kept");
        assert_eq!(visible["output"], "user stdout: do not rewrite me");
        assert_eq!(
            visible["structured_output"],
            json!({"hint": "user-owned-json", "value": 7})
        );
    }

    #[test]
    fn skill_surfaces_keep_requested_policy_bodies_and_still_drop_advisory_prose() {
        let skill = model_visible_tool_output(
            "herdr_skill",
            &json!({}),
            json!({
                "ok": true,
                "content": "planner policy text",
                "hint": "load more policy",
                "instructions": "use another tool",
                "project_skill": {"origin": "bundled", "retry_hint": "retry this way"}
            }),
        );
        assert_eq!(skill["content"], "planner policy text");
        assert!(skill.get("hint").is_none());
        assert!(skill.get("instructions").is_none());
        assert_eq!(skill["project_skill"]["origin"], "bundled");
        assert!(skill["project_skill"].get("retry_hint").is_none());
        assert_eq!(skill["reference_text_exposed"], true);

        // Nested per-skill bodies in progressive load results are policy
        // documents the planner explicitly requested, so they stay verbatim.
        let loaded = model_visible_tool_output(
            "herdr_call",
            &json!({
                "method": "herdr_mcp.skill.load",
                "params": {"ids": ["git-repository", "files-search"]}
            }),
            json!({
                "ok": true,
                "method": "herdr_mcp.skill.load",
                "skills": [
                    {
                        "id": "git-repository",
                        "content": "## Git policy\ncommit by theme",
                        "next_action": "load more"
                    },
                    {"id": "files-search", "content": "## Search policy", "stream_hint": "x"}
                ],
                "recovery": {"action": "reload"}
            }),
        );
        assert_eq!(
            loaded["skills"][0]["content"],
            "## Git policy\ncommit by theme"
        );
        assert!(loaded["skills"][0].get("next_action").is_none());
        assert_eq!(loaded["skills"][1]["content"], "## Search policy");
        assert!(loaded["skills"][1].get("stream_hint").is_none());
        assert!(loaded.get("recovery").is_none());
        assert_eq!(loaded["reference_text_exposed"], true);

        // Non-skill callers never gain the skill marker.
        let exec = model_visible_tool_output(
            "herdr_exec",
            &json!({"workspace": "w1"}),
            json!({"ok": true, "hint": "do something next"}),
        );
        assert!(exec.get("reference_text_exposed").is_none());
    }
}
