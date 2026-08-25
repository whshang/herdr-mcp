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

const RESUBSCRIBE: Duration = Duration::from_secs(25);
const FULL_SNAPSHOT_TTL: Duration = Duration::from_secs(30);
const RECONNECT_BACKOFF: Duration = Duration::from_millis(250);
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

    fn build_subscriptions(&self) -> Vec<Value> {
        let mut subscriptions = [
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
            "pane.updated",
            "pane.focused",
            "pane.moved",
            "pane.exited",
            "pane.agent_detected",
        ]
        .into_iter()
        .map(|kind| json!({"type": kind}))
        .collect::<Vec<_>>();

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
        for pane_id in pane_ids {
            subscriptions.push(json!({
                "type": "pane.agent_status_changed",
                "pane_id": pane_id,
            }));
        }
        subscriptions
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

        if let Some(workspace_id) = resolved_workspace_id.as_deref()
            && !self.live_workspace_ids.contains(workspace_id)
            && !workspace_closed
        {
            self.needs_reconcile = true;
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
            events,
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
                json!({
                    "name": agent.get("agent").cloned().unwrap_or(Value::Null),
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

struct SharedCache {
    state: RwLock<CacheState>,
    ready: Mutex<bool>,
    ready_condvar: Condvar,
    stop: AtomicBool,
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
    while !shared.stop.load(Ordering::Acquire) {
        match snapshot::fetch(&client) {
            Ok(snapshot) => {
                if let Ok(mut state) = shared.state.write() {
                    state.bootstrap(snapshot.value);
                }
                if let Ok(mut ready) = shared.ready.lock() {
                    *ready = true;
                    shared.ready_condvar.notify_all();
                }
            }
            Err(error) => {
                set_last_error(&shared, error);
                sleep_interruptible(&shared.stop, RECONNECT_BACKOFF);
                continue;
            }
        }

        let subscriptions = shared
            .state
            .read()
            .map(|state| state.build_subscriptions())
            .unwrap_or_default();
        let mut stream = match EventStream::subscribe(&client, subscriptions, RESUBSCRIBE) {
            Ok(stream) => stream,
            Err(error) => {
                set_last_error(&shared, error.to_string());
                sleep_interruptible(&shared.stop, RECONNECT_BACKOFF);
                continue;
            }
        };
        shared.stream_live.store(true, Ordering::Release);
        let full_snapshot_started = Instant::now();

        loop {
            if shared.stop.load(Ordering::Acquire) {
                shared.stream_live.store(false, Ordering::Release);
                return;
            }
            if full_snapshot_started.elapsed() >= FULL_SNAPSHOT_TTL || stream.is_expired() {
                break;
            }
            match stream.poll_event(EVENT_POLL) {
                Ok(Some(event)) => {
                    if let Ok(mut state) = shared.state.write() {
                        state.apply_event(event, now_iso());
                        if state.needs_reconcile {
                            break;
                        }
                    }
                }
                Ok(None) => {
                    if stream.is_expired() {
                        break;
                    }
                }
                Err(error) => {
                    set_last_error(&shared, error.to_string());
                    break;
                }
            }
        }
        shared.stream_live.store(false, Ordering::Release);

        sleep_interruptible(&shared.stop, RECONNECT_BACKOFF);
    }
}

fn set_last_error(shared: &SharedCache, error: String) {
    if let Ok(mut value) = shared.last_error.lock() {
        *value = Some(error);
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
    fn subscriptions_include_global_events_and_per_pane_agent_status() {
        let mut state = CacheState::default();
        state.bootstrap(fixture());
        let subscriptions = state.build_subscriptions();
        assert!(subscriptions.contains(&json!({"type": "workspace.updated"})));
        assert!(subscriptions.contains(&json!({
            "type": "pane.agent_status_changed",
            "pane_id": "w1:p1"
        })));
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
        assert_eq!(digest.agents[0]["last_activity_at"], "2026-08-25T04:30:00Z");
        assert_eq!(digest.agents[0]["started_at"], "2026-08-25T04:00:01.123Z");
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
    fn session_timestamp_parser_matches_native_session_names() {
        assert_eq!(
            parse_session_started("/tmp/2026-08-14T07-29-14-679Z_id.jsonl"),
            Some("2026-08-14T07:29:14.679Z".to_owned())
        );
        assert_eq!(parse_session_started("invalid"), None);
    }
}
