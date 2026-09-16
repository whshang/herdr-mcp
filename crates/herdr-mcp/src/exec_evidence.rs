//! Machine-decidable execution evidence for `herdr_exec`-family results.
//!
//! One owner for two questions a caller must be able to answer without prose:
//!
//! 1. Is the failure attributable to the child process, or was the request
//!    rejected before anything ran? (`execution` + `failure_origin`)
//! 2. Did the command produce exactly one JSON object? (`structured_output`)
//!
//! The projection is deliberately additive: existing fields (`ok`, `code`,
//! `phase`, `exit_code`, `output`, `delivery_state`, …) keep their meaning, so
//! older callers are unaffected.
//!
//! `execution.started` is only ever a proven claim. It is `true` when an exec
//! session was created (native child or utility-pane run), `false` when the
//! Herdr control plane refused the request before delivery, and `null` when the
//! start outcome is genuinely uncertain — never `false`, because a
//! transport/control-plane blip after send must not be misclassified as
//! "nothing ran". `execution` is absent when the call never reached the exec
//! backend at all (argument, topology, or project-activity refusal).
//!
//! `failure_origin` is present exactly when `execution` is present and the call
//! failed. `child_process` means the child started and ended without a zero
//! exit. `herdr_control_plane` means the Herdr control plane rejected the
//! request before delivery (with `delivery_state=not_delivered` retained).
//! `timeout` means the bounded wait elapsed while the child kept running
//! (`started=true`, `completed=false`, not a child failure). `unknown` means the
//! start outcome could not be established.

use serde_json::{Map, Value, json};

pub const FAILURE_ORIGIN_CHILD_PROCESS: &str = "child_process";
pub const FAILURE_ORIGIN_HERDR_CONTROL_PLANE: &str = "herdr_control_plane";
pub const FAILURE_ORIGIN_TIMEOUT: &str = "timeout";
pub const FAILURE_ORIGIN_UNKNOWN: &str = "unknown";

/// Upper bound for a `structured_output` candidate. The synchronous
/// `herdr_exec` read already uses this bound; a larger payload is not proven
/// complete and therefore never projected.
pub const STRUCTURED_OUTPUT_MAX_BYTES: usize = 65_536;

/// Evidence that a child/session was created and how it ended.
pub fn session_started(completed: bool, exit_code: Option<i64>) -> Value {
    json!({"started": true, "completed": completed, "exit_code": exit_code})
}

/// Evidence that the request was refused before any delivery.
pub fn rejected_before_start() -> Value {
    json!({"started": false, "completed": false, "exit_code": null})
}

/// Evidence that the start outcome could not be established.
pub fn start_uncertain() -> Value {
    json!({"started": null, "completed": false, "exit_code": null})
}

/// Insert evidence for a completed synchronous exec.
///
/// `exit_code` is the observed wait status. A zero exit code is success; a
/// non-zero exit code, or `None` for a child that ended without a normal exit
/// code (for example a signal), is a child-process failure.
pub fn insert_completed_evidence(result: &mut Map<String, Value>, exit_code: Option<i64>) {
    result.insert("execution".to_owned(), session_started(true, exit_code));
    if exit_code == Some(0) {
        return;
    }
    // Non-zero exit, or no exit status for a signalled child: either way the
    // child process started and ended, so the failure originates there rather
    // than in the Herdr control plane.
    result.insert(
        "failure_origin".to_owned(),
        json!(FAILURE_ORIGIN_CHILD_PROCESS),
    );
}

/// Insert evidence for a bounded-wait timeout. The child is still running.
pub fn insert_timeout_evidence(result: &mut Map<String, Value>) {
    result.insert("execution".to_owned(), session_started(false, None));
    result.insert("failure_origin".to_owned(), json!(FAILURE_ORIGIN_TIMEOUT));
}

/// Insert evidence for a pre-delivery Herdr control-plane refusal.
pub fn insert_control_plane_rejection(result: &mut Map<String, Value>) {
    result.insert("execution".to_owned(), rejected_before_start());
    result.insert(
        "failure_origin".to_owned(),
        json!(FAILURE_ORIGIN_HERDR_CONTROL_PLANE),
    );
}

/// Insert evidence for a start whose outcome is uncertain. Never claims
/// `started=false`.
pub fn insert_uncertain_start(result: &mut Map<String, Value>) {
    result.insert("execution".to_owned(), start_uncertain());
    result.insert("failure_origin".to_owned(), json!(FAILURE_ORIGIN_UNKNOWN));
}

/// Conservative structured-output projection.
///
/// Accepted only when the *complete* bounded content of a stream, after
/// trimming surrounding ASCII whitespace, is exactly one JSON object. Arrays,
/// scalars, JSONL/multiple values, and any surrounding noise are rejected
/// because serde does not allow trailing content and the value must be an
/// object. The original `output`/`text` field is always preserved.
pub fn structured_output_from_text(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.len() > STRUCTURED_OUTPUT_MAX_BYTES {
        return None;
    }
    if !trimmed.starts_with('{') {
        return None;
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(value @ Value::Object(_)) => Some(value),
        _ => None,
    }
}

/// Stream preference for [`project_structured_output`]: stdout wins when both
/// streams each hold exactly one JSON object. `stdout`/`stderr` are the
/// *complete* bounded contents of each stream.
pub fn select_structured_output(stdout: &str, stderr: &str) -> Option<(Value, &'static str)> {
    if let Some(value) = structured_output_from_text(stdout) {
        return Some((value, "stdout"));
    }
    structured_output_from_text(stderr).map(|value| (value, "stderr"))
}

/// Insert `structured_output` when one stream carries exactly one JSON object.
pub fn insert_structured_output(result: &mut Map<String, Value>, stdout: &str, stderr: &str) {
    if let Some((value, stream)) = select_structured_output(stdout, stderr) {
        result.insert("structured_output".to_owned(), value);
        result.insert("structured_output_stream".to_owned(), json!(stream));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_zero_exit_is_success_without_failure_origin() {
        let mut result = Map::new();
        insert_completed_evidence(&mut result, Some(0));
        assert_eq!(
            result["execution"],
            json!({"started": true, "completed": true, "exit_code": 0})
        );
        assert!(result.get("failure_origin").is_none());
    }

    #[test]
    fn completed_non_zero_exit_is_attributed_to_the_child_process() {
        let mut result = Map::new();
        insert_completed_evidence(&mut result, Some(2));
        assert_eq!(
            result["execution"],
            json!({"started": true, "completed": true, "exit_code": 2})
        );
        assert_eq!(result["failure_origin"], FAILURE_ORIGIN_CHILD_PROCESS);
    }

    #[test]
    fn signalled_child_is_a_child_process_failure() {
        let mut result = Map::new();
        insert_completed_evidence(&mut result, None);
        assert_eq!(
            result["execution"],
            json!({"started": true, "completed": true, "exit_code": null})
        );
        assert_eq!(result["failure_origin"], FAILURE_ORIGIN_CHILD_PROCESS);
    }

    #[test]
    fn timeout_is_never_a_child_or_control_plane_failure() {
        let mut result = Map::new();
        insert_timeout_evidence(&mut result);
        assert_eq!(
            result["execution"],
            json!({"started": true, "completed": false, "exit_code": null})
        );
        assert_eq!(result["failure_origin"], FAILURE_ORIGIN_TIMEOUT);
    }

    #[test]
    fn control_plane_rejection_is_not_started_and_not_uncertain() {
        let mut result = Map::new();
        insert_control_plane_rejection(&mut result);
        assert_eq!(
            result["execution"],
            json!({"started": false, "completed": false, "exit_code": null})
        );
        assert_eq!(result["failure_origin"], FAILURE_ORIGIN_HERDR_CONTROL_PLANE);
    }

    #[test]
    fn uncertain_start_never_claims_false() {
        let mut result = Map::new();
        insert_uncertain_start(&mut result);
        assert_eq!(result["execution"]["started"], Value::Null);
        assert_ne!(result["execution"]["started"], json!(false));
        assert_eq!(result["failure_origin"], FAILURE_ORIGIN_UNKNOWN);
    }

    #[test]
    fn structured_output_accepts_exactly_one_json_object() {
        let payload = r#"{"ok":false,"error":{"type":"validation","subtype":"invalid_argument","message":"flag needs an argument: --json"}}"#;
        assert_eq!(
            structured_output_from_text(payload),
            Some(serde_json::from_str::<Value>(payload).unwrap())
        );
        // Surrounding whitespace and pretty printing are still one object.
        assert!(structured_output_from_text(&format!("  {payload}\n")).is_some());
        assert!(structured_output_from_text("{\n  \"a\": 1\n}\n").is_some());
    }

    #[test]
    fn structured_output_rejects_arrays_scalars_jsonl_and_noise() {
        for rejected in [
            "",
            "   ",
            "[{\"a\":1}]",
            "[1,2,3]",
            "\"text\"",
            "42",
            "null",
            "true",
            "{\"a\":1}\n{\"b\":2}",
            "{\"a\":1}{\"b\":2}",
            "log line\n{\"a\":1}",
            "{\"a\":1}\ntrailing noise",
            "prefix {\"a\":1}",
            "{\"a\":1",
        ] {
            assert_eq!(
                structured_output_from_text(rejected),
                None,
                "must reject {rejected:?}"
            );
        }
    }

    #[test]
    fn structured_output_prefers_stdout_and_falls_back_to_stderr() {
        let stdout = r#"{"from":"stdout"}"#;
        let stderr = r#"{"from":"stderr"}"#;
        assert_eq!(
            select_structured_output(stdout, stderr),
            Some((serde_json::from_str::<Value>(stdout).unwrap(), "stdout"))
        );
        assert_eq!(
            select_structured_output("", stderr),
            Some((serde_json::from_str::<Value>(stderr).unwrap(), "stderr"))
        );
        assert_eq!(
            select_structured_output("not json\n", "also not json\n"),
            None
        );
        // A noisy stdout must not hide a clean stderr object.
        assert_eq!(
            select_structured_output("progress 1/2\n", stderr),
            Some((serde_json::from_str::<Value>(stderr).unwrap(), "stderr"))
        );
    }

    #[test]
    fn insert_structured_output_keeps_the_stream_provenance() {
        let mut result = Map::new();
        result.insert("output".to_owned(), json!("{\"a\":1}"));
        insert_structured_output(&mut result, "{\"a\":1}", "noise\n");
        assert_eq!(result["structured_output"], json!({"a": 1}));
        assert_eq!(result["structured_output_stream"], "stdout");
        // The original output is untouched.
        assert_eq!(result["output"], "{\"a\":1}");
    }
}
