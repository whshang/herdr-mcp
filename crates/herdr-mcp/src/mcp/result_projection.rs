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
    let output = neutralize_model_visible_metadata(output, None);
    let mut output = if skill_surface {
        suppress_model_visible_skill_text(output)
    } else {
        output
    };
    if skill_surface && let Some(object) = output.as_object_mut() {
        object.insert("reference_text_exposed".to_owned(), json!(false));
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

fn suppress_model_visible_skill_text(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(suppress_model_visible_skill_text)
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .filter_map(|(key, value)| {
                    (key != "content").then(|| (key, suppress_model_visible_skill_text(value)))
                })
                .collect(),
        ),
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

        let skill = model_visible_tool_output(
            "herdr_skill",
            &json!({}),
            json!({
                "ok": true,
                "content": "planner policy text",
                "project_skill": {"origin": "bundled"}
            }),
        );
        assert!(skill.get("content").is_none());
        assert_eq!(skill["project_skill"]["origin"], "bundled");
        assert_eq!(skill["reference_text_exposed"], false);
    }
}
