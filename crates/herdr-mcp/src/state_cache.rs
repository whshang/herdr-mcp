use crate::events::EventStream;
use crate::herdr::HerdrClient;
use crate::snapshot;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const EVENT_STREAM_LIFETIME: Duration = Duration::from_secs(6 * 60 * 60);
const FULL_SNAPSHOT_TTL: Duration = Duration::from_secs(30);
const RECONNECT_BACKOFF_MIN: Duration = Duration::from_millis(250);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(5);
const EVENT_POLL: Duration = Duration::from_millis(250);
const DIGEST_HISTORY_MAX: usize = 2048;
const INITIAL_DIGEST_TAIL: usize = 64;

#[derive(Debug, Clone)]
pub struct DigestSnapshot {
    pub cursor: u64,
    pub events: Vec<Value>,
    pub agents: Vec<Value>,
    pub workspaces: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct CacheDiagnostics {
    pub event_count: u64,
    pub last_event_at: Option<String>,
    pub needs_reconcile: bool,
}

/// Doctor/runtime health for the event-backed snapshot cache.
///
/// - `Healthy`: ready, subscribed, and not waiting on a snapshot reconcile.
/// - `Reconciling`: ready with no transport error, but between subscription
///   cycles or mid snapshot refresh (`needs_reconcile` / temporary
///   `!stream_live`). Bounded and expected; doctor must PASS.
/// - `Failed`: bootstrap never became ready, or a real transport/stream error
///   left the cache without a live subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventCacheHealth {
    Healthy,
    Reconciling,
    Failed(String),
}

impl EventCacheHealth {
    pub fn doctor_pass(&self) -> bool {
        matches!(self, Self::Healthy | Self::Reconciling)
    }

    pub fn mode(&self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Reconciling => "reconciling",
            Self::Failed(_) => "failed",
        }
    }

    pub fn error_message(&self) -> Option<&str> {
        match self {
            Self::Failed(error) => Some(error.as_str()),
            _ => None,
        }
    }
}

struct DigestInput<'a> {
    at: &'a str,
    raw_type: &'a str,
    data: &'a Map<String, Value>,
    pane: Option<&'a Map<String, Value>>,
    workspace: Option<&'a Map<String, Value>>,
    tab: Option<&'a Map<String, Value>>,
    workspace_id: Option<&'a str>,
}

#[derive(Debug, Clone)]
struct CacheState {
    state: Value,
    digest_history: VecDeque<Value>,
    digest_cursor: u64,
    last_activity_by_pane: HashMap<String, String>,
    live_workspace_ids: HashSet<String>,
    needs_reconcile: bool,
    event_count: u64,
    last_event_at: Option<String>,
}

impl Default for CacheState {
    fn default() -> Self {
        Self {
            state: json!({}),
            digest_history: VecDeque::with_capacity(DIGEST_HISTORY_MAX),
            digest_cursor: 0,
            last_activity_by_pane: HashMap::new(),
            live_workspace_ids: HashSet::new(),
            needs_reconcile: false,
            event_count: 0,
            last_event_at: None,
        }
    }
}

impl CacheState {
    fn bootstrap(&mut self, state: Value) {
        self.state = if state.is_object() { state } else { json!({}) };
        self.live_workspace_ids = array(&self.state, "workspaces")
            .iter()
            .filter_map(workspace_id)
            .map(str::to_owned)
            .collect();
        let live_panes = array(&self.state, "panes")
            .iter()
            .filter_map(|pane| pane.get("pane_id").and_then(Value::as_str))
            .collect::<HashSet<_>>();
        self.last_activity_by_pane
            .retain(|pane, _| live_panes.contains(pane.as_str()));
        self.needs_reconcile = false;
    }

    fn reconcile_snapshot(&mut self, state: Value, at: String) {
        let next = if state.is_object() { state } else { json!({}) };
        let events = snapshot_topology_diff(&self.state, &next);
        self.bootstrap(next);
        for event in events {
            self.apply_event(event, at.clone());
        }
        self.needs_reconcile = false;
    }

    fn build_topology_subscriptions(&self) -> Vec<Value> {
        // Intentionally omit global `pane.updated`: Herdr replays retained
        // history on every subscription and this high-frequency event can
        // keep a reconnect in replay for seconds. Topology/focus/status use
        // dedicated events; bounded snapshots reconcile the remaining pane
        // metadata without turning terminal-title churn into cache traffic.
        [
            "workspace.created",
            "workspace.updated",
            "workspace.metadata_updated",
            "workspace.renamed",
            "workspace.moved",
            "workspace.reordered",
            "workspace.closed",
            "workspace.focused",
            "worktree.created",
            "worktree.opened",
            "worktree.removed",
            "tab.created",
            "tab.closed",
            "tab.focused",
            "tab.renamed",
            "tab.moved",
            "pane.created",
            "pane.closed",
            "pane.focused",
            "pane.moved",
            "pane.exited",
            "pane.agent_detected",
        ]
        .into_iter()
        .map(|kind| json!({"type": kind}))
        .collect::<Vec<_>>()
    }

    fn build_status_subscriptions(&self) -> Vec<Value> {
        let mut pane_ids = array(&self.state, "panes")
            .iter()
            .filter_map(|pane| pane.get("pane_id").and_then(Value::as_str))
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        pane_ids.extend(
            array(&self.state, "agents")
                .iter()
                .filter_map(|agent| agent.get("pane_id").and_then(Value::as_str))
                .map(str::to_owned),
        );
        let mut pane_ids = pane_ids.into_iter().collect::<Vec<_>>();
        pane_ids.sort();
        pane_ids
            .into_iter()
            .map(|pane_id| {
                json!({
                    "type": "pane.agent_status_changed",
                    "pane_id": pane_id,
                })
            })
            .collect()
    }

    fn apply_event(&mut self, event: Value, at: String) {
        let data = event
            .get("data")
            .and_then(Value::as_object)
            .cloned()
            .or_else(|| event.as_object().cloned())
            .unwrap_or_default();
        let raw_type = event
            .get("event")
            .and_then(Value::as_str)
            .or_else(|| data.get("type").and_then(Value::as_str))
            .unwrap_or("unknown")
            .to_owned();
        let kind = normalize_event_type(&raw_type);
        let pane = data.get("pane").and_then(Value::as_object).cloned();
        let workspace = data.get("workspace").and_then(Value::as_object).cloned();
        let tab = data.get("tab").and_then(Value::as_object).cloned();
        let resolved_workspace_id = workspace
            .as_ref()
            .and_then(|value| value.get("workspace_id"))
            .and_then(Value::as_str)
            .or_else(|| data.get("workspace_id").and_then(Value::as_str))
            .or_else(|| {
                pane.as_ref()
                    .and_then(|value| value.get("workspace_id"))
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                tab.as_ref()
                    .and_then(|value| value.get("workspace_id"))
                    .and_then(Value::as_str)
            })
            .map(str::to_owned);
        let workspace_closed = kind == "workspace_closed";

        let safe_missing_scope = matches!(
            kind.as_str(),
            "workspace_closed" | "pane_closed" | "pane_exited" | "tab_closed" | "worktree_removed"
        );
        if let Some(workspace_id) = resolved_workspace_id.as_deref()
            && !self.live_workspace_ids.contains(workspace_id)
            && !safe_missing_scope
        {
            self.needs_reconcile = true;
            return;
        }

        if kind == "pane_updated"
            && let Some(pane) = pane.as_ref()
            && pane_update_only_scroll(&self.state, pane)
            && let Some(pane_id) = pane.get("pane_id").and_then(Value::as_str)
        {
            upsert(
                &mut self.state,
                "panes",
                "pane_id",
                pane_id,
                Value::Object(pane.clone()),
            );
            return;
        }

        self.event_count = self.event_count.saturating_add(1);
        self.last_event_at = Some(at.clone());
        self.digest_cursor = self.digest_cursor.saturating_add(1);
        self.push_digest(DigestInput {
            at: &at,
            raw_type: &raw_type,
            data: &data,
            pane: pane.as_ref(),
            workspace: workspace.as_ref(),
            tab: tab.as_ref(),
            workspace_id: resolved_workspace_id.as_deref(),
        });

        if kind.starts_with("worktree_") {
            self.needs_reconcile = true;
            return;
        }

        let direct_pane_id = data.get("pane_id").and_then(Value::as_str);
        let pane_id = pane
            .as_ref()
            .and_then(|value| value.get("pane_id"))
            .and_then(Value::as_str)
            .or(direct_pane_id)
            .map(str::to_owned);

        if matches!(kind.as_str(), "pane_closed" | "pane_exited") {
            if let Some(pane_id) = pane_id.as_deref() {
                self.remove_pane(pane_id);
            }
        } else if let Some(pane) = pane {
            if let Some(pane_id) = pane
                .get("pane_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
            {
                self.last_activity_by_pane
                    .insert(pane_id.clone(), at.clone());
                upsert(
                    &mut self.state,
                    "panes",
                    "pane_id",
                    &pane_id,
                    Value::Object(pane.clone()),
                );
                if let Some(agent_name) = pane.get("agent").and_then(Value::as_str) {
                    let mut agent = find_by_id(&self.state, "agents", "pane_id", &pane_id)
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    agent.insert("agent".to_owned(), json!(agent_name));
                    agent.insert("pane_id".to_owned(), json!(pane_id));
                    copy_if_present(&pane, &mut agent, "workspace_id");
                    copy_if_present(&pane, &mut agent, "agent_status");
                    copy_if_present(&pane, &mut agent, "terminal_title");
                    copy_if_present(&pane, &mut agent, "state_change_seq");
                    copy_if_present(&pane, &mut agent, "agent_session");
                    if let Some(cwd) = pane
                        .get("cwd")
                        .cloned()
                        .or_else(|| pane.get("foreground_cwd").cloned())
                    {
                        agent.insert("cwd".to_owned(), cwd);
                    }
                    upsert(
                        &mut self.state,
                        "agents",
                        "pane_id",
                        &pane_id,
                        Value::Object(agent),
                    );
                } else {
                    remove_by_id(&mut self.state, "agents", "pane_id", &pane_id);
                }
            }
        } else if kind.starts_with("pane_")
            && !matches!(kind.as_str(), "pane_closed" | "pane_exited")
        {
            self.needs_reconcile = true;
        }

        if workspace_closed {
            if let Some(workspace_id) = resolved_workspace_id.as_deref() {
                self.remove_workspace(workspace_id);
            }
        } else if let Some(workspace) = workspace {
            if let Some(workspace_id) = workspace
                .get("workspace_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
            {
                upsert(
                    &mut self.state,
                    "workspaces",
                    "workspace_id",
                    &workspace_id,
                    Value::Object(workspace),
                );
            }
        } else if kind.starts_with("workspace_") && kind != "workspace_focused" {
            self.needs_reconcile = true;
        }

        if let Some(tab) = tab {
            if let Some(tab_id) = tab.get("tab_id").and_then(Value::as_str).map(str::to_owned) {
                if kind == "tab_closed" {
                    remove_by_id(&mut self.state, "tabs", "tab_id", &tab_id);
                } else {
                    upsert(
                        &mut self.state,
                        "tabs",
                        "tab_id",
                        &tab_id,
                        Value::Object(tab),
                    );
                }
            }
        } else if kind == "tab_closed" {
            if let Some(tab_id) = data.get("tab_id").and_then(Value::as_str) {
                remove_by_id(&mut self.state, "tabs", "tab_id", tab_id);
            }
        } else if kind.starts_with("tab_") && kind != "tab_focused" {
            self.needs_reconcile = true;
        }

        match kind.as_str() {
            "workspace_focused" => {
                if let Some(workspace_id) = resolved_workspace_id {
                    set_field(&mut self.state, "focused_workspace_id", json!(workspace_id));
                }
            }
            "pane_focused" => {
                if let Some(pane_id) = pane_id {
                    set_field(&mut self.state, "focused_pane_id", json!(pane_id));
                }
            }
            "tab_focused" => {
                let tab_id = data.get("tab_id").and_then(Value::as_str).or_else(|| {
                    data.get("tab")
                        .and_then(|value| value.get("tab_id"))
                        .and_then(Value::as_str)
                });
                if let Some(tab_id) = tab_id {
                    set_field(&mut self.state, "focused_tab_id", json!(tab_id));
                }
            }
            _ => {}
        }
    }

    fn push_digest(&mut self, input: DigestInput<'_>) {
        let mut digest = Map::new();
        digest.insert("cursor".to_owned(), json!(self.digest_cursor));
        digest.insert("at".to_owned(), json!(input.at));
        digest.insert("type".to_owned(), json!(input.raw_type));
        if let Some(workspace_id) = input.workspace_id {
            digest.insert("workspace_id".to_owned(), json!(workspace_id));
        }
        let pane_id = input
            .pane
            .and_then(|value| value.get("pane_id"))
            .and_then(Value::as_str)
            .or_else(|| input.data.get("pane_id").and_then(Value::as_str));
        if let Some(pane_id) = pane_id {
            digest.insert("pane_id".to_owned(), json!(pane_id));
        }
        if let Some(pane) = input.pane {
            digest.insert("pane".to_owned(), Value::Object(pane.clone()));
        }
        if let Some(workspace) = input.workspace {
            digest.insert("workspace".to_owned(), Value::Object(workspace.clone()));
        }
        if let Some(tab) = input.tab {
            digest.insert("tab".to_owned(), Value::Object(tab.clone()));
        }
        self.digest_history.push_back(Value::Object(digest));
        while self.digest_history.len() > DIGEST_HISTORY_MAX {
            self.digest_history.pop_front();
        }
    }

    fn remove_pane(&mut self, pane_id: &str) {
        remove_by_id(&mut self.state, "panes", "pane_id", pane_id);
        remove_by_id(&mut self.state, "agents", "pane_id", pane_id);
        self.last_activity_by_pane.remove(pane_id);
    }

    fn remove_workspace(&mut self, workspace_id: &str) {
        self.live_workspace_ids.remove(workspace_id);
        remove_by_id(&mut self.state, "workspaces", "workspace_id", workspace_id);
        remove_where(&mut self.state, "panes", |value| {
            value.get("workspace_id").and_then(Value::as_str) == Some(workspace_id)
        });
        remove_where(&mut self.state, "agents", |value| {
            value.get("workspace_id").and_then(Value::as_str) == Some(workspace_id)
        });
        remove_where(&mut self.state, "tabs", |value| {
            value.get("workspace_id").and_then(Value::as_str) == Some(workspace_id)
        });
        let live_panes = array(&self.state, "panes")
            .iter()
            .filter_map(|pane| pane.get("pane_id").and_then(Value::as_str))
            .collect::<HashSet<_>>();
        self.last_activity_by_pane
            .retain(|pane, _| live_panes.contains(pane.as_str()));
    }

    fn digest_since(&self, cursor: u64) -> DigestSnapshot {
        let events = if cursor > 0 {
            self.digest_history
                .iter()
                .filter(|event| {
                    event
                        .get("cursor")
                        .and_then(Value::as_u64)
                        .is_some_and(|value| value > cursor)
                })
                .cloned()
                .collect()
        } else {
            self.digest_history
                .iter()
                .skip(
                    self.digest_history
                        .len()
                        .saturating_sub(INITIAL_DIGEST_TAIL),
                )
                .cloned()
                .collect()
        };
        DigestSnapshot {
            cursor: self.digest_cursor,
            events: coalesce_digest_updates(events),
            agents: self.agent_views(),
            workspaces: array(&self.state, "workspaces").to_vec(),
        }
    }

    fn agent_views(&self) -> Vec<Value> {
        array(&self.state, "agents")
            .iter()
            .map(|agent| {
                let pane = agent.get("pane_id").and_then(Value::as_str);
                let session = agent.get("agent_session").filter(|value| value.is_object());
                let target = agent
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(|value| Value::String(value.to_owned()))
                    .or_else(|| agent.get("pane_id").cloned())
                    .unwrap_or(Value::Null);
                json!({
                    "name": target,
                    "agent_id": target,
                    "kind": agent.get("kind").cloned().or_else(|| agent.get("agent_kind").cloned()).or_else(|| agent.get("agent").cloned()).unwrap_or(Value::Null),
                    "pane": agent.get("pane_id").cloned().unwrap_or(Value::Null),
                    "status": agent.get("agent_status").cloned().or_else(|| agent.get("status").cloned()).unwrap_or(Value::Null),
                    "workspace": agent.get("workspace_id").cloned().unwrap_or(Value::Null),
                    "cwd": agent.get("cwd").cloned().or_else(|| agent.get("foreground_cwd").cloned()).unwrap_or(Value::Null),
                    "started_at": session
                        .and_then(|value| value.get("value"))
                        .and_then(Value::as_str)
                        .and_then(parse_session_started)
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                    "last_activity_at": pane
                        .and_then(|pane| self.last_activity_by_pane.get(pane))
                        .cloned()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                    "state_change_seq": agent.get("state_change_seq").cloned().unwrap_or(Value::Null),
                    "terminal_title": agent.get("terminal_title").cloned().unwrap_or(Value::Null),
                    "session_ref": session.cloned().unwrap_or(Value::Null),
                })
            })
            .collect()
    }
}

fn snapshot_topology_diff(before: &Value, after: &Value) -> Vec<Value> {
    let mut events = Vec::new();
    let before_workspaces = array(before, "workspaces")
        .iter()
        .filter_map(|item| workspace_id(item).map(|id| (id.to_owned(), item)))
        .collect::<HashMap<_, _>>();
    let after_workspaces = array(after, "workspaces")
        .iter()
        .filter_map(|item| workspace_id(item).map(|id| (id.to_owned(), item)))
        .collect::<HashMap<_, _>>();

    let mut workspace_ids = after_workspaces.keys().cloned().collect::<Vec<_>>();
    workspace_ids.sort();
    for workspace_id in workspace_ids {
        let current = after_workspaces[&workspace_id];
        match before_workspaces.get(&workspace_id) {
            None => events.push(json!({
                "event": "workspace_created",
                "workspace_id": workspace_id,
                "workspace": current,
            })),
            Some(previous) if *previous != current => events.push(json!({
                "event": "workspace_updated",
                "workspace_id": workspace_id,
                "workspace": current,
            })),
            _ => {}
        }
    }

    let before_panes = array(before, "panes")
        .iter()
        .filter_map(|item| {
            item.get("pane_id")
                .and_then(Value::as_str)
                .map(|id| (id.to_owned(), item))
        })
        .collect::<HashMap<_, _>>();
    let after_panes = array(after, "panes")
        .iter()
        .filter_map(|item| {
            item.get("pane_id")
                .and_then(Value::as_str)
                .map(|id| (id.to_owned(), item))
        })
        .collect::<HashMap<_, _>>();

    let mut pane_ids = after_panes.keys().cloned().collect::<Vec<_>>();
    pane_ids.sort();
    for pane_id in pane_ids {
        let current = after_panes[&pane_id];
        let workspace_id = current.get("workspace_id").cloned().unwrap_or(Value::Null);
        match before_panes.get(&pane_id) {
            None => events.push(json!({
                "event": "pane_created",
                "pane_id": pane_id,
                "workspace_id": workspace_id,
                "pane": current,
            })),
            Some(previous) if *previous != current => events.push(json!({
                "event": "pane_updated",
                "pane_id": pane_id,
                "workspace_id": workspace_id,
                "pane": current,
            })),
            _ => {}
        }
    }

    let mut removed_panes = before_panes
        .keys()
        .filter(|pane_id| !after_panes.contains_key(*pane_id))
        .cloned()
        .collect::<Vec<_>>();
    removed_panes.sort();
    for pane_id in removed_panes {
        let previous = before_panes[&pane_id];
        events.push(json!({
            "event": "pane_closed",
            "pane_id": pane_id,
            "workspace_id": previous.get("workspace_id").cloned().unwrap_or(Value::Null),
        }));
    }

    let mut removed_workspaces = before_workspaces
        .keys()
        .filter(|workspace_id| !after_workspaces.contains_key(*workspace_id))
        .cloned()
        .collect::<Vec<_>>();
    removed_workspaces.sort();
    for workspace_id in removed_workspaces {
        events.push(json!({
            "event": "workspace_closed",
            "workspace_id": workspace_id,
        }));
    }
    events
}

fn pane_update_only_scroll(state: &Value, incoming: &Map<String, Value>) -> bool {
    let Some(pane_id) = incoming.get("pane_id").and_then(Value::as_str) else {
        return false;
    };
    let Some(previous) = find_by_id(state, "panes", "pane_id", pane_id).and_then(Value::as_object)
    else {
        return false;
    };
    let mut previous = previous.clone();
    let mut incoming = incoming.clone();
    let previous_scroll = previous.remove("scroll");
    let incoming_scroll = incoming.remove("scroll");
    previous.remove("state_change_seq");
    incoming.remove("state_change_seq");
    previous_scroll != incoming_scroll
        && (previous_scroll.is_some() || incoming_scroll.is_some())
        && previous == incoming
}

fn coalesce_digest_updates(events: Vec<Value>) -> Vec<Value> {
    let mut coalesced = Vec::<Value>::with_capacity(events.len());
    for event in events {
        let replace_last = coalesced
            .last()
            .is_some_and(|last| same_duplicate_update(last, &event));
        if replace_last {
            if let Some(last) = coalesced.last_mut() {
                *last = event;
            }
        } else {
            coalesced.push(event);
        }
    }
    coalesced
}

fn same_duplicate_update(left: &Value, right: &Value) -> bool {
    if digest_update_key(left) != digest_update_key(right) || digest_update_key(left).is_none() {
        return false;
    }
    digest_payload(left) == digest_payload(right)
}

fn digest_payload(event: &Value) -> Value {
    let mut payload = event.clone();
    if let Some(object) = payload.as_object_mut() {
        object.remove("cursor");
        object.remove("at");
        if let Some(kind) = object
            .get("type")
            .and_then(Value::as_str)
            .map(normalize_event_type)
        {
            object.insert("type".to_owned(), json!(kind));
        }
    }
    payload
}

fn digest_update_key(event: &Value) -> Option<(String, String)> {
    let kind = normalize_event_type(event.get("type").and_then(Value::as_str)?);
    let id = match kind.as_str() {
        "workspace_updated" | "workspace_metadata_updated" => event
            .get("workspace_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        "pane_updated" | "pane_agent_status_changed" => event
            .get("pane_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        "tab_updated" => event
            .get("tab")
            .and_then(|tab| tab.get("tab_id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }?;
    Some((kind, id))
}

struct SharedCache {
    state: RwLock<CacheState>,
    ready: Mutex<bool>,
    ready_condvar: Condvar,
    stop: AtomicBool,
    stream_connected: AtomicBool,
    stream_live: AtomicBool,
    last_error: Mutex<Option<String>>,
}

pub struct EventCache {
    shared: Arc<SharedCache>,
    worker: Option<JoinHandle<()>>,
    boot_id: String,
}

impl EventCache {
    pub fn start(client: HerdrClient) -> Self {
        let shared = Arc::new(SharedCache {
            state: RwLock::new(CacheState::default()),
            ready: Mutex::new(false),
            ready_condvar: Condvar::new(),
            stop: AtomicBool::new(false),
            stream_connected: AtomicBool::new(false),
            stream_live: AtomicBool::new(false),
            last_error: Mutex::new(None),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::spawn(move || run_loop(client, worker_shared));
        Self {
            shared,
            worker: Some(worker),
            boot_id: new_boot_id(),
        }
    }

    #[cfg(test)]
    pub fn from_snapshot_for_test(snapshot: Value) -> Self {
        let mut state = CacheState::default();
        state.bootstrap(snapshot);
        Self {
            shared: Arc::new(SharedCache {
                state: RwLock::new(state),
                ready: Mutex::new(true),
                ready_condvar: Condvar::new(),
                stop: AtomicBool::new(false),
                stream_connected: AtomicBool::new(true),
                stream_live: AtomicBool::new(true),
                last_error: Mutex::new(None),
            }),
            worker: None,
            boot_id: "test-boot".to_owned(),
        }
    }

    pub fn boot_id(&self) -> &str {
        &self.boot_id
    }

    pub fn wait_ready(&self, timeout: Duration) -> bool {
        let Ok(ready) = self.shared.ready.lock() else {
            return false;
        };
        if *ready {
            return true;
        }
        self.shared
            .ready_condvar
            .wait_timeout_while(ready, timeout, |ready| !*ready)
            .is_ok_and(|(ready, _)| *ready)
    }

    pub fn wait_stream_live(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.shared.stream_live.load(Ordering::Acquire) {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        self.shared.stream_live.load(Ordering::Acquire)
    }

    pub fn wait_stream_connected(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.shared.stream_connected.load(Ordering::Acquire) {
                return true;
            }
            thread::sleep(Duration::from_millis(10));
        }
        self.shared.stream_connected.load(Ordering::Acquire)
    }

    /// Classify cache health without waiting.
    ///
    /// Temporary `!stream_live` / `needs_reconcile` without a transport error is
    /// `Reconciling`, not `Failed`. Sticky `last_error` while the stream is down
    /// remains a hard failure.
    pub fn classify_health(&self) -> EventCacheHealth {
        let ready = self.shared.ready.lock().ok().is_some_and(|ready| *ready);
        let stream_live = self.shared.stream_live.load(Ordering::Acquire);
        let needs_reconcile = self.diagnostics().needs_reconcile;
        let error = self.last_error();

        if !ready {
            return EventCacheHealth::Failed(
                error.unwrap_or_else(|| "event cache bootstrap timed out".to_owned()),
            );
        }

        if let Some(error) = error {
            if !stream_live {
                return EventCacheHealth::Failed(error);
            }
            // Stream recovered but a prior error was not cleared: treat as failed
            // so sticky errors cannot silently PASS.
            return EventCacheHealth::Failed(error);
        }

        if needs_reconcile || !stream_live {
            return EventCacheHealth::Reconciling;
        }

        EventCacheHealth::Healthy
    }

    /// Wait for bootstrap + initial subscribe, then optionally finish one bounded
    /// reconcile/resubscribe cycle before classifying.
    ///
    /// Does not widen the initial ready/live waits; `reconcile_budget` only covers
    /// an already-observed reconcile/resubscribe window so doctor does not sample
    /// mid-cycle as a hard FAIL.
    pub fn wait_for_doctor_probe(
        &self,
        ready_timeout: Duration,
        live_timeout: Duration,
        reconcile_budget: Duration,
    ) -> EventCacheHealth {
        if !self.wait_ready(ready_timeout) {
            return EventCacheHealth::Failed(
                self.last_error()
                    .unwrap_or_else(|| "event cache bootstrap timed out".to_owned()),
            );
        }

        let _ = self.wait_stream_live(live_timeout);
        match self.classify_health() {
            EventCacheHealth::Reconciling => {
                let deadline = Instant::now() + reconcile_budget;
                while Instant::now() < deadline {
                    match self.classify_health() {
                        EventCacheHealth::Healthy => return EventCacheHealth::Healthy,
                        EventCacheHealth::Failed(error) => {
                            return EventCacheHealth::Failed(error);
                        }
                        EventCacheHealth::Reconciling => {
                            thread::sleep(Duration::from_millis(10));
                        }
                    }
                }
                self.classify_health()
            }
            other => other,
        }
    }

    pub fn digest_since(&self, cursor: u64) -> DigestSnapshot {
        self.shared
            .state
            .read()
            .map(|state| state.digest_since(cursor))
            .unwrap_or_else(|_| DigestSnapshot {
                cursor: 0,
                events: vec![],
                agents: vec![],
                workspaces: vec![],
            })
    }

    pub fn snapshot(&self) -> Value {
        self.shared
            .state
            .read()
            .map(|state| state.state.clone())
            .unwrap_or_else(|_| json!({}))
    }

    /// Return a live event-backed snapshot only while the cache is ready,
    /// subscribed, and not waiting for a reconciliation refresh.
    pub fn fresh_snapshot(&self) -> Option<Value> {
        if !self.shared.stream_live.load(Ordering::Acquire) {
            return None;
        }
        if !self.shared.ready.lock().ok().is_some_and(|ready| *ready) {
            return None;
        }
        self.shared
            .state
            .read()
            .ok()
            .and_then(|state| (!state.needs_reconcile).then(|| state.state.clone()))
    }

    pub fn last_error(&self) -> Option<String> {
        self.shared
            .last_error
            .lock()
            .ok()
            .and_then(|value| value.clone())
    }

    pub fn diagnostics(&self) -> CacheDiagnostics {
        self.shared
            .state
            .read()
            .map(|state| CacheDiagnostics {
                event_count: state.event_count,
                last_event_at: state.last_event_at.clone(),
                needs_reconcile: state.needs_reconcile,
            })
            .unwrap_or(CacheDiagnostics {
                event_count: 0,
                last_event_at: None,
                needs_reconcile: false,
            })
    }

    pub fn shutdown(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.stream_connected.store(false, Ordering::Release);
        self.shared.stream_live.store(false, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for EventCache {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_loop(client: HerdrClient, shared: Arc<SharedCache>) {
    let mut reconnect_failures = 0_u32;
    while !shared.stop.load(Ordering::Acquire) {
        shared.stream_connected.store(false, Ordering::Release);
        shared.stream_live.store(false, Ordering::Release);
        match snapshot::fetch(&client) {
            Ok(snapshot) => {
                if let Ok(mut state) = shared.state.write() {
                    state.bootstrap(snapshot.value);
                }
                if let Ok(mut ready) = shared.ready.lock() {
                    *ready = true;
                    shared.ready_condvar.notify_all();
                }
                // Snapshot bootstrap succeeded; clear stale transport errors from a
                // prior cycle so doctor does not FAIL on recovered streams.
                clear_last_error(&shared);
            }
            Err(error) => {
                set_last_error(&shared, error);
                sleep_interruptible(
                    &shared.stop,
                    next_reconnect_backoff(&mut reconnect_failures),
                );
                continue;
            }
        }

        let topology_subscriptions = shared
            .state
            .read()
            .map(|state| state.build_topology_subscriptions())
            .unwrap_or_default();
        let mut topology_stream =
            match EventStream::subscribe(&client, topology_subscriptions, EVENT_STREAM_LIFETIME) {
                Ok(stream) => stream,
                Err(error) => {
                    set_last_error(&shared, error.to_string());
                    sleep_interruptible(
                        &shared.stop,
                        next_reconnect_backoff(&mut reconnect_failures),
                    );
                    continue;
                }
            };
        let mut status_subscriptions = shared
            .state
            .read()
            .map(|state| state.build_status_subscriptions())
            .unwrap_or_default();
        let mut status_stream = match EventStream::subscribe(
            &client,
            status_subscriptions.clone(),
            EVENT_STREAM_LIFETIME,
        ) {
            Ok(stream) => stream,
            Err(error) => {
                set_last_error(&shared, error.to_string());
                sleep_interruptible(
                    &shared.stop,
                    next_reconnect_backoff(&mut reconnect_failures),
                );
                continue;
            }
        };
        shared.stream_connected.store(true, Ordering::Release);
        clear_last_error(&shared);

        if let Err(error) = discard_initial_replay(&mut topology_stream, &shared.stop) {
            shared.stream_connected.store(false, Ordering::Release);
            set_last_error(&shared, error);
            sleep_interruptible(
                &shared.stop,
                next_reconnect_backoff(&mut reconnect_failures),
            );
            continue;
        }
        if let Err(error) = discard_initial_replay(&mut status_stream, &shared.stop) {
            shared.stream_connected.store(false, Ordering::Release);
            set_last_error(&shared, error);
            sleep_interruptible(
                &shared.stop,
                next_reconnect_backoff(&mut reconnect_failures),
            );
            continue;
        }
        if shared.stop.load(Ordering::Acquire) {
            shared.stream_connected.store(false, Ordering::Release);
            return;
        }

        // `events.subscribe` replays retained history before becoming live.
        // Reconcile once after that replay fence so stale focus/topology events
        // never enter the cache or browser digest. Keep this exact stream open:
        // re-subscribing here would replay the same history again.
        match snapshot::fetch(&client) {
            Ok(snapshot) => {
                if let Ok(mut state) = shared.state.write() {
                    state.reconcile_snapshot(snapshot.value, now_iso());
                }
            }
            Err(error) => {
                shared.stream_connected.store(false, Ordering::Release);
                set_last_error(&shared, error);
                sleep_interruptible(
                    &shared.stop,
                    next_reconnect_backoff(&mut reconnect_failures),
                );
                continue;
            }
        }

        // Topology replay can span several seconds. If the post-fence snapshot
        // contains a different pane set, refresh only the cheap per-pane status
        // stream; never replay the global topology stream for this reason.
        let desired_status_subscriptions = shared
            .state
            .read()
            .map(|state| state.build_status_subscriptions())
            .unwrap_or_default();
        if desired_status_subscriptions != status_subscriptions {
            match replace_status_stream(
                &client,
                &shared,
                desired_status_subscriptions,
                &mut status_stream,
                &mut status_subscriptions,
            ) {
                Ok(()) => {}
                Err(error) => {
                    shared.stream_connected.store(false, Ordering::Release);
                    set_last_error(&shared, error);
                    sleep_interruptible(
                        &shared.stop,
                        next_reconnect_backoff(&mut reconnect_failures),
                    );
                    continue;
                }
            }
        }
        clear_last_error(&shared);
        shared.stream_live.store(true, Ordering::Release);
        reconnect_failures = 0;
        let mut full_snapshot_started = Instant::now();

        loop {
            if shared.stop.load(Ordering::Acquire) {
                shared.stream_connected.store(false, Ordering::Release);
                shared.stream_live.store(false, Ordering::Release);
                return;
            }
            if topology_stream.is_expired() {
                break;
            }
            if full_snapshot_started.elapsed() >= FULL_SNAPSHOT_TTL {
                match snapshot::fetch(&client) {
                    Ok(snapshot) => {
                        if let Ok(mut state) = shared.state.write() {
                            state.reconcile_snapshot(snapshot.value, now_iso());
                        }
                    }
                    Err(error) => {
                        set_last_error(&shared, error);
                        break;
                    }
                }
                full_snapshot_started = Instant::now();
            }

            let desired_status_subscriptions = shared
                .state
                .read()
                .map(|state| state.build_status_subscriptions())
                .unwrap_or_default();
            if (status_stream.is_expired() || desired_status_subscriptions != status_subscriptions)
                && let Err(error) = replace_status_stream(
                    &client,
                    &shared,
                    desired_status_subscriptions,
                    &mut status_stream,
                    &mut status_subscriptions,
                )
            {
                // Keep the old status stream and global topology stream alive
                // on a transient status-subscription failure. A later loop
                // iteration retries the desired status subscription.
                set_last_error(&shared, error);
            }

            match topology_stream.poll_event(EVENT_POLL) {
                Ok(Some(event)) => {
                    if let Err(error) = apply_live_event(&client, &shared, event) {
                        set_last_error(&shared, error);
                        break;
                    }
                }
                Ok(None) => {
                    if topology_stream.is_expired() {
                        break;
                    }
                }
                Err(error) => {
                    set_last_error(&shared, error.to_string());
                    break;
                }
            }

            match status_stream.poll_event(EVENT_POLL) {
                Ok(Some(event)) => {
                    if let Err(error) = apply_live_event(&client, &shared, event) {
                        set_last_error(&shared, error);
                        break;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    set_last_error(&shared, error.to_string());
                    // Force a status-only replacement on the next iteration.
                    status_subscriptions.clear();
                }
            }
        }
        shared.stream_connected.store(false, Ordering::Release);
        shared.stream_live.store(false, Ordering::Release);

        sleep_interruptible(
            &shared.stop,
            next_reconnect_backoff(&mut reconnect_failures),
        );
    }
}

fn next_reconnect_backoff(failures: &mut u32) -> Duration {
    let shift = (*failures).min(5);
    *failures = failures.saturating_add(1);
    let multiplier = 1_u32 << shift;
    RECONNECT_BACKOFF_MIN
        .saturating_mul(multiplier)
        .min(RECONNECT_BACKOFF_MAX)
}

fn replace_status_stream(
    client: &HerdrClient,
    shared: &SharedCache,
    desired: Vec<Value>,
    stream: &mut EventStream,
    subscriptions: &mut Vec<Value>,
) -> Result<(), String> {
    let mut next = EventStream::subscribe(client, desired.clone(), EVENT_STREAM_LIFETIME)
        .map_err(|error| error.to_string())?;
    discard_initial_replay(&mut next, &shared.stop)?;
    if shared.stop.load(Ordering::Acquire) {
        return Ok(());
    }
    let snapshot = snapshot::fetch(client)?;
    if let Ok(mut state) = shared.state.write() {
        state.reconcile_snapshot(snapshot.value, now_iso());
    }
    *stream = next;
    *subscriptions = desired;
    clear_last_error(shared);
    Ok(())
}

fn apply_live_event(
    client: &HerdrClient,
    shared: &SharedCache,
    event: Value,
) -> Result<(), String> {
    let needs_reconcile = if let Ok(mut state) = shared.state.write() {
        state.apply_event(event, now_iso());
        state.needs_reconcile
    } else {
        false
    };
    if needs_reconcile {
        let snapshot = snapshot::fetch(client)?;
        if let Ok(mut state) = shared.state.write() {
            state.reconcile_snapshot(snapshot.value, now_iso());
        }
    }
    Ok(())
}

fn discard_initial_replay(stream: &mut EventStream, stop: &AtomicBool) -> Result<usize, String> {
    let mut discarded = 0usize;
    loop {
        if stop.load(Ordering::Acquire) {
            return Ok(discarded);
        }
        match stream.poll_event(EVENT_POLL) {
            Ok(Some(_)) => {
                discarded = discarded.saturating_add(1);
            }
            Ok(None) => return Ok(discarded),
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn set_last_error(shared: &SharedCache, error: String) {
    if let Ok(mut value) = shared.last_error.lock() {
        *value = Some(error);
    }
}

fn clear_last_error(shared: &SharedCache) {
    if let Ok(mut value) = shared.last_error.lock() {
        *value = None;
    }
}

fn sleep_interruptible(stop: &AtomicBool, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if stop.load(Ordering::Acquire) {
            return;
        }
        thread::sleep(
            Duration::from_millis(10).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

fn new_boot_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{:x}-{:x}", std::process::id(), nanos)
}

fn normalize_event_type(value: &str) -> String {
    value
        .replace('.', "_")
        .strip_suffix("_event")
        .unwrap_or(&value.replace('.', "_"))
        .to_owned()
}

fn parse_session_started(value: &str) -> Option<String> {
    let t = value.find('T')?;
    if t < 10 {
        return None;
    }
    let start = t - 10;
    let z_offset = value[t..].find('Z')?;
    let raw = &value[start..=t + z_offset];
    if raw.len() < 20 {
        return None;
    }
    let date = &raw[..11];
    let time = raw[11..raw.len() - 1].split('-').collect::<Vec<_>>();
    if time.len() < 3 || time[0].len() != 2 || time[1].len() != 2 || time[2].len() != 2 {
        return None;
    }
    let millis = time
        .get(3)
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    Some(format!(
        "{date}{}:{}:{}{millis}Z",
        time[0], time[1], time[2]
    ))
}

fn workspace_id(workspace: &Value) -> Option<&str> {
    workspace
        .get("workspace_id")
        .and_then(Value::as_str)
        .or_else(|| workspace.get("id").and_then(Value::as_str))
}

fn set_field(state: &mut Value, key: &str, value: Value) {
    if !state.is_object() {
        *state = json!({});
    }
    if let Some(object) = state.as_object_mut() {
        object.insert(key.to_owned(), value);
    }
}

fn ensure_array_mut<'a>(state: &'a mut Value, key: &str) -> &'a mut Vec<Value> {
    if !state.is_object() {
        *state = json!({});
    }
    let object = state.as_object_mut().expect("state normalized to object");
    if !object.get(key).is_some_and(Value::is_array) {
        object.insert(key.to_owned(), Value::Array(Vec::new()));
    }
    object
        .get_mut(key)
        .and_then(Value::as_array_mut)
        .expect("array inserted above")
}

fn upsert(state: &mut Value, key: &str, id_key: &str, id: &str, item: Value) {
    let items = ensure_array_mut(state, key);
    if let Some(index) = items
        .iter()
        .position(|value| value.get(id_key).and_then(Value::as_str) == Some(id))
    {
        items[index] = item;
    } else {
        items.push(item);
    }
}

fn find_by_id<'a>(state: &'a Value, key: &str, id_key: &str, id: &str) -> Option<&'a Value> {
    array(state, key)
        .iter()
        .find(|value| value.get(id_key).and_then(Value::as_str) == Some(id))
}

fn copy_if_present(source: &Map<String, Value>, target: &mut Map<String, Value>, key: &str) {
    if let Some(value) = source.get(key) {
        target.insert(key.to_owned(), value.clone());
    }
}

fn remove_by_id(state: &mut Value, key: &str, id_key: &str, id: &str) {
    remove_where(state, key, |value| {
        value.get(id_key).and_then(Value::as_str) == Some(id)
    });
}

fn remove_where(state: &mut Value, key: &str, predicate: impl Fn(&Value) -> bool) {
    ensure_array_mut(state, key).retain(|value| !predicate(value));
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_backoff_is_exponential_and_capped() {
        let mut failures = 0;
        assert_eq!(
            next_reconnect_backoff(&mut failures),
            Duration::from_millis(250)
        );
        assert_eq!(
            next_reconnect_backoff(&mut failures),
            Duration::from_millis(500)
        );
        assert_eq!(
            next_reconnect_backoff(&mut failures),
            Duration::from_secs(1)
        );
        assert_eq!(
            next_reconnect_backoff(&mut failures),
            Duration::from_secs(2)
        );
        assert_eq!(
            next_reconnect_backoff(&mut failures),
            Duration::from_secs(4)
        );
        assert_eq!(
            next_reconnect_backoff(&mut failures),
            Duration::from_secs(5)
        );
        assert_eq!(
            next_reconnect_backoff(&mut failures),
            Duration::from_secs(5)
        );
    }

    fn fixture() -> Value {
        json!({
            "focused_workspace_id": "w1",
            "workspaces": [{"workspace_id": "w1", "label": "demo", "pane_count": 1, "tab_count": 1}],
            "tabs": [{"tab_id": "w1:t1", "workspace_id": "w1"}],
            "panes": [{"pane_id": "w1:p1", "workspace_id": "w1", "cwd": "/tmp/demo"}],
            "agents": [{
                "agent": "pi",
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "agent_status": "idle",
                "cwd": "/tmp/demo",
                "agent_session": {"value": "/tmp/2026-08-25T04-00-01-123Z_session.jsonl"}
            }]
        })
    }

    #[test]
    fn topology_and_status_subscriptions_have_separate_ownership() {
        let mut state = CacheState::default();
        state.bootstrap(fixture());
        let topology = state.build_topology_subscriptions();
        let status = state.build_status_subscriptions();
        assert!(topology.contains(&json!({"type": "workspace.updated"})));
        assert!(!topology.contains(&json!({"type": "pane.updated"})));
        assert!(!topology.iter().any(|subscription| {
            subscription.get("type").and_then(Value::as_str) == Some("pane.agent_status_changed")
        }));
        assert!(status.contains(&json!({
            "type": "pane.agent_status_changed",
            "pane_id": "w1:p1"
        })));
        assert!(!status.contains(&json!({"type": "workspace.updated"})));
    }

    #[test]
    fn fresh_snapshot_requires_live_non_reconciling_cache() {
        let cache = EventCache::from_snapshot_for_test(fixture());
        assert_eq!(
            cache.fresh_snapshot().unwrap()["focused_workspace_id"],
            "w1"
        );
        cache.shared.state.write().unwrap().needs_reconcile = true;
        assert!(cache.fresh_snapshot().is_none());
    }

    #[test]
    fn health_classifies_healthy_when_live_and_settled() {
        let cache = EventCache::from_snapshot_for_test(fixture());
        assert_eq!(cache.classify_health(), EventCacheHealth::Healthy);
        assert!(cache.classify_health().doctor_pass());
    }

    #[test]
    fn health_classifies_reconciling_when_stream_drops_without_error() {
        let cache = EventCache::from_snapshot_for_test(fixture());
        cache.shared.stream_live.store(false, Ordering::Release);
        assert_eq!(cache.classify_health(), EventCacheHealth::Reconciling);
        assert!(cache.classify_health().doctor_pass());
        // Repeated classification stays stable (no PASS/FAIL jitter).
        assert_eq!(cache.classify_health(), EventCacheHealth::Reconciling);
        assert_eq!(cache.classify_health(), EventCacheHealth::Reconciling);
    }

    #[test]
    fn health_classifies_reconciling_when_needs_reconcile() {
        let cache = EventCache::from_snapshot_for_test(fixture());
        cache.shared.state.write().unwrap().needs_reconcile = true;
        assert_eq!(cache.classify_health(), EventCacheHealth::Reconciling);
        assert_eq!(cache.classify_health().mode(), "reconciling");
        assert!(cache.classify_health().doctor_pass());
    }

    #[test]
    fn health_classifies_failed_on_transport_error_without_live_stream() {
        let cache = EventCache::from_snapshot_for_test(fixture());
        cache.shared.stream_live.store(false, Ordering::Release);
        set_last_error(&cache.shared, "stream reset".to_owned());
        match cache.classify_health() {
            EventCacheHealth::Failed(error) => assert_eq!(error, "stream reset"),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(!cache.classify_health().doctor_pass());
    }

    #[test]
    fn health_keeps_failed_when_sticky_error_survives_live_stream() {
        let cache = EventCache::from_snapshot_for_test(fixture());
        set_last_error(&cache.shared, "stale subscribe error".to_owned());
        assert!(matches!(
            cache.classify_health(),
            EventCacheHealth::Failed(_)
        ));
        clear_last_error(&cache.shared);
        assert_eq!(cache.classify_health(), EventCacheHealth::Healthy);
    }

    #[test]
    fn health_classifies_failed_when_not_ready() {
        let cache = EventCache::from_snapshot_for_test(fixture());
        *cache.shared.ready.lock().unwrap() = false;
        cache.shared.stream_live.store(false, Ordering::Release);
        assert!(matches!(
            cache.classify_health(),
            EventCacheHealth::Failed(_)
        ));
    }

    #[test]
    fn doctor_probe_wait_leaves_reconciling_as_pass_when_budget_expires() {
        let cache = EventCache::from_snapshot_for_test(fixture());
        cache.shared.stream_live.store(false, Ordering::Release);
        cache.shared.state.write().unwrap().needs_reconcile = true;
        let health = cache.wait_for_doctor_probe(
            Duration::from_millis(10),
            Duration::from_millis(10),
            Duration::from_millis(30),
        );
        assert_eq!(health, EventCacheHealth::Reconciling);
        assert!(health.doctor_pass());
    }

    #[test]
    fn pane_event_upserts_state_agent_activity_and_digest() {
        let mut state = CacheState::default();
        state.bootstrap(fixture());
        state.apply_event(
            json!({
                "event": "pane_updated",
                "type": "pane_updated",
                "pane": {
                    "pane_id": "w1:p1",
                    "workspace_id": "w1",
                    "cwd": "/tmp/demo",
                    "agent": "pi",
                    "agent_status": "working",
                    "state_change_seq": 9
                }
            }),
            "2026-08-25T04:30:00Z".to_owned(),
        );

        assert_eq!(array(&state.state, "agents")[0]["agent_status"], "working");
        let digest = state.digest_since(0);
        assert_eq!(digest.cursor, 1);
        assert_eq!(digest.events.len(), 1);
        assert_eq!(digest.events[0]["workspace_id"], "w1");
        assert_eq!(digest.agents[0]["name"], "w1:p1");
        assert_eq!(digest.agents[0]["agent_id"], "w1:p1");
        assert_eq!(digest.agents[0]["kind"], "pi");
        assert_eq!(digest.agents[0]["last_activity_at"], "2026-08-25T04:30:00Z");
        assert_eq!(digest.agents[0]["started_at"], "2026-08-25T04:00:01.123Z");
    }

    #[test]
    fn pane_scroll_only_update_does_not_advance_digest_or_activity() {
        let mut state = CacheState::default();
        state.bootstrap(fixture());
        let pane = |max_offset, state_change_seq| {
            json!({
                "event": "pane_updated",
                "pane": {
                    "pane_id": "w1:p1",
                    "workspace_id": "w1",
                    "cwd": "/tmp/demo",
                    "agent": "pi",
                    "agent_status": "working",
                    "state_change_seq": state_change_seq,
                    "scroll": {"max_offset_from_bottom": max_offset, "offset_from_bottom": 0}
                }
            })
        };
        state.apply_event(pane(100, 9), "2026-08-25T04:30:00Z".to_owned());
        assert_eq!(state.digest_cursor, 1);
        state.apply_event(pane(120, 10), "2026-08-25T04:31:00Z".to_owned());
        assert_eq!(state.digest_cursor, 1);
        let digest = state.digest_since(0);
        assert_eq!(digest.events.len(), 1);
        assert_eq!(digest.agents[0]["last_activity_at"], "2026-08-25T04:30:00Z");
        assert_eq!(
            array(&state.state, "panes")[0]["scroll"]["max_offset_from_bottom"],
            120
        );
    }

    #[test]
    fn pane_close_removes_pane_and_agent() {
        let mut state = CacheState::default();
        state.bootstrap(fixture());
        state.apply_event(
            json!({"event": "pane.closed", "type": "pane_closed", "pane_id": "w1:p1", "workspace_id": "w1"}),
            "2026-08-25T04:31:00Z".to_owned(),
        );
        assert!(array(&state.state, "panes").is_empty());
        assert!(array(&state.state, "agents").is_empty());
    }

    #[test]
    fn unknown_workspace_forces_reconciliation_without_admitting_event() {
        let mut state = CacheState::default();
        state.bootstrap(fixture());
        state.apply_event(
            json!({
                "event": "pane_updated",
                "pane": {"pane_id": "w9:p1", "workspace_id": "w9"}
            }),
            "2026-08-25T04:32:00Z".to_owned(),
        );
        assert!(state.needs_reconcile);
        assert_eq!(state.digest_cursor, 0);
        assert_eq!(array(&state.state, "workspaces").len(), 1);
    }

    #[test]
    fn snapshot_reconcile_emits_authoritative_workspace_and_pane_diff() {
        let mut state = CacheState::default();
        state.bootstrap(fixture());
        let next = json!({
            "focused_workspace_id": "w2",
            "workspaces": [
                {"workspace_id": "w1", "label": "demo", "pane_count": 1, "tab_count": 1},
                {"workspace_id": "w2", "label": "second", "pane_count": 1, "tab_count": 1}
            ],
            "tabs": [
                {"tab_id": "w1:t1", "workspace_id": "w1"},
                {"tab_id": "w2:t1", "workspace_id": "w2"}
            ],
            "panes": [
                {"pane_id": "w1:p2", "workspace_id": "w1", "cwd": "/tmp/demo"},
                {"pane_id": "w2:p1", "workspace_id": "w2", "cwd": "/tmp/second"}
            ],
            "agents": []
        });

        state.reconcile_snapshot(next, "2026-08-27T13:00:00Z".to_owned());

        assert!(!state.needs_reconcile);
        let digest = state.digest_since(0);
        let kinds = digest
            .events
            .iter()
            .filter_map(|event| event.get("type").and_then(Value::as_str))
            .map(normalize_event_type)
            .collect::<Vec<_>>();
        assert!(kinds.contains(&"workspace_created".to_owned()));
        assert!(kinds.contains(&"pane_created".to_owned()));
        assert!(kinds.contains(&"pane_closed".to_owned()));
        assert!(digest.events.iter().any(|event| {
            normalize_event_type(event.get("type").and_then(Value::as_str).unwrap_or(""))
                == "pane_created"
                && event.get("pane_id").and_then(Value::as_str) == Some("w2:p1")
        }));
        assert!(digest.events.iter().any(|event| {
            normalize_event_type(event.get("type").and_then(Value::as_str).unwrap_or(""))
                == "pane_closed"
                && event.get("pane_id").and_then(Value::as_str) == Some("w1:p1")
        }));
        assert!(
            array(&state.state, "panes")
                .iter()
                .any(|pane| pane.get("pane_id").and_then(Value::as_str) == Some("w2:p1"))
        );
    }

    #[test]
    fn snapshot_reconcile_can_remove_entire_workspace_without_reconcile_loop() {
        let mut state = CacheState::default();
        state.bootstrap(fixture());
        state.reconcile_snapshot(
            json!({"workspaces": [], "tabs": [], "panes": [], "agents": []}),
            "2026-08-27T13:01:00Z".to_owned(),
        );
        assert!(!state.needs_reconcile);
        let digest = state.digest_since(0);
        assert!(digest.events.iter().any(|event| {
            normalize_event_type(event.get("type").and_then(Value::as_str).unwrap_or(""))
                == "pane_closed"
        }));
        assert!(digest.events.iter().any(|event| {
            normalize_event_type(event.get("type").and_then(Value::as_str).unwrap_or(""))
                == "workspace_closed"
        }));
    }

    #[test]
    fn workspace_close_removes_entire_scope() {
        let mut state = CacheState::default();
        state.bootstrap(fixture());
        state.apply_event(
            json!({"event": "workspace.closed", "workspace_id": "w1"}),
            "2026-08-25T04:33:00Z".to_owned(),
        );
        assert!(array(&state.state, "workspaces").is_empty());
        assert!(array(&state.state, "panes").is_empty());
        assert!(array(&state.state, "agents").is_empty());
        assert!(array(&state.state, "tabs").is_empty());
    }

    #[test]
    fn initial_digest_is_bounded_to_recent_tail() {
        let mut state = CacheState::default();
        state.bootstrap(fixture());
        for index in 0..70 {
            state.apply_event(
                json!({
                    "event": "workspace_updated",
                    "workspace": {"workspace_id": "w1", "label": format!("demo-{index}")}
                }),
                format!("2026-08-25T04:34:{:02}Z", index % 60),
            );
        }
        let digest = state.digest_since(0);
        assert_eq!(digest.cursor, 70);
        assert_eq!(digest.events.len(), 64);
        assert_eq!(digest.events[0]["cursor"], 7);
        assert_eq!(state.digest_since(69).events.len(), 1);
    }

    #[test]
    fn digest_only_coalesces_adjacent_state_updates() {
        let events = vec![
            json!({"cursor": 1, "at": "a", "type": "workspace_updated", "workspace_id": "w1", "workspace": {"workspace_id": "w1", "label": "same"}}),
            json!({"cursor": 2, "at": "b", "type": "workspace.updated", "workspace_id": "w1", "workspace": {"workspace_id": "w1", "label": "same"}}),
            json!({"cursor": 3, "type": "workspace.closed", "workspace_id": "w1"}),
            json!({"cursor": 4, "type": "workspace_updated", "workspace_id": "w1", "workspace": {"workspace_id": "w1", "label": "changed"}}),
        ];
        let coalesced = coalesce_digest_updates(events);
        assert_eq!(coalesced.len(), 3);
        assert_eq!(coalesced[0]["cursor"], 2);
        assert_eq!(coalesced[1]["cursor"], 3);
        assert_eq!(coalesced[2]["cursor"], 4);
    }

    #[test]
    fn digest_preserves_distinct_state_transitions_for_same_object() {
        let events = vec![
            json!({"cursor": 1, "type": "workspace_updated", "workspace_id": "w1", "workspace": {"workspace_id": "w1", "label": "one"}}),
            json!({"cursor": 2, "type": "workspace_updated", "workspace_id": "w1", "workspace": {"workspace_id": "w1", "label": "two"}}),
        ];
        let coalesced = coalesce_digest_updates(events);
        assert_eq!(coalesced.len(), 2);
        assert_eq!(coalesced[0]["workspace"]["label"], "one");
        assert_eq!(coalesced[1]["workspace"]["label"], "two");
    }

    #[test]
    fn session_timestamp_parser_matches_native_session_names() {
        assert_eq!(
            parse_session_started("/tmp/2026-08-14T07-29-14-679Z_id.jsonl"),
            Some("2026-08-14T07:29:14.679Z".to_owned())
        );
        assert_eq!(parse_session_started("invalid"), None);
    }
}
