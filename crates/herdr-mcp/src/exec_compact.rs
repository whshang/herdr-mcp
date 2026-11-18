use serde_json::{Map, Value, json};

// Runtime thresholds (#1010 1c): an ordinary medium-length test run (up to a
// few hundred lines / tens of KiB) is more useful verbatim than compacted, and
// a compaction that costs a follow-up read is a net loss.
const COMPACT_AFTER_BYTES: usize = 32_768;
const COMPACT_AFTER_LINES: usize = 200;
const HEAD_LINES: usize = 40;
const TAIL_LINES: usize = 120;
/// Context kept on each side of an error-stack marker.
const ERROR_WINDOW_RADIUS: usize = 24;
/// Upper bound for one preserved error window, so a runaway backtrace cannot
/// make the compacted output as large as the original.
const ERROR_WINDOW_MAX_LINES: usize = 160;
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
    // #1010 1c: an error/stack block in the omitted middle is the one part a
    // caller always needs next. Preserve it whole instead of cutting it away.
    if let Some(window) = error_stack_window(&lines) {
        return Some(compact_with_error_window(&lines, window, bytes));
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

/// One line that marks a runtime/compile error, or one of its stack frames.
fn is_error_stack_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    line.contains("panicked at")
        || line.contains("stack backtrace:")
        || line.contains("Traceback (most recent call last):")
        || trimmed.starts_with("error[E")
        || trimmed.starts_with("Error:")
        || trimmed.starts_with("error:")
        // JS/TS stack frames, Python `File "...", line N`, Rust backtrace frames.
        || trimmed.starts_with("at ")
        || trimmed.starts_with("File \"")
        || trimmed
            .split_once(": ")
            .is_some_and(|(frame, _)| frame.chars().all(|ch| ch.is_ascii_digit()) && !frame.is_empty())
}

/// The line range to preserve verbatim around an error stack, or `None` when the
/// text carries no such marker.
fn error_stack_window(lines: &[&str]) -> Option<(usize, usize)> {
    let mut first: Option<usize> = None;
    let mut last: Option<usize> = None;
    for (index, line) in lines.iter().enumerate() {
        if !is_error_stack_line(line) {
            continue;
        }
        first.get_or_insert(index);
        last = Some(index);
    }
    let (first, last) = (first?, last?);
    let start = first.saturating_sub(ERROR_WINDOW_RADIUS);
    let end = (last + ERROR_WINDOW_RADIUS + 1).min(lines.len());
    if end.saturating_sub(start) <= ERROR_WINDOW_MAX_LINES {
        return Some((start, end));
    }
    // Runaway backtrace: keep the top of the stack, which names the failure.
    Some((start, start + ERROR_WINDOW_MAX_LINES))
}

/// Head, the error window and the tail, with every gap reported exactly.
fn compact_with_error_window(lines: &[&str], window: (usize, usize), bytes: usize) -> ExecCompact {
    let line_count = lines.len();
    let mut ranges = vec![
        (0usize, HEAD_LINES.min(line_count)),
        (window.0.min(line_count), window.1.min(line_count)),
        (line_count.saturating_sub(TAIL_LINES), line_count),
    ];
    ranges.retain(|(start, end)| start < end);
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some((_, last_end)) if start <= *last_end => *last_end = (*last_end).max(end),
            _ => merged.push((start, end)),
        }
    }
    let kept: usize = merged.iter().map(|(start, end)| end - start).sum();
    let mut compacted = String::new();
    let mut cursor = 0usize;
    for (start, end) in merged {
        if start > cursor {
            compacted.push_str(&format!("\n…[omitted {} lines]…\n\n", start - cursor));
        }
        for line in &lines[start..end] {
            compacted.push_str(line);
            compacted.push('\n');
        }
        cursor = end;
    }
    if cursor < line_count {
        compacted.push_str(&format!("\n…[omitted {} lines]…\n\n", line_count - cursor));
    }
    ExecCompact {
        text: compacted,
        counts: json!({
            "lines": line_count,
            "bytes": bytes,
            "omitted_lines": line_count - kept,
        }),
    }
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
        let text = numbered_lines(260);
        let compact = compact_successful_exec_output(&text).unwrap();
        assert_eq!(compact.counts["lines"], 260);
        assert_eq!(compact.counts["bytes"], text.len());
        assert_eq!(compact.counts["omitted_lines"], 100);
        assert!(compact.text.contains("line-0\n"));
        assert!(compact.text.contains("line-39\n"));
        assert!(compact.text.contains("…[omitted 100 lines]…"));
        assert!(compact.text.contains("line-140\n"));
        assert!(compact.text.contains("line-259\n"));
        assert!(!compact.text.contains("line-40\n"));
        assert!(compact.text.len() < text.len());

        let mut result = Map::new();
        insert_compacted_or_raw(&mut result, "output", &text, true);
        assert_eq!(result["compacted"], true);
        assert_eq!(result["output"], json!(compact.text));
        assert_eq!(result["counts"]["omitted_lines"], 100);
        assert_ne!(result["output"], json!(text));
    }

    #[test]
    fn failure_and_truncated_keep_raw() {
        let text = numbered_lines(260);
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

    /// user agent | Given an ordinary medium-length log (81-200 lines) | When
    /// the runtime decides whether to compact | Then it stays verbatim: a
    /// compaction that costs a follow-up read is a net loss (#1010 1c).
    #[test]
    fn medium_log_under_new_threshold_stays_verbatim() {
        let text = numbered_lines(200);
        assert!(text.len() < COMPACT_AFTER_BYTES);
        assert!(compact_successful_exec_output(&text).is_none());

        // One line over the new budget still compacts.
        let over = numbered_lines(201);
        let compact = compact_successful_exec_output(&over).unwrap();
        assert_eq!(compact.counts["lines"], 201);
        assert_eq!(compact.counts["omitted_lines"], 201 - 160);
    }

    /// user agent | Given a long log whose middle holds an error stack | When it
    /// is compacted | Then the stack block survives line-for-line instead of
    /// being cut away with the omitted middle.
    #[test]
    fn error_stack_in_the_omitted_middle_is_preserved_verbatim() {
        for (marker, frames) in [
            (
                "thread 'worker' panicked at src/main.rs:42:9:",
                vec![
                    "stack backtrace:",
                    "   0: std::panicking::begin_panic",
                    "   1: herdr_mcp::exec::run",
                ],
            ),
            (
                "Traceback (most recent call last):",
                vec![
                    "  File \"/app/main.py\", line 12, in <module>",
                    "    run()",
                    "ValueError: boom",
                ],
            ),
            (
                "Error: request failed",
                vec!["    at handler (/app/src/server.ts:88:11)"],
            ),
            (
                "error[E0308]: mismatched types",
                vec!["  --> src/lib.rs:9:5", "   = note: expected `u8`"],
            ),
        ] {
            let mut lines: Vec<String> = (0..600).map(|i| format!("step-{i}")).collect();
            let marker_at = 300;
            lines[marker_at] = marker.to_owned();
            for (offset, frame) in frames.iter().enumerate() {
                lines[marker_at + 1 + offset] = (*frame).to_owned();
            }
            let text = lines.join("\n") + "\n";

            let compact = compact_successful_exec_output(&text).unwrap();
            let expected: Vec<&str> = std::iter::once(marker)
                .chain(frames.iter().copied())
                .collect();
            for line in expected {
                assert!(
                    compact.text.contains(line),
                    "stack line lost for {marker}: {line}"
                );
            }
            let kept = compact
                .text
                .lines()
                .filter(|line| !line.is_empty() && !line.starts_with("…[omitted"))
                .count();
            assert_eq!(compact.counts["omitted_lines"], 600 - kept);
            assert!(compact.text.len() < text.len());

            let mut result = Map::new();
            insert_compacted_or_raw(&mut result, "output", &text, true);
            assert_eq!(result["compacted"], true);
            assert_eq!(result["counts"]["omitted_lines"], 600 - kept);
        }
    }

    /// user agent | Given a runaway backtrace | When it is compacted | Then the
    /// preserved window is bounded, the top of the stack survives, and the
    /// omitted count stays exact.
    #[test]
    fn runaway_backtrace_window_is_bounded() {
        let mut lines: Vec<String> = (0..4_000).map(|i| format!("filler-{i}")).collect();
        lines[2_000] = "Traceback (most recent call last):".to_owned();
        for offset in 1..500 {
            lines[2_000 + offset] =
                format!("  File \"/app/mod.py\", line {offset}, in step{offset}");
        }
        let text = lines.join("\n") + "\n";
        let compact = compact_successful_exec_output(&text).unwrap();
        assert!(compact.text.contains("Traceback (most recent call last):"));
        let kept = compact
            .text
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with("…[omitted"))
            .count();
        assert!(
            kept <= HEAD_LINES + TAIL_LINES + ERROR_WINDOW_MAX_LINES,
            "kept {kept} exceeds the bounded window"
        );
        assert_eq!(compact.counts["omitted_lines"], 4_000 - kept);
    }

    #[test]
    fn large_uniform_json_array_round_trips_supported_scalars() {
        let original = Value::Array(
            (0..1_200)
                .map(|index| {
                    json!({
                        "id": index,
                        "name": format!("worker-{index:04}"),
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
        assert_eq!(compact.counts["items"], 1_200);
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
            &(0..500)
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

        let scalars = serde_json::to_string(&vec!["x".repeat(200); 400]).unwrap();
        assert!(scalars.len() > COMPACT_AFTER_BYTES);
        assert!(compact_successful_exec_output(&scalars).is_none());

        let unsafe_numbers = (0..1_200)
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
            &(0..70)
                .map(|index| json!({"id": index, "payload": "x".repeat(500)}))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(text.len() > COMPACT_AFTER_BYTES);
        assert!(compact_uniform_json_object_array(&text).is_none());
        assert!(compact_successful_exec_output(&text).is_none());
    }
}
