use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum OperationMode {
    ReadOnly,
    Mutation,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LaneRequest {
    pub id: String,
    pub mode: OperationMode,
    pub file_scope: Vec<String>,
    pub dependencies: Vec<String>,
    pub shared_runtime: bool,
    pub mutation_isolation_required: bool,
    pub delegates_other_workers: bool,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct OrchestrationPlan {
    pub waves: Vec<Vec<String>>,
    pub worktree_recommended: Vec<String>,
}

pub fn plan_lanes(lanes: &[LaneRequest]) -> Result<OrchestrationPlan, String> {
    let by_id = lanes
        .iter()
        .map(|lane| (lane.id.as_str(), lane))
        .collect::<HashMap<_, _>>();
    if by_id.len() != lanes.len() {
        return Err("lane ids must be unique".to_owned());
    }
    for lane in lanes {
        if lane.delegates_other_workers {
            return Err(format!(
                "lane {} attempts middle-manager delegation",
                lane.id
            ));
        }
        for dependency in &lane.dependencies {
            if !by_id.contains_key(dependency.as_str()) {
                return Err(format!(
                    "lane {} references unknown dependency {}",
                    lane.id, dependency
                ));
            }
        }
    }

    let mut completed = BTreeSet::<String>::new();
    let mut remaining = lanes.iter().collect::<Vec<_>>();
    let mut waves = Vec::new();
    while !remaining.is_empty() {
        let ready = remaining
            .iter()
            .copied()
            .filter(|lane| {
                lane.dependencies
                    .iter()
                    .all(|dependency| completed.contains(dependency))
            })
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return Err("lane dependency graph contains a cycle".to_owned());
        }

        let mut wave = Vec::<&LaneRequest>::new();
        for candidate in ready {
            if wave
                .iter()
                .all(|existing| can_share_wave(existing, candidate))
            {
                wave.push(candidate);
            }
        }
        if wave.is_empty() {
            return Err("no schedulable lane found".to_owned());
        }
        let ids = wave.iter().map(|lane| lane.id.clone()).collect::<Vec<_>>();
        for id in &ids {
            completed.insert(id.clone());
        }
        remaining.retain(|lane| !completed.contains(&lane.id));
        waves.push(ids);
    }

    let worktree_recommended = lanes
        .iter()
        .filter(|lane| lane.mode == OperationMode::Mutation && lane.mutation_isolation_required)
        .map(|lane| lane.id.clone())
        .collect();
    Ok(OrchestrationPlan {
        waves,
        worktree_recommended,
    })
}

fn can_share_wave(left: &LaneRequest, right: &LaneRequest) -> bool {
    if left.mode != right.mode {
        return false;
    }
    if left.shared_runtime || right.shared_runtime {
        return false;
    }
    match left.mode {
        OperationMode::ReadOnly => true,
        OperationMode::Mutation => {
            if left.file_scope.is_empty() || right.file_scope.is_empty() {
                return false;
            }
            !scopes_overlap(&left.file_scope, &right.file_scope)
        }
    }
}

fn scopes_overlap(left: &[String], right: &[String]) -> bool {
    left.iter().any(|left_path| {
        right.iter().any(|right_path| {
            left_path == right_path
                || left_path.starts_with(&format!("{right_path}/"))
                || right_path.starts_with(&format!("{left_path}/"))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(id: &str, mode: OperationMode) -> LaneRequest {
        LaneRequest {
            id: id.to_owned(),
            mode,
            file_scope: vec![],
            dependencies: vec![],
            shared_runtime: false,
            mutation_isolation_required: false,
            delegates_other_workers: false,
        }
    }

    #[test]
    fn independent_reads_parallelize_in_one_wave() {
        let lanes = vec![
            lane("read-a", OperationMode::ReadOnly),
            lane("read-b", OperationMode::ReadOnly),
        ];
        let plan = plan_lanes(&lanes).unwrap();
        assert_eq!(
            plan.waves,
            vec![vec!["read-a".to_owned(), "read-b".to_owned()]]
        );
    }

    #[test]
    fn dependent_mutations_serialize() {
        let mut first = lane("mutate-a", OperationMode::Mutation);
        first.file_scope = vec!["src/a.rs".to_owned()];
        let mut second = lane("mutate-b", OperationMode::Mutation);
        second.file_scope = vec!["src/b.rs".to_owned()];
        second.dependencies = vec!["mutate-a".to_owned()];
        let plan = plan_lanes(&[first, second]).unwrap();
        assert_eq!(
            plan.waves,
            vec![vec!["mutate-a".to_owned()], vec!["mutate-b".to_owned()]]
        );
    }

    #[test]
    fn independent_non_overlapping_mutations_can_share_a_wave() {
        let mut first = lane("mutate-a", OperationMode::Mutation);
        first.file_scope = vec!["src/a.rs".to_owned()];
        let mut second = lane("mutate-b", OperationMode::Mutation);
        second.file_scope = vec!["tests/b.rs".to_owned()];
        let plan = plan_lanes(&[first, second]).unwrap();
        assert_eq!(plan.waves.len(), 1);
        assert_eq!(plan.waves[0].len(), 2);
    }

    #[test]
    fn worktree_is_recommended_only_for_isolated_mutation() {
        let read = lane("read", OperationMode::ReadOnly);
        let mut mutation = lane("mutation", OperationMode::Mutation);
        mutation.file_scope = vec!["src/a.rs".to_owned()];
        mutation.mutation_isolation_required = true;
        let plan = plan_lanes(&[read, mutation]).unwrap();
        assert_eq!(plan.worktree_recommended, vec!["mutation".to_owned()]);
    }

    #[test]
    fn shared_file_and_shared_runtime_mutations_serialize() {
        let mut first = lane("a", OperationMode::Mutation);
        first.file_scope = vec!["src".to_owned()];
        let mut second = lane("b", OperationMode::Mutation);
        second.file_scope = vec!["src/b.rs".to_owned()];
        let mut third = lane("c", OperationMode::Mutation);
        third.file_scope = vec!["tests/c.rs".to_owned()];
        third.shared_runtime = true;
        let plan = plan_lanes(&[first, second, third]).unwrap();
        assert_eq!(plan.waves.len(), 3);
    }

    #[test]
    fn middle_manager_delegation_is_rejected() {
        let mut request = lane("manager", OperationMode::ReadOnly);
        request.delegates_other_workers = true;
        let error = plan_lanes(&[request]).unwrap_err();
        assert!(error.contains("middle-manager"));
    }
}
