//! Model alias validation for agent starts (1.2 S5, P7).
//!
//! An explicit model alias must never silently fall back to another model.
//! Validation is per-agent and evidence-based: only agents with an obtainable
//! model list are validated (`pi` today, via its local model catalog); every
//! other agent reports `validated: false` and the request is refused before
//! any pane, agent, or socket mutation, because the runtime cannot prove the
//! alias would be honoured. A missing/empty catalog for a list-capable agent
//! is also `validated: false`, never a guess.
//!
//! The accepted alias forms mirror `pi --model`: `provider/model-id`, a bare
//! `model-id`, and an optional `:<thinking>` suffix. Matching is exact (no
//! glob/fuzzy expansion): a pattern that is not a listed model is refused so
//! the caller picks an explicit id instead of relying on CLI-side resolution.
//!
//! Environment note (S5 review P1-A): `pi --list-models` runs under the
//! service process's environment, not an interactive login shell's. A model
//! pi only exposes because a provider credential lives in the user's shell
//! environment may be absent from this list even though it works inside a
//! terminal pane; that absence is a `validated: false` refusal, not a
//! silently honoured alias.

use serde_json::{Value, json};

/// Outcome of validating one requested model alias for one agent kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelAliasCheck {
    /// The alias exactly matches the agent's obtainable model list.
    Validated { canonical: String },
    /// The alias is not in the obtainable list; carries the available list.
    Unknown { available: Vec<String> },
    /// No model list is obtainable for this agent (or its catalog is
    /// missing/empty). The alias cannot be validated either way.
    Unvalidatable,
}

/// Split one alias into the model pattern and optional thinking suffix.
/// Returns `None` for structurally invalid aliases (empty, oversized,
/// control/whitespace characters, more than one `:`, or an empty provider/id
/// segment).
fn split_alias(alias: &str) -> Option<(&str, Option<&str>)> {
    if alias.is_empty()
        || alias.len() > 256
        || alias
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return None;
    }
    let (pattern, thinking) = match alias.split_once(':') {
        Some((pattern, thinking)) => (pattern, Some(thinking)),
        None => (alias, None),
    };
    // `provider/id` splits on the FIRST slash only: pi model ids may contain
    // slashes themselves (magpie's `aliyun/auto/<model>`), and rejecting those
    // would make the validator disagree with `pi --list-models` (S5 review
    // P1-2). A bare id (no slash) stays valid; an empty provider or id is not.
    if pattern.is_empty() {
        return None;
    }
    if let Some((provider, id)) = pattern.split_once('/')
        && (provider.is_empty() || id.is_empty())
    {
        return None;
    }
    if thinking.is_some_and(|level| level.is_empty() || level.contains([':', '/'])) {
        return None;
    }
    Some((pattern, thinking))
}

/// Validate against an already-obtained list (pure; the production caller
/// supplies `capability_probe::pi_model_aliases()`).
pub fn check_alias_in_list(alias: &str, available: &[String]) -> ModelAliasCheck {
    let Some((pattern, thinking)) = split_alias(alias) else {
        return ModelAliasCheck::Unknown {
            available: available.to_vec(),
        };
    };
    if available.is_empty() {
        return ModelAliasCheck::Unvalidatable;
    }
    // Exact canonical match first, then the bare-id form pi also accepts.
    // A bare id that matches several providers is ambiguous: refuse it and
    // let the caller qualify the provider instead of guessing.
    let canonical = if available.iter().any(|candidate| candidate == pattern) {
        Some(pattern.to_owned())
    } else if !pattern.contains('/') {
        // A bare id matches the id part after the FIRST slash, mirroring how an
        // id may itself contain slashes.
        let mut matches = available.iter().filter(|candidate| {
            candidate
                .split_once('/')
                .is_some_and(|(_provider, id)| id == pattern)
        });
        match (matches.next(), matches.next()) {
            (Some(only), None) => Some(only.clone()),
            _ => None,
        }
    } else {
        None
    };
    match canonical {
        // The validated form is the one delivered to the agent: the listed
        // `provider/id`, carrying the caller's `:thinking` suffix verbatim so
        // the requested effort level survives provider qualification.
        Some(canonical) => ModelAliasCheck::Validated {
            canonical: match thinking {
                Some(level) => format!("{canonical}:{level}"),
                None => canonical,
            },
        },
        None => ModelAliasCheck::Unknown {
            available: available.to_vec(),
        },
    }
}

/// Per-agent validation entrypoint. Only `pi` has an obtainable list today
/// (`capability_scan` populates model evidence for pi alone); every other
/// kind is `Unvalidatable` without touching the filesystem.
pub fn validate_model_alias(kind: &str, alias: &str) -> ModelAliasCheck {
    if split_alias(alias).is_none() {
        // A malformed alias is refused with whatever list is obtainable;
        // for agents without a list the outcome stays Unvalidatable.
        return match obtainable_list(kind) {
            Some(list) => ModelAliasCheck::Unknown { available: list },
            None => ModelAliasCheck::Unvalidatable,
        };
    }
    match obtainable_list(kind) {
        Some(list) => check_alias_in_list(alias, &list),
        None => ModelAliasCheck::Unvalidatable,
    }
}

fn obtainable_list(kind: &str) -> Option<Vec<String>> {
    match kind {
        "pi" => crate::capability_probe::pi_model_aliases(),
        _ => None,
    }
}

/// The `model_validation` fact carried on a successful start or a refusal.
/// `validated` is true only when the alias matched an obtainable list; the
/// `source` distinguishes "list obtained, alias absent" from "no list".
pub fn validation_fact(kind: &str, alias: &str, check: &ModelAliasCheck) -> Value {
    match check {
        ModelAliasCheck::Validated { canonical } => json!({
            "validated": true,
            "kind": kind,
            "requested": alias,
            "canonical": canonical,
            "source": "obtainable_model_list",
        }),
        ModelAliasCheck::Unknown { .. } => json!({
            "validated": false,
            "kind": kind,
            "requested": alias,
            "source": "obtainable_model_list",
        }),
        ModelAliasCheck::Unvalidatable => json!({
            "validated": false,
            "kind": kind,
            "requested": alias,
            "source": "no_obtainable_model_list",
        }),
    }
}

/// Refusal envelope for an explicit model alias that cannot be honoured
/// provably. Shared by the raw `agent.start` forward and the spawn path so
/// both entrypoints return the same code and delivery semantics. Nothing
/// has been delivered to any agent when this is returned.
pub fn model_alias_refusal(kind: &str, alias: &str, check: &ModelAliasCheck) -> Value {
    let mut refusal = json!({
        "ok": false,
        "code": "model_alias_unknown",
        "kind": kind,
        "requested_model": alias,
        "delivery_state": "not_delivered",
        "submitted": false,
        "fallback_allowed": false,
        "retryable": false,
        "model_validation": validation_fact(kind, alias, check),
    });
    match check {
        ModelAliasCheck::Unknown { available } => {
            refusal["available_models"] = json!(available);
            refusal["message"] = json!(format!(
                "model alias '{alias}' is not in the obtainable model list for kind '{kind}'; pick one of available_models (provider/id). No fallback model was used."
            ));
        }
        ModelAliasCheck::Unvalidatable => {
            refusal["available_models"] = json!([]);
            refusal["message"] = json!(format!(
                "no obtainable model list for kind '{kind}'; the runtime cannot validate model alias '{alias}' and refuses to start with an unvalidated model. Omit the model to use the agent's configured default, or use a kind with an obtainable list."
            ));
        }
        ModelAliasCheck::Validated { .. } => {
            // Not a refusal case; keep the envelope total for callers.
            refusal["message"] = json!("model alias validated");
            refusal["ok"] = json!(true);
        }
    }
    refusal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> Vec<String> {
        vec![
            "openai/gpt-4".to_owned(),
            "openai/gpt-5.1-codex".to_owned(),
            "anthropic/claude-sonnet-4-5".to_owned(),
        ]
    }

    #[test]
    fn canonical_and_bare_and_thinking_forms_validate() {
        for alias in [
            "openai/gpt-4",
            "gpt-4",
            "openai/gpt-4:high",
            "gpt-4:low",
            "anthropic/claude-sonnet-4-5",
        ] {
            assert!(
                matches!(
                    check_alias_in_list(alias, &list()),
                    ModelAliasCheck::Validated { .. }
                ),
                "{alias}"
            );
        }
    }

    #[test]
    fn unknown_alias_reports_the_available_list() {
        let check = check_alias_in_list("openai/gpt-99", &list());
        match check {
            ModelAliasCheck::Unknown { available } => assert_eq!(available, list()),
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    #[test]
    fn ambiguous_bare_id_is_refused_not_guessed() {
        let mut ambiguous = list();
        ambiguous.push("zai/gpt-4".to_owned());
        assert!(matches!(
            check_alias_in_list("gpt-4", &ambiguous),
            ModelAliasCheck::Unknown { .. }
        ));
        // The qualified form still validates.
        assert!(matches!(
            check_alias_in_list("zai/gpt-4", &ambiguous),
            ModelAliasCheck::Validated { .. }
        ));
    }

    #[test]
    fn empty_list_is_unvalidatable_not_unknown() {
        assert_eq!(
            check_alias_in_list("gpt-4", &[]),
            ModelAliasCheck::Unvalidatable
        );
    }

    #[test]
    fn malformed_aliases_never_validate() {
        for alias in ["", "a b", "a:b:c", "a/b/c", "openai/gpt-4:", "x\ny"] {
            assert!(
                !matches!(
                    check_alias_in_list(alias, &list()),
                    ModelAliasCheck::Validated { .. }
                ),
                "{alias:?}"
            );
        }
    }

    #[test]
    fn non_pi_kinds_are_unvalidatable_without_a_list() {
        for kind in ["claude", "codex", "gemini", "droid"] {
            assert_eq!(
                validate_model_alias(kind, "some-model"),
                ModelAliasCheck::Unvalidatable
            );
        }
    }

    #[test]
    fn refusal_envelope_is_fail_closed_and_carries_the_list() {
        let check = ModelAliasCheck::Unknown { available: list() };
        let refusal = model_alias_refusal("pi", "gpt-99", &check);
        assert_eq!(refusal["ok"], false);
        assert_eq!(refusal["code"], "model_alias_unknown");
        assert_eq!(refusal["delivery_state"], "not_delivered");
        assert_eq!(refusal["submitted"], false);
        assert_eq!(refusal["fallback_allowed"], false);
        assert_eq!(refusal["model_validation"]["validated"], false);
        assert_eq!(refusal["available_models"], json!(list()));

        let unvalidatable = model_alias_refusal("claude", "x", &ModelAliasCheck::Unvalidatable);
        assert_eq!(unvalidatable["code"], "model_alias_unknown");
        assert_eq!(unvalidatable["model_validation"]["validated"], false);
        assert_eq!(unvalidatable["available_models"], json!([]));
    }
}
