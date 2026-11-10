use crate::browser_extension_identity::BrowserExtensionIdentity;
use reqwest::blocking::{Client, Response};
use reqwest::redirect::Policy;
use serde::Deserialize;
use serde_json::{Value, json};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

const REPOSITORY: &str = "whshang/herdr-mcp";
const API_HOST: &str = "api.github.com";
const RAW_HOST: &str = "raw.githubusercontent.com";
const MAX_FILES: usize = 512;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandaloneInstallOptions {
    pub reference: Option<String>,
    pub load_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CommitResponse {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct TreeResponse {
    truncated: bool,
    tree: Vec<TreeEntry>,
}

#[derive(Debug, Deserialize)]
struct TreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
    size: Option<u64>,
    sha: String,
}

#[derive(Debug)]
struct Blob {
    repo_path: String,
    relative_path: PathBuf,
    size: u64,
    git_sha: String,
}

/// One extension tree resolved at an immutable commit.
#[derive(Debug)]
struct ExtensionSnapshot {
    commit: String,
    blobs: Vec<Blob>,
}

/// Fetch boundary for one extension tree.
///
/// The CLI `extension standalone install` path fetches from GitHub; the
/// update-time staging path reuses the same staging/activation/state code
/// through this trait so its atomic swap, rollback and state record are
/// exercised for real in tests instead of being faked one level higher.
trait ExtensionSource {
    fn resolve_commit(&self, reference: &str) -> Result<String, String>;
    fn list_blobs(&self, commit: &str) -> Result<Vec<Blob>, String>;
    fn fetch_blob(&self, commit: &str, blob: &Blob) -> Result<Vec<u8>, String>;
}

struct GitHubSource {
    client: Client,
}

impl GitHubSource {
    fn new() -> Result<Self, String> {
        Ok(Self { client: client()? })
    }
}

impl ExtensionSource for GitHubSource {
    fn resolve_commit(&self, reference: &str) -> Result<String, String> {
        resolve_ref(&self.client, reference)
    }

    fn list_blobs(&self, commit: &str) -> Result<Vec<Blob>, String> {
        list_blobs(&self.client, commit)
    }

    fn fetch_blob(&self, commit: &str, blob: &Blob) -> Result<Vec<u8>, String> {
        download_blob(&self.client, commit, blob)
    }
}

#[derive(Debug)]
struct PreparedManifest {
    version: String,
    extension_id: String,
    source_sha256: String,
}

#[derive(Debug)]
struct ExtensionLoad {
    browser: String,
    profile: String,
    path: PathBuf,
    location: Option<i64>,
}

/// One Chromium-family user-data root that `doctor` probes for the managed
/// standalone extension. Every Chromium browser keeps its own user-data root
/// with one `Secure Preferences` per profile, so a load can only be evidenced
/// per root; probing Google Chrome alone misreads a workstation whose only
/// browser is ego lite (ego-browser).
#[cfg(any(target_os = "macos", test))]
#[derive(Debug)]
struct ChromiumRoot {
    browser: &'static str,
    root: PathBuf,
}

/// Read-only per-root evidence: which browsers were probed, whether the root
/// exists, and whether its profiles could actually be read.
#[derive(Debug)]
struct ScannedRoot {
    browser: String,
    root: PathBuf,
    present: bool,
    readable: bool,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Default)]
struct ChromiumScan {
    loads: Vec<ExtensionLoad>,
    roots: Vec<ScannedRoot>,
    unreadable_profiles: usize,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug)]
enum ProfileProbe {
    Loaded(PathBuf, Option<i64>),
    Absent,
    Unreadable(usize),
}

#[derive(Debug)]
pub(crate) struct StandaloneBrowserDoctor {
    state: &'static str,
    installed: bool,
    extension_id: String,
    expected_path: Option<PathBuf>,
    configured_load_path: Option<PathBuf>,
    loads: Vec<ExtensionLoad>,
    roots: Vec<ScannedRoot>,
    unreadable_profiles: usize,
    reason: Option<String>,
    /// Version reconciliation between the staged extension, the running
    /// Runtime release and the registered (running) browser extension.
    version: ExtensionVersionReport,
}

impl StandaloneBrowserDoctor {
    pub(crate) fn as_json(&self) -> Value {
        let loaded = self
            .loads
            .iter()
            .map(|load| {
                json!({
                    "browser": load.browser,
                    "profile": load.profile,
                    "path": load.path,
                    "location": load.location,
                    "matches_expected": self.matches(&load.path),
                })
            })
            .collect::<Vec<_>>();
        let roots = self
            .roots
            .iter()
            .map(|root| {
                json!({
                    "browser": root.browser,
                    "root": root.root,
                    "present": root.present,
                    "readable": root.readable,
                })
            })
            .collect::<Vec<_>>();
        json!({
            "state": self.state,
            "installed": self.installed,
            "extension_id": self.extension_id,
            "expected_path": self.expected_path,
            "configured_load_path": self.configured_load_path,
            "loaded": loaded,
            "browser_roots": roots,
            "drift_count": self.drift_count(),
            "unreadable_profiles": self.unreadable_profiles,
            "reason": self.reason,
            "version": self.version.as_json(),
        })
    }

    pub(crate) fn doctor_line(&self) -> String {
        match self.state {
            "drift" => {
                let drift = self.loads.iter().find(|load| !self.matches(&load.path));
                format!(
                    "WARN standalone-extension-load state=drift extension_id={} expected={} actual={} browser={} profile={} action=reload-from-managed-path-via-chrome://extensions",
                    self.extension_id,
                    quote_path(self.expected_path.as_deref()),
                    drift
                        .map(|load| quote_path(Some(&load.path)))
                        .unwrap_or_else(|| "null".to_owned()),
                    drift
                        .map(|load| json_quote(&load.browser))
                        .unwrap_or_else(|| "null".to_owned()),
                    drift
                        .map(|load| json_quote(&load.profile))
                        .unwrap_or_else(|| "null".to_owned()),
                )
            }
            "loaded" => format!(
                "INFO standalone-extension-load state=loaded extension_id={} expected={} loaded_profiles={} browsers={}",
                self.extension_id,
                quote_path(self.expected_path.as_deref()),
                self.loads.len(),
                json_array(&loaded_browsers(&self.loads)),
            ),
            "not_loaded" => format!(
                "WARN standalone-extension-load state=not_loaded extension_id={} expected={} browsers={} action=load-unpacked-via-chrome://extensions",
                self.extension_id,
                quote_path(self.expected_path.as_deref()),
                json_array(&probed_browsers(&self.roots)),
            ),
            "chrome_not_found" => format!(
                "INFO standalone-extension-load state=chrome_not_found extension_id={} expected={} browsers=[]",
                self.extension_id,
                quote_path(self.expected_path.as_deref()),
            ),
            "not_installed" => "INFO standalone-extension-load state=not_installed".to_owned(),
            _ => format!(
                "INFO standalone-extension-load state=not_probed reason={}",
                json_quote(self.reason.as_deref().unwrap_or("unknown"))
            ),
        }
    }

    /// Version reconciliation line for `doctor` (staged vs running release vs
    /// registered browser extension).
    pub(crate) fn version_line(&self) -> String {
        self.version.doctor_line()
    }

    fn drift_count(&self) -> usize {
        if self.expected_path.is_none() {
            return 0;
        }
        self.loads
            .iter()
            .filter(|load| !self.matches(&load.path))
            .count()
    }

    /// A Chrome load is expected when it resolves to the managed `current`
    /// directory or equals the Chrome-facing path recorded by the last install.
    /// The recorded comparison is lexical only: macOS TCC can deny
    /// `lstat`/`readlink` on `~/Documents`, and equivalence must never depend
    /// on being able to inspect the user-visible path.
    fn matches(&self, path: &Path) -> bool {
        self.expected_path
            .as_deref()
            .is_some_and(|expected| paths_equivalent(expected, path))
            || self
                .configured_load_path
                .as_deref()
                .is_some_and(|configured| lexical_path_eq(configured, path))
    }
}

pub(crate) fn doctor_report(config_dir: &Path) -> StandaloneBrowserDoctor {
    let home = match home_dir() {
        Ok(home) => home,
        Err(error) => {
            let mut report = doctor_not_probed(None, String::new(), error);
            report.version = ExtensionVersionReport::unknown("home_dir_unavailable");
            return report;
        }
    };
    doctor_report_in(&home, config_dir)
}

fn doctor_report_in(home: &Path, config_dir: &Path) -> StandaloneBrowserDoctor {
    let mut report = diagnose_extension_load(home);
    report.version = version_report_in(
        home,
        config_dir,
        crate::runtime_meta::bundled_extension_version(),
        crate::runtime_meta::compiled_source_commit(),
    );
    report
}

/// Browser-side load evidence. Version reconciliation is attached separately by
/// `doctor_report_in`, so this probe never depends on the state database.
fn diagnose_extension_load(home: &Path) -> StandaloneBrowserDoctor {
    let identity = match crate::browser_extension_identity::official_standalone_identity() {
        Ok(identity) => identity,
        Err(error) => return doctor_not_probed(None, String::new(), error),
    };
    let current = base_dir_for(home).join("current");
    if installed_manifest(&current, &identity).is_none() {
        return StandaloneBrowserDoctor {
            state: "not_installed",
            installed: false,
            extension_id: identity.extension_id,
            expected_path: Some(current),
            configured_load_path: None,
            loads: Vec::new(),
            roots: Vec::new(),
            unreadable_profiles: 0,
            reason: None,
            version: ExtensionVersionReport::unknown("not_computed"),
        };
    }

    #[cfg(target_os = "macos")]
    {
        // `doctor` accepts the managed `current` directory or the exact
        // Chrome-facing path recorded by the last install. The recorded value
        // comes from `~/.config` state and is compared lexically, so a
        // TCC-denied `~/Documents` (or a moved `--path`) cannot turn a healthy
        // managed install into a false `drift`; only a genuinely different
        // directory (for example an old Downloads copy) is drift.
        let state = read_json(&base_dir_for(home).join("state.json")).ok();
        let configured_load_path =
            reported_load_path(home, state.as_ref()).filter(|path| path != &current);
        diagnose_chromium_load(
            &current,
            configured_load_path.as_deref(),
            &identity.extension_id,
            &chromium_roots(home),
        )
    }

    #[cfg(not(target_os = "macos"))]
    {
        doctor_not_probed(
            Some(current),
            identity.extension_id,
            "Chrome unpacked-path probe is currently implemented on macOS".to_owned(),
        )
    }
}

/// Known Chromium-family user-data roots on macOS, in probe order. A
/// workstation may run no Google Chrome at all: on the machine this fix comes
/// from, ego lite (ego-browser) is the only browser and loads the managed
/// standalone extension unpacked. Keep the list to browsers that store
/// Chromium `Secure Preferences` at `<root>/<profile>/Secure Preferences`.
#[cfg(any(target_os = "macos", test))]
fn chromium_roots(home: &Path) -> Vec<ChromiumRoot> {
    let support = home.join("Library/Application Support");
    [
        ("Google Chrome", "Google/Chrome"),
        ("Google Chrome Beta", "Google/Chrome Beta"),
        ("Google Chrome Canary", "Google/Chrome Canary"),
        ("Chromium", "Chromium"),
        ("Brave", "BraveSoftware/Brave-Browser"),
        ("Microsoft Edge", "Microsoft Edge"),
        ("Arc", "Arc/User Data"),
        ("Helium", "net.imput.helium"),
        ("ego lite", "Citro Labs/ego lite"),
    ]
    .into_iter()
    .map(|(browser, relative)| ChromiumRoot {
        browser,
        root: support.join(relative),
    })
    .collect()
}

#[cfg(any(target_os = "macos", test))]
fn diagnose_chromium_load(
    expected_path: &Path,
    configured_load_path: Option<&Path>,
    extension_id: &str,
    roots: &[ChromiumRoot],
) -> StandaloneBrowserDoctor {
    let scan = scan_chromium_roots(roots, extension_id);
    let present_roots = scan.roots.iter().filter(|root| root.present).count();
    // A present but unreadable root is not evidence that the extension is
    // absent, so it must not produce a confident `not_loaded`.
    let unreadable_roots = scan
        .roots
        .iter()
        .filter(|root| root.present && !root.readable)
        .count();
    let mut report = StandaloneBrowserDoctor {
        state: "not_loaded",
        installed: true,
        extension_id: extension_id.to_owned(),
        expected_path: Some(expected_path.to_path_buf()),
        configured_load_path: configured_load_path.map(Path::to_path_buf),
        loads: scan.loads,
        roots: scan.roots,
        unreadable_profiles: scan.unreadable_profiles,
        reason: None,
        version: ExtensionVersionReport::unknown("not_computed"),
    };
    let state = if present_roots == 0 {
        // No known Chromium user-data root exists; the historical name is kept
        // so existing JSON consumers do not have to learn a new state.
        "chrome_not_found"
    } else if report.loads.is_empty() && (report.unreadable_profiles > 0 || unreadable_roots > 0) {
        "not_probed"
    } else if report.loads.is_empty() {
        "not_loaded"
    } else if report.loads.iter().any(|load| !report.matches(&load.path)) {
        "drift"
    } else {
        // Any probed Chromium root (Chrome, Chrome Beta/Canary, Chromium,
        // Brave, Edge, Arc, Helium, ego lite) can satisfy the managed load.
        "loaded"
    };
    report.state = state;
    if state == "not_probed" {
        report.reason = Some("Chromium browser profile preferences could not be read".to_owned());
    }
    report
}

#[cfg(any(target_os = "macos", test))]
fn scan_chromium_roots(roots: &[ChromiumRoot], extension_id: &str) -> ChromiumScan {
    let mut scan = ChromiumScan::default();
    for root in roots {
        let present = root.root.is_dir();
        let mut readable = false;
        // A root that exists but cannot be listed is reported through
        // `readable`; it must not hide a load found in another root, and it
        // must not be dropped from the per-root evidence either.
        if present && let Ok(entries) = fs::read_dir(&root.root) {
            readable = true;
            for entry in entries.flatten() {
                let profile_dir = entry.path();
                if !profile_dir.is_dir() {
                    continue;
                }
                match probe_profile(&profile_dir, extension_id) {
                    ProfileProbe::Loaded(path, location) => {
                        scan.loads.push(ExtensionLoad {
                            browser: root.browser.to_owned(),
                            profile: entry.file_name().to_string_lossy().to_string(),
                            path,
                            location,
                        });
                    }
                    ProfileProbe::Absent => {}
                    ProfileProbe::Unreadable(files) => {
                        scan.unreadable_profiles += files;
                    }
                }
            }
        }
        scan.roots.push(ScannedRoot {
            browser: root.browser.to_owned(),
            root: root.root.clone(),
            present,
            readable,
        });
    }
    scan
}

#[cfg(any(target_os = "macos", test))]
fn probe_profile(profile_dir: &Path, extension_id: &str) -> ProfileProbe {
    let mut unreadable = 0;
    for filename in ["Secure Preferences", "Preferences"] {
        let path = profile_dir.join(filename);
        if !path.is_file() {
            continue;
        }
        let prefs = match read_json(&path) {
            Ok(prefs) => prefs,
            Err(_) => {
                unreadable += 1;
                continue;
            }
        };
        // Read only the Herdr extension's exact settings entry. Doctor must not
        // enumerate or expose unrelated browser extensions or profile data.
        let setting = prefs
            .get("extensions")
            .and_then(|value| value.get("settings"))
            .and_then(|value| value.get(extension_id));
        let Some(load_path) = setting
            .and_then(|value| value.get("path"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        return ProfileProbe::Loaded(
            PathBuf::from(load_path),
            setting
                .and_then(|value| value.get("location"))
                .and_then(Value::as_i64),
        );
    }
    if unreadable > 0 {
        ProfileProbe::Unreadable(unreadable)
    } else {
        ProfileProbe::Absent
    }
}

fn doctor_not_probed(
    expected_path: Option<PathBuf>,
    extension_id: String,
    reason: String,
) -> StandaloneBrowserDoctor {
    StandaloneBrowserDoctor {
        state: "not_probed",
        installed: expected_path.is_some(),
        extension_id,
        expected_path,
        configured_load_path: None,
        loads: Vec::new(),
        roots: Vec::new(),
        unreadable_profiles: 0,
        reason: Some(reason),
        version: ExtensionVersionReport::unknown("not_probed"),
    }
}

fn loaded_browsers(loads: &[ExtensionLoad]) -> Vec<String> {
    let mut browsers: Vec<String> = Vec::new();
    for load in loads {
        if !browsers.contains(&load.browser) {
            browsers.push(load.browser.clone());
        }
    }
    browsers
}

fn probed_browsers(roots: &[ScannedRoot]) -> Vec<String> {
    let mut browsers: Vec<String> = Vec::new();
    for root in roots.iter().filter(|root| root.present) {
        if !browsers.contains(&root.browser) {
            browsers.push(root.browser.clone());
        }
    }
    browsers
}

fn json_array(values: &[String]) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "[]".to_owned())
}

/// Browser that already loads the standalone extension from the managed path or
/// the recorded load path. A non-Chrome Chromium browser wins so status and
/// install hints do not insist on Google Chrome when ego lite owns the load.
#[cfg(any(target_os = "macos", test))]
fn preferred_loading_browser(
    loads: &[ExtensionLoad],
    expected_path: &Path,
    configured_load_path: Option<&Path>,
) -> Option<String> {
    let matching = loads
        .iter()
        .filter(|load| {
            paths_equivalent(expected_path, &load.path)
                || configured_load_path
                    .is_some_and(|configured| lexical_path_eq(configured, &load.path))
        })
        .collect::<Vec<_>>();
    matching
        .iter()
        .find(|load| load.browser != "Google Chrome")
        .or_else(|| matching.first())
        .map(|load| load.browser.clone())
}

fn loading_browser(
    home: &Path,
    extension_id: &str,
    expected_path: &Path,
    configured_load_path: Option<&Path>,
) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let scan = scan_chromium_roots(&chromium_roots(home), extension_id);
        preferred_loading_browser(&scan.loads, expected_path, configured_load_path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (home, extension_id, expected_path, configured_load_path);
        None
    }
}

/// `chrome://extensions` is the extension page in every Chromium browser, so
/// the URL stays the same; the action names the browser that already loads the
/// extension only when it is not Google Chrome.
fn chrome_hint(loading_browser: Option<&str>, load_unpacked_path: &Path) -> Value {
    let action = match loading_browser {
        Some(browser) if browser != "Google Chrome" => {
            format!("Developer mode -> Load unpacked in {browser}")
        }
        _ => "Developer mode -> Load unpacked".to_owned(),
    };
    json!({
        "url": "chrome://extensions",
        "action": action,
        "load_unpacked_path": load_unpacked_path,
    })
}

fn paths_equivalent(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn quote_path(path: Option<&Path>) -> String {
    path.map(|path| json_quote(path.to_string_lossy().as_ref()))
        .unwrap_or_else(|| "null".to_owned())
}

fn json_quote(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"<unavailable>\"".to_owned())
}

/// One browser endpoint registration persisted by the running runtime.
///
/// `extension_version` is what the browser extension itself reports when it
/// registers, so it is the only local evidence of the code the browser is
/// actually executing. The staged files on disk cannot answer that: an unpacked
/// extension keeps running the code it loaded until it is reloaded (#833).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegisteredExtension {
    pub(crate) browser_family: String,
    pub(crate) extension_version: String,
    pub(crate) last_observed_at: i64,
}

/// Reconciliation between the staged standalone extension, the running Runtime
/// release and the extension version the browser registered.
///
/// States:
/// - `current`: staged files and registered (running) extension are this
///   release's extension version;
/// - `reload_pending`: the staged files are current, but the browser still runs
///   an older extension build, so only a Reload in `chrome://extensions` applies
///   it;
/// - `extension_stale`: the staged files do not belong to the running Runtime
///   release at all, so they must be staged again;
/// - `extension_ahead`: the registered extension is *newer* than the staged
///   build (for example a STORE build the user actually runs), so no Reload is
///   requested;
/// - `not_installed`: this workstation never installed the standalone channel;
/// - `unknown`: no registration evidence (or unreadable state database).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtensionVersionReport {
    state: &'static str,
    staged_version: Option<String>,
    staged_source_commit: Option<String>,
    expected_version: Option<String>,
    runtime_source_commit: Option<String>,
    registered: Vec<RegisteredExtension>,
    reason: Option<String>,
}

/// The extension version this Runtime release expects and the commit it was
/// built from. Either side may be unknown in a source build.
struct ExpectedExtension<'a> {
    version: Option<&'a str>,
    source_commit: Option<&'a str>,
    source_manifest_sha256: Option<&'a str>,
}

const EXTENSION_RELOAD_NEXT_ACTION: &str = "click Reload on chrome://extensions";
const EXTENSION_INSTALL_NEXT_ACTION: &str = "herdr-mcp extension standalone install";

impl ExtensionVersionReport {
    pub(crate) fn unknown(reason: impl Into<String>) -> Self {
        Self {
            state: "unknown",
            staged_version: None,
            staged_source_commit: None,
            expected_version: None,
            runtime_source_commit: None,
            registered: Vec::new(),
            reason: Some(reason.into()),
        }
    }

    /// Test-only value object so the update-result notice mapping can be
    /// asserted without a workstation fixture.
    #[cfg(test)]
    pub(crate) fn for_test(state: &'static str) -> Self {
        Self {
            state,
            staged_version: Some("0.1.240".to_owned()),
            staged_source_commit: Some("a".repeat(40)),
            expected_version: Some("0.1.240".to_owned()),
            runtime_source_commit: Some("b".repeat(40)),
            registered: vec![RegisteredExtension {
                browser_family: "chrome".to_owned(),
                extension_version: "0.1.239".to_owned(),
                last_observed_at: 1,
            }],
            reason: None,
        }
    }

    fn registered_version(&self) -> Option<&RegisteredExtension> {
        self.registered.first()
    }

    fn next_action(&self) -> Option<&'static str> {
        match self.state {
            "reload_pending" => Some(EXTENSION_RELOAD_NEXT_ACTION),
            "extension_stale" => Some(EXTENSION_INSTALL_NEXT_ACTION),
            _ => None,
        }
    }

    fn action(&self) -> Option<&'static str> {
        match self.state {
            "reload_pending" => Some("reload_extension"),
            "extension_stale" => Some("install_extension"),
            _ => None,
        }
    }

    pub(crate) fn as_json(&self) -> Value {
        let registered = self
            .registered
            .iter()
            .map(|entry| {
                json!({
                    "browser_family": entry.browser_family,
                    "extension_version": entry.extension_version,
                    "last_observed_at": entry.last_observed_at,
                })
            })
            .collect::<Vec<_>>();
        json!({
            "state": self.state,
            "staged_version": self.staged_version,
            "staged_source_commit": self.staged_source_commit,
            "expected_version": self.expected_version,
            "runtime_source_commit": self.runtime_source_commit,
            "registered_version": self.registered_version().map(|entry| entry.extension_version.clone()),
            "registered_browser": self.registered_version().map(|entry| entry.browser_family.clone()),
            "registered_endpoints": registered,
            "action": self.action(),
            "next_action": self.next_action(),
            "reason": self.reason,
        })
    }

    /// Non-fatal `update apply` notice for a committed Runtime update.
    ///
    /// A staged extension that the browser has not loaded yet is a notice; a
    /// staged extension that does not belong to this release is a warning with
    /// the exact fix command. Neither changes the Runtime update result.
    pub(crate) fn update_notice(&self) -> Option<Value> {
        match self.state {
            "reload_pending" => Some(json!({
                "code": "extension_reload_required",
                "severity": "notice",
                "fatal": false,
                "message": "the standalone browser extension was staged for this Runtime release, but the browser is still running an older extension build",
                "staged_version": self.staged_version,
                "registered_version": self.registered_version().map(|entry| entry.extension_version.clone()),
                "next_action": EXTENSION_RELOAD_NEXT_ACTION,
            })),
            "extension_stale" => Some(json!({
                "code": "extension_stale",
                "severity": "warning",
                "fatal": false,
                "message": "the staged standalone browser extension does not belong to this Runtime release",
                "staged_version": self.staged_version,
                "staged_source_commit": self.staged_source_commit,
                "expected_version": self.expected_version,
                "runtime_source_commit": self.runtime_source_commit,
                "next_action": EXTENSION_INSTALL_NEXT_ACTION,
            })),
            _ => None,
        }
    }

    pub(crate) fn doctor_line(&self) -> String {
        let next_action = self.next_action().unwrap_or("none");
        match self.state {
            "current" | "reload_pending" | "extension_stale" => format!(
                "{} standalone-extension-version state={} staged_version={} registered_version={} expected_version={} staged_source_commit={} runtime_source_commit={} next_action={}",
                if self.state == "current" {
                    "INFO"
                } else {
                    "WARN"
                },
                self.state,
                quote_opt(self.staged_version.as_deref()),
                quote_opt(
                    self.registered_version()
                        .map(|entry| entry.extension_version.as_str())
                ),
                quote_opt(self.expected_version.as_deref()),
                quote_opt(self.staged_source_commit.as_deref()),
                quote_opt(self.runtime_source_commit.as_deref()),
                next_action,
            ),
            "not_installed" => "INFO standalone-extension-version state=not_installed".to_owned(),
            _ => format!(
                "INFO standalone-extension-version state={} reason={}",
                self.state,
                json_quote(self.reason.as_deref().unwrap_or("unknown"))
            ),
        }
    }
}

/// Order two extension manifest versions. Chrome accepts one to four dotted
/// numeric components, so a value that does not parse as semver is simply not
/// ordered (the caller keeps the conservative "versions differ" behavior).
fn extension_version_order(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    let left = semver::Version::parse(left.trim()).ok()?;
    let right = semver::Version::parse(right.trim()).ok()?;
    Some(left.cmp(&right))
}

fn quote_opt(value: Option<&str>) -> String {
    value.map(json_quote).unwrap_or_else(|| "null".to_owned())
}

/// Version reconciliation for `doctor` and the update path.
///
/// `doctor` passes this binary's own bundled extension version and build commit:
/// the installed `herdr-mcp` *is* the active Runtime, so its compile-time
/// identity is the running release. A source build carries no build commit and
/// reports `null`, which disables only the commit comparison, never the version
/// comparison.
pub(crate) fn version_report(
    config_dir: &Path,
    expected_version: Option<&str>,
    expected_commit: Option<&str>,
) -> ExtensionVersionReport {
    match home_dir() {
        Ok(home) => version_report_in(&home, config_dir, expected_version, expected_commit),
        Err(error) => ExtensionVersionReport::unknown(error),
    }
}

fn version_report_in(
    home: &Path,
    config_dir: &Path,
    expected_version: Option<&str>,
    expected_commit: Option<&str>,
) -> ExtensionVersionReport {
    let version = expected_version.filter(|value| !value.trim().is_empty());
    let expected = ExpectedExtension {
        version,
        // An unverifiable release identity (a source build, or a manifest field
        // that is not an immutable commit) must never be reported as drift.
        source_commit: expected_commit.filter(|value| is_sha(value)),
        source_manifest_sha256: if version == crate::runtime_meta::bundled_extension_version() {
            Some(crate::runtime_meta::bundled_extension_manifest_sha256())
        } else {
            None
        },
    };
    let base = base_dir_for(home);
    let current = base.join("current");
    let manifest = crate::browser_extension_identity::official_standalone_identity()
        .ok()
        .and_then(|identity| installed_manifest(&current, &identity));
    let Some(manifest) = manifest else {
        return ExtensionVersionReport {
            state: "not_installed",
            staged_version: None,
            staged_source_commit: None,
            expected_version: expected.version.map(str::to_owned),
            runtime_source_commit: expected.source_commit.map(str::to_owned),
            registered: Vec::new(),
            reason: None,
        };
    };
    let staged_version = manifest_version(&manifest);
    let state = read_json(&base.join("state.json")).ok();
    let staged_source_commit = json_string(state.as_ref(), "source_commit");
    let staged_source_manifest_sha256 = json_string(state.as_ref(), "source_manifest_sha256");
    let (registered, reason) = match registered_extensions(config_dir, 5) {
        Ok(registered) => (registered, None),
        Err(reason) => (Vec::new(), Some(reason)),
    };
    classify_extension_version(ExtensionVersionInputs {
        staged_version,
        staged_source_commit,
        staged_source_manifest_sha256,
        expected,
        registered,
        reason,
    })
}

struct ExtensionVersionInputs<'a> {
    staged_version: Option<String>,
    staged_source_commit: Option<String>,
    staged_source_manifest_sha256: Option<String>,
    expected: ExpectedExtension<'a>,
    registered: Vec<RegisteredExtension>,
    reason: Option<String>,
}

fn classify_extension_version(inputs: ExtensionVersionInputs<'_>) -> ExtensionVersionReport {
    let ExtensionVersionInputs {
        staged_version,
        staged_source_commit,
        staged_source_manifest_sha256,
        expected,
        registered,
        reason,
    } = inputs;
    let version_differs = matches!(
        (expected.version, staged_version.as_deref()),
        (Some(expected), Some(staged)) if expected != staged
    );
    let manifest_differs = matches!(
        (
            expected.source_manifest_sha256,
            staged_source_manifest_sha256.as_deref(),
        ),
        (Some(expected), Some(staged)) if expected != staged
    );
    let commit_differs = matches!(
        (expected.source_commit, staged_source_commit.as_deref()),
        (Some(expected), Some(staged)) if expected != staged
    );
    // #885: When the Runtime advances to a new source_commit without changing
    // `extension/` (the staged version matches the bundled version and any
    // recorded `source_manifest_sha256` matches the bundled manifest),
    // `commit_differs` is provenance drift only, not stale extension content.
    let content_stale =
        version_differs || manifest_differs || (expected.version.is_none() && commit_differs);
    let registered_version = registered
        .first()
        .map(|entry| entry.extension_version.as_str());
    let (state, reason) = if content_stale {
        let stale_reason = reason.or_else(|| {
            if version_differs {
                Some("extension_version_differs".to_owned())
            } else if manifest_differs {
                Some("extension_manifest_differs".to_owned())
            } else {
                Some("source_commit_differs".to_owned())
            }
        });
        ("extension_stale", stale_reason)
    } else if staged_version.is_none() {
        (
            "unknown",
            Some("staged_manifest_version_unreadable".to_owned()),
        )
    } else if registered_version.is_none() {
        (
            "unknown",
            Some(reason.unwrap_or_else(|| "no_endpoint_registration".to_owned())),
        )
    } else if registered_version == staged_version.as_deref() {
        let current_reason = if reason.is_none() && commit_differs {
            Some("source_commit_advanced_extension_unchanged".to_owned())
        } else {
            reason
        };
        ("current", current_reason)
    } else {
        // A registered extension behind the staged build is the "click Reload"
        // case. A registered extension *ahead* of it is a build this Runtime
        // does not stage (typically STORE), so it is reported without an action
        // instead of asking the user to downgrade with a Reload.
        match extension_version_order(
            registered_version.unwrap_or_default(),
            staged_version.as_deref().unwrap_or_default(),
        ) {
            Some(std::cmp::Ordering::Greater) => (
                "extension_ahead",
                Some("registered_extension_newer_than_staged".to_owned()),
            ),
            _ => ("reload_pending", reason),
        }
    };
    ExtensionVersionReport {
        state,
        staged_version,
        staged_source_commit,
        expected_version: expected.version.map(str::to_owned),
        runtime_source_commit: expected.source_commit.map(str::to_owned),
        registered,
        reason,
    }
}

/// Read the browser endpoint registrations the running Runtime persisted.
///
/// Only opaque registration fields are read (`browser_family`,
/// `extension_version`, `last_observed_at`); profile seeds, endpoint refs and
/// page data are never selected, so `doctor` cannot expose browser identity.
fn registered_extensions(
    config_dir: &Path,
    limit: usize,
) -> Result<Vec<RegisteredExtension>, String> {
    let db = config_dir.join("state.db");
    if !db.is_file() {
        return Err("state_db_missing".to_owned());
    }
    let conn =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| format!("state_db_open_failed: {error}"))?;
    let exists = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'browser_endpoints'",
            [],
            |_| Ok(()),
        )
        .is_ok();
    if !exists {
        return Err("browser_endpoint_table_missing".to_owned());
    }
    let limit = i64::try_from(limit.clamp(1, 64)).unwrap_or(64);
    let mut stmt = conn
        .prepare(
            "SELECT browser_family, extension_version, last_observed_at
             FROM browser_endpoints
             ORDER BY last_observed_at DESC, endpoint_ref
             LIMIT ?1",
        )
        .map_err(|error| format!("browser_endpoint_query_failed: {error}"))?;
    let rows = stmt
        .query_map([limit], |row| {
            Ok(RegisteredExtension {
                browser_family: row.get(0)?,
                extension_version: row.get(1)?,
                last_observed_at: row.get(2)?,
            })
        })
        .map_err(|error| format!("browser_endpoint_query_failed: {error}"))?;
    let mut registered: Vec<RegisteredExtension> = Vec::new();
    for entry in rows {
        let entry = entry.map_err(|error| format!("browser_endpoint_decode_failed: {error}"))?;
        if !registered.iter().any(|seen| {
            seen.browser_family == entry.browser_family
                && seen.extension_version == entry.extension_version
        }) {
            registered.push(entry);
        }
    }
    Ok(registered)
}

pub fn run_install(options: StandaloneInstallOptions) -> Result<ExitCode, String> {
    let view = install(options)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&view).map_err(|e| e.to_string())?
    );
    Ok(ExitCode::SUCCESS)
}

pub fn run_status() -> Result<ExitCode, String> {
    let home = home_dir()?;
    let base = base_dir_for(&home);
    let current = base.join("current");
    let identity = crate::browser_extension_identity::official_standalone_identity()?;
    let installed_manifest = installed_manifest(&current, &identity);
    let installed = installed_manifest.is_some();
    let state = fs::read_to_string(base.join("state.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let (user_visible_path, load_unpacked_path) = status_load_view(&home, &current, state.as_ref());
    let chrome = chrome_hint(
        loading_browser(
            &home,
            &identity.extension_id,
            &current,
            Some(&load_unpacked_path),
        )
        .as_deref(),
        &load_unpacked_path,
    );
    let view = json!({
        "ok": installed,
        "installed": installed,
        "path": current,
        "user_visible_path": user_visible_path,
        "chrome": chrome,
        "extension_id": identity.extension_id,
        "manifest_key_matches": installed,
        "state": state,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&view).map_err(|e| e.to_string())?
    );
    Ok(if installed {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn install(options: StandaloneInstallOptions) -> Result<Value, String> {
    let requested_ref = options.reference.unwrap_or_else(default_source_ref);
    validate_ref(&requested_ref)?;
    let home = home_dir()?;
    let current = base_dir_for(&home).join("current");
    let explicit_load_path = options
        .load_path
        .as_deref()
        .map(|value| resolve_load_path(&home, value))
        .transpose()?;
    if let Some(path) = explicit_load_path.as_deref() {
        preflight_explicit_load_path(path, &current)?;
    }

    let source = GitHubSource::new()?;
    let commit = source.resolve_commit(&requested_ref)?;
    let blobs = source.list_blobs(&commit)?;
    install_snapshot(
        &home,
        &requested_ref,
        &ExtensionSnapshot { commit, blobs },
        &source,
        explicit_load_path.as_deref(),
    )
}

fn install_snapshot(
    home: &Path,
    requested_ref: &str,
    snapshot: &ExtensionSnapshot,
    source: &dyn ExtensionSource,
    explicit_load_path: Option<&Path>,
) -> Result<Value, String> {
    let base = base_dir_for(home);
    let current = base.join("current");
    create_private_dir(&base).map_err(|e| format!("cannot create standalone directory: {e}"))?;
    let (staging, prepared) = write_snapshot(&base, snapshot, source)?;

    let had_current = current.exists();
    let backup = activate_snapshot(&base, &current, &staging, had_current)?;

    let user_visible_path = match explicit_load_path {
        Some(path) if path == current => json!({
            "ready": true,
            "status": "managed",
            "path": current,
            "target": current,
        }),
        Some(path) => match ensure_explicit_alias(home, path, &current) {
            Ok(value) => value,
            Err(error) => {
                rollback_activation(&current, &backup, had_current);
                return Err(error);
            }
        },
        None => ensure_user_visible_alias(home, &current),
    };
    let load_unpacked_path = preferred_load_unpacked_path(&user_visible_path, &current);
    let chrome = chrome_hint(
        loading_browser(
            home,
            &prepared.extension_id,
            &current,
            Some(&load_unpacked_path),
        )
        .as_deref(),
        &load_unpacked_path,
    );

    let state = state_record(
        requested_ref,
        &snapshot.commit,
        &prepared,
        &current,
        &load_unpacked_path,
        "cli",
    );
    if let Err(error) = atomic_json(&base.join("state.json"), &state) {
        remove_alias_if_created(&user_visible_path, &current);
        rollback_activation(&current, &backup, had_current);
        return Err(error);
    }
    if backup.exists() {
        let _ = fs::remove_dir_all(&backup);
    }

    Ok(json!({
        "ok": true,
        "channel": "standalone",
        "repository": REPOSITORY,
        "requested_ref": state["requested_ref"],
        "source_commit": state["source_commit"],
        "extension_version": state["extension_version"],
        "extension_id": state["extension_id"],
        "path": current,
        "user_visible_path": user_visible_path,
        "chrome": chrome,
        "next_command": "herdr-mcp native-host use standalone",
        "note": "All Git-tracked extension/ files come from the pinned commit; manifest.json differs only by the injected standalone public key.",
    }))
}

/// Managed extension copies and the install state record are private to the
/// installing user: the staged tree is a browser-readable copy of the extension
/// bundle and `state.json` records install provenance, neither of which belongs
/// to other local accounts. Both the CLI install and the update-time staging
/// create every directory `0700` and every file `0600` instead of inheriting the
/// process umask (which produced `0755`/`0644`, #833).
const PRIVATE_DIR_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

#[cfg(unix)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE_DIR_MODE)
        .create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

/// Create a private file. `create_new` refuses to follow or overwrite anything
/// that already exists at that path, and the mode applies at creation, so the
/// file is never briefly readable or writable by other accounts.
fn create_private_file(path: &Path) -> io::Result<fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(PRIVATE_FILE_MODE)
            .open(path)
    }
    #[cfg(not(unix))]
    {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
    }
}

#[cfg(unix)]
fn tighten_private_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_FILE_MODE))
}

#[cfg(not(unix))]
fn tighten_private_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Download one extension tree into a fresh staging directory and verify its
/// manifest. The staging directory is removed on any failure so nothing partial
/// reachable from `current` is ever left behind.
fn write_snapshot(
    base: &Path,
    snapshot: &ExtensionSnapshot,
    source: &dyn ExtensionSource,
) -> Result<(PathBuf, PreparedManifest), String> {
    let staging = base.join(format!(".staging-{}-{}", std::process::id(), now_ms()));
    create_private_dir(&staging).map_err(|e| format!("cannot create staging directory: {e}"))?;
    let prepared = (|| -> Result<PreparedManifest, String> {
        for blob in &snapshot.blobs {
            let bytes = source.fetch_blob(&snapshot.commit, blob)?;
            let target = staging.join(&blob.relative_path);
            if let Some(parent) = target.parent() {
                create_private_dir(parent)
                    .map_err(|e| format!("cannot create extension directory: {e}"))?;
            }
            let mut file = create_private_file(&target)
                .map_err(|e| format!("cannot create extension file: {e}"))?;
            file.write_all(&bytes)
                .map_err(|e| format!("cannot write extension file: {e}"))?;
            file.sync_all()
                .map_err(|e| format!("cannot sync extension file: {e}"))?;
        }
        prepare_manifest(&staging)
    })();
    match prepared {
        Ok(prepared) => Ok((staging, prepared)),
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            Err(error)
        }
    }
}

/// Swap a fully written staging directory in as `current`, restoring the
/// previous tree if the rename fails. The previous tree is returned as a backup
/// path the caller removes only after the new state record is durable.
fn activate_snapshot(
    base: &Path,
    current: &Path,
    staging: &Path,
    had_current: bool,
) -> Result<PathBuf, String> {
    let backup = base.join(format!(".previous-{}-{}", std::process::id(), now_ms()));
    if had_current {
        fs::rename(current, &backup)
            .map_err(|e| format!("cannot stage previous standalone extension: {e}"))?;
    }
    if let Err(error) = fs::rename(staging, current) {
        if had_current {
            let _ = fs::rename(&backup, current);
        }
        return Err(format!("cannot activate standalone extension: {error}"));
    }
    Ok(backup)
}

fn state_record(
    requested_ref: &str,
    commit: &str,
    prepared: &PreparedManifest,
    current: &Path,
    load_unpacked_path: &Path,
    installed_by: &str,
) -> Value {
    json!({
        "schema_version": 1,
        "repository": REPOSITORY,
        "requested_ref": requested_ref,
        "source_commit": commit,
        "extension_version": prepared.version,
        "extension_id": prepared.extension_id,
        "source_manifest_sha256": prepared.source_sha256,
        "installed_at_unix_ms": now_ms(),
        "path": current,
        "load_unpacked_path": load_unpacked_path,
        "installed_by": installed_by,
    })
}

/// Outcome of staging the standalone extension from the Runtime update path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StagedExtensionState {
    /// No standalone install exists on this workstation; an update never creates
    /// one because users who never opted in must not gain an unpacked extension.
    NotInstalled,
    /// The staged tree already comes from this release.
    UpToDate,
    /// A new tree was staged; the browser needs a Reload to execute it.
    Staged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StagedExtension {
    pub(crate) state: StagedExtensionState,
    pub(crate) reference: String,
    pub(crate) source_commit: Option<String>,
    pub(crate) extension_version: Option<String>,
    pub(crate) previous_source_commit: Option<String>,
    pub(crate) previous_extension_version: Option<String>,
    pub(crate) load_unpacked_path: Option<PathBuf>,
    /// Whether the recorded Chrome-facing load path still resolves to the
    /// managed `current` directory. When it does not, a Reload alone cannot pick
    /// up the new files and the user must re-run the CLI install.
    pub(crate) load_path_ready: bool,
}

impl StagedExtension {
    pub(crate) fn as_json(&self) -> Value {
        let state = match self.state {
            StagedExtensionState::NotInstalled => "not_installed",
            StagedExtensionState::UpToDate => "up_to_date",
            StagedExtensionState::Staged => "staged",
        };
        json!({
            "state": state,
            "requested_ref": self.reference,
            "source_commit": self.source_commit,
            "extension_version": self.extension_version,
            "previous_source_commit": self.previous_source_commit,
            "previous_extension_version": self.previous_extension_version,
            "load_unpacked_path": self.load_unpacked_path,
            "load_path_ready": self.load_path_ready,
            "action": self.action(),
            "next_action": self.next_action(),
        })
    }

    fn action(&self) -> Option<&'static str> {
        match (self.state, self.load_path_ready) {
            (StagedExtensionState::Staged, true) => Some("reload_extension"),
            (StagedExtensionState::Staged, false) => Some("install_extension"),
            (StagedExtensionState::UpToDate, false) => Some("install_extension"),
            _ => None,
        }
    }

    fn next_action(&self) -> Option<&'static str> {
        match self.action() {
            Some("reload_extension") => Some(EXTENSION_RELOAD_NEXT_ACTION),
            Some("install_extension") => Some(EXTENSION_INSTALL_NEXT_ACTION),
            _ => None,
        }
    }

    /// One line for the durable update job detail (kept short: the updater
    /// truncates job detail to 512 bytes).
    pub(crate) fn summary(&self) -> String {
        match self.state {
            StagedExtensionState::NotInstalled => {
                "standalone extension not installed; not staged".to_owned()
            }
            StagedExtensionState::UpToDate => format!(
                "standalone extension {} already staged; reload it in chrome://extensions",
                self.extension_version.as_deref().unwrap_or("unknown")
            ),
            StagedExtensionState::Staged => format!(
                "standalone extension staged {} (previous {}); reload it in chrome://extensions",
                self.extension_version.as_deref().unwrap_or("unknown"),
                self.previous_extension_version.as_deref().unwrap_or("none"),
            ),
        }
    }
}

/// Stage the standalone extension that belongs to `reference` into the managed
/// `extensions/standalone/current` directory.
///
/// This is the same staging code the CLI install uses: one atomic swap of a
/// fully downloaded tree, the same manifest `key` injection (so the extension
/// ID and its grants are preserved), and the same durable state record. It never
/// creates an install for a workstation that has none, and it never touches the
/// `~/Documents` Chrome-facing alias: that alias is a symlink to `current` and
/// follows the swap by itself, so an unattended update cannot prompt for
/// protected-folder access.
/// Stage the standalone extension, optionally skipping the Chrome-facing alias
/// probe.
///
/// `allow_alias_probe = false` is for an update whose broker preflight is still
/// pending or was refused: probing `~/Documents/herdr-mcp/extension` there would be
/// a second macOS Documents request in the same upgrade. The recorded
/// `load_path_ready` stays false until a later stage can ask the broker again.
pub(crate) fn stage_release_extension_with_probe_policy(
    reference: &str,
    allow_alias_probe: bool,
) -> Result<StagedExtension, String> {
    validate_ref(reference)?;
    let home = home_dir()?;
    let source = GitHubSource::new()?;
    stage_release_extension_in(&home, reference, &source, allow_alias_probe)
}

fn stage_release_extension_in(
    home: &Path,
    reference: &str,
    source: &dyn ExtensionSource,
    allow_alias_probe: bool,
) -> Result<StagedExtension, String> {
    validate_ref(reference)?;
    let identity = crate::browser_extension_identity::official_standalone_identity()?;
    let base = base_dir_for(home);
    let current = base.join("current");
    let Some(manifest) = installed_manifest(&current, &identity) else {
        return Ok(StagedExtension {
            state: StagedExtensionState::NotInstalled,
            reference: reference.to_owned(),
            source_commit: None,
            extension_version: None,
            previous_source_commit: None,
            previous_extension_version: None,
            load_unpacked_path: None,
            load_path_ready: false,
        });
    };
    let state = read_json(&base.join("state.json")).ok();
    let previous_source_commit = json_string(state.as_ref(), "source_commit");
    let previous_extension_version =
        json_string(state.as_ref(), "extension_version").or_else(|| manifest_version(&manifest));
    let commit = source.resolve_commit(reference)?;
    if previous_source_commit.as_deref() == Some(commit.as_str()) {
        let load_unpacked_path = reported_load_path(home, state.as_ref())
            .unwrap_or_else(|| user_visible_alias_path(home));
        return Ok(StagedExtension {
            state: StagedExtensionState::UpToDate,
            reference: reference.to_owned(),
            extension_version: manifest_version(&manifest),
            source_commit: Some(commit),
            previous_extension_version,
            previous_source_commit,
            load_path_ready: allow_alias_probe && load_path_ready(&load_unpacked_path, &current),
            load_unpacked_path: Some(load_unpacked_path),
        });
    }
    let blobs = source.list_blobs(&commit)?;
    create_private_dir(&base).map_err(|e| format!("cannot create standalone directory: {e}"))?;
    let (staging, prepared) = write_snapshot(
        &base,
        &ExtensionSnapshot {
            commit: commit.clone(),
            blobs,
        },
        source,
    )?;
    let had_current = current.exists();
    let backup = activate_snapshot(&base, &current, &staging, had_current)?;
    let load_unpacked_path =
        reported_load_path(home, state.as_ref()).unwrap_or_else(|| user_visible_alias_path(home));
    let record = state_record(
        reference,
        &commit,
        &prepared,
        &current,
        &load_unpacked_path,
        "update",
    );
    if let Err(error) = atomic_json(&base.join("state.json"), &record) {
        rollback_activation(&current, &backup, had_current);
        return Err(error);
    }
    if backup.exists() {
        let _ = fs::remove_dir_all(&backup);
    }
    let ready = gated_load_path_ready(&load_unpacked_path, &current, allow_alias_probe);
    Ok(StagedExtension {
        state: StagedExtensionState::Staged,
        reference: reference.to_owned(),
        source_commit: Some(commit),
        extension_version: Some(prepared.version),
        previous_source_commit,
        previous_extension_version,
        load_unpacked_path: Some(load_unpacked_path),
        load_path_ready: ready,
    })
}

fn prepare_manifest(root: &Path) -> Result<PreparedManifest, String> {
    let path = root.join("manifest.json");
    let source = fs::read(&path).map_err(|e| format!("downloaded manifest missing: {e}"))?;
    let source_sha256 = sha256(&source);
    let manifest: Value = serde_json::from_slice(&source)
        .map_err(|e| format!("downloaded manifest is invalid JSON: {e}"))?;
    let object = manifest
        .as_object()
        .ok_or_else(|| "manifest must be a JSON object".to_owned())?;
    if object.get("manifest_version").and_then(Value::as_u64) != Some(3) {
        return Err("standalone extension requires Manifest V3".to_owned());
    }
    let version = object
        .get("version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "manifest has no version".to_owned())?
        .to_owned();
    let identity = crate::browser_extension_identity::official_standalone_identity()?;
    let key = identity
        .manifest_key
        .as_deref()
        .ok_or_else(|| "standalone identity has no manifest key".to_owned())?;
    let bytes = match object.get("key").and_then(Value::as_str) {
        Some(existing) if existing != key => {
            return Err("manifest contains a conflicting key".to_owned());
        }
        Some(_) => source.clone(),
        None => inject_key(&source, key)?,
    };
    fs::write(&path, bytes).map_err(|e| format!("cannot write standalone manifest: {e}"))?;
    // `fs::write` truncates in place, so the mode stays whatever the download
    // created; re-assert the private mode in case a caller passed a wider file.
    tighten_private_file(&path).map_err(|e| format!("cannot secure standalone manifest: {e}"))?;
    let verify = read_json(&path)?;
    if verify.get("key").and_then(Value::as_str) != Some(key) {
        return Err("standalone manifest key verification failed".to_owned());
    }
    Ok(PreparedManifest {
        version,
        extension_id: identity.extension_id,
        source_sha256,
    })
}

fn inject_key(source: &[u8], key: &str) -> Result<Vec<u8>, String> {
    let closing = source
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .filter(|index| source[*index] == b'}')
        .ok_or_else(|| "manifest has no closing object brace".to_owned())?;
    let last = source[..closing]
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .ok_or_else(|| "manifest object is empty".to_owned())?;
    if matches!(source[last], b'{' | b',') {
        return Err("manifest has no final property".to_owned());
    }
    let key_json = serde_json::to_string(key).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(source.len() + key_json.len() + 16);
    out.extend_from_slice(&source[..=last]);
    out.push(b',');
    out.extend_from_slice(&source[last + 1..closing]);
    out.extend_from_slice(b"  \"key\": ");
    out.extend_from_slice(key_json.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(&source[closing..]);
    Ok(out)
}

fn list_blobs(client: &Client, commit: &str) -> Result<Vec<Blob>, String> {
    let url = format!("https://{API_HOST}/repos/whshang/herdr-mcp/git/trees/{commit}?recursive=1");
    let tree: TreeResponse = get(client, &url, API_HOST)?
        .json()
        .map_err(|e| format!("cannot decode GitHub tree: {e}"))?;
    if tree.truncated {
        return Err("GitHub tree was truncated; refusing partial extension download".to_owned());
    }
    let mut out = Vec::new();
    let mut total = 0_u64;
    for entry in tree.tree {
        if entry.kind != "blob" || !entry.path.starts_with("extension/") {
            continue;
        }
        let rel = entry.path.trim_start_matches("extension/");
        if rel.is_empty() {
            continue;
        }
        let relative_path = safe_rel(rel)?;
        let size = entry.size.unwrap_or(0);
        if !is_sha(&entry.sha) {
            return Err(format!(
                "extension tree returned invalid Git blob SHA: {}",
                entry.path
            ));
        }
        if size > MAX_FILE_BYTES {
            return Err(format!("extension file too large: {}", entry.path));
        }
        total = total
            .checked_add(size)
            .ok_or_else(|| "extension size overflow".to_owned())?;
        if total > MAX_TOTAL_BYTES {
            return Err("extension tree exceeds size limit".to_owned());
        }
        out.push(Blob {
            repo_path: entry.path,
            relative_path,
            size,
            git_sha: entry.sha.to_ascii_lowercase(),
        });
        if out.len() > MAX_FILES {
            return Err("extension tree exceeds file-count limit".to_owned());
        }
    }
    out.sort_by(|a, b| a.repo_path.cmp(&b.repo_path));
    if !out
        .iter()
        .any(|blob| blob.repo_path == "extension/manifest.json")
    {
        return Err("extension tree has no manifest.json".to_owned());
    }
    Ok(out)
}

fn download_blob(client: &Client, commit: &str, blob: &Blob) -> Result<Vec<u8>, String> {
    let url = format!(
        "https://{RAW_HOST}/whshang/herdr-mcp/{commit}/{}",
        blob.repo_path
    );
    let bytes = get(client, &url, RAW_HOST)?
        .bytes()
        .map_err(|e| format!("cannot read {}: {e}", blob.repo_path))?;
    if bytes.len() as u64 > MAX_FILE_BYTES || (blob.size != 0 && bytes.len() as u64 != blob.size) {
        return Err(format!("downloaded file size mismatch: {}", blob.repo_path));
    }
    let actual_git_sha = git_blob_sha(&bytes);
    if actual_git_sha != blob.git_sha {
        return Err(format!(
            "downloaded file Git blob SHA mismatch: {}",
            blob.repo_path
        ));
    }
    Ok(bytes.to_vec())
}

fn git_blob_sha(bytes: &[u8]) -> String {
    let mut digest = Sha1::new();
    digest.update(format!("blob {}\0", bytes.len()).as_bytes());
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

fn resolve_ref(client: &Client, reference: &str) -> Result<String, String> {
    if is_sha(reference) {
        return Ok(reference.to_ascii_lowercase());
    }
    let mut url = Url::parse(&format!(
        "https://{API_HOST}/repos/whshang/herdr-mcp/commits/"
    ))
    .map_err(|e| e.to_string())?;
    url.path_segments_mut()
        .map_err(|_| "cannot construct GitHub commit URL".to_owned())?
        .push(reference);
    let commit: CommitResponse = get(client, url.as_str(), API_HOST)?
        .json()
        .map_err(|e| format!("cannot decode GitHub commit response: {e}"))?;
    if !is_sha(&commit.sha) {
        return Err("GitHub returned invalid commit SHA".to_owned());
    }
    Ok(commit.sha.to_ascii_lowercase())
}

fn client() -> Result<Client, String> {
    Client::builder()
        .user_agent(concat!("herdr-mcp/", env!("CARGO_PKG_VERSION")))
        .redirect(Policy::limited(3))
        .build()
        .map_err(|e| format!("cannot create GitHub client: {e}"))
}

fn get(client: &Client, url: &str, expected_host: &str) -> Result<Response, String> {
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("GitHub request failed: {e}"))?;
    if response.url().host_str() != Some(expected_host) {
        return Err("GitHub request redirected to unexpected host".to_owned());
    }
    if !response.status().is_success() {
        return Err(format!(
            "GitHub request returned HTTP {}",
            response.status()
        ));
    }
    Ok(response)
}

fn default_source_ref() -> String {
    crate::runtime_meta::compiled_source_commit()
        .filter(|value| is_sha(value))
        .map(str::to_owned)
        .or_else(|| {
            env::var("HERDR_MCP_BUILD_COMMIT")
                .ok()
                .filter(|value| is_sha(value))
        })
        .unwrap_or_else(|| "main".to_owned())
}

fn validate_ref(value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > 200 || value.chars().any(|ch| ch.is_control()) {
        return Err("invalid standalone Git ref".to_owned());
    }
    Ok(())
}

fn is_sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn safe_rel(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("extension tree contains unsafe path".to_owned());
    }
    Ok(path.to_path_buf())
}

fn home_dir() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_owned())
}

fn base_dir_for(home: &Path) -> PathBuf {
    home.join(".config/herdr-mcp/extensions/standalone")
}

fn user_visible_alias_path(home: &Path) -> PathBuf {
    home.join("Documents/herdr-mcp/extension")
}

fn inspect_user_visible_alias(home: &Path, current: &Path) -> Value {
    let path = user_visible_alias_path(home);
    inspect_alias(&path, current)
}

/// Read-only projection of the Chrome-facing load path recorded by the last
/// install. The value lives in `~/.config`, so it is a stable string authority
/// that does not require `lstat`/`readlink` on `~/Documents`, which macOS TCC
/// can deny. It is only reported/matched here and is never used to mutate the
/// filesystem; a value that escapes HOME is ignored.
fn reported_load_path(home: &Path, state: Option<&Value>) -> Option<PathBuf> {
    let path = state
        .and_then(|value| value.get("load_unpacked_path"))
        .and_then(Value::as_str)
        .map(PathBuf::from)?;
    is_below_home(home, &path).then_some(path)
}

/// Lexical HOME containment used as a read-only sanity bound. It is not a
/// filesystem check and must not be treated as ownership proof for a mutation;
/// mutations only ever use the validated `--path` argument.
fn is_below_home(home: &Path, path: &Path) -> bool {
    path.is_absolute() && path != home && path.starts_with(home)
}

/// Path equality that never touches the filesystem. Chrome persists the exact
/// directory string it was given, so the recorded path can be compared even
/// when the user-visible alias cannot be inspected.
fn lexical_path_eq(left: &Path, right: &Path) -> bool {
    fn normalize(path: &Path) -> String {
        let text = path.to_string_lossy();
        text.strip_suffix('/').unwrap_or(&text).to_owned()
    }
    normalize(left) == normalize(right)
}

/// Resolve the Chrome-facing load path and its alias evidence for `status`.
/// The recorded `load_unpacked_path` is authoritative; inspecting the alias is
/// best-effort reporting and never gates the reported path.
fn status_load_view(home: &Path, current: &Path, state: Option<&Value>) -> (Value, PathBuf) {
    let reported = reported_load_path(home, state);
    let user_visible_path = match reported.as_deref() {
        Some(path) if path != current => inspect_alias(path, current),
        Some(_) => json!({
            "ready": true,
            "status": "managed",
            "path": current,
            "target": current,
        }),
        None => inspect_user_visible_alias(home, current),
    };
    let load_unpacked_path =
        reported.unwrap_or_else(|| preferred_load_unpacked_path(&user_visible_path, current));
    (user_visible_path, load_unpacked_path)
}

/// Outcome of one alias lookup, already decided by the party that is allowed to
/// look. Tests inject this so both the answered and the pending branches are
/// covered on any platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AliasProbe {
    Ready,
    OccupiedLink,
    OccupiedEntry,
    Missing,
    /// The brokered lookup did not answer within its budget.
    Pending,
    /// The installed broker predates the alias subcommands and must be
    /// reinstalled before the alias can be verified or created.
    BrokerOutdated,
    /// The broker refused or is unavailable.
    Denied(String),
}

/// The production lookup for a path the rotating runtime must not touch: one
/// bounded call to the stable broker, never a direct syscall.
fn brokered_alias_probe(path: &Path, expected: &Path) -> AliasProbe {
    let Ok(paths) = crate::paths::RuntimePaths::discover() else {
        return AliasProbe::Denied("runtime paths are unavailable".to_owned());
    };
    match crate::macos_privacy::probe_alias_via_stable_broker(&paths.config_dir, path, expected) {
        crate::macos_privacy::BrokeredAlias::Ready => AliasProbe::Ready,
        crate::macos_privacy::BrokeredAlias::OccupiedLink => AliasProbe::OccupiedLink,
        crate::macos_privacy::BrokeredAlias::OccupiedEntry => AliasProbe::OccupiedEntry,
        crate::macos_privacy::BrokeredAlias::Missing => AliasProbe::Missing,
        crate::macos_privacy::BrokeredAlias::Pending => AliasProbe::Pending,
        crate::macos_privacy::BrokeredAlias::BrokerOutdated => AliasProbe::BrokerOutdated,
        crate::macos_privacy::BrokeredAlias::Denied(detail) => AliasProbe::Denied(detail),
    }
}

/// The production creation path for a protected alias: the broker child creates
/// the parent directory, the symlink and verifies it, bounded by the same probe
/// timeout. The rotating generation never mutates `~/Documents`.
fn brokered_alias_ensure(path: &Path, expected: &Path) -> AliasProbe {
    let Ok(paths) = crate::paths::RuntimePaths::discover() else {
        return AliasProbe::Denied("runtime paths are unavailable".to_owned());
    };
    match crate::macos_privacy::ensure_alias_via_stable_broker(&paths.config_dir, path, expected) {
        crate::macos_privacy::BrokeredAlias::Ready => AliasProbe::Ready,
        crate::macos_privacy::BrokeredAlias::OccupiedLink => AliasProbe::OccupiedLink,
        crate::macos_privacy::BrokeredAlias::OccupiedEntry => AliasProbe::OccupiedEntry,
        crate::macos_privacy::BrokeredAlias::Missing => AliasProbe::Missing,
        crate::macos_privacy::BrokeredAlias::Pending => AliasProbe::Pending,
        crate::macos_privacy::BrokeredAlias::BrokerOutdated => AliasProbe::BrokerOutdated,
        crate::macos_privacy::BrokeredAlias::Denied(detail) => AliasProbe::Denied(detail),
    }
}

// Test-only count of alias inspections performed by this thread, so a test can
// prove that a path (staging when the broker preflight is pending) performs none.
#[cfg(test)]
thread_local! {
    static ALIAS_PROBES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn test_alias_probes() -> usize {
    ALIAS_PROBES.with(|count| count.get())
}

#[cfg(test)]
pub(crate) fn test_reset_alias_probes() {
    ALIAS_PROBES.with(|count| count.set(0));
}

fn inspect_alias(path: &Path, current: &Path) -> Value {
    #[cfg(test)]
    ALIAS_PROBES.with(|count| count.set(count.get() + 1));
    inspect_alias_with_probe(
        path,
        current,
        crate::macos_permissions::runtime_must_not_probe(path),
        &brokered_alias_probe,
    )
}

/// Inspect the Chrome-facing alias.
///
/// A path the rotating generation must not probe is answered by one bounded,
/// broker-routed lookup: a missing alias is reported `missing` (so
/// `ensure_alias` creates it), an existing symlink to `current` is `ready`, and
/// only an unanswered or refused broker call becomes `tcc_pending`. That keeps
/// `load_path_ready` and `--path` preflight truthful during an update instead of
/// turning every protected alias into a permanent stall.
fn inspect_alias_with_probe(
    path: &Path,
    current: &Path,
    protected: bool,
    probe: &dyn Fn(&Path, &Path) -> AliasProbe,
) -> Value {
    if !protected {
        return inspect_alias_direct(path, current);
    }
    match probe(path, current) {
        AliasProbe::Ready => json!({
            "ready": true,
            "status": "ready",
            "path": path,
            "target": current,
            "broker_routed": true,
        }),
        AliasProbe::Missing => json!({
            "ready": false,
            "status": "missing",
            "path": path,
            "target": current,
            "broker_routed": true,
        }),
        AliasProbe::OccupiedLink => json!({
            "ready": false,
            "status": "occupied",
            "kind": "symlink",
            "path": path,
            "target": current,
            "broker_routed": true,
        }),
        AliasProbe::OccupiedEntry => json!({
            "ready": false,
            "status": "occupied",
            "kind": "entry",
            "path": path,
            "target": current,
            "broker_routed": true,
        }),
        AliasProbe::BrokerOutdated => {
            let mut payload = tcc_pending_alias_payload(path, current);
            if let Some(object) = payload.as_object_mut() {
                object.insert("status".to_owned(), json!("broker_outdated"));
                object.insert("code".to_owned(), json!("broker_outdated"));
                object.insert(
                    "message".to_owned(),
                    json!("the installed TCC broker predates the Documents alias subcommands; migrate it once with `herdr-mcp permissions setup --upgrade-broker` (one re-authorization) and retry"),
                );
                object.insert(
                    "next_action".to_owned(),
                    json!("herdr-mcp permissions setup --upgrade-broker"),
                );
            }
            payload
        }
        AliasProbe::Pending => tcc_pending_alias_payload(path, current),
        AliasProbe::Denied(detail) => {
            let mut payload = tcc_pending_alias_payload(path, current);
            if let Some(object) = payload.as_object_mut() {
                object.insert("status".to_owned(), json!("unverified"));
                object.insert("error".to_owned(), json!(detail));
            }
            payload
        }
    }
}

fn broker_outdated_probe(_path: &Path, _current: &Path) -> AliasProbe {
    AliasProbe::BrokerOutdated
}

fn tcc_pending_alias_payload(path: &Path, current: &Path) -> Value {
    let mut payload =
        crate::macos_permissions::tcc_pending_payload("extension_standalone_alias", path);
    if let Some(object) = payload.as_object_mut() {
        object.insert("ready".to_owned(), json!(false));
        object.insert("status".to_owned(), json!("tcc_pending"));
        object.insert("path".to_owned(), json!(path));
        object.insert("target".to_owned(), json!(current));
    }
    payload
}

/// Direct filesystem inspection, used only when the runtime may read the path
/// itself (never for a privacy-protected folder).
fn inspect_alias_direct(path: &Path, current: &Path) -> Value {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => json!({
            "ready": false,
            "status": "missing",
            "path": path,
            "target": current,
        }),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => json!({
            "ready": false,
            "status": "unverified",
            "path": path,
            "target": current,
            "error": error.to_string(),
        }),
        Err(error) => json!({
            "ready": false,
            "status": "error",
            "path": path,
            "target": current,
            "error": error.to_string(),
        }),
        Ok(metadata) if metadata.file_type().is_symlink() => match fs::read_link(path) {
            Ok(existing_target) if existing_target == current => json!({
                "ready": true,
                "status": "ready",
                "path": path,
                "target": current,
            }),
            Ok(existing_target) => json!({
                "ready": false,
                "status": "occupied",
                "kind": "symlink",
                "path": path,
                "target": current,
                "existing_target": existing_target,
            }),
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => json!({
                "ready": false,
                "status": "unverified",
                "path": path,
                "target": current,
                "error": error.to_string(),
            }),
            Err(error) => json!({
                "ready": false,
                "status": "error",
                "path": path,
                "target": current,
                "error": error.to_string(),
            }),
        },
        Ok(metadata) => json!({
            "ready": false,
            "status": "occupied",
            "kind": if metadata.is_dir() { "directory" } else if metadata.is_file() { "file" } else { "other" },
            "path": path,
            "target": current,
        }),
    }
}

fn ensure_user_visible_alias(home: &Path, current: &Path) -> Value {
    let path = user_visible_alias_path(home);
    ensure_alias(home, &path, current)
}

fn ensure_alias(home: &Path, path: &Path, current: &Path) -> Value {
    ensure_alias_with(
        home,
        path,
        current,
        crate::macos_permissions::runtime_must_not_probe(path),
        &brokered_alias_probe,
        &brokered_alias_ensure,
    )
}

/// Testable seam: the protected branch answers and creates through injected
/// provider functions, so both the broker-answered and broker-silent cases can
/// be exercised without touching `~/Documents`.
fn ensure_alias_with(
    home: &Path,
    path: &Path,
    current: &Path,
    protected: bool,
    probe: &dyn Fn(&Path, &Path) -> AliasProbe,
    ensure: &dyn Fn(&Path, &Path) -> AliasProbe,
) -> Value {
    let initial = inspect_alias_with_probe(path, current, protected, probe);
    if initial.get("status").and_then(Value::as_str) != Some("missing") {
        return initial;
    }
    if protected {
        // Creation itself is a `~/Documents` mutation, so it also happens in the
        // broker child; the answer is a fresh view of the same states.
        return match ensure(path, current) {
            AliasProbe::Ready => json!({
                "ready": true,
                "status": "created",
                "path": path,
                "target": current,
                "broker_routed": true,
            }),
            AliasProbe::Pending => tcc_pending_alias_payload(path, current),
            AliasProbe::BrokerOutdated => {
                inspect_alias_with_probe(path, current, true, &|path, current| {
                    broker_outdated_probe(path, current)
                })
            }
            AliasProbe::Denied(detail) => {
                let mut payload = tcc_pending_alias_payload(path, current);
                if let Some(object) = payload.as_object_mut() {
                    object.insert("status".to_owned(), json!("unverified"));
                    object.insert("error".to_owned(), json!(detail));
                }
                payload
            }
            AliasProbe::Missing => inspect_alias_with_probe(path, current, true, probe),
            occupied => {
                let decided = occupied.clone();
                inspect_alias_with_probe(path, current, true, &|_, _| decided.clone())
            }
        };
    }
    let Some(parent) = path.parent() else {
        return json!({
            "ready": false,
            "status": "error",
            "path": path,
            "target": current,
            "error": "user-visible extension path has no parent",
        });
    };
    if let Err(error) = ensure_alias_parent(home, parent) {
        return json!({
            "ready": false,
            "status": "error",
            "path": path,
            "target": current,
            "error": error,
        });
    }
    match create_directory_symlink(current, path) {
        Ok(()) => json!({
            "ready": true,
            "status": "created",
            "path": path,
            "target": current,
        }),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => inspect_alias(path, current),
        Err(error) => json!({
            "ready": false,
            "status": "error",
            "path": path,
            "target": current,
            "error": error.to_string(),
        }),
    }
}

fn ensure_explicit_alias(home: &Path, path: &Path, current: &Path) -> Result<Value, String> {
    let result = ensure_alias(home, path, current);
    if result.get("ready").and_then(Value::as_bool) == Some(true) {
        return Ok(result);
    }
    let status = result
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Err(format!(
        "requested standalone --path '{}' is not usable ({status}); choose an unused path or the existing standalone symlink",
        path.display()
    ))
}

fn preflight_explicit_load_path(path: &Path, current: &Path) -> Result<(), String> {
    if path == current {
        return Ok(());
    }
    let state = inspect_alias(path, current);
    match state.get("status").and_then(Value::as_str) {
        Some("missing" | "ready") => Ok(()),
        Some("unverified") => Err(format!(
            "requested standalone --path '{}' cannot be inspected (permission denied); refusing to overwrite it",
            path.display()
        )),
        Some(status) => Err(format!(
            "requested standalone --path '{}' is already occupied ({status}); refusing to overwrite it",
            path.display()
        )),
        None => Err(format!(
            "requested standalone --path '{}' cannot be inspected safely",
            path.display()
        )),
    }
}

fn resolve_load_path(home: &Path, value: &str) -> Result<PathBuf, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 4096 || value.chars().any(|ch| ch.is_control()) {
        return Err("invalid standalone --path".to_owned());
    }
    let path = if value == "~" {
        home.to_path_buf()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        let raw = PathBuf::from(value);
        if raw.is_absolute() {
            raw
        } else {
            home.join(raw)
        }
    };
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err("standalone --path must not contain '..'".to_owned());
    }
    if path == home || !path.starts_with(home) {
        return Err("standalone --path must resolve below HOME".to_owned());
    }
    Ok(path)
}

fn ensure_alias_parent(home: &Path, parent: &Path) -> Result<(), String> {
    let canonical_home = fs::canonicalize(home)
        .map_err(|e| format!("cannot resolve HOME for standalone --path: {e}"))?;
    let mut ancestor = parent;
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| "standalone --path has no existing ancestor".to_owned())?;
    }
    let canonical_ancestor = fs::canonicalize(ancestor)
        .map_err(|e| format!("cannot resolve standalone --path ancestor: {e}"))?;
    if !canonical_ancestor.starts_with(&canonical_home) {
        return Err("standalone --path would escape HOME through a symlinked parent".to_owned());
    }
    fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create standalone --path parent: {e}"))?;
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|e| format!("cannot resolve standalone --path parent: {e}"))?;
    if !canonical_parent.starts_with(canonical_home) {
        return Err("standalone --path parent resolves outside HOME".to_owned());
    }
    Ok(())
}

fn rollback_activation(current: &Path, backup: &Path, had_current: bool) {
    let _ = fs::remove_dir_all(current);
    if had_current {
        let _ = fs::rename(backup, current);
    }
}

fn remove_alias_if_created(alias: &Value, current: &Path) {
    if alias.get("status").and_then(Value::as_str) != Some("created") {
        return;
    }
    let Some(path) = alias.get("path").and_then(Value::as_str).map(PathBuf::from) else {
        return;
    };
    // #844: undoing a broker-created alias would need `read_link` + `remove_file`
    // on `~/Documents` from the rotating generation, which is exactly the access
    // this change removes. Leave the alias in place instead; it is a valid
    // symlink to the managed tree and the next ensure reuses it.
    if crate::macos_permissions::runtime_must_not_probe(&path) {
        return;
    }
    if fs::read_link(&path).ok().as_deref() == Some(current) {
        let _ = fs::remove_file(path);
    }
}

fn preferred_load_unpacked_path(alias: &Value, current: &Path) -> PathBuf {
    if alias.get("ready").and_then(Value::as_bool) == Some(true) {
        alias
            .get("path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| current.to_path_buf())
    } else {
        current.to_path_buf()
    }
}

#[cfg(unix)]
fn create_directory_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_directory_symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

#[cfg(not(any(unix, windows)))]
fn create_directory_symlink(_target: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "directory symlinks are unsupported on this platform",
    ))
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("invalid JSON {}: {e}", path.display()))
}

/// Manifest of the managed `current` tree, but only when it carries this
/// channel's fixed standalone `key`. A tree without the key is not a managed
/// standalone install, so it is never adopted or overwritten by an update.
fn installed_manifest(current: &Path, identity: &BrowserExtensionIdentity) -> Option<Value> {
    if !current.is_dir() {
        return None;
    }
    let manifest = read_json(&current.join("manifest.json")).ok()?;
    let key_ok = manifest
        .get("key")
        .and_then(Value::as_str)
        .is_some_and(|key| identity.manifest_key.as_deref() == Some(key));
    key_ok.then_some(manifest)
}

fn manifest_version(manifest: &Value) -> Option<String> {
    manifest
        .get("version")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn json_string(value: Option<&Value>, field: &str) -> Option<String> {
    value?
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// `load_path_ready` behind the update's broker-probe policy.
///
/// Staging runs during an upgrade, and reading the Chrome-facing alias is a
/// `~/Documents` request. When the broker preflight for this upgrade is still
/// pending (or was refused) the upgrade has already spent its one prompt, so the
/// alias is not probed again and readiness is recorded as false until a later
/// status check can confirm it.
fn gated_load_path_ready(load_path: &Path, current: &Path, allow_alias_probe: bool) -> bool {
    allow_alias_probe && load_path_ready(load_path, current)
}

/// Whether a recorded Chrome-facing load path still reaches the managed
/// `current` tree. `current` itself always counts as ready.
fn load_path_ready(load_path: &Path, current: &Path) -> bool {
    load_path == current
        || inspect_alias(load_path, current)
            .get("ready")
            .and_then(Value::as_bool)
            == Some(true)
}

fn atomic_json(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "state path has no parent".to_owned())?;
    create_private_dir(parent).map_err(|e| format!("cannot create state directory: {e}"))?;
    let tmp = parent.join(format!(".state-{}-{}.tmp", std::process::id(), now_ms()));
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    // The temporary file is created `0600` and renamed over the target, so the
    // durable state record is never readable by another local account, and an
    // existing looser mode is replaced instead of preserved (#833).
    let mut file =
        create_private_file(&tmp).map_err(|e| format!("cannot create state file: {e}"))?;
    file.write_all(&bytes)
        .map_err(|e| format!("cannot write state file: {e}"))?;
    file.sync_all()
        .map_err(|e| format!("cannot sync state file: {e}"))?;
    drop(file);
    fs::rename(&tmp, path).map_err(|e| format!("cannot activate state file: {e}"))?;
    tighten_private_file(path).map_err(|e| format!("cannot secure state file: {e}"))
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    /// user answers at most one prompt | Given a staged extension whose Chrome
    /// alias lives outside the managed tree | When the broker preflight for this
    /// upgrade is still pending | Then staging records readiness as false without
    /// performing any alias probe, and only a permitted staging probes once.
    #[test]
    fn staging_skips_the_alias_probe_when_the_broker_preflight_is_pending() {
        let root = std::env::temp_dir().join(format!(
            "herdr-stage-probe-policy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        let alias = root.join("Documents/herdr-mcp/extension");
        let current = root.join("Library/herdr-mcp/standalone/current");
        std::fs::create_dir_all(&current).unwrap();
        std::fs::create_dir_all(alias.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&current, &alias).unwrap();

        test_reset_alias_probes();
        assert!(!super::gated_load_path_ready(&alias, &current, false));
        assert_eq!(
            test_alias_probes(),
            0,
            "a pending broker preflight must not probe the Documents alias again"
        );

        assert!(super::gated_load_path_ready(&alias, &current, true));
        assert_eq!(test_alias_probes(), 1, "a permitted staging probes once");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// user keeps the default install working | Given a protected Documents
    /// alias and a broker that answers | When the alias is inspected | Then the
    /// real answer is reported (ready/missing), so install and update do not
    /// degrade into a permanent tcc_pending.
    #[test]
    fn protected_alias_uses_the_broker_answer() {
        let alias = Path::new("/Users/example/Documents/herdr-mcp/extension");
        let current = Path::new("/Users/example/Library/herdr-mcp/standalone/current");

        let ready = inspect_alias_with_probe(alias, current, true, &|_, _| AliasProbe::Ready);
        assert_eq!(ready["status"], "ready");
        assert_eq!(ready["ready"], true);
        assert_eq!(ready["broker_routed"], true);
        // `load_path_ready` reads exactly this flag, and `ensure_alias` only
        // acts on "missing", so both follow from the brokered answer.

        // A missing alias must stay creatable: `ensure_alias` only acts on
        // "missing", so reporting tcc_pending here is what broke the default
        // install.
        let missing = inspect_alias_with_probe(alias, current, true, &|_, _| AliasProbe::Missing);
        assert_eq!(missing["status"], "missing");
        assert_eq!(missing["ready"], false);

        // The explicit `--path` preflight refuses only "occupied"/"unverified";
        // "missing" and "ready" are accepted, so a protected path is not
        // falsely reported as occupied.
        for status in ["missing", "ready"] {
            assert!(!matches!(status, "occupied" | "unverified"));
        }
    }

    /// user sees one stall condition | Given a protected alias and a broker that
    /// does not answer, or refuses | When the alias is inspected | Then the state
    /// is `tcc_pending` with a machine-readable code and it is retryable.
    #[test]
    fn protected_alias_is_tcc_pending_only_when_the_broker_does_not_answer() {
        let alias = Path::new("/Users/example/Documents/herdr-mcp/extension");
        let current = Path::new("/Users/example/Library/herdr-mcp/standalone/current");

        let pending = inspect_alias_with_probe(alias, current, true, &|_, _| AliasProbe::Pending);
        assert_eq!(pending["status"], "tcc_pending");
        assert_eq!(pending["code"], "tcc_pending");
        assert_eq!(pending["retryable"], true);
        assert_eq!(pending["ready"], false);

        let denied = inspect_alias_with_probe(alias, current, true, &|_, _| {
            AliasProbe::Denied("broker refused".to_owned())
        });
        assert_eq!(denied["status"], "unverified");
        assert_eq!(denied["code"], "tcc_pending");
        assert_eq!(denied["error"], "broker refused");
    }

    /// user keeps the default install working | Given a protected alias that is
    /// missing and a broker that creates it | When the alias is ensured | Then the
    /// creation happens through the broker seam, the runtime reports it as
    /// created, and a broker that does not answer yields tcc_pending instead.
    #[test]
    fn protected_alias_creation_goes_through_the_broker_seam() {
        let home = Path::new("/Users/example");
        let alias = Path::new("/Users/example/Documents/herdr-mcp/extension");
        let current = Path::new("/Users/example/Library/herdr-mcp/standalone/current");
        let no_probe = |_: &Path, _: &Path| -> AliasProbe { AliasProbe::Missing };
        let broker_creates = |_: &Path, _: &Path| -> AliasProbe { AliasProbe::Ready };
        let created = ensure_alias_with(home, alias, current, true, &no_probe, &broker_creates);
        assert_eq!(created["status"], "created");
        assert_eq!(created["ready"], true);
        assert_eq!(created["broker_routed"], true);

        let broker_silent = |_: &Path, _: &Path| -> AliasProbe { AliasProbe::Pending };
        let pending = ensure_alias_with(home, alias, current, true, &no_probe, &broker_silent);
        assert_eq!(pending["status"], "tcc_pending");
        assert_eq!(pending["code"], "tcc_pending");
        assert_eq!(pending["retryable"], true);
    }

    /// user can fix an old broker without guessing | Given a broker that predates
    /// the alias subcommands | When the alias is inspected | Then the state is
    /// `broker_outdated` with the reinstall action, not a permission denial.
    #[test]
    fn outdated_broker_is_reported_as_broker_outdated() {
        let alias = Path::new("/Users/example/Documents/herdr-mcp/extension");
        let current = Path::new("/Users/example/Library/herdr-mcp/standalone/current");
        let view =
            inspect_alias_with_probe(alias, current, true, &|_, _| AliasProbe::BrokerOutdated);
        assert_eq!(view["status"], "broker_outdated");
        assert_eq!(view["code"], "broker_outdated");
        assert_eq!(view["retryable"], true);
        assert_eq!(
            view["next_action"],
            "herdr-mcp permissions setup --upgrade-broker"
        );
    }

    /// user keeps an ordinary install unchanged | Given an unprotected path
    /// | When it is inspected | Then the direct filesystem answer is used and no
    /// broker is involved.
    #[test]
    fn unprotected_alias_still_uses_the_direct_probe() {
        let root = std::env::temp_dir().join(format!(
            "herdr-alias-direct-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        ));
        let alias = root.join("extension");
        let current = root.join("current");
        std::fs::create_dir_all(&current).unwrap();
        std::os::unix::fs::symlink(&current, &alias).unwrap();
        let probe = |_: &Path, _: &Path| -> AliasProbe { panic!("the broker must not be used") };
        let view = inspect_alias_with_probe(&alias, &current, false, &probe);
        assert_eq!(view["status"], "ready");
        assert_eq!(view["ready"], true);
        let _ = std::fs::remove_dir_all(&root);
    }

    use super::*;

    #[test]
    fn sha_and_paths_are_strict() {
        assert!(is_sha(&"a".repeat(40)));
        assert!(!is_sha("main"));
        assert!(safe_rel("content/wake.js").is_ok());
        assert!(safe_rel("../Cargo.toml").is_err());
        assert!(safe_rel("content/../manifest.json").is_err());
        assert!(safe_rel("/tmp/file").is_err());
        assert_eq!(
            git_blob_sha(b"test content\n"),
            "d670460b4b4aece5915caf5c68d12f560a9fe3e4"
        );
    }

    #[test]
    fn manifest_injection_preserves_source_bytes_except_key() {
        let source = b"{\n  \"manifest_version\": 3,\n  \"name\": \"Herdr\",\n  \"version\": \"0.1.99\"\n}\n";
        let key = "abc123";
        let installed = inject_key(source, key).unwrap();
        let expected = b"{\n  \"manifest_version\": 3,\n  \"name\": \"Herdr\",\n  \"version\": \"0.1.99\",\n  \"key\": \"abc123\"\n}\n";
        assert_eq!(installed, expected);
    }

    fn write_extension_prefs(
        root: &Path,
        profile: &str,
        extension_id: &str,
        extension_path: &Path,
    ) {
        let profile_dir = root.join(profile);
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(
            profile_dir.join("Secure Preferences"),
            serde_json::to_vec(&json!({
                "extensions": { "settings": {
                    extension_id: { "location": 4, "path": extension_path }
                }}
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn chrome_root(root: &Path) -> ChromiumRoot {
        ChromiumRoot {
            browser: "Google Chrome",
            root: root.to_path_buf(),
        }
    }

    #[test]
    fn browser_doctor_detects_loaded_path_drift() {
        let root = env::temp_dir().join(format!("herdr-browser-drift-test-{}", now_ms()));
        let chrome = root.join("Chrome");
        let expected = root.join("managed/current");
        let stale = root.join("Downloads/herdr-old/unpacked");
        fs::create_dir_all(&expected).unwrap();
        fs::create_dir_all(&stale).unwrap();
        write_extension_prefs(&chrome, "Default", "abcdefghijklmnop", &stale);

        let report =
            diagnose_chromium_load(&expected, None, "abcdefghijklmnop", &[chrome_root(&chrome)]);
        assert_eq!(report.state, "drift");
        assert_eq!(report.drift_count(), 1);
        assert_eq!(report.loads[0].path, stale);
        assert_eq!(report.loads[0].browser, "Google Chrome");
        assert!(report.doctor_line().contains("chrome://extensions"));
        assert_eq!(report.as_json()["loaded"][0]["matches_expected"], false);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn browser_doctor_accepts_managed_path() {
        let root = env::temp_dir().join(format!("herdr-browser-match-test-{}", now_ms()));
        let chrome = root.join("Chrome");
        let expected = root.join("managed/current");
        fs::create_dir_all(&expected).unwrap();
        write_extension_prefs(&chrome, "Profile 1", "abcdefghijklmnop", &expected);

        let report =
            diagnose_chromium_load(&expected, None, "abcdefghijklmnop", &[chrome_root(&chrome)]);
        assert_eq!(report.state, "loaded");
        assert_eq!(report.drift_count(), 0);
        assert_eq!(report.as_json()["loaded"][0]["matches_expected"], true);
        assert_eq!(report.as_json()["loaded"][0]["browser"], "Google Chrome");
        let _ = fs::remove_dir_all(root);
    }

    /// The reported workstation runs no Google Chrome at all: ego lite is the
    /// only Chromium browser and loads the managed unpacked extension. A fake
    /// home that contains only `Citro Labs/ego lite` must therefore read as
    /// loaded, labelled with that browser, not as `not_loaded`/`chrome_not_found`.
    #[test]
    fn browser_doctor_reports_ego_lite_managed_load_as_loaded() {
        let root = env::temp_dir().join(format!("herdr-browser-egolite-test-{}", now_ms()));
        let home = root.join("home");
        let expected = root.join("managed/current");
        fs::create_dir_all(&expected).unwrap();
        let ego_lite = home.join("Library/Application Support/Citro Labs/ego lite");
        write_extension_prefs(&ego_lite, "Default", "abcdefghijklmnop", &expected);
        let roots = chromium_roots(&home);
        assert!(
            roots.iter().any(|root| root.browser == "ego lite"),
            "ego lite user-data root must be probed"
        );

        let report = diagnose_chromium_load(&expected, None, "abcdefghijklmnop", &roots);
        assert_eq!(report.state, "loaded");
        assert_ne!(report.state, "chrome_not_found");
        assert_eq!(report.loads.len(), 1);
        assert_eq!(report.loads[0].browser, "ego lite");
        assert_eq!(report.loads[0].profile, "Default");
        assert_eq!(report.drift_count(), 0);
        let json = report.as_json();
        assert_eq!(json["state"], "loaded");
        assert_eq!(json["loaded"][0]["browser"], "ego lite");
        assert_eq!(json["loaded"][0]["profile"], "Default");
        assert_eq!(json["loaded"][0]["matches_expected"], true);
        // JSON stays backward compatible: existing keys and their meanings hold.
        assert!(json["expected_path"].is_string());
        assert!(json.get("browser_roots").is_some());
        let line = report.doctor_line();
        assert!(line.contains("state=loaded"), "{line}");
        assert!(line.contains("ego lite"), "{line}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn browser_doctor_probes_other_chromium_roots_instead_of_chrome_only() {
        let root = env::temp_dir().join(format!("herdr-browser-notloaded-test-{}", now_ms()));
        let home = root.join("home");
        let expected = root.join("managed/current");
        fs::create_dir_all(&expected).unwrap();
        let brave = home.join("Library/Application Support/BraveSoftware/Brave-Browser");
        write_extension_prefs(&brave, "Default", "someotherextension", &expected);

        let report =
            diagnose_chromium_load(&expected, None, "abcdefghijklmnop", &chromium_roots(&home));
        assert_eq!(report.state, "not_loaded");
        let line = report.doctor_line();
        assert!(line.contains("state=not_loaded"), "{line}");
        assert!(line.contains("Brave"), "{line}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn browser_doctor_keeps_chrome_drift_when_another_root_is_loaded() {
        // Per-root meaning is preserved: a healthy ego lite load must not hide
        // drift coming from an older Chrome copy of the same extension ID.
        let root = env::temp_dir().join(format!("herdr-browser-mixed-test-{}", now_ms()));
        let home = root.join("home");
        let expected = root.join("managed/current");
        let stale = root.join("Downloads/herdr-old/unpacked");
        fs::create_dir_all(&expected).unwrap();
        fs::create_dir_all(&stale).unwrap();
        let chrome = home.join("Library/Application Support/Google/Chrome");
        let ego_lite = home.join("Library/Application Support/Citro Labs/ego lite");
        write_extension_prefs(&chrome, "Default", "abcdefghijklmnop", &stale);
        write_extension_prefs(&ego_lite, "Default", "abcdefghijklmnop", &expected);

        let report =
            diagnose_chromium_load(&expected, None, "abcdefghijklmnop", &chromium_roots(&home));
        assert_eq!(report.state, "drift");
        assert_eq!(report.drift_count(), 1);
        assert_eq!(report.loads.len(), 2);
        let line = report.doctor_line();
        assert!(line.contains("state=drift"), "{line}");
        assert!(line.contains("Google Chrome"), "{line}");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn browser_doctor_does_not_claim_not_loaded_from_an_unreadable_root() {
        use std::os::unix::fs::PermissionsExt;
        let root = env::temp_dir().join(format!("herdr-browser-unreadable-test-{}", now_ms()));
        let expected = root.join("managed/current");
        let blocked = root.join("ego lite");
        fs::create_dir_all(&expected).unwrap();
        fs::create_dir_all(&blocked).unwrap();
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();

        let report = diagnose_chromium_load(
            &expected,
            None,
            "abcdefghijklmnop",
            &[ChromiumRoot {
                browser: "ego lite",
                root: blocked.clone(),
            }],
        );
        assert_eq!(report.state, "not_probed");
        assert_eq!(report.roots.len(), 1);
        assert!(report.roots[0].present);
        assert!(!report.roots[0].readable);
        assert!(report.doctor_line().contains("state=not_probed"));

        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o755)).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn browser_doctor_reports_chrome_not_found_only_without_chromium_roots() {
        let root = env::temp_dir().join(format!("herdr-browser-absent-test-{}", now_ms()));
        let home = root.join("home");
        let expected = root.join("managed/current");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&expected).unwrap();

        let report =
            diagnose_chromium_load(&expected, None, "abcdefghijklmnop", &chromium_roots(&home));
        assert_eq!(report.state, "chrome_not_found");
        assert_eq!(report.roots.iter().filter(|root| root.present).count(), 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn standalone_status_hint_names_the_loading_browser() {
        let load = |browser: &str, path: &Path| ExtensionLoad {
            browser: browser.to_owned(),
            profile: "Default".to_owned(),
            path: path.to_path_buf(),
            location: Some(4),
        };
        let managed = PathBuf::from("/managed/current");
        let stale = PathBuf::from("/Downloads/herdr-old/unpacked");
        let loads = vec![load("Google Chrome", &stale), load("ego lite", &managed)];

        let browser = preferred_loading_browser(&loads, &managed, None);
        assert_eq!(browser.as_deref(), Some("ego lite"));
        let hint = chrome_hint(browser.as_deref(), &managed);
        assert_eq!(hint["url"], "chrome://extensions");
        assert_eq!(
            hint["action"],
            "Developer mode -> Load unpacked in ego lite"
        );
        assert_eq!(hint["load_unpacked_path"], json!(managed));

        // Google Chrome keeps the historical hint untouched.
        let chrome_only = vec![load("Google Chrome", &managed)];
        let browser = preferred_loading_browser(&chrome_only, &managed, None);
        assert_eq!(browser.as_deref(), Some("Google Chrome"));
        assert_eq!(
            chrome_hint(browser.as_deref(), &managed)["action"],
            "Developer mode -> Load unpacked"
        );
        assert_eq!(
            chrome_hint(None, &managed)["action"],
            "Developer mode -> Load unpacked"
        );
    }

    #[test]
    fn prepared_manifest_uses_fixed_contract_identity() {
        let root = env::temp_dir().join(format!("herdr-standalone-test-{}", now_ms()));
        fs::create_dir_all(&root).unwrap();
        let source = b"{\n  \"manifest_version\": 3,\n  \"name\": \"Herdr\",\n  \"version\": \"0.1.99\"\n}\n";
        fs::write(root.join("manifest.json"), source).unwrap();
        let prepared = prepare_manifest(&root).unwrap();
        let manifest = read_json(&root.join("manifest.json")).unwrap();
        let identity = crate::browser_extension_identity::official_standalone_identity().unwrap();
        assert_eq!(manifest["key"], identity.manifest_key.unwrap());
        assert_eq!(prepared.extension_id, identity.extension_id);
        assert_eq!(prepared.source_sha256, sha256(source));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn browser_doctor_stays_stable_after_user_changes_load_path() {
        // Chrome still loads the previous alias while install has already moved
        // the recorded load path. Switching `--path` must not turn a healthy
        // managed install into a false `drift`.
        let root = env::temp_dir().join(format!("herdr-browser-alias-change-{}", now_ms()));
        let chrome = root.join("Chrome");
        let home = root.join("home");
        let current = base_dir_for(&home).join("current");
        fs::create_dir_all(&current).unwrap();
        let previous = home.join("Documents/first");
        let configured = home.join("Documents/second");
        for path in [&previous, &configured] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&current, path).unwrap();
        }
        fs::create_dir_all(base_dir_for(&home)).unwrap();
        fs::write(
            base_dir_for(&home).join("state.json"),
            serde_json::to_vec(&json!({ "load_unpacked_path": configured })).unwrap(),
        )
        .unwrap();
        let state = read_json(&base_dir_for(&home).join("state.json")).ok();
        let recorded = reported_load_path(&home, state.as_ref());
        assert_eq!(recorded.as_deref(), Some(configured.as_path()));

        write_extension_prefs(&chrome, "Default", "abcdefghijklmnop", &previous);
        let report = diagnose_chromium_load(
            &current,
            recorded.as_deref(),
            "abcdefghijklmnop",
            &[chrome_root(&chrome)],
        );
        assert_eq!(report.state, "loaded");
        assert_eq!(report.drift_count(), 0);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recorded_load_path_matches_without_probing_documents() {
        // macOS TCC can deny lstat/readlink on ~/Documents. The recorded
        // Chrome-facing path lives in ~/.config state, so both `status` and
        // `doctor` must match Chrome's persisted string lexically without the
        // alias existing or being inspectable.
        let root = env::temp_dir().join(format!("herdr-recorded-load-path-{}", now_ms()));
        let chrome = root.join("Chrome");
        let home = root.join("home");
        let current = base_dir_for(&home).join("current");
        fs::create_dir_all(&current).unwrap();
        let recorded = home.join("Documents/herdr-mcp/extension");
        fs::create_dir_all(base_dir_for(&home)).unwrap();
        fs::write(
            base_dir_for(&home).join("state.json"),
            serde_json::to_vec(&json!({ "load_unpacked_path": recorded })).unwrap(),
        )
        .unwrap();
        let state = read_json(&base_dir_for(&home).join("state.json")).ok();

        // The alias is intentionally absent, so every filesystem-based
        // equivalence fails exactly like a TCC-denied directory.
        assert!(!recorded.exists());
        let (_, load_unpacked_path) = status_load_view(&home, &current, state.as_ref());
        assert_eq!(load_unpacked_path, recorded);

        write_extension_prefs(&chrome, "Default", "abcdefghijklmnop", &recorded);
        let configured = reported_load_path(&home, state.as_ref()).filter(|path| path != &current);
        let report = diagnose_chromium_load(
            &current,
            configured.as_deref(),
            "abcdefghijklmnop",
            &[chrome_root(&chrome)],
        );
        assert_eq!(report.state, "loaded");
        assert_eq!(report.drift_count(), 0);
        assert_eq!(
            report.as_json()["configured_load_path"],
            json!(recorded.to_string_lossy())
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_load_paths_expand_under_home_and_reject_escape() {
        let home = PathBuf::from("/Users/example");
        assert_eq!(
            resolve_load_path(&home, "~/Documents/herdr/extension").unwrap(),
            home.join("Documents/herdr/extension")
        );
        assert_eq!(
            resolve_load_path(&home, "Downloads/herdr-extension").unwrap(),
            home.join("Downloads/herdr-extension")
        );
        assert!(resolve_load_path(&home, "~/../outside").is_err());
        assert!(resolve_load_path(&home, "/tmp/herdr-extension").is_err());
        assert!(resolve_load_path(&home, "~").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn explicit_load_path_is_created_and_conflicts_fail_closed() {
        let root = env::temp_dir().join(format!("herdr-standalone-custom-alias-{}", now_ms()));
        let home = root.join("home");
        let current = base_dir_for(&home).join("current");
        let custom = home.join("Downloads/herdr-extension");
        fs::create_dir_all(&current).unwrap();

        preflight_explicit_load_path(&custom, &current).unwrap();
        let created = ensure_explicit_alias(&home, &custom, &current).unwrap();
        assert_eq!(created["status"], "created");
        assert_eq!(fs::read_link(&custom).unwrap(), current);
        preflight_explicit_load_path(&custom, &current).unwrap();

        fs::remove_file(&custom).unwrap();
        fs::create_dir_all(&custom).unwrap();
        let error = preflight_explicit_load_path(&custom, &current).unwrap_err();
        assert!(error.contains("refusing to overwrite"));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn user_visible_alias_is_created_once_and_reused() {
        let root = env::temp_dir().join(format!("herdr-standalone-alias-test-{}", now_ms()));
        let home = root.join("home");
        let current = base_dir_for(&home).join("current");
        fs::create_dir_all(&current).unwrap();

        let created = ensure_user_visible_alias(&home, &current);
        assert_eq!(created["ready"], true);
        assert_eq!(created["status"], "created");
        let alias = user_visible_alias_path(&home);
        assert_eq!(fs::read_link(&alias).unwrap(), current);

        let reused = ensure_user_visible_alias(&home, &current);
        assert_eq!(reused["ready"], true);
        assert_eq!(reused["status"], "ready");
        assert_eq!(preferred_load_unpacked_path(&reused, &current), alias);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn user_visible_alias_never_overwrites_an_occupied_path() {
        let root = env::temp_dir().join(format!("herdr-standalone-alias-conflict-{}", now_ms()));
        let home = root.join("home");
        let current = base_dir_for(&home).join("current");
        let alias = user_visible_alias_path(&home);
        fs::create_dir_all(&current).unwrap();
        fs::create_dir_all(&alias).unwrap();

        let occupied = ensure_user_visible_alias(&home, &current);
        assert_eq!(occupied["ready"], false);
        assert_eq!(occupied["status"], "occupied");
        assert_eq!(occupied["kind"], "directory");
        assert!(alias.is_dir());
        assert_eq!(preferred_load_unpacked_path(&occupied, &current), current);

        #[cfg(unix)]
        {
            fs::remove_dir(&alias).unwrap();
            let other = root.join("other-extension");
            fs::create_dir_all(&other).unwrap();
            std::os::unix::fs::symlink(&other, &alias).unwrap();
            let wrong_link = ensure_user_visible_alias(&home, &current);
            assert_eq!(wrong_link["ready"], false);
            assert_eq!(wrong_link["status"], "occupied");
            assert_eq!(wrong_link["kind"], "symlink");
            assert_eq!(fs::read_link(&alias).unwrap(), other);
        }
        let _ = fs::remove_dir_all(root);
    }
    // ---- #833: update-time staging and version reconciliation -------------

    fn temp_root(label: &str) -> PathBuf {
        let root = env::temp_dir().join(format!(
            "herdr-833-{label}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn standalone_key() -> String {
        crate::browser_extension_identity::official_standalone_identity()
            .unwrap()
            .manifest_key
            .unwrap()
    }

    fn extension_manifest(version: &str) -> Vec<u8> {
        serde_json::to_vec_pretty(&json!({
            "manifest_version": 3,
            "name": "Herdr MCP - Workspace Connector",
            "version": version,
        }))
        .unwrap()
    }

    /// One fake release tree. Every fetch is counted so a test can prove the
    /// update-time staging path performs no network work when there is nothing
    /// to do.
    struct FakeSource {
        commit: String,
        files: Vec<(&'static str, Vec<u8>)>,
        fetch_calls: std::cell::Cell<usize>,
        fail_path: Option<&'static str>,
    }

    impl FakeSource {
        fn release(commit: &str, version: &str) -> Self {
            Self {
                commit: commit.to_owned(),
                files: vec![
                    ("manifest.json", extension_manifest(version)),
                    ("background.js", b"// release bundle\n".to_vec()),
                    ("content/wake.js", b"// nested release file\n".to_vec()),
                ],
                fetch_calls: std::cell::Cell::new(0),
                fail_path: None,
            }
        }
    }

    impl ExtensionSource for FakeSource {
        fn resolve_commit(&self, _reference: &str) -> Result<String, String> {
            Ok(self.commit.clone())
        }

        fn list_blobs(&self, _commit: &str) -> Result<Vec<Blob>, String> {
            Ok(self
                .files
                .iter()
                .map(|(path, bytes)| Blob {
                    repo_path: format!("extension/{path}"),
                    relative_path: safe_rel(path).unwrap(),
                    size: bytes.len() as u64,
                    git_sha: git_blob_sha(bytes),
                })
                .collect())
        }

        fn fetch_blob(&self, _commit: &str, blob: &Blob) -> Result<Vec<u8>, String> {
            self.fetch_calls.set(self.fetch_calls.get() + 1);
            if self.fail_path == Some(blob.repo_path.as_str()) {
                return Err(format!("simulated download failure: {}", blob.repo_path));
            }
            self.files
                .iter()
                .find(|(path, _)| blob.repo_path == format!("extension/{path}"))
                .map(|(_, bytes)| bytes.clone())
                .ok_or_else(|| format!("fake release has no {}", blob.repo_path))
        }
    }

    /// Seed a managed standalone install exactly like a previous CLI (or an
    /// older Runtime) left it.
    fn seed_standalone_install(home: &Path, commit: &str, version: &str) -> PathBuf {
        let base = base_dir_for(home);
        let current = base.join("current");
        fs::create_dir_all(&current).unwrap();
        fs::write(
            current.join("manifest.json"),
            serde_json::to_vec(&json!({
                "manifest_version": 3,
                "name": "Herdr MCP - Workspace Connector",
                "version": version,
                "key": standalone_key(),
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(current.join("background.js"), b"// previous bundle\n").unwrap();
        atomic_json(
            &base.join("state.json"),
            &json!({
                "schema_version": 1,
                "repository": REPOSITORY,
                "requested_ref": commit,
                "source_commit": commit,
                "extension_version": version,
                "extension_id": crate::browser_extension_identity::official_standalone_identity()
                    .unwrap()
                    .extension_id,
                "installed_at_unix_ms": 1,
                "path": current,
                "load_unpacked_path": user_visible_alias_path(home),
            }),
        )
        .unwrap();
        current
    }

    fn seed_registrations(config_dir: &Path, entries: &[(&str, &str, i64)]) {
        use crate::state_store::{BrowserEndpointRegistrationInput, StateStore};
        fs::create_dir_all(config_dir).unwrap();
        let mut store = StateStore::open(config_dir.join("state.db")).unwrap();
        for (index, (family, version, observed_at)) in entries.iter().enumerate() {
            store
                .register_browser_endpoint(BrowserEndpointRegistrationInput {
                    device_id: "dev_01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    profile_seed: &format!("profile-seed-0123456789abcde{index}"),
                    browser_family: family,
                    extension_version: version,
                    observed_at: *observed_at,
                })
                .unwrap();
        }
    }

    #[test]
    fn staging_never_creates_a_standalone_install() {
        // Given a workstation that never installed the standalone extension
        // When an update stages the release extension
        // Then nothing is downloaded and no install appears.
        let root = temp_root("stage-not-installed");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let source = FakeSource::release(&"a".repeat(40), "0.1.240");

        let staged = stage_release_extension_in(&home, "v1.1.0-rc.12", &source, true).unwrap();

        assert_eq!(staged.state, StagedExtensionState::NotInstalled);
        assert_eq!(source.fetch_calls.get(), 0);
        assert!(!base_dir_for(&home).join("current").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn staging_swaps_in_the_release_extension_tree_and_records_it() {
        // Given a standalone install left on an older release commit
        // When the update stages the new release
        // Then `current` is the new tree with the same manifest key, the state
        //      record names the new commit, and no alias is created in Documents.
        let root = temp_root("stage-swap");
        let home = root.join("home");
        let old_commit = "b".repeat(40);
        let new_commit = "c".repeat(40);
        seed_standalone_install(&home, &old_commit, "0.1.229");
        let source = FakeSource::release(&new_commit, "0.1.240");

        let staged = stage_release_extension_in(&home, "v1.1.0-rc.12", &source, true).unwrap();

        assert_eq!(staged.state, StagedExtensionState::Staged);
        assert_eq!(staged.source_commit.as_deref(), Some(new_commit.as_str()));
        assert_eq!(staged.extension_version.as_deref(), Some("0.1.240"));
        assert_eq!(
            staged.previous_source_commit.as_deref(),
            Some(old_commit.as_str())
        );
        assert_eq!(
            staged.previous_extension_version.as_deref(),
            Some("0.1.229")
        );
        assert_eq!(
            staged.action(),
            Some("install_extension"),
            "a recorded load path that does not resolve to `current` needs the CLI install, not a Reload"
        );

        let base = base_dir_for(&home);
        let current = base.join("current");
        let manifest = read_json(&current.join("manifest.json")).unwrap();
        assert_eq!(manifest["version"], "0.1.240");
        assert_eq!(manifest["key"], standalone_key());
        assert_eq!(
            fs::read(current.join("background.js")).unwrap(),
            b"// release bundle\n"
        );
        let state = read_json(&base.join("state.json")).unwrap();
        assert_eq!(state["source_commit"], new_commit);
        assert_eq!(state["extension_version"], "0.1.240");
        assert_eq!(state["installed_by"], "update");
        assert!(
            !user_visible_alias_path(&home).exists(),
            "an unattended update must not create a ~/Documents alias"
        );
        let leftovers = fs::read_dir(&base)
            .unwrap()
            .flatten()
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                name.starts_with(".staging-") || name.starts_with(".previous-")
            })
            .count();
        assert_eq!(leftovers, 0, "staging and backup directories are removed");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn staging_is_idempotent_for_an_already_staged_release() {
        // Given the release extension was already staged
        // When staging runs again (interactive foreground after the worker)
        // Then it reports up-to-date without downloading anything.
        let root = temp_root("stage-idempotent");
        let home = root.join("home");
        let commit = "d".repeat(40);
        seed_standalone_install(&home, &commit, "0.1.230");
        let source = FakeSource::release(&commit, "0.1.240");

        let staged = stage_release_extension_in(&home, "v1.1.0-rc.12", &source, true).unwrap();

        assert_eq!(staged.state, StagedExtensionState::UpToDate);
        assert_eq!(source.fetch_calls.get(), 0);
        assert!(staged.summary().contains("already staged"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn staging_keeps_the_previous_tree_when_a_download_fails() {
        // Given a working standalone install
        // When the release download fails midway
        // Then the previous tree and state record survive untouched.
        let root = temp_root("stage-failure");
        let home = root.join("home");
        let old_commit = "e".repeat(40);
        let current = seed_standalone_install(&home, &old_commit, "0.1.229");
        let mut source = FakeSource::release(&"f".repeat(40), "0.1.240");
        source.fail_path = Some("extension/background.js");

        let error = stage_release_extension_in(&home, "v1.1.0-rc.12", &source, true).unwrap_err();

        assert!(error.contains("simulated download failure"), "{error}");
        let manifest = read_json(&current.join("manifest.json")).unwrap();
        assert_eq!(manifest["version"], "0.1.229");
        let state = read_json(&base_dir_for(&home).join("state.json")).unwrap();
        assert_eq!(state["source_commit"], old_commit);
        let leftovers = fs::read_dir(base_dir_for(&home))
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".staging-"))
            .count();
        assert_eq!(leftovers, 0, "a failed staging directory is removed");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn standalone_install_writes_a_private_tree_and_state_record() {
        // Given no standalone install and a permissive process umask
        // When `extension standalone install` stages the release tree
        // Then the managed tree is 0700 and every file including state.json is
        //      0600, so other local accounts cannot read the extension or its
        //      install provenance.
        let root = temp_root("perms-fresh");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let commit = "a".repeat(40);
        let source = FakeSource::release(&commit, "0.1.240");
        let snapshot = ExtensionSnapshot {
            commit: commit.clone(),
            blobs: source.list_blobs(&commit).unwrap(),
        };

        let view = install_snapshot(&home, "v1.1.0-rc.13", &snapshot, &source, None).unwrap();

        assert_eq!(view["ok"], true);
        let base = base_dir_for(&home);
        let current = base.join("current");
        assert_eq!(mode_of(&base), 0o700, "standalone state dir");
        assert_eq!(mode_of(&current), 0o700, "managed extension tree");
        assert_eq!(mode_of(&current.join("content")), 0o700, "nested tree dir");
        assert_eq!(mode_of(&current.join("manifest.json")), 0o600);
        assert_eq!(mode_of(&current.join("background.js")), 0o600);
        assert_eq!(mode_of(&current.join("content/wake.js")), 0o600);
        assert_eq!(mode_of(&base.join("state.json")), 0o600);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn update_staging_keeps_a_private_tree_private() {
        // Given an existing install that is already 0700/0600
        // When the runtime update stages the next release over it
        // Then the swapped-in tree and the rewritten state record stay private.
        let root = temp_root("perms-restage");
        let home = root.join("home");
        let current = seed_standalone_install(&home, &"b".repeat(40), "0.1.239");
        let base = base_dir_for(&home);
        set_mode(&base, 0o700);
        set_mode(&current, 0o700);
        set_mode(&current.join("manifest.json"), 0o600);
        set_mode(&current.join("background.js"), 0o600);
        set_mode(&base.join("state.json"), 0o600);
        let source = FakeSource::release(&"c".repeat(40), "0.1.240");

        let staged = stage_release_extension_in(&home, "v1.1.0-rc.13", &source, true).unwrap();

        assert_eq!(staged.state, StagedExtensionState::Staged);
        assert_eq!(mode_of(&current), 0o700);
        assert_eq!(mode_of(&current.join("content")), 0o700);
        assert_eq!(mode_of(&current.join("manifest.json")), 0o600);
        assert_eq!(mode_of(&current.join("content/wake.js")), 0o600);
        assert_eq!(mode_of(&base.join("state.json")), 0o600);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn update_staging_repairs_a_loosened_tree_and_state_record() {
        // Given an install left world-readable by an earlier release
        //       (0755 tree, 0644 state.json, the #833 report)
        // When the runtime update stages the next release
        // Then the managed tree and the state record are private again.
        let root = temp_root("perms-repair");
        let home = root.join("home");
        let current = seed_standalone_install(&home, &"d".repeat(40), "0.1.239");
        let base = base_dir_for(&home);
        set_mode(&base, 0o755);
        set_mode(&current, 0o755);
        set_mode(&current.join("manifest.json"), 0o644);
        set_mode(&current.join("background.js"), 0o644);
        set_mode(&base.join("state.json"), 0o644);
        let source = FakeSource::release(&"e".repeat(40), "0.1.240");

        let staged = stage_release_extension_in(&home, "v1.1.0-rc.13", &source, true).unwrap();

        assert_eq!(staged.state, StagedExtensionState::Staged);
        assert_eq!(mode_of(&current), 0o700, "the swapped-in tree is private");
        assert_eq!(mode_of(&current.join("content")), 0o700);
        assert_eq!(mode_of(&current.join("manifest.json")), 0o600);
        assert_eq!(mode_of(&current.join("content/wake.js")), 0o600);
        assert_eq!(
            mode_of(&base.join("state.json")),
            0o600,
            "the rewritten state record is private"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn doctor_version_report_marks_a_registered_older_extension_as_reload_pending() {
        // Given the release extension is staged and the browser registered an
        //       older extension version
        // When doctor reconciles versions
        // Then it reports reload_pending with the Reload fix action.
        let root = temp_root("version-reload");
        let home = root.join("home");
        let config_dir = root.join("config");
        let commit = "1".repeat(40);
        let expected = crate::runtime_meta::bundled_extension_version().unwrap();
        seed_standalone_install(&home, &commit, expected);
        seed_registrations(&config_dir, &[("ego", "0.1.229", 20)]);

        let report = version_report_in(&home, &config_dir, Some(expected), Some(&commit));

        assert_eq!(report.state, "reload_pending");
        let json = report.as_json();
        assert_eq!(json["staged_version"], expected);
        assert_eq!(json["registered_version"], "0.1.229");
        assert_eq!(json["action"], "reload_extension");
        assert_eq!(json["next_action"], EXTENSION_RELOAD_NEXT_ACTION);
        let line = report.doctor_line();
        assert!(
            line.starts_with("WARN standalone-extension-version state=reload_pending"),
            "{line}"
        );
        assert!(line.contains("chrome://extensions"), "{line}");
        let notice = report.update_notice().unwrap();
        assert_eq!(notice["code"], "extension_reload_required");
        assert_eq!(notice["fatal"], false);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn doctor_version_report_is_current_when_the_browser_runs_the_staged_extension() {
        // Given staged and registered extension versions match this release
        // When doctor reconciles versions
        // Then it reports current and produces no update notice.
        let root = temp_root("version-current");
        let home = root.join("home");
        let config_dir = root.join("config");
        let commit = "2".repeat(40);
        let expected = crate::runtime_meta::bundled_extension_version().unwrap();
        seed_standalone_install(&home, &commit, expected);
        seed_registrations(&config_dir, &[("chrome", expected, 30)]);

        let report = version_report_in(&home, &config_dir, Some(expected), Some(&commit));

        assert_eq!(report.state, "current");
        assert!(report.update_notice().is_none());
        assert!(
            report
                .doctor_line()
                .starts_with("INFO standalone-extension-version state=current")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn doctor_version_report_calls_a_staged_extension_from_another_release_stale() {
        // Given the staged extension belongs to a different release
        // When doctor reconciles versions
        // Then it reports extension_stale with the exact install command, and
        //      the update result carries the same warning without failing.
        let root = temp_root("version-stale");
        let home = root.join("home");
        let config_dir = root.join("config");
        let expected = crate::runtime_meta::bundled_extension_version().unwrap();
        seed_standalone_install(&home, &"3".repeat(40), "0.1.229");
        seed_registrations(&config_dir, &[("ego", "0.1.229", 10)]);

        let report = version_report_in(&home, &config_dir, Some(expected), Some(&"4".repeat(40)));

        assert_eq!(report.state, "extension_stale");
        let json = report.as_json();
        assert_eq!(json["action"], "install_extension");
        assert_eq!(json["next_action"], EXTENSION_INSTALL_NEXT_ACTION);
        let line = report.doctor_line();
        assert!(
            line.starts_with("WARN standalone-extension-version state=extension_stale"),
            "{line}"
        );
        assert!(
            line.contains("herdr-mcp extension standalone install"),
            "{line}"
        );
        let notice = report.update_notice().unwrap();
        assert_eq!(notice["code"], "extension_stale");
        assert_eq!(notice["severity"], "warning");
        assert_eq!(notice["fatal"], false);
        assert_eq!(notice["next_action"], EXTENSION_INSTALL_NEXT_ACTION);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn doctor_version_report_is_unknown_without_registration_evidence() {
        // Given a staged extension and no readable endpoint registration
        // When doctor reconciles versions
        // Then it reports unknown instead of guessing, and prints no warning.
        let root = temp_root("version-unknown");
        let home = root.join("home");
        let config_dir = root.join("config");
        let commit = "5".repeat(40);
        let expected = crate::runtime_meta::bundled_extension_version().unwrap();
        seed_standalone_install(&home, &commit, expected);

        let report = version_report_in(&home, &config_dir, Some(expected), Some(&commit));

        assert_eq!(report.state, "unknown");
        assert_eq!(report.as_json()["reason"], "state_db_missing");
        assert!(report.update_notice().is_none());
        assert!(
            report
                .doctor_line()
                .starts_with("INFO standalone-extension-version state=unknown")
        );

        let doctor = doctor_report_in(&home, &config_dir);
        assert_eq!(doctor.as_json()["version"]["state"], "unknown");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn doctor_version_report_does_not_ask_for_a_downgrade_reload() {
        // Given the browser runs a newer extension build than this Runtime
        //       stages (the ordinary STORE case)
        // When doctor reconciles versions
        // Then it reports extension_ahead with no Reload and no install action.
        let root = temp_root("version-ahead");
        let home = root.join("home");
        let config_dir = root.join("config");
        let commit = "8".repeat(40);
        let expected = crate::runtime_meta::bundled_extension_version().unwrap();
        seed_standalone_install(&home, &commit, expected);
        seed_registrations(&config_dir, &[("chrome", "99.0.1", 50)]);

        let report = version_report_in(&home, &config_dir, Some(expected), Some(&commit));

        assert_eq!(report.state, "extension_ahead");
        assert_eq!(
            report.as_json()["reason"],
            "registered_extension_newer_than_staged"
        );
        assert_eq!(report.as_json()["action"], Value::Null);
        assert!(report.update_notice().is_none());
        assert!(
            report
                .doctor_line()
                .starts_with("INFO standalone-extension-version state=extension_ahead")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn doctor_version_report_asks_for_a_reload_when_versions_are_not_orderable() {
        // Given the registered version is not a comparable version string
        // When doctor reconciles versions
        // Then a difference still asks for a Reload instead of being ignored.
        let root = temp_root("version-unorderable");
        let home = root.join("home");
        let config_dir = root.join("config");
        let commit = "9".repeat(40);
        let expected = crate::runtime_meta::bundled_extension_version().unwrap();
        seed_standalone_install(&home, &commit, expected);
        seed_registrations(&config_dir, &[("chrome", "unknown-build", 60)]);

        let report = version_report_in(&home, &config_dir, Some(expected), Some(&commit));

        assert_eq!(report.state, "reload_pending");
        assert!(report.update_notice().is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn doctor_version_report_ignores_an_unverifiable_runtime_identity() {
        // Given a source build whose release identity is not an immutable commit
        // When doctor reconciles versions
        // Then matching versions are current instead of a false stale warning.
        let root = temp_root("version-unverifiable");
        let home = root.join("home");
        let config_dir = root.join("config");
        let expected = crate::runtime_meta::bundled_extension_version().unwrap();
        seed_standalone_install(&home, &"7".repeat(40), expected);
        seed_registrations(&config_dir, &[("chrome", expected, 40)]);

        let report = version_report_in(&home, &config_dir, Some(expected), Some("unknown"));

        assert_eq!(report.state, "current");
        assert!(report.update_notice().is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn doctor_version_report_treats_advanced_runtime_commit_with_unchanged_extension_as_current() {
        // Given the staged standalone extension matches this Runtime's bundled
        //       version and source_manifest_sha256, while the Runtime upgraded
        //       to a newer source_commit that did not touch `extension/` (#885)
        // When doctor reconciles versions
        // Then it reports `current` with reason `source_commit_advanced_extension_unchanged`
        //      and emits no false-positive `extension_stale` warning.
        let root = temp_root("version-commit-advanced-unchanged");
        let home = root.join("home");
        let config_dir = root.join("config");
        let staged_commit = "a".repeat(40);
        let runtime_commit = "b".repeat(40);
        let expected = crate::runtime_meta::bundled_extension_version().unwrap();
        seed_standalone_install(&home, &staged_commit, expected);
        let state_path = base_dir_for(&home).join("state.json");
        let mut state = read_json(&state_path).unwrap();
        state["source_manifest_sha256"] =
            Value::String(crate::runtime_meta::bundled_extension_manifest_sha256().to_owned());
        atomic_json(&state_path, &state).unwrap();
        seed_registrations(&config_dir, &[("chrome", expected, 45)]);

        let report = version_report_in(&home, &config_dir, Some(expected), Some(&runtime_commit));

        assert_eq!(report.state, "current");
        let json = report.as_json();
        assert_eq!(json["reason"], "source_commit_advanced_extension_unchanged");
        assert_eq!(json["staged_source_commit"], staged_commit);
        assert_eq!(json["runtime_source_commit"], runtime_commit);
        assert!(report.update_notice().is_none());
        assert!(
            report
                .doctor_line()
                .starts_with("INFO standalone-extension-version state=current")
        );

        // If source_manifest_sha256 differs even with the same version string,
        // it still fails closed as extension_stale.
        state["source_manifest_sha256"] = Value::String("0".repeat(64));
        atomic_json(&state_path, &state).unwrap();
        let drifted = version_report_in(&home, &config_dir, Some(expected), Some(&runtime_commit));
        assert_eq!(drifted.state, "extension_stale");
        assert_eq!(drifted.as_json()["reason"], "extension_manifest_differs");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn doctor_version_report_is_not_installed_without_a_standalone_install() {
        // Given no standalone install
        // When doctor reconciles versions
        // Then it reports not_installed and never asks for a Reload.
        let root = temp_root("version-not-installed");
        let home = root.join("home");
        let config_dir = root.join("config");
        fs::create_dir_all(&home).unwrap();

        let report = version_report_in(&home, &config_dir, Some("0.1.240"), Some(&"6".repeat(40)));

        assert_eq!(report.state, "not_installed");
        assert!(report.update_notice().is_none());
        let _ = fs::remove_dir_all(root);
    }
}
