//! The one reviewable home for every semantic decision threshold.
//!
//! Semantic answers are advisory. A threshold in this file decides only whether
//! an advisory answer is confident enough to be *used*; it never grants a
//! mutation, a deletion, or a permission. Anything below its threshold falls
//! back to the existing deterministic/rule behavior.
//!
//! Keep the thresholds per question kind. A Noul probability, a Choice
//! confidence, and a destructive-action recommendation are different promises
//! and must never borrow each other's number.
//!
//! The Worker proxy (`edge/cloudflare/src/semantic-proxy.ts`) shares the
//! routing/backoff/budget semantics described in `docs/semantic-routing.md`; it
//! does not gate on these thresholds because gating happens in the runtime.

/// Minimum `choice` confidence for a semantic choice to be used instead of the
/// deterministic/rule result. Below this value the caller must fall back.
pub const CHOICE_MIN_CONFIDENCE: f64 = 0.5;

/// Minimum `score` confidence for a semantic score to be used instead of the
/// deterministic/rule result. Below this value the caller must fall back.
pub const SCORE_MIN_CONFIDENCE: f64 = 0.5;

/// Noul probability at or above which a semantic yes/no answer is `true`.
/// Between this and [`NOUL_FALSE_THRESHOLD`] the answer stays `uncertain`.
pub const NOUL_TRUE_THRESHOLD: f64 = 0.80;

/// Noul probability at or below which a semantic yes/no answer is `false`.
pub const NOUL_FALSE_THRESHOLD: f64 = 0.20;

/// Backward-compatible alias of [`NOUL_TRUE_THRESHOLD`] for existing callers
/// that still spell the Noul upper boundary `DEFAULT_DECISION_THRESHOLD`.
pub const DEFAULT_DECISION_THRESHOLD: f64 = NOUL_TRUE_THRESHOLD;

/// Backward-compatible alias of [`NOUL_FALSE_THRESHOLD`].
pub const DEFAULT_NEGATIVE_DECISION_THRESHOLD: f64 = NOUL_FALSE_THRESHOLD;

/// Minimum `choice` confidence before a *destructive* recommendation
/// (cleanup/reclaim candidates) may be reported as actionable. Below this value
/// the caller reports the non-destructive/rule result instead.
pub const DESTRUCTIVE_ACTION_MIN_CONFIDENCE: f64 = 0.9;

/// The gate decision for one advisory confidence value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Confident enough: the advisory answer may be used.
    Accept,
    /// Not confident enough: use the deterministic/rule result.
    Fallback,
}

impl Gate {
    pub fn accepted(self) -> bool {
        matches!(self, Self::Accept)
    }
}

fn gate(confidence: f64, minimum: f64) -> Gate {
    if confidence.is_finite() && confidence >= minimum {
        Gate::Accept
    } else {
        Gate::Fallback
    }
}

/// Gate a `choice` answer confidence against [`CHOICE_MIN_CONFIDENCE`].
pub fn gate_choice(confidence: f64) -> Gate {
    gate(confidence, CHOICE_MIN_CONFIDENCE)
}

/// Gate a destructive recommendation against
/// [`DESTRUCTIVE_ACTION_MIN_CONFIDENCE`].
pub fn gate_destructive(confidence: f64) -> Gate {
    gate(confidence, DESTRUCTIVE_ACTION_MIN_CONFIDENCE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_and_destructive_gates_use_their_own_thresholds() {
        assert_eq!(gate_choice(0.49), Gate::Fallback);
        assert_eq!(gate_choice(0.50), Gate::Accept);
        // A choice-shaped answer that clears the Choice gate is not thereby
        // safe for a destructive recommendation.
        assert_eq!(gate_destructive(0.60), Gate::Fallback);
        assert_eq!(gate_destructive(0.90), Gate::Accept);
        assert!(!gate_choice(f64::NAN).accepted());
    }
}
