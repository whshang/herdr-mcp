use serde_json::{Map, Value, json};

const COMPACT_AFTER_BYTES: usize = 8_192;
const COMPACT_AFTER_LINES: usize = 80;
const HEAD_LINES: usize = 20;
const TAIL_LINES: usize = 40;
const JSON_TABLE_MIN_SAVED_BYTES: usize = 4 * 1024;
const JSON_TABLE_MIN_SAVED_PERCENT: usize = 20;

pub(crate) struct ExecCompact {
    pub text: String,
    pub counts: Value,
}

pub(crate) fn compact_successful_exec_output(text: &str) -> Option<ExecCompact> {
    let bytes = text.len();
    if bytes > COMPACT_AFTER_BYTES
        && let Some(compact) = compact_uniform_json_object_array(text)
    {
        return Some(compact);
    }
    let lines: Vec<&str> = text.lines().collect();
    let line_count = lines.len();
    if bytes <= COMPACT_AFTER_BYTES && line_count <= COMPACT_AFTER_LINES {
        return None;
    }
    let keep = HEAD_LINES.saturating_add(TAIL_LINES);
    if line_count <= keep {
        return None;
    }
    let omitted_lines = line_count - keep;
    let mut compacted = String::new();
    for line in &lines[..HEAD_LINES] {
        compacted.push_str(line);
        compacted.push('\n');
    }
    compacted.push_str(&format!("\n…[omitted {omitted_lines} lines]…\n\n"));
    for line in &lines[line_count - TAIL_LINES..] {
        compacted.push_str(line);
        compacted.push('\n');
    }
    Some(ExecCompact {
        text: compacted,
        counts: json!({
            "lines": line_count,
            "bytes": bytes,
            "omitted_lines": omitted_lines,
        }),
    })
}

fn compact_uniform_json_object_array(text: &str) -> Option<ExecCompact> {
    let trimmed = text.trim();
    let inner = trimmed.strip_prefix('[')?.strip_suffix(']')?.trim();
    if !inner.starts_with('{') || !inner.ends_with('}') {
        return None;
    }

    let Value::Array(items) = serde_json::from_str::<Value>(trimmed).ok()? else {
        return None;
    };
    if items.len() < 2 {
        return None;
    }

    let columns = items
        .first()?
        .as_object()?
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    if columns.is_empty() {
        return None;
    }

    let item_count = items.len();
    let column_count = columns.len();
    let mut rows = Vec::with_capacity(item_count);
    for item in items {
        let Value::Object(mut object) = item else {
            return None;
        };
        if object.len() != column_count || !columns.iter().all(|key| object.contains_key(key)) {
            return None;
        }
        if !columns
            .iter()
            .all(|key| object.get(key).is_some_and(json_table_scalar_is_lossless))
        {
            return None;
        }
        rows.push(
            columns
                .iter()
                .map(|key| object.remove(key).expect("uniform key checked above"))
                .collect::<Vec<_>>(),
        );
    }

    let compacted = serde_json::to_string(&json!({
        "$herdr_table": {
            "encoding": "object-array/v1",
            "columns": columns,
            "rows": rows,
            "items": item_count,
        }
    }))
    .ok()?;
    let saved_bytes = text.len().saturating_sub(compacted.len());
    if saved_bytes < JSON_TABLE_MIN_SAVED_BYTES
        || saved_bytes.saturating_mul(100) < text.len().saturating_mul(JSON_TABLE_MIN_SAVED_PERCENT)
    {
        return None;
    }

    Some(ExecCompact {
        counts: json!({
            "lines": text.lines().count(),
            "bytes": text.len(),
            "compacted_bytes": compacted.len(),
            "saved_bytes": saved_bytes,
            "items": item_count,
            "columns": column_count,
            "strategy": "json_object_table_v1",
        }),
        text: compacted,
    })
}

fn json_table_scalar_is_lossless(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => true,
        Value::Number(number) => number.as_i64().is_some() || number.as_u64().is_some(),
        Value::Array(_) | Value::Object(_) => false,
    }
}

pub(crate) fn insert_compacted_or_raw(
    result: &mut Map<String, Value>,
    field: &str,
    text: &str,
    compact_allowed: bool,
) {
    if compact_allowed && let Some(compact) = compact_successful_exec_output(text) {
        result.insert(field.to_owned(), json!(compact.text));
        result.insert("counts".to_owned(), compact.counts);
        result.insert("compacted".to_owned(), json!(true));
        return;
    }
    result.insert(field.to_owned(), json!(text));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered_lines(count: usize) -> String {
        (0..count)
            .map(|index| format!("line-{index}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    }

    #[test]
    fn small_success_stays_verbatim() {
        let text = numbered_lines(12);
        assert!(compact_successful_exec_output(&text).is_none());
        let mut result = Map::new();
        insert_compacted_or_raw(&mut result, "text", &text, true);
        assert_eq!(result["text"], json!(text));
        assert!(result.get("compacted").is_none());
        assert!(result.get("counts").is_none());
    }

    #[test]
    fn large_success_keeps_head_and_tail() {
        let text = numbered_lines(90);
        let compact = compact_successful_exec_output(&text).unwrap();
        assert_eq!(compact.counts["lines"], 90);
        assert_eq!(compact.counts["bytes"], text.len());
        assert_eq!(compact.counts["omitted_lines"], 30);
        assert!(compact.text.contains("line-0\n"));
        assert!(compact.text.contains("line-19\n"));
        assert!(compact.text.contains("…[omitted 30 lines]…"));
        assert!(compact.text.contains("line-50\n"));
        assert!(compact.text.contains("line-89\n"));
        assert!(!compact.text.contains("line-40\n"));
        assert!(compact.text.len() < text.len());

        let mut result = Map::new();
        insert_compacted_or_raw(&mut result, "output", &text, true);
        assert_eq!(result["compacted"], true);
        assert_eq!(result["output"], json!(compact.text));
        assert_eq!(result["counts"]["omitted_lines"], 30);
        assert_ne!(result["output"], json!(text));
    }

    #[test]
    fn failure_and_truncated_keep_raw() {
        let text = numbered_lines(90);
        let mut failure = Map::new();
        insert_compacted_or_raw(&mut failure, "text", &text, false);
        assert_eq!(failure["text"], json!(text));
        assert!(failure.get("compacted").is_none());

        let mut truncated = Map::new();
        insert_compacted_or_raw(&mut truncated, "text", &text, false);
        assert_eq!(truncated["text"], json!(text));
        assert!(truncated.get("compacted").is_none());
        assert!(!text.contains("…[omitted"));
    }

    #[test]
    fn long_lines_below_line_budget_stay_raw_when_head_covers_all() {
        let text = "x".repeat(9_000);
        assert_eq!(text.lines().count(), 1);
        assert!(compact_successful_exec_output(&text).is_none());
    }

    #[test]
    fn large_uniform_json_array_round_trips_supported_scalars() {
        let original = Value::Array(
            (0..200)
                .map(|index| {
                    json!({
                        "id": index,
                        "name": format!("worker-{index:03}"),
                        "status": "idle",
                        "workspace": "w1",
                    })
                })
                .collect(),
        );
        let text = serde_json::to_string(&original).unwrap();
        assert!(text.len() > COMPACT_AFTER_BYTES);

        let compact = compact_successful_exec_output(&text).unwrap();
        assert_eq!(compact.counts["strategy"], "json_object_table_v1");
        assert_eq!(compact.counts["items"], 200);
        assert!(compact.text.len() < text.len());

        let encoded: Value = serde_json::from_str(&compact.text).unwrap();
        let table = &encoded["$herdr_table"];
        let columns = table["columns"].as_array().unwrap();
        let rows = table["rows"].as_array().unwrap();
        let restored = Value::Array(
            rows.iter()
                .map(|row| {
                    let row = row.as_array().unwrap();
                    Value::Object(
                        columns
                            .iter()
                            .zip(row)
                            .map(|(key, value)| (key.as_str().unwrap().to_owned(), value.clone()))
                            .collect(),
                    )
                })
                .collect(),
        );
        assert_eq!(restored, original);
    }

    #[test]
    fn heterogeneous_or_non_object_json_arrays_stay_raw() {
        let heterogeneous = serde_json::to_string(
            &(0..200)
                .map(|index| {
                    if index == 100 {
                        json!({"id": index, "different": true, "padding": "x".repeat(80)})
                    } else {
                        json!({"id": index, "name": "x".repeat(80)})
                    }
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(heterogeneous.len() > COMPACT_AFTER_BYTES);
        assert!(compact_successful_exec_output(&heterogeneous).is_none());

        let scalars = serde_json::to_string(&vec!["x".repeat(100); 100]).unwrap();
        assert!(scalars.len() > COMPACT_AFTER_BYTES);
        assert!(compact_successful_exec_output(&scalars).is_none());

        let unsafe_numbers = (0..200)
            .map(|index| format!("{{\"id\":184467440737095516160,\"name\":\"worker-{index:03}\"}}"))
            .collect::<Vec<_>>()
            .join(",");
        let unsafe_numbers = format!("[{unsafe_numbers}]");
        assert!(unsafe_numbers.len() > COMPACT_AFTER_BYTES);
        assert!(compact_successful_exec_output(&unsafe_numbers).is_none());
    }

    #[test]
    fn marginal_json_table_savings_keep_original_shape() {
        let text = serde_json::to_string(
            &(0..24)
                .map(|index| json!({"id": index, "payload": "x".repeat(500)}))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(text.len() > COMPACT_AFTER_BYTES);
        assert!(compact_uniform_json_object_array(&text).is_none());
        assert!(compact_successful_exec_output(&text).is_none());
    }
}
