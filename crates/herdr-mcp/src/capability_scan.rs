use crate::capability_inventory::{
    AgentCapabilityRecord, CapabilityInventoryStore, Evidence, INVENTORY_SCHEMA_VERSION, ProbeLevel,
};
use crate::capability_probe::{
    PROBE_ADAPTER_VERSION, deep_probe, find_executable, fingerprint, herdr_declared_start_kinds,
    version_probe,
};
use crate::herdr::HerdrClient;
use crate::paths::RuntimePaths;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_AGENT_MANIFESTS: usize = 256;
const MAX_PARALLEL_AGENT_PROBES: usize = 4;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ScanOptions {
    pub json: bool,
    pub refresh: bool,
    pub probe: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize)]
struct LiveAgentInstance {
    agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    interactive_ready: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ScanReport {
    schema_version: i64,
    observed_at_ms: i64,
    refresh_requested: bool,
    deep_probe_requested: bool,
    manifest_count: usize,
    herdr_start_kind_count: usize,
    cache_hits: usize,
    probe_misses: usize,
    probe_parallelism: usize,
    inventory_updated: bool,
    inventory_path: String,
    agents: Vec<AgentCapabilityRecord>,
    live_instances: Vec<LiveAgentInstance>,
    warnings: Vec<String>,
}

#[derive(Debug)]
struct ScanJob {
    agent: String,
    manifest: Value,
    binary: Option<PathBuf>,
    herdr_startable: Option<bool>,
    fingerprint: String,
}

pub fn run(options: ScanOptions) -> Result<ExitCode, String> {
    let paths = RuntimePaths::discover()?;
    let socket = paths
        .herdr_socket
        .clone()
        .ok_or_else(|| "Herdr socket is unavailable on this platform".to_owned())?;
    let client = HerdrClient::new(socket);

    if options.refresh {
        client
            .call_with_timeout(
                "server.reload_agent_manifests",
                json!({}),
                Duration::from_secs(10),
            )
            .map_err(|error| format!("cannot refresh Herdr agent manifests: {error}"))?;
    }

    let manifests = client
        .call_with_timeout("server.agent_manifests", json!({}), Duration::from_secs(10))
        .map_err(|error| format!("cannot read Herdr agent manifests: {error}"))?;
    let manifest_rows = manifest_rows(&manifests)?;
    let live = client
        .call_with_timeout("agent.list", json!({}), Duration::from_secs(10))
        .map_err(|error| format!("cannot read live Herdr agents: {error}"))?;
    let live_instances = live_instances(&live);
    let mut warnings = Vec::new();
    let declared_start_kinds = match herdr_declared_start_kinds() {
        Ok(kinds) => Some(kinds),
        Err(error) => {
            warnings.push(format!("start_kind_discovery_unavailable: {error}"));
            None
        }
    };
    let start_kind_set = declared_start_kinds
        .as_ref()
        .map(|kinds| kinds.iter().cloned().collect::<BTreeSet<_>>());
    let manifest_by_agent = manifest_rows
        .iter()
        .filter_map(|manifest| {
            let agent = manifest.get("agent")?.as_str()?.to_owned();
            Some((agent, manifest.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let discovered_agents = discovered_agent_kinds(
        &manifest_by_agent,
        declared_start_kinds.as_deref(),
        &live_instances,
    );

    let mut store = CapabilityInventoryStore::open(&paths.config_dir)?;
    let requested_level = if options.probe {
        ProbeLevel::Deep
    } else {
        ProbeLevel::Version
    };
    let observed_at_ms = unix_ms();
    let existing_records = CapabilityInventoryStore::load_existing(&paths.config_dir)?;
    let mut cache_hits = 0_usize;
    let mut records = Vec::with_capacity(discovered_agents.len());
    let mut probe_jobs = Vec::new();
    for agent in discovered_agents {
        let manifest = manifest_by_agent.get(&agent).unwrap_or(&Value::Null);
        let herdr_startable = start_kind_set.as_ref().map(|kinds| kinds.contains(&agent));
        let binary = find_executable(&agent);
        let fingerprint = fingerprint(&agent, manifest, binary.as_deref(), herdr_startable)?;
        if !options.refresh
            && let Some(cached) = store.get(&agent)?
            && cached.fingerprint == fingerprint
            && cached.probe_level >= requested_level
            && cached.probe_adapter_version == PROBE_ADAPTER_VERSION
        {
            cache_hits += 1;
            records.push(cached);
            continue;
        }
        probe_jobs.push(ScanJob {
            agent,
            manifest: manifest.clone(),
            binary,
            herdr_startable,
            fingerprint,
        });
    }
    let probe_misses = probe_jobs.len();
    let probe_parallelism = probe_misses.min(MAX_PARALLEL_AGENT_PROBES);
    records.extend(scan_jobs_bounded(
        &probe_jobs,
        requested_level,
        observed_at_ms,
    )?);
    records.sort_by(|left, right| left.agent.cmp(&right.agent));
    let inventory_updated = records != existing_records;
    if inventory_updated {
        store.replace_all(&records)?;
    }

    let report = ScanReport {
        schema_version: INVENTORY_SCHEMA_VERSION,
        observed_at_ms,
        refresh_requested: options.refresh,
        deep_probe_requested: options.probe,
        manifest_count: manifest_rows.len(),
        herdr_start_kind_count: declared_start_kinds.as_ref().map_or(0, Vec::len),
        cache_hits,
        probe_misses,
        probe_parallelism,
        inventory_updated,
        inventory_path: store.path().display().to_string(),
        agents: records,
        live_instances,
        warnings,
    };
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("cannot encode capability scan report: {error}"))?
        );
    } else {
        print_human(&report);
    }
    Ok(ExitCode::SUCCESS)
}

fn scan_jobs_bounded(
    jobs: &[ScanJob],
    probe_level: ProbeLevel,
    observed_at_ms: i64,
) -> Result<Vec<AgentCapabilityRecord>, String> {
    let mut records = Vec::with_capacity(jobs.len());
    for chunk in jobs.chunks(MAX_PARALLEL_AGENT_PROBES) {
        let batch = thread::scope(|scope| {
            let handles = chunk
                .iter()
                .map(|job| {
                    scope.spawn(move || {
                        scan_agent(
                            &job.agent,
                            &job.manifest,
                            job.binary.as_deref(),
                            job.herdr_startable,
                            job.fingerprint.clone(),
                            probe_level,
                            observed_at_ms,
                        )
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| "capability probe worker panicked".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()
        })?;
        records.extend(batch);
    }
    Ok(records)
}

fn scan_agent(
    agent: &str,
    manifest: &Value,
    binary: Option<&Path>,
    herdr_startable: Option<bool>,
    fingerprint: String,
    probe_level: ProbeLevel,
    observed_at_ms: i64,
) -> AgentCapabilityRecord {
    let binary_version = binary.and_then(|path| version_probe(agent, path, observed_at_ms));
    let startable_evidence = herdr_startable.map(|value| Evidence {
        value,
        source: "herdr_cli:agent_start_help".to_owned(),
        authority: "herdr_declared".to_owned(),
        observed_at_ms,
        detail: Some("kind is declared by the installed Herdr CLI".to_owned()),
    });
    let executable_available = Some(Evidence {
        value: binary.is_some(),
        source: "path_lookup".to_owned(),
        authority: "observed".to_owned(),
        observed_at_ms,
        detail: binary.map(|path| path.display().to_string()),
    });
    let available_for_start = herdr_startable.map(|startable| Evidence {
        value: startable && binary.is_some(),
        source: "herdr_start_kind+path_lookup".to_owned(),
        authority: "derived".to_owned(),
        observed_at_ms,
        detail: None,
    });
    let deep = if probe_level == ProbeLevel::Deep {
        binary
            .map(|path| deep_probe(agent, path, observed_at_ms))
            .unwrap_or_default()
    } else {
        Default::default()
    };
    AgentCapabilityRecord {
        schema_version: INVENTORY_SCHEMA_VERSION,
        agent: agent.to_owned(),
        manifest_version: manifest
            .get("active_version")
            .and_then(Value::as_str)
            .map(str::to_owned),
        manifest_source: manifest
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_owned),
        manifest_source_kind: manifest
            .get("source_kind")
            .and_then(Value::as_str)
            .map(str::to_owned),
        binary_path: binary.map(|path| path.display().to_string()),
        herdr_startable: startable_evidence,
        executable_available,
        available_for_start,
        binary_version,
        provider: None,
        model: None,
        profile: None,
        supports_code_edit: deep.supports_code_edit,
        supports_shell: deep.supports_shell,
        supports_vision: None,
        reasoning_tier: None,
        latency_tier: None,
        cost_tier: None,
        context_tier: None,
        interactive_only: None,
        can_run_headless: deep.can_run_headless,
        probe_level,
        probe_adapter_version: PROBE_ADAPTER_VERSION,
        fingerprint,
        observed_at_ms,
    }
}

fn manifest_rows(value: &Value) -> Result<Vec<Value>, String> {
    let rows = value
        .get("manifests")
        .and_then(Value::as_array)
        .ok_or_else(|| "Herdr agent manifest response is missing manifests array".to_owned())?;
    if rows.len() > MAX_AGENT_MANIFESTS {
        return Err(format!(
            "Herdr returned {} agent manifests; maximum is {MAX_AGENT_MANIFESTS}",
            rows.len()
        ));
    }
    Ok(rows.clone())
}

fn discovered_agent_kinds(
    manifests: &BTreeMap<String, Value>,
    start_kinds: Option<&[String]>,
    live_instances: &[LiveAgentInstance],
) -> BTreeSet<String> {
    let mut kinds = manifests.keys().cloned().collect::<BTreeSet<_>>();
    if let Some(start_kinds) = start_kinds {
        kinds.extend(start_kinds.iter().cloned());
    }
    kinds.extend(live_instances.iter().map(|instance| instance.agent.clone()));
    kinds
}

fn live_instances(value: &Value) -> Vec<LiveAgentInstance> {
    let mut rows = value
        .get("agents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|agent| {
            Some(LiveAgentInstance {
                agent: agent.get("agent")?.as_str()?.to_owned(),
                name: agent.get("name").and_then(Value::as_str).map(str::to_owned),
                status: agent
                    .get("agent_status")
                    .or_else(|| agent.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
                cwd: agent.get("cwd").and_then(Value::as_str).map(str::to_owned),
                pane_id: agent
                    .get("pane_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                workspace_id: agent
                    .get("workspace_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                interactive_ready: agent.get("interactive_ready").and_then(Value::as_bool),
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.agent
            .cmp(&right.agent)
            .then(left.name.cmp(&right.name))
            .then(left.pane_id.cmp(&right.pane_id))
    });
    rows
}

fn print_human(report: &ScanReport) {
    println!("Herdr capability scan schema {}", report.schema_version);
    println!("manifests: {}", report.manifest_count);
    println!("Herdr start kinds: {}", report.herdr_start_kind_count);
    println!("cache hits: {}", report.cache_hits);
    println!("probe misses: {}", report.probe_misses);
    println!("probe parallelism: {}", report.probe_parallelism);
    println!("inventory updated: {}", report.inventory_updated);
    println!("inventory: {}", report.inventory_path);
    println!("agents:");
    for record in &report.agents {
        let version = record
            .binary_version
            .as_ref()
            .map(|evidence| evidence.value.as_str())
            .unwrap_or("unknown");
        let binary = record.binary_path.as_deref().unwrap_or("not-on-path");
        let headless = record
            .can_run_headless
            .as_ref()
            .map(|evidence| if evidence.value { "yes" } else { "no" })
            .unwrap_or("unknown");
        let live_count = report
            .live_instances
            .iter()
            .filter(|instance| instance.agent == record.agent)
            .count();
        let available = record
            .available_for_start
            .as_ref()
            .map(|evidence| if evidence.value { "yes" } else { "no" })
            .unwrap_or("unknown");
        println!(
            "  {} available={} manifest={} binary={} version={} headless={} live={}",
            record.agent,
            available,
            record.manifest_version.as_deref().unwrap_or("unknown"),
            binary,
            version,
            headless,
            live_count
        );
    }
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_rows_are_bounded_and_shape_checked() {
        assert!(
            manifest_rows(&json!({}))
                .unwrap_err()
                .contains("missing manifests array")
        );
        let oversized = vec![json!({"agent": "pi"}); MAX_AGENT_MANIFESTS + 1];
        assert!(
            manifest_rows(&json!({"manifests": oversized}))
                .unwrap_err()
                .contains("maximum")
        );
        assert_eq!(
            manifest_rows(&json!({"manifests": [{"agent": "pi"}]}))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn probe_batches_never_exceed_the_parallelism_cap() {
        let jobs = (0..11)
            .map(|index| ScanJob {
                agent: format!("agent-{index}"),
                manifest: Value::Null,
                binary: None,
                herdr_startable: None,
                fingerprint: format!("fp-{index}"),
            })
            .collect::<Vec<_>>();
        let sizes = jobs
            .chunks(MAX_PARALLEL_AGENT_PROBES)
            .map(<[ScanJob]>::len)
            .collect::<Vec<_>>();
        assert_eq!(sizes, vec![4, 4, 3]);
        assert!(sizes.iter().all(|size| *size <= MAX_PARALLEL_AGENT_PROBES));
    }

    #[test]
    fn live_state_projection_keeps_runtime_fields_separate_from_inventory() {
        let value = json!({
            "agents": [{
                "agent": "pi",
                "name": "worker",
                "agent_status": "idle",
                "cwd": "/repo",
                "pane_id": "w1:p1",
                "workspace_id": "w1",
                "interactive_ready": true
            }]
        });
        assert_eq!(
            live_instances(&value),
            vec![LiveAgentInstance {
                agent: "pi".to_owned(),
                name: Some("worker".to_owned()),
                status: "idle".to_owned(),
                cwd: Some("/repo".to_owned()),
                pane_id: Some("w1:p1".to_owned()),
                workspace_id: Some("w1".to_owned()),
                interactive_ready: Some(true),
            }]
        );
    }

    #[test]
    fn discovery_unions_manifests_start_kinds_and_live_agents() {
        let manifests = BTreeMap::from([
            ("pi".to_owned(), json!({"agent": "pi"})),
            ("claude".to_owned(), json!({"agent": "claude"})),
        ]);
        let start_kinds = vec!["pi".to_owned(), "qwen".to_owned()];
        let live = vec![LiveAgentInstance {
            agent: "omp".to_owned(),
            name: None,
            status: "idle".to_owned(),
            cwd: None,
            pane_id: None,
            workspace_id: None,
            interactive_ready: Some(true),
        }];
        assert_eq!(
            discovered_agent_kinds(&manifests, Some(&start_kinds), &live),
            ["claude", "omp", "pi", "qwen"]
                .into_iter()
                .map(str::to_owned)
                .collect()
        );
    }
}
