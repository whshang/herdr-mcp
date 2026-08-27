use serde_json::{Map, Value, json};

const COMPACT_AFTER_BYTES: usize = 8_192;
const COMPACT_AFTER_LINES: usize = 80;
const HEAD_LINES: usize = 20;
const TAIL_LINES: usize = 40;

pub(crate) struct ExecCompact {
    pub text: String,
    pub counts: Value,
}

pub(crate) fn compact_successful_exec_output(text: &str) -> Option<ExecCompact> {
    let bytes = text.len();
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
}
