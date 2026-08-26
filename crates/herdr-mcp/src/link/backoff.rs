//! Deterministic reconnect backoff parity with `src/link/backoff.ts`.
//!
//! The transport supplies the RNG sample explicitly. This keeps the staged
//! Rust reliability core pure and makes reconnect scheduling deterministic in
//! tests without owning a random-number generator or timer.

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BackoffOptions {
    pub base_ms: Option<f64>,
    pub max_ms: Option<f64>,
    pub factor: Option<f64>,
    pub jitter: Option<f64>,
}

/// Mirrors the Node helper: reject non-finite/negative values, otherwise floor.
pub fn clamp_nonnegative(value: Option<f64>, fallback: f64) -> f64 {
    match value {
        Some(value) if value.is_finite() && value >= 0.0 => value.floor(),
        _ => fallback,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExponentialBackoff {
    pub base_ms: f64,
    pub max_ms: f64,
    pub factor: f64,
    pub jitter: f64,
    attempt: u64,
}

impl ExponentialBackoff {
    pub fn new(options: BackoffOptions) -> Self {
        let base_ms = clamp_nonnegative(options.base_ms, 1_000.0).max(1.0);
        let max_ms = clamp_nonnegative(options.max_ms, 60_000.0).max(base_ms);
        let factor = clamp_nonnegative(options.factor, 2.0).max(1.0);
        let jitter = clamp_nonnegative(options.jitter, 1.0).min(1.0);
        Self {
            base_ms,
            max_ms,
            factor,
            jitter,
            attempt: 0,
        }
    }

    pub fn attempt(&self) -> u64 {
        self.attempt
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Delay for `attempt` without advancing state.
    ///
    /// `rng_sample` intentionally is not clamped: the Node oracle trusts the
    /// injected RNG contract to produce a value in `[0, 1]`.
    pub fn peek(&self, attempt: u64, rng_sample: f64) -> f64 {
        let exponent = self.factor.powf(attempt as f64);
        let cap = (self.base_ms * exponent).min(self.max_ms);
        let raw = cap * (1.0 - self.jitter) + cap * self.jitter * rng_sample;
        raw.floor()
    }

    pub fn next(&mut self, rng_sample: f64) -> f64 {
        let delay = self.peek(self.attempt, rng_sample);
        self.attempt = self.attempt.saturating_add(1);
        delay
    }
}

impl Default for ExponentialBackoff {
    fn default() -> Self {
        Self::new(BackoffOptions::default())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{BackoffOptions, ExponentialBackoff, clamp_nonnegative};

    fn fixture_option(value: &Value, key: &str) -> Option<f64> {
        match value.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::Number(number)) => number.as_f64(),
            Some(Value::String(text)) if text == "NaN" => Some(f64::NAN),
            other => panic!("unsupported fixture option {key}: {other:?}"),
        }
    }

    fn fixture_backoff(value: &Value) -> (ExponentialBackoff, f64) {
        let options = BackoffOptions {
            base_ms: fixture_option(value, "baseMs"),
            max_ms: fixture_option(value, "maxMs"),
            factor: fixture_option(value, "factor"),
            jitter: fixture_option(value, "jitter"),
        };
        let rng = fixture_option(value, "rng").expect("fixture rng");
        (ExponentialBackoff::new(options), rng)
    }

    #[test]
    fn clamp_nonnegative_matches_node_floor_and_fallback() {
        assert_eq!(clamp_nonnegative(None, 42.0), 42.0);
        assert_eq!(clamp_nonnegative(Some(f64::NAN), 7.0), 7.0);
        assert_eq!(clamp_nonnegative(Some(-3.0), 7.0), 7.0);
        assert_eq!(clamp_nonnegative(Some(9.9), 7.0), 9.0);
    }

    #[test]
    fn full_jitter_matches_node_cap_formula() {
        let backoff = ExponentialBackoff::new(BackoffOptions {
            base_ms: Some(1_000.0),
            max_ms: Some(60_000.0),
            factor: Some(2.0),
            jitter: Some(1.0),
        });
        assert_eq!(backoff.peek(0, 0.5), 500.0);
        assert_eq!(backoff.peek(1, 0.5), 1_000.0);
        assert_eq!(backoff.peek(10, 0.5), 30_000.0);
    }

    #[test]
    fn zero_jitter_is_exact_cap() {
        let backoff = ExponentialBackoff::new(BackoffOptions {
            base_ms: Some(1_000.0),
            max_ms: Some(60_000.0),
            factor: Some(2.0),
            jitter: Some(0.0),
        });
        assert_eq!(backoff.peek(0, 0.25), 1_000.0);
        assert_eq!(backoff.peek(1, 0.25), 2_000.0);
        assert_eq!(backoff.peek(5, 0.25), 32_000.0);
    }

    #[test]
    fn next_advances_and_reset_restores_attempt() {
        let mut backoff = ExponentialBackoff::new(BackoffOptions {
            base_ms: Some(1_000.0),
            max_ms: None,
            factor: Some(2.0),
            jitter: None,
        });
        assert_eq!(backoff.attempt(), 0);
        assert_eq!(backoff.next(0.5), 500.0);
        assert_eq!(backoff.attempt(), 1);
        assert_eq!(backoff.next(0.5), 1_000.0);
        assert_eq!(backoff.attempt(), 2);
        backoff.reset();
        assert_eq!(backoff.attempt(), 0);
        assert_eq!(backoff.next(0.5), 500.0);
    }

    #[test]
    fn defaults_and_option_sanitization_match_node() {
        let defaults = ExponentialBackoff::new(BackoffOptions::default());
        assert_eq!(defaults.base_ms, 1_000.0);
        assert_eq!(defaults.max_ms, 60_000.0);
        assert_eq!(defaults.peek(0, 0.5), 500.0);
        assert_eq!(defaults.peek(5, 0.5), 16_000.0);

        let sanitized = ExponentialBackoff::new(BackoffOptions {
            base_ms: Some(-5.0),
            max_ms: Some(10.0),
            factor: Some(0.0),
            jitter: Some(3.0),
        });
        assert_eq!(sanitized.base_ms, 1_000.0);
        assert_eq!(sanitized.max_ms, 1_000.0);
        assert_eq!(sanitized.factor, 1.0);
        assert_eq!(sanitized.jitter, 1.0);
        assert_eq!(sanitized.peek(0, 1.0), 1_000.0);
    }

    #[test]
    fn shared_batch4_fixture_matches_node_backoff_oracle() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/link-reliability-batch4.json"
        ))
        .expect("shared link reliability fixture");
        let backoff = fixture.get("backoff").expect("backoff fixture");

        let sanitization = backoff
            .get("sanitization_cases")
            .and_then(Value::as_array)
            .expect("sanitization_cases");
        assert_eq!(sanitization.len(), 3);
        for case in sanitization {
            assert_eq!(
                case.get("oracle").and_then(Value::as_str),
                Some("node_parity")
            );
            let (candidate, rng) = fixture_backoff(case.get("options").expect("options"));
            let expected = case.get("expected").expect("expected");
            let sanitized = expected.get("sanitized").expect("sanitized");
            assert_eq!(
                candidate.base_ms,
                sanitized.get("baseMs").unwrap().as_f64().unwrap()
            );
            assert_eq!(
                candidate.max_ms,
                sanitized.get("maxMs").unwrap().as_f64().unwrap()
            );
            assert_eq!(
                candidate.factor,
                sanitized.get("factor").unwrap().as_f64().unwrap()
            );
            assert_eq!(
                candidate.jitter,
                sanitized.get("jitter").unwrap().as_f64().unwrap()
            );
            for vector in expected.get("peek").and_then(Value::as_array).unwrap() {
                let attempt = vector.get("attempt").unwrap().as_u64().unwrap();
                let delay = vector.get("delay").unwrap().as_f64().unwrap();
                assert_eq!(candidate.peek(attempt, rng), delay);
            }
        }

        let peek_cases = backoff
            .get("peek_cases")
            .and_then(Value::as_array)
            .expect("peek_cases");
        assert_eq!(peek_cases.len(), 3);
        for case in peek_cases {
            let (candidate, rng) = fixture_backoff(case.get("options").expect("options"));
            for vector in case.get("peek").and_then(Value::as_array).unwrap() {
                let attempt = vector.get("attempt").unwrap().as_u64().unwrap();
                let delay = vector.get("delay").unwrap().as_f64().unwrap();
                assert_eq!(candidate.peek(attempt, rng), delay);
            }
        }

        let sequences = backoff
            .get("sequence_cases")
            .and_then(Value::as_array)
            .expect("sequence_cases");
        assert_eq!(sequences.len(), 1);
        for case in sequences {
            let (mut candidate, rng) = fixture_backoff(case.get("options").expect("options"));
            for step in case.get("steps").and_then(Value::as_array).unwrap() {
                match step.get("op").and_then(Value::as_str).unwrap() {
                    "assert_attempt" => assert_eq!(
                        candidate.attempt(),
                        step.get("value").unwrap().as_u64().unwrap()
                    ),
                    "next" => assert_eq!(
                        candidate.next(rng),
                        step.get("expected").unwrap().as_f64().unwrap()
                    ),
                    "reset" => candidate.reset(),
                    other => panic!("unknown backoff fixture op {other}"),
                }
            }
        }
    }
}
