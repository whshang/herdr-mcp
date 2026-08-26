//! Link-local runtime-generation request ownership and stale-result fencing.
//!
//! The Node `RuntimeGenerationManager` already preserves an old generation for
//! in-flight work while new requests switch to a newly active generation. This
//! staged Rust core captures that routing invariant without owning transports,
//! health probes, or the durable service-generation ledger.
//!
//! Rust strengthens one safety boundary: a completion must name the generation
//! that actually served the request. If it disagrees with the request owner, the
//! completion is rejected without mutating ownership state. This prevents a
//! post-activation result from being relabelled as the new active generation.

use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationPhase {
    Active,
    Standby,
    Draining,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationStatus {
    pub generation: String,
    pub phase: GenerationPhase,
    pub in_flight: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationLease {
    pub request_id: String,
    pub generation: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationTransition {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceError {
    UnknownGeneration(String),
    RejectedGeneration(String),
    DuplicateRequest(String),
    UnknownRequest(String),
    GenerationMismatch {
        request_id: String,
        expected: String,
        observed: String,
    },
    ActiveGenerationMismatch {
        expected: String,
        observed: String,
    },
    NoPreviousGeneration,
    CannotRemoveActiveGeneration(String),
    GenerationHasInFlightRequests(String),
    OwnershipInvariant {
        request_id: String,
        generation: String,
    },
}

#[derive(Debug, Clone)]
struct GenerationRecord {
    phase: GenerationPhase,
    in_flight: usize,
}

#[derive(Debug, Clone)]
pub struct GenerationFence {
    active_generation: String,
    previous_generation: Option<String>,
    generations: BTreeMap<String, GenerationRecord>,
    request_owners: BTreeMap<String, String>,
}

impl GenerationFence {
    pub fn new(base_generation: impl Into<String>) -> Self {
        let base_generation = base_generation.into();
        let mut generations = BTreeMap::new();
        generations.insert(
            base_generation.clone(),
            GenerationRecord {
                phase: GenerationPhase::Active,
                in_flight: 0,
            },
        );
        Self {
            active_generation: base_generation,
            previous_generation: None,
            generations,
            request_owners: BTreeMap::new(),
        }
    }

    pub fn active_generation(&self) -> &str {
        &self.active_generation
    }

    pub fn previous_generation(&self) -> Option<&str> {
        self.previous_generation.as_deref()
    }

    pub fn register_standby(&mut self, generation: impl Into<String>) {
        let generation = generation.into();
        self.generations
            .entry(generation)
            .or_insert(GenerationRecord {
                phase: GenerationPhase::Standby,
                in_flight: 0,
            });
    }

    pub fn reject_generation(&mut self, generation: &str) -> Result<(), FenceError> {
        if generation == self.active_generation {
            return Err(FenceError::CannotRemoveActiveGeneration(
                generation.to_owned(),
            ));
        }
        let has_owned_requests = self.generation_has_owned_requests(generation);
        let record = self
            .generations
            .get_mut(generation)
            .ok_or_else(|| FenceError::UnknownGeneration(generation.to_owned()))?;
        if record.in_flight > 0 || has_owned_requests {
            return Err(FenceError::GenerationHasInFlightRequests(
                generation.to_owned(),
            ));
        }
        record.phase = GenerationPhase::Rejected;
        Ok(())
    }

    /// Bind a request to the generation that is active at dispatch time.
    ///
    /// Duplicate request ids fail closed instead of replacing the original
    /// ownership record.
    pub fn begin_request(
        &mut self,
        request_id: impl Into<String>,
    ) -> Result<GenerationLease, FenceError> {
        let request_id = request_id.into();
        if self.request_owners.contains_key(&request_id) {
            return Err(FenceError::DuplicateRequest(request_id));
        }
        let generation = self.active_generation.clone();
        let record = self
            .generations
            .get_mut(&generation)
            .ok_or_else(|| FenceError::UnknownGeneration(generation.clone()))?;
        if record.phase != GenerationPhase::Active {
            return Err(FenceError::ActiveGenerationMismatch {
                expected: generation,
                observed: format!("{:?}", record.phase),
            });
        }
        record.in_flight = record.in_flight.saturating_add(1);
        self.request_owners
            .insert(request_id.clone(), generation.clone());
        Ok(GenerationLease {
            request_id,
            generation,
        })
    }

    /// Switch the active pointer. Existing requests remain owned by their old
    /// generation and force it into `Draining` until all leases complete.
    pub fn activate_generation(
        &mut self,
        generation: &str,
    ) -> Result<Option<GenerationTransition>, FenceError> {
        if generation == self.active_generation {
            return Ok(None);
        }
        let target = self
            .generations
            .get(generation)
            .ok_or_else(|| FenceError::UnknownGeneration(generation.to_owned()))?;
        if target.phase == GenerationPhase::Rejected {
            return Err(FenceError::RejectedGeneration(generation.to_owned()));
        }

        let previous = self.active_generation.clone();
        let previous_record = self
            .generations
            .get_mut(&previous)
            .ok_or_else(|| FenceError::UnknownGeneration(previous.clone()))?;
        previous_record.phase = if previous_record.in_flight > 0 {
            GenerationPhase::Draining
        } else {
            GenerationPhase::Standby
        };

        let target_record = self
            .generations
            .get_mut(generation)
            .ok_or_else(|| FenceError::UnknownGeneration(generation.to_owned()))?;
        target_record.phase = GenerationPhase::Active;
        self.active_generation = generation.to_owned();
        self.previous_generation = Some(previous.clone());
        Ok(Some(GenerationTransition {
            from: previous,
            to: generation.to_owned(),
        }))
    }

    /// Generation to receive a cancellation. This mirrors the Node manager:
    /// known in-flight requests route to their owner; an unknown request falls
    /// back to the current active generation for best-effort cancellation.
    pub fn cancel_target(&self, request_id: &str) -> &str {
        self.request_owners
            .get(request_id)
            .map(String::as_str)
            .unwrap_or(&self.active_generation)
    }

    pub fn request_owner(&self, request_id: &str) -> Option<&str> {
        self.request_owners.get(request_id).map(String::as_str)
    }

    fn generation_has_owned_requests(&self, generation: &str) -> bool {
        self.request_owners
            .values()
            .any(|owner| owner == generation)
    }

    /// Complete one request only if the transport proves the same generation
    /// that owns the lease served the result.
    ///
    /// A mismatch leaves all state untouched so callers can surface a
    /// fail-closed stale-generation error without losing recovery evidence.
    pub fn complete_request(
        &mut self,
        request_id: &str,
        serving_generation: &str,
    ) -> Result<(), FenceError> {
        let owner = self
            .request_owners
            .get(request_id)
            .cloned()
            .ok_or_else(|| FenceError::UnknownRequest(request_id.to_owned()))?;
        if owner != serving_generation {
            return Err(FenceError::GenerationMismatch {
                request_id: request_id.to_owned(),
                expected: owner,
                observed: serving_generation.to_owned(),
            });
        }

        let record = self
            .generations
            .get(serving_generation)
            .ok_or_else(|| FenceError::UnknownGeneration(serving_generation.to_owned()))?;
        if record.in_flight == 0 {
            return Err(FenceError::OwnershipInvariant {
                request_id: request_id.to_owned(),
                generation: serving_generation.to_owned(),
            });
        }

        self.request_owners.remove(request_id);
        let record = self
            .generations
            .get_mut(serving_generation)
            .expect("serving generation prevalidated before lease consumption");
        record.in_flight -= 1;
        if record.in_flight == 0 && record.phase == GenerationPhase::Draining {
            record.phase = GenerationPhase::Standby;
        }
        Ok(())
    }

    /// Roll back an activation after post-switch health observation fails.
    pub fn rollback_active_generation(
        &mut self,
        failed_generation: &str,
    ) -> Result<GenerationTransition, FenceError> {
        if self.active_generation != failed_generation {
            return Err(FenceError::ActiveGenerationMismatch {
                expected: failed_generation.to_owned(),
                observed: self.active_generation.clone(),
            });
        }
        let previous = self
            .previous_generation
            .clone()
            .ok_or(FenceError::NoPreviousGeneration)?;
        if !self.generations.contains_key(failed_generation) {
            return Err(FenceError::UnknownGeneration(failed_generation.to_owned()));
        }
        let previous_phase = self
            .generations
            .get(&previous)
            .ok_or_else(|| FenceError::UnknownGeneration(previous.clone()))?
            .phase;
        if previous_phase == GenerationPhase::Rejected {
            return Err(FenceError::RejectedGeneration(previous));
        }

        let failed = self
            .generations
            .get_mut(failed_generation)
            .expect("failed generation prevalidated before rollback mutation");
        failed.phase = if failed.in_flight > 0 {
            GenerationPhase::Draining
        } else {
            GenerationPhase::Rejected
        };

        let previous_record = self
            .generations
            .get_mut(&previous)
            .expect("previous generation prevalidated before rollback mutation");
        previous_record.phase = GenerationPhase::Active;
        self.active_generation = previous.clone();
        self.previous_generation = Some(failed_generation.to_owned());
        Ok(GenerationTransition {
            from: failed_generation.to_owned(),
            to: previous,
        })
    }

    pub fn remove_generation(&mut self, generation: &str) -> Result<bool, FenceError> {
        if generation == self.active_generation {
            return Err(FenceError::CannotRemoveActiveGeneration(
                generation.to_owned(),
            ));
        }
        let Some(record) = self.generations.get(generation) else {
            return Ok(false);
        };
        if record.in_flight > 0 || self.generation_has_owned_requests(generation) {
            return Err(FenceError::GenerationHasInFlightRequests(
                generation.to_owned(),
            ));
        }
        self.generations.remove(generation);
        if self.previous_generation.as_deref() == Some(generation) {
            self.previous_generation = None;
        }
        Ok(true)
    }

    pub fn status(&self) -> Vec<GenerationStatus> {
        self.generations
            .iter()
            .map(|(generation, record)| GenerationStatus {
                generation: generation.clone(),
                phase: record.phase,
                in_flight: record.in_flight,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{FenceError, GenerationFence, GenerationPhase};

    fn phase_from_fixture(value: &str) -> GenerationPhase {
        match value {
            "active" => GenerationPhase::Active,
            "standby" => GenerationPhase::Standby,
            "draining" => GenerationPhase::Draining,
            "rejected" => GenerationPhase::Rejected,
            other => panic!("unknown fixture phase {other}"),
        }
    }

    fn phase(fence: &GenerationFence, generation: &str) -> GenerationPhase {
        fence
            .status()
            .into_iter()
            .find(|entry| entry.generation == generation)
            .expect("generation status")
            .phase
    }

    #[test]
    fn old_in_flight_request_drains_on_old_generation_after_activation() {
        let mut fence = GenerationFence::new("stable");
        fence.register_standby("candidate");
        let old = fence.begin_request("old-r1").unwrap();
        assert_eq!(old.generation, "stable");

        fence.activate_generation("candidate").unwrap();
        assert_eq!(fence.active_generation(), "candidate");
        assert_eq!(phase(&fence, "stable"), GenerationPhase::Draining);
        assert_eq!(fence.cancel_target("old-r1"), "stable");

        let fresh = fence.begin_request("new-r1").unwrap();
        assert_eq!(fresh.generation, "candidate");
        assert_eq!(fence.cancel_target("new-r1"), "candidate");

        fence.complete_request("old-r1", "stable").unwrap();
        assert_eq!(phase(&fence, "stable"), GenerationPhase::Standby);
        fence.complete_request("new-r1", "candidate").unwrap();
    }

    #[test]
    fn stale_or_mislabelled_completion_fails_closed_without_losing_owner() {
        let mut fence = GenerationFence::new("stable");
        fence.register_standby("candidate");
        fence.begin_request("old-r1").unwrap();
        fence.activate_generation("candidate").unwrap();

        let error = fence.complete_request("old-r1", "candidate").unwrap_err();
        assert_eq!(
            error,
            FenceError::GenerationMismatch {
                request_id: "old-r1".to_owned(),
                expected: "stable".to_owned(),
                observed: "candidate".to_owned(),
            }
        );
        assert_eq!(fence.request_owner("old-r1"), Some("stable"));
        assert_eq!(phase(&fence, "stable"), GenerationPhase::Draining);

        fence.complete_request("old-r1", "stable").unwrap();
        assert_eq!(fence.request_owner("old-r1"), None);
        assert_eq!(phase(&fence, "stable"), GenerationPhase::Standby);
    }

    #[test]
    fn duplicate_request_id_fails_closed_and_preserves_original_owner() {
        let mut fence = GenerationFence::new("stable");
        fence.begin_request("r1").unwrap();
        assert_eq!(
            fence.begin_request("r1").unwrap_err(),
            FenceError::DuplicateRequest("r1".to_owned())
        );
        assert_eq!(fence.request_owner("r1"), Some("stable"));
        assert_eq!(
            fence
                .status()
                .into_iter()
                .find(|entry| entry.generation == "stable")
                .unwrap()
                .in_flight,
            1
        );
    }

    #[test]
    fn unknown_cancel_falls_back_to_active_generation_like_node_manager() {
        let mut fence = GenerationFence::new("stable");
        fence.register_standby("candidate");
        fence.activate_generation("candidate").unwrap();
        assert_eq!(fence.cancel_target("missing"), "candidate");
    }

    #[test]
    fn failed_activation_rolls_pointer_back_and_preserves_candidate_drain() {
        let mut fence = GenerationFence::new("stable");
        fence.register_standby("candidate");
        fence.activate_generation("candidate").unwrap();
        fence.begin_request("candidate-r1").unwrap();

        let transition = fence.rollback_active_generation("candidate").unwrap();
        assert_eq!(transition.from, "candidate");
        assert_eq!(transition.to, "stable");
        assert_eq!(fence.active_generation(), "stable");
        assert_eq!(phase(&fence, "stable"), GenerationPhase::Active);
        assert_eq!(phase(&fence, "candidate"), GenerationPhase::Draining);

        fence.complete_request("candidate-r1", "candidate").unwrap();
        assert_eq!(phase(&fence, "candidate"), GenerationPhase::Standby);
    }

    #[test]
    fn second_rollback_cannot_revive_a_rejected_previous_generation() {
        let mut fence = GenerationFence::new("stable");
        fence.register_standby("candidate");
        fence.activate_generation("candidate").unwrap();
        fence.rollback_active_generation("candidate").unwrap();
        assert_eq!(phase(&fence, "candidate"), GenerationPhase::Rejected);
        assert_eq!(fence.active_generation(), "stable");

        assert_eq!(
            fence.rollback_active_generation("stable").unwrap_err(),
            FenceError::RejectedGeneration("candidate".to_owned())
        );
        assert_eq!(fence.active_generation(), "stable");
        assert_eq!(phase(&fence, "stable"), GenerationPhase::Active);
        assert_eq!(phase(&fence, "candidate"), GenerationPhase::Rejected);
    }

    #[test]
    fn rejected_or_busy_generations_cannot_be_activated_or_removed_unsafely() {
        let mut fence = GenerationFence::new("stable");
        fence.register_standby("bad");
        fence.reject_generation("bad").unwrap();
        assert_eq!(
            fence.activate_generation("bad").unwrap_err(),
            FenceError::RejectedGeneration("bad".to_owned())
        );
        assert!(fence.remove_generation("bad").unwrap());
        assert!(!fence.remove_generation("bad").unwrap());
        assert_eq!(
            fence.remove_generation("stable").unwrap_err(),
            FenceError::CannotRemoveActiveGeneration("stable".to_owned())
        );
    }

    #[test]
    fn shared_batch4_fixture_matches_node_ownership_and_rust_fencing_invariants() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/link-reliability-batch4.json"
        ))
        .expect("shared link reliability fixture");
        let scenarios = fixture
            .get("runtime_generation")
            .and_then(|value| value.get("scenarios"))
            .and_then(Value::as_array)
            .expect("runtime generation scenarios");
        assert_eq!(scenarios.len(), 6);
        assert_eq!(
            scenarios
                .iter()
                .filter(|scenario| scenario.get("oracle").and_then(Value::as_str)
                    == Some("node_parity"))
                .count(),
            4
        );
        assert_eq!(
            scenarios
                .iter()
                .filter(|scenario| {
                    scenario.get("oracle").and_then(Value::as_str)
                        == Some("rust_safety_strengthening")
                })
                .count(),
            2
        );

        for scenario in scenarios {
            match scenario.get("oracle").and_then(Value::as_str).unwrap() {
                "node_parity" => run_node_parity_scenario(scenario),
                "rust_safety_strengthening" => run_rust_strengthening_scenario(scenario),
                other => panic!("unknown oracle {other}"),
            }
        }
    }

    fn setup_fence(scenario: &Value) -> GenerationFence {
        let setup = scenario.get("setup").expect("setup");
        let base = setup
            .get("base")
            .and_then(|value| value.get("generation"))
            .and_then(Value::as_str)
            .expect("base generation");
        GenerationFence::new(base)
    }

    fn generation_for_port(scenario: &Value, port: &str) -> String {
        let setup = scenario.get("setup").expect("setup");
        let matches_port = |spec: &Value| {
            spec.get("endpoint")
                .and_then(Value::as_str)
                .is_some_and(|endpoint| endpoint.contains(&format!(":{port}/")))
        };
        let base = setup.get("base").expect("base");
        if matches_port(base) {
            return base
                .get("generation")
                .and_then(Value::as_str)
                .unwrap()
                .to_owned();
        }
        setup
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.iter().find(|candidate| matches_port(candidate)))
            .and_then(|candidate| candidate.get("generation"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("no generation for fixture port {port}"))
            .to_owned()
    }

    fn assert_fixture_status(fence: &GenerationFence, step: &Value) {
        if let Some(active) = step.get("active_generation").and_then(Value::as_str) {
            assert_eq!(fence.active_generation(), active);
        }
        if step.get("previous_generation").is_some() {
            let expected = step.get("previous_generation").and_then(Value::as_str);
            assert_eq!(fence.previous_generation(), expected);
        }
        if let Some(phases) = step.get("generation_phase").and_then(Value::as_object) {
            for (generation, expected) in phases {
                assert_eq!(
                    phase(fence, generation),
                    phase_from_fixture(expected.as_str().unwrap()),
                    "phase for {generation}"
                );
            }
        }
        if let Some(counts) = step.get("generation_in_flight").and_then(Value::as_object) {
            let status = fence.status();
            for (generation, expected) in counts {
                let actual = status
                    .iter()
                    .find(|entry| entry.generation == *generation)
                    .unwrap_or_else(|| panic!("missing generation {generation}"))
                    .in_flight;
                assert_eq!(actual, expected.as_u64().unwrap() as usize);
            }
        }
    }

    fn run_node_parity_scenario(scenario: &Value) {
        let mut fence = setup_fence(scenario);
        let steps = scenario
            .get("steps")
            .and_then(Value::as_array)
            .expect("node parity steps");
        for step in steps {
            match step.get("op").and_then(Value::as_str).unwrap() {
                "register" => {
                    fence.register_standby(step.get("generation").and_then(Value::as_str).unwrap())
                }
                "activate" => {
                    fence
                        .activate_generation(
                            step.get("generation").and_then(Value::as_str).unwrap(),
                        )
                        .unwrap();
                }
                "dispatch" => {
                    let request_id = step.get("request_id").and_then(Value::as_str).unwrap();
                    let lease = fence.begin_request(request_id).unwrap();
                    if let Some(port) = step.get("expect_port").and_then(Value::as_str) {
                        assert_eq!(lease.generation, generation_for_port(scenario, port));
                    }
                    if step.get("defer").and_then(Value::as_bool) != Some(true) {
                        fence
                            .complete_request(request_id, &lease.generation)
                            .unwrap();
                    }
                }
                "release" => {
                    let request_id = step.get("request_id").and_then(Value::as_str).unwrap();
                    let owner = fence.request_owner(request_id).unwrap().to_owned();
                    let port = step.get("expect_port").and_then(Value::as_str).unwrap();
                    assert_eq!(owner, generation_for_port(scenario, port));
                    fence.complete_request(request_id, &owner).unwrap();
                }
                "cancel" => {
                    let request_id = step.get("request_id").and_then(Value::as_str).unwrap();
                    let owner = fence.cancel_target(request_id).to_owned();
                    assert_eq!(fence.request_owner(request_id), Some(owner.as_str()));
                    fence.complete_request(request_id, &owner).unwrap();
                }
                "assert_status" => assert_fixture_status(&fence, step),
                other => panic!("unknown runtime generation fixture op {other}"),
            }
        }
    }

    fn run_rust_strengthening_scenario(scenario: &Value) {
        match scenario.get("intent").and_then(Value::as_str).unwrap() {
            "reject_duplicate_request_id" => {
                let mut fence = GenerationFence::new("stable");
                fence.begin_request("dup-r1").unwrap();
                assert_eq!(
                    fence.begin_request("dup-r1").unwrap_err(),
                    FenceError::DuplicateRequest("dup-r1".to_owned())
                );
                assert_eq!(fence.request_owner("dup-r1"), Some("stable"));
            }
            "fence_mismatched_serving_generation" => {
                let mut fence = GenerationFence::new("stable");
                fence.register_standby("candidate");
                fence.begin_request("old-r1").unwrap();
                fence.activate_generation("candidate").unwrap();
                assert!(matches!(
                    fence.complete_request("old-r1", "candidate"),
                    Err(FenceError::GenerationMismatch { .. })
                ));
                assert_eq!(fence.request_owner("old-r1"), Some("stable"));
                fence.complete_request("old-r1", "stable").unwrap();
            }
            other => panic!("unknown Rust strengthening intent {other}"),
        }
    }
}
