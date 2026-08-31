use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

const VISIBILITY_HINT: &str = "All discovered agents are visible by default. Set HERDR_MCP_AGENT_ALLOW to an explicit comma-separated allowlist to restrict discovery; '*' or 'all' restores full visibility.";

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum AgentVisibility {
    All,
    Allow(BTreeSet<String>),
}

impl AgentVisibility {
    pub fn from_env() -> Self {
        parse_allowlist(std::env::var("HERDR_MCP_AGENT_ALLOW").ok().as_deref())
    }

    pub fn is_visible(&self, name: Option<&str>, kind: Option<&str>) -> bool {
        let Self::Allow(allow) = self else {
            return true;
        };
        let candidates = [name, kind]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return false;
        }

        candidates.iter().any(|candidate| {
            allow.iter().any(|token| {
                candidate == token
                    || candidate.starts_with(&format!("{token}-"))
                    || candidate.starts_with(&format!("{token}_"))
            })
        })
    }

    pub fn filter_agents(&self, agents: Vec<Value>) -> (Vec<Value>, usize) {
        let original = agents.len();
        let visible = agents
            .into_iter()
            .filter(|agent| {
                self.is_visible(
                    agent.get("name").and_then(Value::as_str),
                    agent.get("kind").and_then(Value::as_str),
                )
            })
            .collect::<Vec<_>>();
        let hidden = original.saturating_sub(visible.len());
        (visible, hidden)
    }

    pub fn redact_panes(&self, panes: Vec<Value>) -> Vec<Value> {
        panes
            .into_iter()
            .map(|mut pane| {
                let visible = pane
                    .get("agent")
                    .filter(|agent| agent.is_object())
                    .is_none_or(|agent| {
                        self.is_visible(
                            agent.get("name").and_then(Value::as_str),
                            agent.get("kind").and_then(Value::as_str),
                        )
                    });
                if !visible && let Some(object) = pane.as_object_mut() {
                    object.insert("agent".to_owned(), Value::Null);
                }
                pane
            })
            .collect()
    }

    pub fn append_meta(&self, output: &mut Map<String, Value>, hidden: usize) {
        match self {
            Self::All => {
                output.insert("agent_visibility".to_owned(), json!("all"));
                output.insert("agents_hidden".to_owned(), json!(0));
            }
            Self::Allow(allow) => {
                output.insert("agent_visibility".to_owned(), json!("allowlist"));
                output.insert(
                    "agent_allow".to_owned(),
                    Value::Array(allow.iter().map(|value| json!(value)).collect()),
                );
                output.insert("agents_hidden".to_owned(), json!(hidden));
                output.insert("hint".to_owned(), json!(VISIBILITY_HINT));
            }
        }
    }
}

fn parse_allowlist(raw: Option<&str>) -> AgentVisibility {
    match raw {
        Some(value) if value.trim() == "*" || value.trim().eq_ignore_ascii_case("all") => {
            AgentVisibility::All
        }
        Some(value) => AgentVisibility::Allow(
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_ascii_lowercase)
                .collect(),
        ),
        None => AgentVisibility::All,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_visibility_allows_runtime_self_discovery() {
        assert_eq!(parse_allowlist(None), AgentVisibility::All);
    }

    #[test]
    fn wildcard_and_explicit_empty_have_distinct_meaning() {
        assert_eq!(parse_allowlist(Some("*")), AgentVisibility::All);
        assert_eq!(parse_allowlist(Some("ALL")), AgentVisibility::All);
        assert_eq!(
            parse_allowlist(Some("")),
            AgentVisibility::Allow(BTreeSet::new())
        );
    }

    #[test]
    fn visibility_matches_name_kind_and_segment_prefixes() {
        let visibility = parse_allowlist(Some("pi,grok"));
        assert!(visibility.is_visible(Some("pi"), None));
        assert!(visibility.is_visible(Some("pi-agent"), None));
        assert!(visibility.is_visible(Some("other"), Some("grok_auditor")));
        assert!(!visibility.is_visible(Some("claude"), None));
        assert!(!visibility.is_visible(None, None));
    }

    #[test]
    fn filters_agents_and_redacts_nested_pane_agent() {
        let visibility = parse_allowlist(Some("pi"));
        let (agents, hidden) =
            visibility.filter_agents(vec![json!({"name": "pi"}), json!({"name": "claude"})]);
        assert_eq!(agents.len(), 1);
        assert_eq!(hidden, 1);

        let panes = visibility.redact_panes(vec![
            json!({"id": "p1", "agent": {"name": "pi"}}),
            json!({"id": "p2", "agent": {"name": "claude"}}),
            json!({"id": "p3", "agent": null}),
        ]);
        assert_eq!(panes[0]["agent"]["name"], "pi");
        assert!(panes[1]["agent"].is_null());
        assert!(panes[2]["agent"].is_null());
    }
}
