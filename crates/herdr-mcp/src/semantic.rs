use reqwest::blocking::Client;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
#[cfg(target_os = "macos")]
use std::io::Read;
#[cfg(target_os = "macos")]
use std::path::Path;
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
#[cfg(target_os = "macos")]
use std::thread;
use std::time::Duration;
#[cfg(target_os = "macos")]
use std::time::Instant;
use url::Url;

pub const DEFAULT_DECISION_THRESHOLD: f64 = 0.70;
pub const TYPESAFE_PROVIDER_ID: &str = "typesafe-jev";

const TYPESAFE_API_KEY_ENV: &str = "TYPESAFE_API_KEY";
const TYPESAFE_BASE_URL_ENV: &str = "TYPESAFE_BASE_URL";
const TYPESAFE_MODEL_ENV: &str = "TYPESAFE_MODEL";
#[cfg(any(target_os = "macos", test))]
const PROVIDER_ENV_KEYS: [&str; 3] = [
    TYPESAFE_API_KEY_ENV,
    TYPESAFE_BASE_URL_ENV,
    TYPESAFE_MODEL_ENV,
];
const DEFAULT_TYPESAFE_BASE_URL: &str = "https://api.typesafe.ai/v1";
const DEFAULT_TYPESAFE_MODEL: &str = "jev-latest";
const PROVIDER_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_STATE_BYTES: usize = 64 * 1024;
const MAX_QUESTIONS: usize = 32;
const MAX_CRITERIA: usize = 64;

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
}

trait SemanticProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn evaluate(&self, request: &SemanticRequest) -> Result<SemanticResponse, SemanticError>;
}

pub struct SemanticService {
    providers: Vec<Box<dyn SemanticProvider>>,
}

impl SemanticService {
    pub fn from_env() -> Self {
        let mut providers: Vec<Box<dyn SemanticProvider>> = Vec::new();
        if let Some(provider) = TypeSafeProvider::from_env() {
            providers.push(Box::new(provider));
        }
        Self { providers }
    }

    pub fn configured(&self) -> bool {
        !self.providers.is_empty()
    }

    pub fn evaluate(&self, request: &SemanticRequest) -> Result<SemanticResponse, SemanticError> {
        request.validate()?;
        if self.providers.is_empty() {
            return Err(SemanticError::new("not_configured"));
        }
        let mut last = SemanticError::new("provider_unavailable");
        for provider in &self.providers {
            match provider.evaluate(request) {
                Ok(response) => return Ok(response),
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    pub fn capability_json(&self) -> Value {
        json!({
            "available": self.configured(),
            "providers": self.providers.iter().map(|provider| provider.id()).collect::<Vec<_>>(),
            "policy": "advisory_only",
            "fallback": "existing_behavior",
        })
    }
}

#[cfg(any(target_os = "macos", test))]
pub fn service_environment(inherited: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    PROVIDER_ENV_KEYS
        .into_iter()
        .filter_map(|key| {
            clean_env(key)
                .or_else(|| {
                    inherited
                        .get(key)
                        .map(|value| value.trim().to_owned())
                        .filter(|value| valid_env_value(value))
                })
                .map(|value| (key.to_owned(), value))
        })
        .collect()
}

#[cfg(target_os = "macos")]
pub fn service_environment_matches_user_shell(
    inherited: &BTreeMap<String, String>,
    home: &Path,
) -> bool {
    service_environment_with_user_shell(inherited, home) == provider_environment_from_map(inherited)
}

#[cfg(target_os = "macos")]
fn provider_environment_from_map(inherited: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    PROVIDER_ENV_KEYS
        .into_iter()
        .filter_map(|key| {
            inherited
                .get(key)
                .map(|value| value.trim().to_owned())
                .filter(|value| valid_env_value(value))
                .map(|value| (key.to_owned(), value))
        })
        .collect()
}

#[cfg(target_os = "macos")]
pub fn service_environment_with_user_shell(
    inherited: &BTreeMap<String, String>,
    home: &Path,
) -> BTreeMap<String, String> {
    let exported = provider_environment_from_process();
    match provider_environment_from_zsh(home) {
        Some(shell) => PROVIDER_ENV_KEYS
            .into_iter()
            .filter_map(|key| {
                exported
                    .get(key)
                    .cloned()
                    .or_else(|| shell.get(key).cloned())
                    .map(|value| (key.to_owned(), value))
            })
            .collect(),
        None => service_environment(inherited),
    }
}

#[cfg(target_os = "macos")]
fn provider_environment_from_process() -> BTreeMap<String, String> {
    PROVIDER_ENV_KEYS
        .into_iter()
        .filter_map(|key| clean_env(key).map(|value| (key.to_owned(), value)))
        .collect()
}

#[cfg(target_os = "macos")]
fn provider_environment_from_zsh(home: &Path) -> Option<BTreeMap<String, String>> {
    const PREFIX: &str = "__HERDR_SEMANTIC_ENV__";
    const SCRIPT: &str = r#"
for key in TYPESAFE_API_KEY TYPESAFE_BASE_URL TYPESAFE_MODEL; do
  value=${(P)key}
  if [[ -n "$value" ]]; then
    print -r -- "__HERDR_SEMANTIC_ENV__${key}=${value}"
  fi
done
"#;
    let mut child = Command::new("/bin/zsh")
        .args(["-ic", SCRIPT])
        .env("HOME", home)
        .env_remove("ZDOTDIR")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut stdout = stdout;
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    }?;
    if !status.success() {
        return None;
    }
    let bytes = reader.join().ok()?.ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let mut environment = BTreeMap::new();
    for line in text.lines() {
        let Some(payload) = line.strip_prefix(PREFIX) else {
            continue;
        };
        let Some((key, value)) = payload.split_once('=') else {
            continue;
        };
        if PROVIDER_ENV_KEYS.contains(&key) && valid_env_value(value) {
            environment.insert(key.to_owned(), value.to_owned());
        }
    }
    Some(environment)
}

struct TypeSafeProvider {
    api_key: String,
    endpoint: Url,
    model: String,
    client: Client,
}

impl TypeSafeProvider {
    fn from_env() -> Option<Self> {
        Self::new(
            clean_env(TYPESAFE_API_KEY_ENV)?,
            clean_env(TYPESAFE_BASE_URL_ENV)
                .unwrap_or_else(|| DEFAULT_TYPESAFE_BASE_URL.to_owned()),
            clean_env(TYPESAFE_MODEL_ENV).unwrap_or_else(|| DEFAULT_TYPESAFE_MODEL.to_owned()),
        )
        .ok()
    }

    fn new(api_key: String, base_url: String, model: String) -> Result<Self, SemanticError> {
        if !valid_env_value(&model) || model.len() > 128 {
            return Err(SemanticError::new("model_invalid"));
        }
        Ok(Self {
            api_key,
            endpoint: systemone_url(&base_url)?,
            model,
            client: Client::builder()
                .timeout(PROVIDER_TIMEOUT)
                .build()
                .map_err(|_| SemanticError::new("client_unavailable"))?,
        })
    }
}

impl SemanticProvider for TypeSafeProvider {
    fn id(&self) -> &'static str {
        TYPESAFE_PROVIDER_ID
    }

    fn evaluate(&self, request: &SemanticRequest) -> Result<SemanticResponse, SemanticError> {
        let questions = request
            .questions
            .iter()
            .map(|(id, question)| (id.clone(), question.to_json()))
            .collect::<Map<_, _>>();
        let response = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(&self.api_key)
            .json(&json!({
                "state": request.state,
                "model": self.model,
                "questions": questions,
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
        parse_response(
            request,
            response
                .json::<Value>()
                .map_err(|_| SemanticError::new("invalid_response"))?,
        )
    }
}

fn parse_response(
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
                let probabilities = probability_map(raw.get("probabilities"))?;
                if !probabilities.contains_key(&choice) {
                    return Err(SemanticError::new("invalid_response"));
                }
                SemanticAnswer::Choice {
                    choice,
                    probabilities,
                    confidence: probability(raw.get("confidence"))?,
                }
            }
            SemanticQuestion::Score { .. } => SemanticAnswer::Score {
                score: raw
                    .get("score")
                    .and_then(Value::as_f64)
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| SemanticError::new("invalid_response"))?,
                probabilities: probability_map(raw.get("probabilities"))?,
                confidence: probability(raw.get("confidence"))?,
            },
        };
        answers.insert(id.clone(), answer);
    }
    Ok(SemanticResponse {
        provider: TYPESAFE_PROVIDER_ID.to_owned(),
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

fn systemone_url(raw: &str) -> Result<Url, SemanticError> {
    let mut base = Url::parse(raw.trim()).map_err(|_| SemanticError::new("base_url_invalid"))?;
    if base.username() != ""
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
    {
        return Err(SemanticError::new("base_url_invalid"));
    }
    let secure = base.scheme() == "https";
    let loopback_http = base.scheme() == "http"
        && base
            .host_str()
            .is_some_and(|host| matches!(host, "127.0.0.1" | "::1" | "localhost"));
    if !secure && !loopback_http {
        return Err(SemanticError::new("base_url_invalid"));
    }
    let path = base.path().trim_end_matches('/').to_owned();
    base.set_path(&format!("{path}/systemone"));
    Ok(base)
}

fn clean_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| valid_env_value(value))
}

fn valid_env_value(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn one_shot_server(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 16 * 1024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        format!("http://127.0.0.1:{}/v1", address.port())
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
        let response =
            TypeSafeProvider::new("test-key".to_owned(), base_url, "jev-test".to_owned())
                .unwrap()
                .evaluate(&request)
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
    fn user_keeps_existing_behavior_without_provider() {
        let _guard = crate::test_env::lock();
        let previous = std::env::var_os(TYPESAFE_API_KEY_ENV);
        unsafe { std::env::remove_var(TYPESAFE_API_KEY_ENV) };
        let service = SemanticService::from_env();
        assert!(!service.configured());
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
        unsafe {
            match previous {
                Some(value) => std::env::set_var(TYPESAFE_API_KEY_ENV, value),
                None => std::env::remove_var(TYPESAFE_API_KEY_ENV),
            }
        }
    }

    #[test]
    fn service_environment_prefers_current_and_preserves_existing_provider_values() {
        let _guard = crate::test_env::lock();
        let previous_key = std::env::var_os(TYPESAFE_API_KEY_ENV);
        let previous_base = std::env::var_os(TYPESAFE_BASE_URL_ENV);
        unsafe {
            std::env::set_var(TYPESAFE_API_KEY_ENV, "current-key");
            std::env::remove_var(TYPESAFE_BASE_URL_ENV);
        }
        let inherited = BTreeMap::from([
            (TYPESAFE_API_KEY_ENV.to_owned(), "old-key".to_owned()),
            (
                TYPESAFE_BASE_URL_ENV.to_owned(),
                "https://api.typesafe.ai/v1".to_owned(),
            ),
        ]);
        let environment = service_environment(&inherited);
        assert_eq!(
            environment.get(TYPESAFE_API_KEY_ENV).map(String::as_str),
            Some("current-key")
        );
        assert_eq!(
            environment.get(TYPESAFE_BASE_URL_ENV).map(String::as_str),
            Some("https://api.typesafe.ai/v1")
        );
        unsafe {
            match previous_key {
                Some(value) => std::env::set_var(TYPESAFE_API_KEY_ENV, value),
                None => std::env::remove_var(TYPESAFE_API_KEY_ENV),
            }
            match previous_base {
                Some(value) => std::env::set_var(TYPESAFE_BASE_URL_ENV, value),
                None => std::env::remove_var(TYPESAFE_BASE_URL_ENV),
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn service_environment_reads_unexported_zshrc_and_allows_removal() {
        let _guard = crate::test_env::lock();
        let previous_key = std::env::var_os(TYPESAFE_API_KEY_ENV);
        let previous_base = std::env::var_os(TYPESAFE_BASE_URL_ENV);
        let previous_model = std::env::var_os(TYPESAFE_MODEL_ENV);
        unsafe {
            std::env::remove_var(TYPESAFE_API_KEY_ENV);
            std::env::remove_var(TYPESAFE_BASE_URL_ENV);
            std::env::remove_var(TYPESAFE_MODEL_ENV);
        }

        let home =
            std::env::temp_dir().join(format!("herdr-semantic-shell-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join(".zshrc"),
            "TYPESAFE_API_KEY=unexported-test-key\nTYPESAFE_MODEL=jev-test\n",
        )
        .unwrap();

        let inherited = BTreeMap::from([(
            TYPESAFE_API_KEY_ENV.to_owned(),
            "stale-service-key".to_owned(),
        )]);
        assert!(
            !service_environment_matches_user_shell(&inherited, &home),
            "a changed shell credential must invalidate the service no-op fast path"
        );
        let environment = service_environment_with_user_shell(&inherited, &home);
        assert_eq!(
            environment.get(TYPESAFE_API_KEY_ENV).map(String::as_str),
            Some("unexported-test-key")
        );
        assert_eq!(
            environment.get(TYPESAFE_MODEL_ENV).map(String::as_str),
            Some("jev-test")
        );
        assert!(service_environment_matches_user_shell(&environment, &home));

        fs::write(home.join(".zshrc"), "").unwrap();
        let environment = service_environment_with_user_shell(&inherited, &home);
        assert!(
            !service_environment_matches_user_shell(&inherited, &home),
            "removing the shell credential must invalidate a stale service environment"
        );
        assert!(
            !environment.contains_key(TYPESAFE_API_KEY_ENV),
            "a successful shell probe with no key must remove stale inherited provider credentials"
        );

        let _ = fs::remove_dir_all(&home);
        unsafe {
            match previous_key {
                Some(value) => std::env::set_var(TYPESAFE_API_KEY_ENV, value),
                None => std::env::remove_var(TYPESAFE_API_KEY_ENV),
            }
            match previous_base {
                Some(value) => std::env::set_var(TYPESAFE_BASE_URL_ENV, value),
                None => std::env::remove_var(TYPESAFE_BASE_URL_ENV),
            }
            match previous_model {
                Some(value) => std::env::set_var(TYPESAFE_MODEL_ENV, value),
                None => std::env::remove_var(TYPESAFE_MODEL_ENV),
            }
        }
    }

    #[test]
    #[ignore = "requires TYPESAFE_API_KEY and live network"]
    fn live_typesafe_contract_smoke() {
        let service = SemanticService::from_env();
        assert!(service.configured(), "TYPESAFE_API_KEY must be set");
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
