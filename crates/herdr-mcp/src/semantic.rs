use reqwest::blocking::Client;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};
use url::Url;

pub const DEFAULT_DECISION_THRESHOLD: f64 = 0.70;
pub const EDGE_SEMANTIC_PROVIDER_ID: &str = "edge-semantic";

/// Vercel AI Gateway requires this protocol version header before evaluating the request.
const VERCEL_GATEWAY_PROTOCOL_VERSION: &str = "0.0.1";
/// Measured decision-route latency is 1.1-1.4s median with a ~2.6s tail, so a single
/// attempt is given 4s. The pool keeps a separate 10s budget so one slow route cannot
/// spend the failover allowance of the routes behind it.
const DECISION_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(4);
const DECISION_POOL_BUDGET: Duration = Duration::from_secs(10);
const SEMANTIC_ATTEMPT_LIMIT: usize = 3;
const MAX_STATE_BYTES: usize = 64 * 1024;
const MAX_QUESTIONS: usize = 32;
const MAX_CRITERIA: usize = 64;
const MAX_CHAT_MESSAGES: usize = 32;
const MAX_CHAT_MESSAGE_BYTES: usize = 64 * 1024;
const EDGE_PROXY_PROBE_TTL: Duration = Duration::from_secs(60);
static EDGE_PROXY_AVAILABILITY: Mutex<Option<(Instant, crate::worker::SemanticProxyCapabilities)>> =
    Mutex::new(None);
static ROUTE_CURSOR: AtomicUsize = AtomicUsize::new(0);
static CHAT_ROUTE_CURSOR: AtomicUsize = AtomicUsize::new(0);
static ROUTE_COOLDOWNS: OnceLock<Mutex<BTreeMap<String, Instant>>> = OnceLock::new();

#[derive(Debug, Clone)]
pub enum SemanticQuestion {
    Noul {
        instructions: String,
        yes: String,
        no: String,
    },
    Choice {
        instructions: String,
        criteria: BTreeMap<String, Option<String>>,
    },
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
}

impl SemanticQuestion {
    pub fn noul(
        instructions: impl Into<String>,
        yes: impl Into<String>,
        no: impl Into<String>,
    ) -> Self {
        Self::Noul {
            instructions: instructions.into(),
            yes: yes.into(),
            no: no.into(),
        }
    }

    pub fn choice(
        instructions: impl Into<String>,
        criteria: BTreeMap<String, Option<String>>,
    ) -> Self {
        Self::Choice {
            instructions: instructions.into(),
            criteria,
        }
    }

    pub fn score(instructions: impl Into<String>, criteria: Vec<String>) -> Self {
        Self::Score {
            instructions: instructions.into(),
            criteria,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Noul { .. } => "noul",
            Self::Choice { .. } => "choice",
            Self::Score { .. } => "score",
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Noul {
                instructions,
                yes,
                no,
            } => json!({
                "type": "noul",
                "instructions": instructions,
                "criteria": {"true": yes, "false": no},
            }),
            Self::Choice {
                instructions,
                criteria,
            } => json!({
                "type": "choice",
                "instructions": instructions,
                "criteria": criteria,
            }),
            Self::Score {
                instructions,
                criteria,
            } => json!({
                "type": "score",
                "instructions": instructions,
                "criteria": criteria,
            }),
        }
    }

    fn to_vercel_json(&self) -> Value {
        match self {
            Self::Noul {
                instructions,
                yes,
                no,
            } => json!({
                "type": "boolean",
                "instructions": instructions,
                "criteria": {"true": yes, "false": no},
            }),
            Self::Choice {
                instructions,
                criteria,
            } => json!({
                "type": "choice",
                "instructions": instructions,
                "criteria": criteria,
            }),
            Self::Score {
                instructions,
                criteria,
            } => json!({
                "type": "score",
                "instructions": instructions,
                "criteria": criteria,
            }),
        }
    }

    fn valid(&self) -> bool {
        let instructions = match self {
            Self::Noul { instructions, .. }
            | Self::Choice { instructions, .. }
            | Self::Score { instructions, .. } => instructions,
        };
        if instructions.trim().is_empty() || instructions.len() > 4096 {
            return false;
        }
        match self {
            Self::Noul { yes, no, .. } => {
                !yes.trim().is_empty()
                    && !no.trim().is_empty()
                    && yes.len() <= 4096
                    && no.len() <= 4096
            }
            Self::Choice { criteria, .. } => {
                (2..=MAX_CRITERIA).contains(&criteria.len())
                    && criteria.iter().all(|(key, value)| {
                        !key.is_empty()
                            && key.len() <= 256
                            && value.as_deref().is_none_or(|value| value.len() <= 4096)
                    })
            }
            Self::Score { criteria, .. } => {
                (2..=MAX_CRITERIA).contains(&criteria.len())
                    && criteria
                        .iter()
                        .all(|value| !value.is_empty() && value.len() <= 4096)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct SemanticRequest {
    state: Value,
    questions: BTreeMap<String, SemanticQuestion>,
}

impl SemanticRequest {
    pub fn new(state: Value) -> Self {
        Self {
            state,
            questions: BTreeMap::new(),
        }
    }

    pub fn ask(mut self, id: impl Into<String>, question: SemanticQuestion) -> Self {
        self.questions.insert(id.into(), question);
        self
    }

    fn validate(&self) -> Result<(), SemanticError> {
        if serde_json::to_vec(&self.state)
            .map_err(|_| SemanticError::new("invalid_request"))?
            .len()
            > MAX_STATE_BYTES
            || self.questions.is_empty()
            || self.questions.len() > MAX_QUESTIONS
        {
            return Err(SemanticError::new("invalid_request"));
        }
        if self.questions.iter().any(|(id, question)| {
            id.is_empty()
                || id.len() > 64
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                || !question.valid()
        }) {
            return Err(SemanticError::new("invalid_request"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum SemanticAnswer {
    Noul(f64),
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
}

impl SemanticAnswer {
    fn to_json(&self) -> Value {
        match self {
            Self::Noul(probability) => json!({"type": "noul", "noul": probability}),
            Self::Choice {
                choice,
                probabilities,
                confidence,
            } => json!({
                "type": "choice",
                "choice": choice,
                "probabilities": probabilities,
                "confidence": confidence,
            }),
            Self::Score {
                score,
                probabilities,
                confidence,
            } => json!({
                "type": "score",
                "score": score,
                "probabilities": probabilities,
                "confidence": confidence,
            }),
        }
    }

    pub fn noul_probability(&self) -> Option<f64> {
        match self {
            Self::Noul(probability) => Some(*probability),
            _ => None,
        }
    }

    pub fn choice_value(&self) -> Option<(&str, &BTreeMap<String, f64>, f64)> {
        match self {
            Self::Choice {
                choice,
                probabilities,
                confidence,
            } => Some((choice, probabilities, *confidence)),
            _ => None,
        }
    }

    pub fn score_value(&self) -> Option<(f64, &BTreeMap<String, f64>, f64)> {
        match self {
            Self::Score {
                score,
                probabilities,
                confidence,
            } => Some((*score, probabilities, *confidence)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SemanticResponse {
    pub provider: String,
    pub model: String,
    answers: BTreeMap<String, SemanticAnswer>,
}

impl SemanticResponse {
    pub fn answer(&self, id: &str) -> Option<&SemanticAnswer> {
        self.answers.get(id)
    }

    fn to_json(&self) -> Value {
        json!({
            "ok": true,
            "provider": self.provider,
            "model": self.model,
            "answers": self.answers.iter()
                .map(|(id, answer)| (id.clone(), answer.to_json()))
                .collect::<Map<_, _>>(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticError(String);

impl SemanticError {
    fn new(code: impl Into<String>) -> Self {
        Self(code.into())
    }

    pub fn code(&self) -> &str {
        &self.0
    }

    fn http_status(&self) -> Option<u16> {
        self.0.strip_prefix("http_")?.parse().ok()
    }

    fn allows_route_failover(&self) -> bool {
        true
    }

    fn cooldown(&self) -> Option<Duration> {
        match self.http_status() {
            Some(401 | 403) => Some(Duration::from_secs(300)),
            Some(400 | 404 | 422) => Some(Duration::from_secs(300)),
            Some(408 | 409 | 429) => Some(Duration::from_secs(30)),
            Some(status) if status >= 500 => Some(Duration::from_secs(30)),
            Some(_) => None,
            None if matches!(
                self.code(),
                "timeout"
                    | "network"
                    | "invalid_response"
                    | "edge_proxy_unavailable"
                    | "provider_unavailable"
            ) =>
            {
                Some(Duration::from_secs(30))
            }
            None => None,
        }
    }
}

trait SemanticProvider: Send + Sync {
    fn id(&self) -> &str;
    fn evaluate(
        &self,
        request: &SemanticRequest,
        timeout: Duration,
    ) -> Result<SemanticResponse, SemanticError>;
}

#[derive(Debug, Clone)]
pub struct SemanticChatMessage {
    pub role: String,
    pub content: String,
}

impl SemanticChatMessage {
    fn valid(&self) -> bool {
        matches!(self.role.as_str(), "system" | "user" | "assistant")
            && !self.content.trim().is_empty()
            && self.content.len() <= MAX_CHAT_MESSAGE_BYTES
    }

    fn to_json(&self) -> Value {
        json!({"role": self.role, "content": self.content})
    }
}

#[derive(Debug, Clone)]
pub struct SemanticChatResponse {
    pub provider: String,
    pub model: String,
    pub content: String,
    pub usage: Option<Value>,
}

trait SemanticChatProvider: Send + Sync {
    fn id(&self) -> &str;
    fn chat(
        &self,
        messages: &[SemanticChatMessage],
        timeout: Duration,
    ) -> Result<SemanticChatResponse, SemanticError>;
}

pub struct SemanticService {
    providers: Vec<Box<dyn SemanticProvider>>,
    chat_providers: Vec<Box<dyn SemanticChatProvider>>,
}

fn edge_proxy_capabilities_cached() -> crate::worker::SemanticProxyCapabilities {
    if let Ok(cache) = EDGE_PROXY_AVAILABILITY.lock()
        && let Some((observed_at, capabilities)) = *cache
        && observed_at.elapsed() < EDGE_PROXY_PROBE_TTL
    {
        return capabilities;
    }

    let capabilities = crate::worker::semantic_proxy_capabilities();
    if let Ok(mut cache) = EDGE_PROXY_AVAILABILITY.lock() {
        *cache = Some((Instant::now(), capabilities));
    }
    capabilities
}

fn route_cooldowns() -> &'static Mutex<BTreeMap<String, Instant>> {
    ROUTE_COOLDOWNS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn route_is_cooling_down(id: &str) -> bool {
    let Ok(mut cooldowns) = route_cooldowns().lock() else {
        return false;
    };
    let now = Instant::now();
    cooldowns.retain(|_, until| *until > now);
    cooldowns.get(id).is_some_and(|until| *until > now)
}

fn mark_route_cooldown(id: &str, duration: Duration) {
    if let Ok(mut cooldowns) = route_cooldowns().lock() {
        cooldowns.insert(id.to_owned(), Instant::now() + duration);
    }
}

fn clear_route_cooldown(id: &str) {
    if let Ok(mut cooldowns) = route_cooldowns().lock() {
        cooldowns.remove(id);
    }
}

/// Timeout allowance for one route pool run: `attempt` caps a single route call,
/// `total` caps the whole failover sequence.
#[derive(Debug, Clone, Copy)]
struct RouteBudget {
    attempt: Duration,
    total: Duration,
}

impl RouteBudget {
    fn split(attempt: Duration, total: Duration) -> Self {
        Self { attempt, total }
    }

    /// One caller-owned allowance that a single route may spend in full.
    fn single(total: Duration) -> Self {
        Self {
            attempt: total,
            total,
        }
    }
}

fn execute_route_pool<T>(
    route_count: usize,
    cursor: &AtomicUsize,
    budget: RouteBudget,
    mut route_id: impl FnMut(usize) -> String,
    mut call: impl FnMut(usize, Duration) -> Result<T, SemanticError>,
) -> Result<T, SemanticError> {
    let start = cursor.fetch_add(1, Ordering::Relaxed) % route_count;
    let deadline = Instant::now() + budget.total;
    let mut last = SemanticError::new("provider_unavailable");
    let mut attempted = false;
    let mut attempts = 0;
    for offset in 0..route_count {
        if attempts >= SEMANTIC_ATTEMPT_LIMIT {
            break;
        }
        let index = (start + offset) % route_count;
        let id = route_id(index);
        if route_is_cooling_down(&id) {
            continue;
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        if remaining.is_zero() {
            break;
        }
        attempted = true;
        attempts += 1;
        match call(index, remaining.min(budget.attempt)) {
            Ok(response) => {
                clear_route_cooldown(&id);
                return Ok(response);
            }
            Err(error) => {
                if let Some(duration) = error.cooldown() {
                    mark_route_cooldown(&id, duration);
                }
                if !error.allows_route_failover() {
                    return Err(error);
                }
                last = error;
            }
        }
    }
    if attempted {
        Err(last)
    } else {
        Err(SemanticError::new("routes_cooling_down"))
    }
}

impl SemanticService {
    pub fn from_config() -> Self {
        Self::from_config_with_edge_capabilities(edge_proxy_capabilities_cached)
    }

    #[cfg(test)]
    pub(crate) fn test_empty() -> Self {
        Self {
            providers: Vec::new(),
            chat_providers: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn test_decision_route(id: &str, url: &str) -> Result<Self, SemanticError> {
        let provider = HttpSemanticProvider::new(
            id.to_owned(),
            SemanticProtocol::Decision,
            "test".to_owned(),
            url.to_owned(),
            "jev-test".to_owned(),
        )?;
        Ok(Self {
            providers: vec![Box::new(provider)],
            chat_providers: Vec::new(),
        })
    }

    #[cfg(test)]
    fn from_config_with_edge_probe(edge_available: impl FnOnce() -> bool) -> Self {
        let available = edge_available();
        Self::from_config_with_edge_capabilities(|| crate::worker::SemanticProxyCapabilities {
            evaluate: available,
            chat: available,
        })
    }

    fn from_config_with_edge_capabilities(
        edge_capabilities: impl FnOnce() -> crate::worker::SemanticProxyCapabilities,
    ) -> Self {
        let mut providers = local_semantic_providers();
        let mut chat_providers = local_semantic_chat_providers();
        let needs_edge_evaluate = providers.is_empty();
        let needs_edge_chat = chat_providers.is_empty();
        let edge = if (needs_edge_evaluate || needs_edge_chat)
            && crate::worker::semantic_proxy_configured()
        {
            edge_capabilities()
        } else {
            crate::worker::SemanticProxyCapabilities::default()
        };
        if needs_edge_evaluate && edge.evaluate {
            providers.push(Box::new(EdgeSemanticProvider));
        }
        if needs_edge_chat && edge.chat {
            chat_providers.push(Box::new(EdgeSemanticChatProvider));
        }
        Self {
            providers,
            chat_providers,
        }
    }

    pub fn configured(&self) -> bool {
        !self.providers.is_empty()
    }

    pub fn chat_configured(&self) -> bool {
        !self.chat_providers.is_empty()
    }

    pub fn evaluate(&self, request: &SemanticRequest) -> Result<SemanticResponse, SemanticError> {
        self.evaluate_with_budget(
            request,
            RouteBudget::split(DECISION_ATTEMPT_TIMEOUT, DECISION_POOL_BUDGET),
        )
    }

    pub(crate) fn evaluate_with_timeout(
        &self,
        request: &SemanticRequest,
        timeout: Duration,
    ) -> Result<SemanticResponse, SemanticError> {
        self.evaluate_with_budget(request, RouteBudget::single(timeout))
    }

    fn evaluate_with_budget(
        &self,
        request: &SemanticRequest,
        budget: RouteBudget,
    ) -> Result<SemanticResponse, SemanticError> {
        request.validate()?;
        if self.providers.is_empty() {
            return Err(SemanticError::new("not_configured"));
        }
        execute_route_pool(
            self.providers.len(),
            &ROUTE_CURSOR,
            budget,
            |index| self.providers[index].id().to_owned(),
            |index, remaining| self.providers[index].evaluate(request, remaining),
        )
    }

    pub fn chat(
        &self,
        messages: &[SemanticChatMessage],
        timeout: Duration,
    ) -> Result<SemanticChatResponse, SemanticError> {
        if messages.is_empty()
            || messages.len() > MAX_CHAT_MESSAGES
            || messages.iter().any(|message| !message.valid())
        {
            return Err(SemanticError::new("invalid_request"));
        }
        if self.chat_providers.is_empty() {
            return Err(SemanticError::new("not_configured"));
        }
        execute_route_pool(
            self.chat_providers.len(),
            &CHAT_ROUTE_CURSOR,
            RouteBudget::single(timeout),
            |index| self.chat_providers[index].id().to_owned(),
            |index, remaining| self.chat_providers[index].chat(messages, remaining),
        )
    }

    pub fn capability_json(&self) -> Value {
        json!({
            "available": self.configured() || self.chat_configured(),
            "evaluate_available": self.configured(),
            "chat_available": self.chat_configured(),
            "providers": self.providers.iter().map(|provider| provider.id()).collect::<Vec<_>>(),
            "chat_providers": self.chat_providers.iter().map(|provider| provider.id()).collect::<Vec<_>>(),
            "policy": "advisory_only",
            "fallback": "existing_behavior",
        })
    }
}

pub fn extension_status_json() -> Value {
    SemanticService::from_config().capability_json()
}

pub fn extension_evaluate_json(payload: &Value) -> Value {
    let request = match semantic_request_from_json(payload) {
        Ok(request) => request,
        Err(error) => return json!({"ok": false, "code": error.code()}),
    };
    match SemanticService::from_config().evaluate(&request) {
        Ok(response) => response.to_json(),
        Err(error) => json!({"ok": false, "code": error.code()}),
    }
}

pub fn extension_chat_json(payload: &Value) -> Value {
    let Some(object) = payload.as_object() else {
        return json!({"ok": false, "code": "invalid_request"});
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "messages" | "timeout_ms"))
    {
        return json!({"ok": false, "code": "invalid_request"});
    }
    let Some(messages) = object.get("messages").and_then(Value::as_array) else {
        return json!({"ok": false, "code": "invalid_request"});
    };
    let messages = messages
        .iter()
        .map(|value| {
            let object = value.as_object()?;
            if object
                .keys()
                .any(|key| !matches!(key.as_str(), "role" | "content"))
            {
                return None;
            }
            Some(SemanticChatMessage {
                role: object.get("role")?.as_str()?.to_owned(),
                content: object.get("content")?.as_str()?.to_owned(),
            })
        })
        .collect::<Option<Vec<_>>>();
    let Some(messages) = messages else {
        return json!({"ok": false, "code": "invalid_request"});
    };
    let timeout_ms = object
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(15_000)
        .clamp(1_000, 60_000);
    match SemanticService::from_config().chat(&messages, Duration::from_millis(timeout_ms)) {
        Ok(response) => json!({
            "ok": true,
            "provider": response.provider,
            "model": response.model,
            "content": response.content,
            "usage": response.usage,
        }),
        Err(error) => json!({"ok": false, "code": error.code()}),
    }
}

fn semantic_request_from_json(payload: &Value) -> Result<SemanticRequest, SemanticError> {
    let object = payload
        .as_object()
        .ok_or_else(|| SemanticError::new("invalid_request"))?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "state" | "questions"))
    {
        return Err(SemanticError::new("invalid_request"));
    }
    let state = object
        .get("state")
        .cloned()
        .ok_or_else(|| SemanticError::new("invalid_request"))?;
    let raw_questions = object
        .get("questions")
        .and_then(Value::as_object)
        .ok_or_else(|| SemanticError::new("invalid_request"))?;
    let mut request = SemanticRequest::new(state);
    for (id, raw) in raw_questions {
        let raw = raw
            .as_object()
            .ok_or_else(|| SemanticError::new("invalid_request"))?;
        let question_type = raw
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| SemanticError::new("invalid_request"))?;
        let instructions = raw
            .get("instructions")
            .and_then(Value::as_str)
            .ok_or_else(|| SemanticError::new("invalid_request"))?
            .to_owned();
        let question = match question_type {
            "noul" => {
                let criteria = raw
                    .get("criteria")
                    .and_then(Value::as_object)
                    .ok_or_else(|| SemanticError::new("invalid_request"))?;
                SemanticQuestion::noul(
                    instructions,
                    criteria
                        .get("true")
                        .and_then(Value::as_str)
                        .ok_or_else(|| SemanticError::new("invalid_request"))?,
                    criteria
                        .get("false")
                        .and_then(Value::as_str)
                        .ok_or_else(|| SemanticError::new("invalid_request"))?,
                )
            }
            "choice" => {
                let criteria = raw
                    .get("criteria")
                    .and_then(Value::as_object)
                    .ok_or_else(|| SemanticError::new("invalid_request"))?
                    .iter()
                    .map(|(key, value)| {
                        let value = if value.is_null() {
                            Some(None)
                        } else {
                            value.as_str().map(|value| Some(value.to_owned()))
                        }
                        .ok_or_else(|| SemanticError::new("invalid_request"))?;
                        Ok((key.clone(), value))
                    })
                    .collect::<Result<BTreeMap<_, _>, SemanticError>>()?;
                SemanticQuestion::choice(instructions, criteria)
            }
            "score" => {
                let criteria = raw
                    .get("criteria")
                    .and_then(Value::as_array)
                    .ok_or_else(|| SemanticError::new("invalid_request"))?
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| SemanticError::new("invalid_request"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                SemanticQuestion::score(instructions, criteria)
            }
            _ => return Err(SemanticError::new("invalid_request")),
        };
        request = request.ask(id.clone(), question);
    }
    request.validate()?;
    Ok(request)
}

fn load_semantic_config() -> Option<(PathBuf, crate::config::SemanticConfig)> {
    let paths = crate::paths::RuntimePaths::discover().ok()?;
    let config =
        crate::config::Config::load_for_instance(&paths.config_file, &paths.instance).ok()?;
    Some((paths.config_file, config.semantic))
}

fn config_file_allows_secret(path: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return false;
    }
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SemanticProtocol {
    Decision,
    DecisionVercel,
    OpenAiChat,
}

impl SemanticProtocol {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "decision" => Some(Self::Decision),
            "decision-vercel" => Some(Self::DecisionVercel),
            "openai-chat" => Some(Self::OpenAiChat),
            _ => None,
        }
    }
}

struct EdgeSemanticProvider;

impl SemanticProvider for EdgeSemanticProvider {
    fn id(&self) -> &str {
        EDGE_SEMANTIC_PROVIDER_ID
    }

    fn evaluate(
        &self,
        request: &SemanticRequest,
        timeout: Duration,
    ) -> Result<SemanticResponse, SemanticError> {
        let questions = request
            .questions
            .iter()
            .map(|(id, question)| (id.clone(), question.to_json()))
            .collect::<Map<_, _>>();
        let payload = crate::worker::semantic_proxy_request(
            &json!({
                "state": request.state,
                "questions": questions,
            }),
            timeout,
        )
        .map_err(|_| SemanticError::new("edge_proxy_unavailable"))?;
        parse_response(EDGE_SEMANTIC_PROVIDER_ID, request, payload)
    }
}

struct EdgeSemanticChatProvider;

impl SemanticChatProvider for EdgeSemanticChatProvider {
    fn id(&self) -> &str {
        "edge-semantic-chat"
    }

    fn chat(
        &self,
        messages: &[SemanticChatMessage],
        timeout: Duration,
    ) -> Result<SemanticChatResponse, SemanticError> {
        let payload = crate::worker::semantic_proxy_chat_request(
            &json!({
                "messages": messages.iter().map(SemanticChatMessage::to_json).collect::<Vec<_>>(),
            }),
            timeout,
        )
        .map_err(|_| SemanticError::new("edge_proxy_unavailable"))?;
        parse_chat_response("edge-semantic-chat", payload)
    }
}

struct HttpSemanticProvider {
    id: String,
    protocol: SemanticProtocol,
    api_key: String,
    endpoint: Url,
    model: String,
    client: Client,
}

impl HttpSemanticProvider {
    fn new(
        id: String,
        protocol: SemanticProtocol,
        api_key: String,
        url: String,
        model: String,
    ) -> Result<Self, SemanticError> {
        if !valid_semantic_value(&api_key)
            || !valid_semantic_value(&model)
            || model.len() > 256
            || id.is_empty()
            || id.len() > 128
        {
            return Err(SemanticError::new("route_invalid"));
        }
        Ok(Self {
            id,
            protocol,
            api_key,
            endpoint: semantic_endpoint_url(&url)?,
            model,
            client: Client::builder()
                .timeout(DECISION_ATTEMPT_TIMEOUT)
                .build()
                .map_err(|_| SemanticError::new("client_unavailable"))?,
        })
    }

    fn from_config(
        id: String,
        route: crate::config::SemanticRouteConfig,
        allow_inline_secret: bool,
    ) -> Option<Self> {
        let protocol = SemanticProtocol::parse(&route.protocol)?;
        if !matches!(
            protocol,
            SemanticProtocol::Decision | SemanticProtocol::DecisionVercel
        ) {
            return None;
        }
        if !allow_inline_secret {
            return None;
        }
        Self::new(id, protocol, route.api_key?, route.url?, route.model?).ok()
    }

    fn questions_json(&self, request: &SemanticRequest) -> Map<String, Value> {
        request
            .questions
            .iter()
            .map(|(id, question)| {
                let value = match self.protocol {
                    SemanticProtocol::DecisionVercel => question.to_vercel_json(),
                    SemanticProtocol::Decision => question.to_json(),
                    SemanticProtocol::OpenAiChat => return (id.clone(), Value::Null),
                };
                (id.clone(), value)
            })
            .collect()
    }
}

impl SemanticProvider for HttpSemanticProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn evaluate(
        &self,
        request: &SemanticRequest,
        timeout: Duration,
    ) -> Result<SemanticResponse, SemanticError> {
        let questions = self.questions_json(request);
        let mut builder = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(&self.api_key);
        let body = match self.protocol {
            SemanticProtocol::Decision => {
                json!({
                    "state": request.state,
                    "model": self.model,
                    "questions": questions,
                })
            }
            SemanticProtocol::DecisionVercel => {
                builder = builder
                    .header(
                        "ai-gateway-protocol-version",
                        VERCEL_GATEWAY_PROTOCOL_VERSION,
                    )
                    .header("ai-evaluation-model-specification-version", "4")
                    .header("ai-model-id", &self.model);
                json!({
                    "state": request.state,
                    "questions": questions,
                })
            }
            SemanticProtocol::OpenAiChat => return Err(SemanticError::new("route_invalid")),
        };
        let response = builder
            .timeout(timeout)
            .json(&body)
            .send()
            .map_err(|error| {
                if error.is_timeout() {
                    SemanticError::new("timeout")
                } else {
                    SemanticError::new("network")
                }
            })?;
        if !response.status().is_success() {
            return Err(SemanticError::new(format!(
                "http_{}",
                response.status().as_u16()
            )));
        }
        let payload = response
            .json::<Value>()
            .map_err(|_| SemanticError::new("invalid_response"))?;
        match self.protocol {
            SemanticProtocol::Decision => parse_response(&self.id, request, payload),
            SemanticProtocol::DecisionVercel => {
                parse_vercel_response(&self.id, &self.model, request, payload)
            }
            SemanticProtocol::OpenAiChat => Err(SemanticError::new("route_invalid")),
        }
    }
}

struct OpenAiChatProvider {
    id: String,
    api_key: String,
    endpoint: Url,
    model: String,
    client: Client,
}

impl OpenAiChatProvider {
    fn new(id: String, api_key: String, url: String, model: String) -> Result<Self, SemanticError> {
        if !valid_semantic_value(&api_key)
            || !valid_semantic_value(&model)
            || model.len() > 256
            || id.is_empty()
            || id.len() > 128
        {
            return Err(SemanticError::new("route_invalid"));
        }
        Ok(Self {
            id,
            api_key,
            endpoint: semantic_endpoint_url(&url)?,
            model,
            client: Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .map_err(|_| SemanticError::new("client_unavailable"))?,
        })
    }

    fn from_config(
        id: String,
        route: crate::config::SemanticRouteConfig,
        allow_inline_secret: bool,
    ) -> Option<Self> {
        if route.protocol != "openai-chat" {
            return None;
        }
        if !allow_inline_secret {
            return None;
        }
        Self::new(id, route.api_key?, route.url?, route.model?).ok()
    }
}

impl SemanticChatProvider for OpenAiChatProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn chat(
        &self,
        messages: &[SemanticChatMessage],
        timeout: Duration,
    ) -> Result<SemanticChatResponse, SemanticError> {
        let response = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(&self.api_key)
            .timeout(timeout)
            .json(&json!({
                "model": self.model,
                "messages": messages.iter().map(SemanticChatMessage::to_json).collect::<Vec<_>>(),
                "temperature": 0,
                "stream": false,
            }))
            .send()
            .map_err(|error| {
                if error.is_timeout() {
                    SemanticError::new("timeout")
                } else {
                    SemanticError::new("network")
                }
            })?;
        if !response.status().is_success() {
            return Err(SemanticError::new(format!(
                "http_{}",
                response.status().as_u16()
            )));
        }
        parse_chat_response(
            &self.id,
            response
                .json::<Value>()
                .map_err(|_| SemanticError::new("invalid_response"))?,
        )
    }
}

fn local_semantic_providers() -> Vec<Box<dyn SemanticProvider>> {
    let Some((config_path, semantic)) = load_semantic_config() else {
        return Vec::new();
    };
    let allow_inline_secret = config_file_allows_secret(&config_path);
    semantic
        .routes
        .into_iter()
        .filter_map(|route| {
            let id = format!("config-route:{}", route.name);
            HttpSemanticProvider::from_config(id, route, allow_inline_secret)
                .map(|provider| Box::new(provider) as Box<dyn SemanticProvider>)
        })
        .collect()
}

fn local_semantic_chat_providers() -> Vec<Box<dyn SemanticChatProvider>> {
    let Some((config_path, semantic)) = load_semantic_config() else {
        return Vec::new();
    };
    let allow_inline_secret = config_file_allows_secret(&config_path);
    semantic
        .routes
        .into_iter()
        .filter_map(|route| {
            let id = format!("config-route:{}", route.name);
            OpenAiChatProvider::from_config(id, route, allow_inline_secret)
                .map(|provider| Box::new(provider) as Box<dyn SemanticChatProvider>)
        })
        .collect()
}

fn semantic_endpoint_url(raw: &str) -> Result<Url, SemanticError> {
    let url = Url::parse(raw.trim()).map_err(|_| SemanticError::new("endpoint_invalid"))?;
    if url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(SemanticError::new("endpoint_invalid"));
    }
    let secure = url.scheme() == "https";
    let loopback_http = url.scheme() == "http"
        && url
            .host_str()
            .is_some_and(|host| matches!(host, "127.0.0.1" | "::1" | "localhost"));
    if (!secure && !loopback_http) || url.host_str().is_none() {
        return Err(SemanticError::new("endpoint_invalid"));
    }
    Ok(url)
}

fn parse_chat_response(
    provider: &str,
    payload: Value,
) -> Result<SemanticChatResponse, SemanticError> {
    if let Some(content) = payload.get("content").and_then(Value::as_str) {
        let model = payload
            .get("model")
            .and_then(Value::as_str)
            .ok_or_else(|| SemanticError::new("invalid_response"))?;
        if content.trim().is_empty() {
            return Err(SemanticError::new("invalid_response"));
        }
        return Ok(SemanticChatResponse {
            provider: provider.to_owned(),
            model: model.to_owned(),
            content: content.to_owned(),
            usage: payload.get("usage").cloned(),
        });
    }
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| SemanticError::new("invalid_response"))?;
    let content = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .filter(|content| !content.trim().is_empty())
        .ok_or_else(|| SemanticError::new("invalid_response"))?;
    Ok(SemanticChatResponse {
        provider: provider.to_owned(),
        model: model.to_owned(),
        content: content.to_owned(),
        usage: payload.get("usage").cloned(),
    })
}

fn parse_vercel_response(
    provider: &str,
    model: &str,
    request: &SemanticRequest,
    payload: Value,
) -> Result<SemanticResponse, SemanticError> {
    let raw_answers = payload
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| SemanticError::new("invalid_response"))?;
    let mut answers = BTreeMap::new();
    for (id, question) in &request.questions {
        let raw = raw_answers
            .get(id)
            .and_then(Value::as_object)
            .ok_or_else(|| SemanticError::new("invalid_response"))?;
        let answer = match question {
            SemanticQuestion::Noul { .. } => {
                if raw.get("type").and_then(Value::as_str) != Some("boolean") {
                    return Err(SemanticError::new("invalid_response"));
                }
                SemanticAnswer::Noul(probability(raw.get("probability"))?)
            }
            SemanticQuestion::Choice { .. } => {
                if raw.get("type").and_then(Value::as_str) != Some("choice") {
                    return Err(SemanticError::new("invalid_response"));
                }
                let choice = raw
                    .get("choice")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty() && value.len() <= 256)
                    .ok_or_else(|| SemanticError::new("invalid_response"))?
                    .to_owned();
                let probabilities = optional_probability_map(raw.get("probabilities"))?;
                let confidence = probabilities.get(&choice).copied().unwrap_or(0.0);
                SemanticAnswer::Choice {
                    choice,
                    probabilities,
                    confidence,
                }
            }
            SemanticQuestion::Score { .. } => {
                if raw.get("type").and_then(Value::as_str) != Some("score") {
                    return Err(SemanticError::new("invalid_response"));
                }
                let probabilities = optional_probability_map(raw.get("probabilities"))?;
                let confidence = probabilities.values().copied().fold(0.0_f64, f64::max);
                SemanticAnswer::Score {
                    score: raw
                        .get("score")
                        .and_then(Value::as_f64)
                        .filter(|value| value.is_finite())
                        .ok_or_else(|| SemanticError::new("invalid_response"))?,
                    probabilities,
                    confidence,
                }
            }
        };
        answers.insert(id.clone(), answer);
    }
    Ok(SemanticResponse {
        provider: provider.to_owned(),
        model: model.to_owned(),
        answers,
    })
}

fn optional_probability_map(value: Option<&Value>) -> Result<BTreeMap<String, f64>, SemanticError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    probability_map(Some(value))
}

fn parse_response(
    provider: &str,
    request: &SemanticRequest,
    payload: Value,
) -> Result<SemanticResponse, SemanticError> {
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .ok_or_else(|| SemanticError::new("invalid_response"))?
        .to_owned();
    let raw_answers = payload
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| SemanticError::new("invalid_response"))?;
    let mut answers = BTreeMap::new();
    for (id, question) in &request.questions {
        let raw = raw_answers
            .get(id)
            .and_then(Value::as_object)
            .ok_or_else(|| SemanticError::new("invalid_response"))?;
        if raw.get("type").and_then(Value::as_str) != Some(question.kind()) {
            return Err(SemanticError::new("invalid_response"));
        }
        let answer = match question {
            SemanticQuestion::Noul { .. } => SemanticAnswer::Noul(probability(raw.get("noul"))?),
            SemanticQuestion::Choice { .. } => {
                let choice = raw
                    .get("choice")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty() && value.len() <= 256)
                    .ok_or_else(|| SemanticError::new("invalid_response"))?
                    .to_owned();
                let probabilities = optional_probability_map(raw.get("probabilities"))?;
                let confidence = raw
                    .get("confidence")
                    .map(|value| probability(Some(value)))
                    .transpose()?
                    .or_else(|| probabilities.get(&choice).copied())
                    .unwrap_or(0.0);
                SemanticAnswer::Choice {
                    choice,
                    probabilities,
                    confidence,
                }
            }
            SemanticQuestion::Score { .. } => {
                let probabilities = optional_probability_map(raw.get("probabilities"))?;
                let confidence = raw
                    .get("confidence")
                    .map(|value| probability(Some(value)))
                    .transpose()?
                    .unwrap_or_else(|| probabilities.values().copied().fold(0.0_f64, f64::max));
                SemanticAnswer::Score {
                    score: raw
                        .get("score")
                        .and_then(Value::as_f64)
                        .filter(|value| value.is_finite())
                        .ok_or_else(|| SemanticError::new("invalid_response"))?,
                    probabilities,
                    confidence,
                }
            }
        };
        answers.insert(id.clone(), answer);
    }
    Ok(SemanticResponse {
        provider: provider.to_owned(),
        model,
        answers,
    })
}

fn probability(value: Option<&Value>) -> Result<f64, SemanticError> {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
        .ok_or_else(|| SemanticError::new("invalid_response"))
}

fn probability_map(value: Option<&Value>) -> Result<BTreeMap<String, f64>, SemanticError> {
    let map = value
        .and_then(Value::as_object)
        .ok_or_else(|| SemanticError::new("invalid_response"))?;
    if map.is_empty() || map.len() > MAX_CRITERIA {
        return Err(SemanticError::new("invalid_response"));
    }
    map.iter()
        .map(|(key, value)| Ok((key.clone(), probability(Some(value))?)))
        .collect()
}

fn valid_semantic_value(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::thread;

    fn one_shot_server_with_status(status: u16, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 16 * 1024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        format!("http://127.0.0.1:{}/v1", address.port())
    }

    fn one_shot_server(body: &'static str) -> String {
        one_shot_server_with_status(200, body)
    }

    #[test]
    fn user_gets_typed_semantic_answers() {
        let base_url = one_shot_server(
            r#"{"model":"jev-test","answers":{"edit":{"type":"noul","noul":0.96},"route":{"type":"choice","choice":"code","probabilities":{"code":0.9,"docs":0.1},"confidence":0.88},"effort":{"type":"score","score":1.7,"legend":{"0":"low","1":"medium","2":"high"},"probabilities":{"0":0.05,"1":0.2,"2":0.75},"confidence":0.81}}}"#,
        );
        let request = SemanticRequest::new(json!("edit Rust code"))
            .ask(
                "edit",
                SemanticQuestion::noul(
                    "Does this require code edits?",
                    "Code edit",
                    "No code edit",
                ),
            )
            .ask(
                "route",
                SemanticQuestion::choice(
                    "Choose route",
                    BTreeMap::from([
                        ("code".to_owned(), Some("Source changes".to_owned())),
                        ("docs".to_owned(), Some("Documentation only".to_owned())),
                    ]),
                ),
            )
            .ask(
                "effort",
                SemanticQuestion::score(
                    "Rate effort",
                    vec!["low".to_owned(), "medium".to_owned(), "high".to_owned()],
                ),
            );
        let response = HttpSemanticProvider::new(
            "route:test".to_owned(),
            SemanticProtocol::Decision,
            "test-key".to_owned(),
            base_url,
            "jev-test".to_owned(),
        )
        .unwrap()
        .evaluate(&request, DECISION_ATTEMPT_TIMEOUT)
        .unwrap();

        assert_eq!(
            response
                .answer("edit")
                .and_then(SemanticAnswer::noul_probability),
            Some(0.96)
        );
        let (choice, probabilities, confidence) = response
            .answer("route")
            .and_then(SemanticAnswer::choice_value)
            .unwrap();
        assert_eq!((choice, confidence), ("code", 0.88));
        assert_eq!(probabilities.get("code"), Some(&0.9));
        let (score, probabilities, confidence) = response
            .answer("effort")
            .and_then(SemanticAnswer::score_value)
            .unwrap();
        assert_eq!((score, confidence), (1.7, 0.81));
        assert_eq!(probabilities.get("2"), Some(&0.75));
    }

    #[test]
    fn provider_pool_rotates_healthy_typed_routes() {
        let _guard = crate::test_env::lock();
        ROUTE_CURSOR.store(0, Ordering::Relaxed);
        if let Ok(mut cooldowns) = route_cooldowns().lock() {
            cooldowns.clear();
        }

        let route_a =
            one_shot_server(r#"{"model":"jev-a","answers":{"q":{"type":"noul","noul":0.91}}}"#);
        let route_b =
            one_shot_server(r#"{"model":"jev-b","answers":{"q":{"type":"noul","noul":0.92}}}"#);
        let request = SemanticRequest::new(json!({"task":"route"})).ask(
            "q",
            SemanticQuestion::noul("Can work continue?", "yes", "no"),
        );
        let service = SemanticService {
            providers: vec![
                Box::new(
                    HttpSemanticProvider::new(
                        "route-a".to_owned(),
                        SemanticProtocol::Decision,
                        "key-a".to_owned(),
                        route_a,
                        "jev-a".to_owned(),
                    )
                    .unwrap(),
                ),
                Box::new(
                    HttpSemanticProvider::new(
                        "route-b".to_owned(),
                        SemanticProtocol::Decision,
                        "key-b".to_owned(),
                        route_b,
                        "jev-b".to_owned(),
                    )
                    .unwrap(),
                ),
            ],
            chat_providers: Vec::new(),
        };

        let first = service.evaluate(&request).unwrap();
        let second = service.evaluate(&request).unwrap();
        assert_eq!(first.provider, "route-a");
        assert_eq!(second.provider, "route-b");
    }

    #[test]
    fn provider_pool_fails_over_for_typed_and_chat_requests() {
        let _guard = crate::test_env::lock();
        ROUTE_CURSOR.store(0, Ordering::Relaxed);
        CHAT_ROUTE_CURSOR.store(0, Ordering::Relaxed);
        if let Ok(mut cooldowns) = route_cooldowns().lock() {
            cooldowns.clear();
        }

        let typed_bad = one_shot_server_with_status(500, r#"{"error":"temporary"}"#);
        let typed_good =
            one_shot_server(r#"{"model":"jev-good","answers":{"q":{"type":"noul","noul":0.93}}}"#);
        let request = SemanticRequest::new(json!({"task":"continue"})).ask(
            "q",
            SemanticQuestion::noul("Can work continue?", "yes", "no"),
        );
        let typed_service = SemanticService {
            providers: vec![
                Box::new(
                    HttpSemanticProvider::new(
                        "typed-bad".to_owned(),
                        SemanticProtocol::Decision,
                        "key-a".to_owned(),
                        typed_bad,
                        "jev-bad".to_owned(),
                    )
                    .unwrap(),
                ),
                Box::new(
                    HttpSemanticProvider::new(
                        "typed-good".to_owned(),
                        SemanticProtocol::Decision,
                        "key-b".to_owned(),
                        typed_good,
                        "jev-good".to_owned(),
                    )
                    .unwrap(),
                ),
            ],
            chat_providers: Vec::new(),
        };
        let typed = typed_service.evaluate(&request).unwrap();
        assert_eq!(typed.provider, "typed-good");
        assert_eq!(
            typed.answer("q").and_then(SemanticAnswer::noul_probability),
            Some(0.93)
        );

        let chat_bad = one_shot_server_with_status(500, r#"{"error":"temporary"}"#);
        let chat_good = one_shot_server(
            r#"{"model":"chat-good","choices":[{"message":{"content":"continue"}}]}"#,
        );
        let chat_service = SemanticService {
            providers: Vec::new(),
            chat_providers: vec![
                Box::new(
                    OpenAiChatProvider::new(
                        "chat-bad".to_owned(),
                        "key-c".to_owned(),
                        chat_bad,
                        "chat-bad".to_owned(),
                    )
                    .unwrap(),
                ),
                Box::new(
                    OpenAiChatProvider::new(
                        "chat-good".to_owned(),
                        "key-d".to_owned(),
                        chat_good,
                        "chat-good".to_owned(),
                    )
                    .unwrap(),
                ),
            ],
        };
        let chat = chat_service
            .chat(
                &[SemanticChatMessage {
                    role: "user".to_owned(),
                    content: "continue?".to_owned(),
                }],
                Duration::from_secs(2),
            )
            .unwrap();
        assert_eq!(chat.provider, "chat-good");
        assert_eq!(chat.model, "chat-good");
        assert_eq!(chat.content, "continue");
    }

    #[test]
    fn user_keeps_existing_behavior_without_local_or_edge_provider() {
        let _guard = crate::test_env::lock();
        let previous_config = std::env::var_os("HERDR_MCP_CONFIG_DIR");
        let config_dir =
            std::env::temp_dir().join(format!("herdr-semantic-empty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&config_dir);
        fs::create_dir_all(&config_dir).unwrap();
        unsafe {
            std::env::set_var("HERDR_MCP_CONFIG_DIR", &config_dir);
        }

        let service = SemanticService::from_config_with_edge_probe(|| false);
        assert!(!service.configured());
        assert!(!service.chat_configured());
        assert_eq!(
            service
                .evaluate(&SemanticRequest::new(json!("task")).ask(
                    "q",
                    SemanticQuestion::noul("Is this relevant?", "Relevant", "Not relevant"),
                ))
                .unwrap_err()
                .code(),
            "not_configured"
        );

        let _ = fs::remove_dir_all(&config_dir);
        unsafe {
            match previous_config {
                Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
            }
        }
    }

    #[test]
    fn config_json_is_the_only_local_semantic_configuration_source() {
        let _guard = crate::test_env::lock();
        let previous_config = std::env::var_os("HERDR_MCP_CONFIG_DIR");
        let config_dir =
            std::env::temp_dir().join(format!("herdr-semantic-config-{}", std::process::id()));
        let _ = fs::remove_dir_all(&config_dir);
        fs::create_dir_all(&config_dir).unwrap();
        let path = config_dir.join("config.json");
        fs::write(
            &path,
            r#"{
  "semantic": {
    "routes": [
      {
        "name": "fast_a",
        "protocol": "decision",
        "url": "https://api.typesafe.ai/v1/systemone",
        "model": "jev-latest",
        "api_key": "file-key"
      },
      {
        "name": "chat_a",
        "protocol": "openai-chat",
        "url": "https://chat.example/v1/chat/completions",
        "model": "chat-model",
        "api_key": "chat-key"
      }
    ]
  }
}
"#,
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        unsafe {
            std::env::set_var("HERDR_MCP_CONFIG_DIR", &config_dir);
            std::env::set_var("TYPESAFE_API_KEY", "ignored-env-key");
            std::env::set_var("OPENROUTER_API_KEY", "ignored-env-key");
            std::env::set_var("AI_GATEWAY_API_KEY", "ignored-env-key");
            std::env::set_var("HERDR_LLM_API_KEY", "ignored-env-key");
            std::env::set_var("HERDR_SEMANTIC_ROUTES", "[]");
        }

        let providers = local_semantic_providers();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id(), "config-route:fast_a");
        let chat_providers = local_semantic_chat_providers();
        assert_eq!(chat_providers.len(), 1);
        assert_eq!(chat_providers[0].id(), "config-route:chat_a");

        #[cfg(unix)]
        {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(local_semantic_providers().is_empty());
            assert!(local_semantic_chat_providers().is_empty());
        }

        let _ = fs::remove_dir_all(&config_dir);
        unsafe {
            for key in [
                "TYPESAFE_API_KEY",
                "OPENROUTER_API_KEY",
                "AI_GATEWAY_API_KEY",
                "HERDR_LLM_API_KEY",
                "HERDR_SEMANTIC_ROUTES",
            ] {
                std::env::remove_var(key);
            }
            match previous_config {
                Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
            }
        }
    }

    #[test]
    fn local_config_takes_precedence_over_worker_for_matching_capabilities() {
        let _guard = crate::test_env::lock();
        let previous_config = std::env::var_os("HERDR_MCP_CONFIG_DIR");
        let config_dir =
            std::env::temp_dir().join(format!("herdr-semantic-local-first-{}", std::process::id()));
        let _ = fs::remove_dir_all(&config_dir);
        fs::create_dir_all(&config_dir).unwrap();
        let path = config_dir.join("config.json");
        fs::write(
            &path,
            r#"{
  "edge": {
    "public_origin": "https://edge.example",
    "device_id": "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV"
  },
  "semantic": {
    "routes": [
      {
        "name": "fast_a",
        "protocol": "decision",
        "url": "https://api.typesafe.ai/v1/systemone",
        "model": "jev-latest",
        "api_key": "local-eval-key"
      },
      {
        "name": "chat_a",
        "protocol": "openai-chat",
        "url": "https://chat.example/v1/chat/completions",
        "model": "chat-model",
        "api_key": "local-chat-key"
      }
    ]
  }
}
"#,
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        unsafe {
            std::env::set_var("HERDR_MCP_CONFIG_DIR", &config_dir);
        }

        let service = SemanticService::from_config_with_edge_capabilities(|| {
            panic!(
                "worker semantic capabilities must not be probed when local config covers both capabilities"
            )
        });
        assert_eq!(
            service
                .providers
                .iter()
                .map(|provider| provider.id())
                .collect::<Vec<_>>(),
            vec!["config-route:fast_a"]
        );
        assert_eq!(
            service
                .chat_providers
                .iter()
                .map(|provider| provider.id())
                .collect::<Vec<_>>(),
            vec!["config-route:chat_a"]
        );

        let _ = fs::remove_dir_all(&config_dir);
        unsafe {
            match previous_config {
                Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
            }
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn worker_semantic_routes_are_fallback_when_local_config_has_no_routes() {
        let _guard = crate::test_env::lock();
        let previous_config = std::env::var_os("HERDR_MCP_CONFIG_DIR");
        let config_dir =
            std::env::temp_dir().join(format!("herdr-semantic-edge-config-{}", std::process::id()));
        let _ = fs::remove_dir_all(&config_dir);
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config.json"),
            r#"{
  "edge": {
    "public_origin": "https://edge.example",
    "device_id": "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV"
  }
}
"#,
        )
        .unwrap();
        unsafe {
            std::env::set_var("HERDR_MCP_CONFIG_DIR", &config_dir);
        }

        let service = SemanticService::from_config_with_edge_probe(|| true);
        assert!(service.configured());
        assert!(service.chat_configured());
        assert_eq!(
            service.capability_json()["providers"],
            json!([EDGE_SEMANTIC_PROVIDER_ID])
        );
        assert_eq!(
            service
                .chat_providers
                .iter()
                .map(|provider| provider.id())
                .collect::<Vec<_>>(),
            vec!["edge-semantic-chat"]
        );

        let _ = fs::remove_dir_all(&config_dir);
        unsafe {
            match previous_config {
                Some(value) => std::env::set_var("HERDR_MCP_CONFIG_DIR", value),
                None => std::env::remove_var("HERDR_MCP_CONFIG_DIR"),
            }
        }
    }

    #[test]
    fn a_slow_route_leaves_failover_budget_for_the_routes_behind_it() {
        let _guard = crate::test_env::lock();
        if let Ok(mut cooldowns) = route_cooldowns().lock() {
            cooldowns.clear();
        }
        let cursor = AtomicUsize::new(0);
        let budget = RouteBudget::split(Duration::from_millis(50), Duration::from_millis(200));
        let mut granted = Vec::new();

        let served = execute_route_pool(
            3,
            &cursor,
            budget,
            |index| format!("budget-test-{index}"),
            |index, allowance| {
                granted.push(allowance);
                if index == 0 {
                    thread::sleep(Duration::from_millis(60));
                    Err(SemanticError::new("timeout"))
                } else {
                    Ok(index)
                }
            },
        )
        .unwrap();

        assert_eq!(served, 1);
        assert_eq!(granted.len(), 2);
        assert!(granted.iter().all(|allowance| *allowance <= budget.attempt));

        cursor.store(0, Ordering::Relaxed);
        let after_timeout = execute_route_pool(
            3,
            &cursor,
            budget,
            |index| format!("budget-test-{index}"),
            |index, _| Ok(index),
        )
        .unwrap();
        assert_eq!(after_timeout, 1);

        if let Ok(mut cooldowns) = route_cooldowns().lock() {
            cooldowns.clear();
        }
    }

    #[test]
    #[ignore = "requires a live semantic route in config.json and network access"]
    fn live_configured_semantic_contract_smoke() {
        let service = SemanticService::from_config();
        assert!(
            service.configured(),
            "config.json must contain a typed semantic route"
        );
        let response = service
            .evaluate(
                &SemanticRequest::new(json!("The task edits Rust code and runs cargo tests.")).ask(
                    "requires_code_edit",
                    SemanticQuestion::noul(
                        "Does completing this task require editing source code?",
                        "Source code must change",
                        "No source code edit is required",
                    ),
                ),
            )
            .unwrap();
        assert!(
            response
                .answer("requires_code_edit")
                .and_then(SemanticAnswer::noul_probability)
                .is_some_and(|probability| probability >= DEFAULT_DECISION_THRESHOLD)
        );
    }
}
