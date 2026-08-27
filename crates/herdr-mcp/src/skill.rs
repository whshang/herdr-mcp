use crate::contract;
use crate::exec_sessions::enriched_exec_path;
use crate::paths::RuntimePaths;
use crate::progressive_skills::ProgressiveSkillService;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, USER_AGENT};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const DEFAULT_PROJECT_SKILL_URL: &str = "https://whshang.github.io/herdr-mcp/herdr-mcp-SKILL.md";
const PROJECT_BUNDLED_SOURCE: &str = "bundled:assets/herdr-mcp-SKILL.md";
const NATIVE_LOCAL_SOURCE: &str = "local:herdr --skill";
const NATIVE_BUNDLED_SOURCE: &str = "bundled:assets/herdr-agent-SKILL.md";
const BUNDLED_PROJECT_SKILL: &str = include_str!("../../../assets/herdr-mcp-SKILL.md");
const BUNDLED_NATIVE_SKILL: &str = include_str!("../../../assets/herdr-agent-SKILL.md");
const MAX_PROJECT_BYTES: usize = 512 * 1024;
const MAX_NATIVE_BYTES: usize = 512 * 1024;
const MAX_JSON_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone)]
struct SkillConfig {
    project_url: String,
    cache_ttl: Duration,
    fetch_timeout: Duration,
    native_timeout: Duration,
    network: bool,
    runtime_status_path: PathBuf,
    self_update_path: PathBuf,
    dsh_home: PathBuf,
}

impl SkillConfig {
    fn from_env() -> Result<Self, String> {
        let paths = RuntimePaths::discover()?;
        let project_url = env::var("HERDR_MCP_SKILL_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_PROJECT_SKILL_URL.to_owned());
        let cache_ttl_secs = env_u64("HERDR_SKILL_CACHE_SEC", 3_600).max(60);
        let fetch_timeout_ms = env_u64("HERDR_SKILL_FETCH_TIMEOUT_MS", 15_000).clamp(3_000, 60_000);
        let native_timeout_ms =
            env_u64("HERDR_NATIVE_SKILL_TIMEOUT_MS", 5_000).clamp(1_000, 15_000);
        let state_dir = env::var_os("HERDR_MCP_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or(paths.config_dir);
        let runtime_status_path = env::var_os("HERDR_RUNTIME_STATUS_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| state_dir.join("runtime-status-prod.json"));
        let self_update_path = env::var_os("HERDR_SELF_UPDATE_STATUS_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| state_dir.join("self-update-status.json"));
        let dsh_home = env::var_os("DSH_HOME")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join(".dsh")))
            .unwrap_or_else(|| PathBuf::from(".dsh"));
        Ok(Self {
            project_url,
            cache_ttl: Duration::from_secs(cache_ttl_secs),
            fetch_timeout: Duration::from_millis(fetch_timeout_ms),
            native_timeout: Duration::from_millis(native_timeout_ms),
            network: env::var("HERDR_SKILL_NETWORK").ok().as_deref() != Some("0"),
            runtime_status_path,
            self_update_path,
            dsh_home,
        })
    }
}

#[derive(Debug, Clone)]
struct ProjectCache {
    content: String,
    source: String,
    fetched_at_ms: u64,
}

#[derive(Debug, Clone)]
struct ProjectSkill {
    content: String,
    source: String,
    origin: &'static str,
    fetched_at_ms: u64,
    cached: bool,
    stale: bool,
    refresh_error: Option<String>,
}

#[derive(Debug, Clone)]
struct NativeSkill {
    content: String,
    source: &'static str,
    origin: &'static str,
    sha256: String,
}

#[derive(Debug, Clone)]
pub struct SkillService {
    cache: Arc<Mutex<Option<ProjectCache>>>,
    progressive: Arc<ProgressiveSkillService>,
}

impl Default for SkillService {
    fn default() -> Self {
        Self {
            cache: Arc::new(Mutex::new(None)),
            progressive: Arc::new(ProgressiveSkillService::new()),
        }
    }
}

impl SkillService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fetch_for_runtime(&self, args: &Value, snapshot: &Value) -> Value {
        self.fetch_for_runtime_mode(args, snapshot, ProgressiveSkillService::enabled_from_env())
    }

    fn fetch_for_runtime_mode(
        &self,
        args: &Value,
        snapshot: &Value,
        progressive_enabled: bool,
    ) -> Value {
        if progressive_enabled {
            match progressive_mode_requested(args) {
                Ok(true) => return self.progressive.bootstrap(snapshot),
                Ok(false) => return self.fetch(args),
                Err(error) => return error,
            }
        }
        self.fetch(args)
    }

    pub fn local_call(&self, method: &str, params: &Value, snapshot: &Value) -> Option<Value> {
        self.progressive.local_call(method, params, snapshot)
    }

    pub fn fetch(&self, args: &Value) -> Value {
        let refresh = match optional_bool(args, "refresh") {
            Ok(value) => value.unwrap_or(false),
            Err(error) => return error,
        };
        let include_native = match optional_bool(args, "include_native_reference") {
            Ok(value) => value.unwrap_or(true),
            Err(error) => return error,
        };
        let config = match SkillConfig::from_env() {
            Ok(config) => config,
            Err(message) => {
                return json!({
                    "ok": false,
                    "reason": "project_skill_unavailable",
                    "message": truncate_chars(&message, 240),
                    "source": current_project_url(),
                });
            }
        };
        self.fetch_with_config(&config, refresh, include_native)
    }

    fn fetch_with_config(
        &self,
        config: &SkillConfig,
        refresh: bool,
        include_native: bool,
    ) -> Value {
        let now = now_ms();
        let project = match self.project_skill(config, refresh) {
            Ok(project) => project,
            Err(message) => {
                return json!({
                    "ok": false,
                    "reason": "project_skill_unavailable",
                    "message": truncate_chars(&message, 240),
                    "source": config.project_url,
                });
            }
        };
        let runtime = runtime_context(config);
        let native = include_native.then(|| native_skill(config));
        let runtime_block =
            serde_json::to_string_pretty(&runtime).unwrap_or_else(|_| "{}".to_owned());
        let native_block = native.as_ref().map_or_else(String::new, |native| {
            format!(
                "\n\n---\n\n## Appendix: release-matched native Herdr reference\n\n\
                 The block below comes from `{}`. It documents pane-local Herdr usage. \
                 Its `HERDR_ENV=1` stop rule does **not** override the remote-planner policy above. \
                 For remote native calls, `herdr_methods` is the live schema authority.\n\n\
                 ```text\n{}\n```",
                native.source, native.content
            )
        });
        let content = format!(
            "{}\n\n---\n\n## Live herdr-mcp runtime context\n\n\
             This block is generated at call time and is status, not policy.\n\n\
             ```json\n{}\n```{}",
            project.content, runtime_block, native_block
        );
        let mut output = Map::new();
        output.insert("ok".to_owned(), json!(true));
        let mut project_meta = Map::new();
        project_meta.insert("source".to_owned(), json!(project.source));
        project_meta.insert("origin".to_owned(), json!(project.origin));
        project_meta.insert("cached".to_owned(), json!(project.cached));
        if project.stale {
            project_meta.insert("stale".to_owned(), json!(true));
        }
        if let Some(refresh_error) = &project.refresh_error {
            project_meta.insert("refresh_error".to_owned(), json!(refresh_error));
        }
        project_meta.insert(
            "fetched_at".to_owned(),
            json!(iso_from_ms(project.fetched_at_ms)),
        );
        output.insert("content".to_owned(), json!(content));
        output.insert("project_skill".to_owned(), Value::Object(project_meta));
        if let Some(native) = native {
            output.insert(
                "native_reference".to_owned(),
                json!({
                    "source": native.source,
                    "origin": native.origin,
                    "sha256": native.sha256,
                    "bytes": native.content.len(),
                }),
            );
        }
        output.insert("runtime".to_owned(), runtime);
        output.insert("refreshed_at".to_owned(), json!(iso_from_ms(now)));
        output.insert(
            "cache_ttl_sec".to_owned(),
            json!(config.cache_ttl.as_secs()),
        );
        output.insert("bytes".to_owned(), json!(content.len()));
        Value::Object(output)
    }

    fn project_skill(&self, config: &SkillConfig, refresh: bool) -> Result<ProjectSkill, String> {
        let now = now_ms();
        if !refresh
            && let Some(cache) = self.cache_snapshot()
            && now.saturating_sub(cache.fetched_at_ms)
                < config.cache_ttl.as_millis().min(u128::from(u64::MAX)) as u64
        {
            return Ok(ProjectSkill {
                content: cache.content,
                source: cache.source,
                origin: "cache",
                fetched_at_ms: cache.fetched_at_ms,
                cached: true,
                stale: false,
                refresh_error: None,
            });
        }

        let mut refresh_error = None;
        if config.network {
            match fetch_text(config) {
                Ok(content) if !content.trim().is_empty() => {
                    let content = content.trim().to_owned();
                    let cache = ProjectCache {
                        content: content.clone(),
                        source: config.project_url.clone(),
                        fetched_at_ms: now,
                    };
                    self.store_cache(cache.clone());
                    return Ok(ProjectSkill {
                        content,
                        source: cache.source,
                        origin: "network",
                        fetched_at_ms: now,
                        cached: false,
                        stale: false,
                        refresh_error: None,
                    });
                }
                Ok(_) => {
                    refresh_error = Some("project skill document was empty".to_owned());
                    if let Some(cache) = self.cache_snapshot() {
                        return Ok(ProjectSkill {
                            content: cache.content,
                            source: cache.source,
                            origin: "cache",
                            fetched_at_ms: cache.fetched_at_ms,
                            cached: true,
                            stale: true,
                            refresh_error: refresh_error.clone(),
                        });
                    }
                }
                Err(error) => {
                    refresh_error = Some(truncate_chars(&error, 240));
                    if let Some(cache) = self.cache_snapshot() {
                        return Ok(ProjectSkill {
                            content: cache.content,
                            source: cache.source,
                            origin: "cache",
                            fetched_at_ms: cache.fetched_at_ms,
                            cached: true,
                            stale: true,
                            refresh_error: refresh_error.clone(),
                        });
                    }
                }
            }
        }

        let content = BUNDLED_PROJECT_SKILL.trim();
        if content.is_empty() {
            return Err("bundled herdr-mcp skill is empty".to_owned());
        }
        Ok(ProjectSkill {
            content: content.to_owned(),
            source: PROJECT_BUNDLED_SOURCE.to_owned(),
            origin: "bundled",
            fetched_at_ms: now,
            cached: false,
            stale: false,
            refresh_error,
        })
    }

    fn cache_snapshot(&self) -> Option<ProjectCache> {
        self.cache.lock().ok().and_then(|cache| cache.clone())
    }

    fn store_cache(&self, value: ProjectCache) {
        if let Ok(mut cache) = self.cache.lock() {
            *cache = Some(value);
        }
    }
}

pub fn pointer() -> Value {
    let upstream = current_project_url();
    json!({
        "tool": "herdr_skill",
        "project_upstream": upstream.trim_start_matches("https://").trim_start_matches("http://"),
        "project_bundled": PROJECT_BUNDLED_SOURCE,
        "native_reference": NATIVE_LOCAL_SOURCE,
        "self_update": "herdr-self-update",
        "hint": "Remote-planner policy first; live runtime/update context is generated per call; release-matched native Herdr skill is appended as scoped reference. refresh=true rechecks project policy upstream.",
    })
}

fn current_project_url() -> String {
    env::var("HERDR_MCP_SKILL_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_PROJECT_SKILL_URL.to_owned())
}

fn fetch_text(config: &SkillConfig) -> Result<String, String> {
    let project_url = config.project_url.clone();
    let fetch_timeout = config.fetch_timeout;
    thread::spawn(move || fetch_text_on_dedicated_thread(&project_url, fetch_timeout))
        .join()
        .map_err(|_| "skill fetch worker panicked".to_owned())?
}

fn fetch_text_on_dedicated_thread(
    project_url: &str,
    fetch_timeout: Duration,
) -> Result<String, String> {
    let client = Client::builder()
        .timeout(fetch_timeout)
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|error| format!("cannot build skill HTTP client: {error}"))?;
    let mut response = client
        .get(project_url)
        .header(ACCEPT, "text/plain, text/markdown, */*")
        .header(
            USER_AGENT,
            format!("herdr-mcp/{} skill-refresh", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .map_err(|error| format!("skill fetch failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take((MAX_PROJECT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read skill response: {error}"))?;
    if bytes.len() > MAX_PROJECT_BYTES {
        return Err("project skill exceeds 512 KiB limit".to_owned());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn native_skill(config: &SkillConfig) -> NativeSkill {
    if let Ok(output) = run_bounded_command(
        "herdr",
        &["--skill"],
        config.native_timeout,
        MAX_NATIVE_BYTES,
    ) {
        let content = output.stdout.trim().to_owned();
        if output.status.success() && !content.is_empty() {
            return NativeSkill {
                sha256: sha256(&content),
                content,
                source: NATIVE_LOCAL_SOURCE,
                origin: "local",
            };
        }
    }
    let content = BUNDLED_NATIVE_SKILL.trim().to_owned();
    NativeSkill {
        sha256: sha256(&content),
        content,
        source: NATIVE_BUNDLED_SOURCE,
        origin: "bundled",
    }
}

fn runtime_context(config: &SkillConfig) -> Value {
    let runtime_status = read_optional_json(&config.runtime_status_path);
    let update_status = read_optional_json(&config.self_update_path);
    let worker_fallbacks = worker_fallback_context(config);
    let contract = contract::identity().ok();
    json!({
        "server_version": env!("CARGO_PKG_VERSION"),
        "build_commit": env::var("HERDR_MCP_BUILD_COMMIT").unwrap_or_else(|_| "dev".to_owned()),
        "contract_profile": env::var("HERDR_MCP_CONTRACT_PROFILE").unwrap_or_else(|_| "current".to_owned()),
        "tool_execution": {
            "contract_epoch": contract.as_ref().map(|value| value.epoch),
            "tool_count": contract.as_ref().map(|value| value.tool_count),
            "server_concurrent_requests": true,
            "jsonrpc_batch": false,
            "multi_operation_tool_args": false,
            "read_policy": "parallel independent reads when the MCP client supports concurrent calls",
            "mutation_policy": "ordered by default within one project",
        },
        "network_skill_refresh": config.network,
        "worker_fallbacks": worker_fallbacks,
        "runtime_generation": compact_runtime_status(runtime_status.as_ref()),
        "self_update": compact_update_status(update_status.as_ref()),
    })
}

fn read_optional_json(path: &Path) -> Option<Value> {
    let metadata = fs::metadata(path).ok()?;
    if metadata.len() > MAX_JSON_BYTES {
        return None;
    }
    let raw = fs::read(path).ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    value.is_object().then_some(value)
}

fn compact_runtime_status(value: Option<&Value>) -> Value {
    let Some(manager) = value
        .and_then(|value| value.get("manager"))
        .and_then(Value::as_object)
    else {
        return Value::Null;
    };
    json!({
        "active_generation": manager.get("active_generation").cloned().unwrap_or(Value::Null),
        "previous_generation": manager.get("previous_generation").cloned().unwrap_or(Value::Null),
        "last_good_generation": manager.get("last_good_generation").cloned().unwrap_or(Value::Null),
        "transition_seq": manager.get("transition_seq").cloned().unwrap_or(Value::Null),
    })
}

fn compact_update_status(value: Option<&Value>) -> Value {
    let Some(value) = value.and_then(Value::as_object) else {
        return Value::Null;
    };
    json!({
        "state": value.get("state").or_else(|| value.get("status")).cloned().unwrap_or(Value::Null),
        "target_version": value.get("target_version").cloned().unwrap_or(Value::Null),
        "source": value.get("source").cloned().unwrap_or(Value::Null),
        "updated_at": value.get("updated_at").cloned().unwrap_or(Value::Null),
    })
}

fn worker_fallback_context(config: &SkillConfig) -> Value {
    let dsh_version = cli_version("dsh", &["--version"]);
    let tui_profile_version = dsh_tui_profile_version(config);
    json!({
        "dsh_headless": {
            "available": dsh_version.is_some(),
            "version": dsh_version,
            "invocation": "herdr_exec_start -> dsh --profile headless <task>",
            "note": "long-running fallback; inspect mutations before retrying after timeout",
        },
        "dsh_tui": {
            "available": tui_profile_version.is_some(),
            "profile_package": tui_profile_version,
            "role": "human-interactive fallback",
        },
    })
}

fn cli_version(command: &str, args: &[&str]) -> Option<String> {
    let output = run_bounded_command(command, args, Duration::from_secs(2), 64 * 1024).ok()?;
    if !output.status.success() {
        return None;
    }
    let combined = if output.stdout.trim().is_empty() {
        output.stderr
    } else {
        output.stdout
    };
    combined
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
}

fn dsh_tui_profile_version(config: &SkillConfig) -> Option<String> {
    let path = config
        .dsh_home
        .join("profiles")
        .join("dsh-tui")
        .join("package.json");
    let value = read_optional_json(&path)?;
    value
        .pointer("/dependencies/@deepseek-harness-tui~1dsh-tui")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("dependencies")
                .and_then(Value::as_object)
                .and_then(|deps| deps.get("@deepseek-harness-tui/dsh-tui"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
}

#[derive(Debug)]
struct BoundedOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

fn run_bounded_command(
    command: &str,
    args: &[&str],
    timeout: Duration,
    max_bytes: usize,
) -> Result<BoundedOutput, String> {
    let mut child = Command::new(command)
        .args(args)
        .env("PATH", enriched_exec_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot start {command}: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .map(|stream| thread::spawn(move || read_limited(stream, max_bytes)));
    let stderr = child
        .stderr
        .take()
        .map(|stream| thread::spawn(move || read_limited(stream, max_bytes)));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{command} timed out"));
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("cannot wait for {command}: {error}"));
            }
        }
    };
    let stdout = stdout
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let stderr = stderr
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    Ok(BoundedOutput {
        status,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

fn read_limited<R: Read>(mut reader: R, max_bytes: usize) -> Vec<u8> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let room = max_bytes.saturating_sub(output.len());
        if room > 0 {
            output.extend_from_slice(&buffer[..read.min(room)]);
        }
    }
    output
}

fn sha256(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    format!("{digest:x}")
}

fn progressive_mode_requested(args: &Value) -> Result<bool, Value> {
    optional_bool(args, "refresh")?;
    optional_bool(args, "include_native_reference")?;
    let legacy_full = optional_bool(args, "legacy_full")?.unwrap_or(false);
    Ok(!legacy_full)
}

fn optional_bool(args: &Value, key: &str) -> Result<Option<bool>, Value> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        _ => Err(json!({
            "ok": false,
            "code": "invalid_params",
            "message": format!("{key} must be a boolean"),
        })),
    }
}

fn env_u64(key: &str, fallback: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(fallback)
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn iso_from_ms(ms: u64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()
        .and_then(|value| value.format(&Rfc3339).ok())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned())
}

fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    fn config() -> SkillConfig {
        let root = env::temp_dir().join(format!(
            "herdr-mcp-skill-test-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        SkillConfig {
            project_url: "http://127.0.0.1:1/unused".to_owned(),
            cache_ttl: Duration::from_secs(3_600),
            fetch_timeout: Duration::from_secs(2),
            native_timeout: Duration::from_millis(200),
            network: false,
            runtime_status_path: root.join("runtime-status-prod.json"),
            self_update_path: root.join("self-update-status.json"),
            dsh_home: root.join("dsh"),
        }
    }

    #[test]
    fn bundled_skill_builds_runtime_block_without_native_reference() {
        let service = SkillService::new();
        let config = config();
        let result = service.fetch_with_config(&config, false, false);
        assert_eq!(result["ok"], true);
        assert_eq!(result["project_skill"]["origin"], "bundled");
        assert!(result.get("native_reference").is_none());
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("## Live herdr-mcp runtime context"));
        assert!(content.contains("# herdr-mcp remote planner skill"));
        assert!(content.contains("## 1A. Latency-aware tool scheduling"));
        assert!(content.contains("dependency-aware **wave**"));
        assert!(content.contains("are already compacted"));
        assert!(content.contains(
            "Long build/test/process work belongs in `herdr_exec_start` / `herdr_exec_read`"
        ));
        assert!(content.contains(
            "prefer `herdr_exec_start` -> `herdr_exec_read` (delta) over a blocking `herdr_exec`"
        ));
        assert!(content.contains("phase=started"));
        assert!(content.contains("phase=completed"));
        assert!(content.contains("bytes_read"));
        assert!(content.contains("bytes_total"));
        assert!(content.contains("elapsed_ms"));
        fs::remove_dir_all(config.runtime_status_path.parent().unwrap()).unwrap();
    }

    #[test]
    fn network_result_is_cached_and_refresh_can_use_stale_cache() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            let body = "# network skill\nnetwork-body\n";
            let mut stream = stream;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let service = SkillService::new();
        let mut config = config();
        config.network = true;
        config.project_url = format!("http://{address}/skill.md");
        let first = service.project_skill(&config, false).unwrap();
        assert_eq!(first.origin, "network");
        server.join().unwrap();
        let cached = service.project_skill(&config, false).unwrap();
        assert_eq!(cached.origin, "cache");
        let stale = service.project_skill(&config, true).unwrap();
        assert_eq!(stale.origin, "cache");
        assert!(stale.stale);
        fs::remove_dir_all(config.runtime_status_path.parent().unwrap()).unwrap();
    }

    #[test]
    fn runtime_status_is_compacted_and_never_echoes_extra_fields() {
        let config = config();
        fs::write(
            &config.runtime_status_path,
            br#"{"manager":{"active_generation":"g2","previous_generation":"g1","last_good_generation":"g2","transition_seq":7,"secret":"do-not-echo"}}"#,
        )
        .unwrap();
        fs::write(
            &config.self_update_path,
            br#"{"state":"idle","target_version":"0.4.0","source":"release","updated_at":"now","token":"do-not-echo"}"#,
        )
        .unwrap();
        let runtime = runtime_context(&config);
        assert_eq!(runtime["runtime_generation"]["active_generation"], "g2");
        assert_eq!(runtime["self_update"]["state"], "idle");
        assert_eq!(runtime["tool_execution"]["contract_epoch"], 2);
        assert_eq!(runtime["tool_execution"]["tool_count"], 18);
        assert_eq!(
            runtime["tool_execution"]["server_concurrent_requests"],
            true
        );
        assert_eq!(runtime["tool_execution"]["jsonrpc_batch"], false);
        assert_eq!(
            runtime["tool_execution"]["multi_operation_tool_args"],
            false
        );
        let encoded = runtime.to_string();
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("token"));
        fs::remove_dir_all(config.runtime_status_path.parent().unwrap()).unwrap();
    }

    #[test]
    fn pointer_matches_public_inspect_contract() {
        let value = pointer();
        assert_eq!(value["tool"], "herdr_skill");
        assert_eq!(value["project_bundled"], PROJECT_BUNDLED_SOURCE);
        assert_eq!(value["native_reference"], NATIVE_LOCAL_SOURCE);
    }

    #[test]
    fn legacy_full_is_an_internal_progressive_compatibility_escape_hatch() {
        assert!(progressive_mode_requested(&json!({})).unwrap());
        assert!(!progressive_mode_requested(&json!({"legacy_full": true})).unwrap());
        let error = progressive_mode_requested(&json!({"legacy_full": "yes"})).unwrap_err();
        assert_eq!(error["code"], "invalid_params");
    }

    #[test]
    fn progressive_runtime_preserves_explicit_legacy_full_response_shape() {
        let service = SkillService::new();
        let snapshot = json!({"agents": []});
        let progressive = service.fetch_for_runtime_mode(&json!({}), &snapshot, true);
        assert_eq!(progressive["mode"], "progressive");
        assert!(progressive.get("catalog").is_some());

        let legacy = service.fetch_for_runtime_mode(
            &json!({
                "legacy_full": true,
                "include_native_reference": false
            }),
            &snapshot,
            true,
        );
        assert_eq!(legacy["ok"], true);
        assert!(legacy.get("mode").is_none());
        assert!(legacy.get("project_skill").is_some());
        assert!(legacy.get("runtime").is_some());
        assert!(
            legacy["content"]
                .as_str()
                .is_some_and(|content| content.contains("# herdr-mcp remote planner skill"))
        );
    }

    #[test]
    fn progressive_disabled_preserves_legacy_shape_without_new_arguments() {
        let service = SkillService::new();
        let result = service.fetch_for_runtime_mode(&json!({}), &json!({"agents": []}), false);
        assert_eq!(result["ok"], true);
        assert!(result.get("mode").is_none());
        assert!(result.get("project_skill").is_some());
        assert!(result.get("runtime").is_some());
    }

    #[test]
    fn sha256_is_stable() {
        assert_eq!(
            sha256("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
