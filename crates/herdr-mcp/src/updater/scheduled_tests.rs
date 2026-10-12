//! Scheduled-update acceptance tests. Only release networking is substituted:
//! parsing, auto dispatch, byte verification, probing, job persistence and
//! detached candidate execution use the real updater. Platform activation is
//! represented by the service manager's real durable generation transaction;
//! no installed runtime or platform service is touched.

use super::*;
use crate::contract::ContractIdentity;
use crate::state_store::{ServiceRollbackRecord, StateStore};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicU64, Ordering};

thread_local! {
    static RELEASE: RefCell<Option<ReleaseFixture>> = const { RefCell::new(None) };
}

struct ReleaseFixture {
    manifest: Vec<u8>,
    identity: ReleaseIdentity,
    local: ContractIdentity,
    asset_url: Url,
    asset_name: String,
    asset_sha256: String,
    attestations: Vec<String>,
}

pub(super) fn local_contract() -> Option<ContractIdentity> {
    RELEASE.with(|slot| slot.borrow().as_ref().map(|release| release.local.clone()))
}

pub(super) fn metadata(url: &Url, label: &str) -> Option<Vec<u8>> {
    RELEASE.with(|slot| {
        let slot = slot.borrow();
        let release = slot.as_ref()?;
        assert_eq!(label, "release manifest");
        assert!(url.as_str().ends_with("/runtime-manifest.json"));
        Some(release.manifest.clone())
    })
}

pub(super) fn asset_url(original: &Url) -> Url {
    RELEASE.with(|slot| {
        slot.borrow().as_ref().map_or_else(
            || original.clone(),
            |release| {
                assert!(original.as_str().ends_with(&release.asset_name));
                release.asset_url.clone()
            },
        )
    })
}

// External signature-verifier boundary. The real protocol/signature contract
// is covered by release_trust's signed GitHub attestation fixtures.
pub(super) fn attestation(
    name: &str,
    digest: &str,
    identity: &ReleaseIdentity,
) -> Option<Result<(), String>> {
    RELEASE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let release = slot.as_mut()?;
        assert_eq!(identity, &release.identity);
        let expected = if name == "runtime-manifest.json" {
            sha256_bytes(&release.manifest)
        } else {
            assert_eq!(name, release.asset_name);
            release.asset_sha256.clone()
        };
        assert_eq!(digest, expected);
        release.attestations.push(name.to_owned());
        Some(Ok(()))
    })
}

struct Scenario {
    root: PathBuf,
    previous_env: Vec<(&'static str, Option<std::ffi::OsString>)>,
    bytes: Vec<u8>,
}

impl Scenario {
    fn new(edit: impl FnOnce(&mut Value), bad_probe: bool) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = env::temp_dir().join(format!(
            "herdr-auto-{}-{}-{}",
            std::process::id(),
            now_ms_i64(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&root).unwrap();
        let local = crate::contract::previous_identity().unwrap();
        let target = crate::contract::identity().unwrap();
        assert_eq!(local.epoch, 4);
        assert_eq!(target.epoch, 5);
        let reported_hash = if bad_probe { &local.hash } else { &target.hash };
        let bytes = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = version ]; then\n\
             printf '%s\\n' 'herdr-mcp 9.9.9' 'contract epoch {} / {} tools' \
             'state schema {SCHEMA_VERSION}' 'contract hash {reported_hash}'\n\
             else\n\
             printf '%s\\n' \"$@\" > \"$HERDR_MCP_CONFIG_DIR/worker-argv\"\n\
             fi\n",
            target.epoch, target.tool_count,
        )
        .into_bytes();
        let name = format!("herdr-mcp-9.9.9-{}", current_target().unwrap());
        let mut manifest = json!({
            "schema_version": release_trust::MANIFEST_SCHEMA_VERSION,
            "product": "herdr-mcp",
            "state_schema": SCHEMA_VERSION,
            "version": "9.9.9",
            "tag": "v9.9.9",
            "release_identity": {
                "tag": "v9.9.9", "source_commit": "b".repeat(40),
                "source_ref": "refs/tags/v9.9.9",
            },
            "repository_identity": {
                "repository": release_trust::RELEASE_REPOSITORY,
                "repository_id": release_trust::RELEASE_REPOSITORY_ID,
            },
            "provenance": {
                "predicate_type": release_trust::SLSA_PROVENANCE_V1,
                "attestation": release_trust::GITHUB_ARTIFACT_ATTESTATION,
                "bundle_media_type": release_trust::SIGSTORE_BUNDLE_V03,
                "workflow": release_trust::RELEASE_WORKFLOW,
                "workflow_name": release_trust::RELEASE_WORKFLOW_NAME,
                "issuer": release_trust::RELEASE_ISSUER,
                "runner_environment": release_trust::RELEASE_RUNNER_ENVIRONMENT,
            },
            "contract": {"epoch": target.epoch, "hash": target.hash, "tool_count": target.tool_count},
            "contract_migration": {
                "from": {"epoch": local.epoch, "hash": local.hash, "tool_count": local.tool_count},
                "state_schema_from": SCHEMA_VERSION,
            },
            "assets": [{
                "target": current_target().unwrap(), "name": name,
                "size": bytes.len(), "sha256": sha256_bytes(&bytes),
                "url": format!("https://github.com/whshang/herdr-mcp/releases/download/v9.9.9/{name}"),
            }],
        });
        edit(&mut manifest);
        RELEASE.with(|slot| {
            *slot.borrow_mut() = Some(ReleaseFixture {
                identity: release_trust::parse_manifest_identity(&manifest, "v9.9.9").unwrap(),
                manifest: serde_json::to_vec(&manifest).unwrap(),
                local,
                asset_url: Url::parse("http://127.0.0.1:1/unused").unwrap(),
                asset_name: name,
                asset_sha256: sha256_bytes(&bytes),
                attestations: Vec::new(),
            });
        });
        let values = [
            ("HERDR_MCP_CONFIG_DIR", Some(root.as_os_str())),
            ("HERDR_MCP_INSTANCE", None),
            (
                "HERDR_MCP_UPDATE_MANIFEST_URL",
                Some(std::ffi::OsStr::new(
                    "https://github.com/whshang/herdr-mcp/releases/download/v9.9.9/runtime-manifest.json",
                )),
            ),
        ];
        let previous_env = values
            .into_iter()
            .map(|(name, value)| {
                let previous = env::var_os(name);
                unsafe {
                    match value {
                        Some(value) => env::set_var(name, value),
                        None => env::remove_var(name),
                    }
                }
                (name, previous)
            })
            .collect();
        fs::write(
            root.join("config.json"),
            br#"{"update":{"check":true,"channel":"stable"}}"#,
        )
        .unwrap();
        Self {
            root,
            previous_env,
            bytes,
        }
    }

    fn run_auto(&self) -> Result<ExitCode, String> {
        // A real local HTTP response exercises declared size and digest checks.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        RELEASE.with(|slot| {
            slot.borrow_mut().as_mut().unwrap().asset_url = Url::parse(&format!(
                "http://{}/candidate",
                listener.local_addr().unwrap()
            ))
            .unwrap();
        });
        let bytes = self.bytes.clone();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 8192];
            if stream.read(&mut request).unwrap() == 0 {
                return;
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            stream.write_all(&bytes).unwrap();
        });
        let result = auto();
        // Wake the fixture server even if admission fails before downloading.
        let _ = std::net::TcpStream::connect(address);
        server.join().unwrap();
        result
    }

    fn queued_job(&self) -> UpdateJobRecord {
        UpdateStore::open(&RuntimePaths::discover().unwrap())
            .unwrap()
            .active_update_job()
            .unwrap()
            .unwrap()
    }

    fn wait_worker(&self, job: &UpdateJobRecord) {
        let pid = job.worker_pid.unwrap() as libc::pid_t;
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert_eq!(status, 0);
        assert_eq!(
            fs::read_to_string(self.root.join("worker-argv")).unwrap(),
            format!("update\nworker\n--job\n{}\n", job.job_id),
        );
    }
}

impl Drop for Scenario {
    fn drop(&mut self) {
        RELEASE.with(|slot| *slot.borrow_mut() = None);
        for (name, value) in &self.previous_env {
            unsafe {
                match value {
                    Some(value) => env::set_var(name, value),
                    None => env::remove_var(name),
                }
            }
        }
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn auto_adjacent_stable_release_queues_only_an_attested_verified_candidate() {
    let _guard = crate::test_env::lock();
    let scenario = Scenario::new(|_| {}, false);
    assert_eq!(scenario.run_auto().unwrap(), ExitCode::SUCCESS);
    let job = scenario.queued_job();
    assert_eq!(job.state, "queued");
    assert_eq!(job.version, "9.9.9");
    verify_staged_file(Path::new(&job.binary_path), &job.sha256).unwrap();
    scenario.wait_worker(&job);
    RELEASE.with(|slot| {
        assert_eq!(
            slot.borrow().as_ref().unwrap().attestations,
            ["runtime-manifest.json", &job.asset_name]
        );
    });
    drop(scenario);

    let bad_probe = Scenario::new(|_| {}, true);
    assert!(bad_probe.run_auto().unwrap_err().contains("identity"));
    assert!(
        UpdateStore::open(&RuntimePaths::discover().unwrap())
            .unwrap()
            .active_update_job()
            .unwrap()
            .is_none()
    );
    assert!(!bad_probe.root.join("worker-argv").exists());
}

#[test]
fn auto_refuses_skipped_epoch_missing_declaration_and_predecessor_mismatch_without_jobs() {
    let _guard = crate::test_env::lock();
    for case in ["skip", "missing", "from"] {
        let scenario = Scenario::new(
            |manifest| match case {
                "skip" => manifest["contract"]["epoch"] = json!(6),
                "missing" => {
                    manifest
                        .as_object_mut()
                        .unwrap()
                        .remove("contract_migration");
                }
                "from" => {
                    manifest["contract_migration"]["from"]["hash"] =
                        json!(format!("sha256:{}", "d".repeat(64)))
                }
                _ => unreachable!(),
            },
            false,
        );
        let error = auto().unwrap_err();
        assert!(error.contains("local epoch 4"), "{case}: {error}");
        assert!(!scenario.root.join("update").exists());
        assert!(!scenario.root.join("worker-argv").exists());
        let paths = RuntimePaths::discover().unwrap();
        let store = UpdateStore::open(&paths).unwrap();
        assert!(store.active_update_job().unwrap().is_none());
        assert!(store.latest_update_job().unwrap().is_none());
    }
}

#[test]
fn auto_cross_epoch_install_preserves_epoch_four_generation_for_single_use_rollback() {
    let _guard = crate::test_env::lock();
    let scenario = Scenario::new(|_| {}, false);
    let mut state = StateStore::open_in_dir(&scenario.root, "state").unwrap();
    let previous_bytes = b"epoch-four-installed-generation";
    let previous_sha = sha256_bytes(previous_bytes);
    let previous_id = format!("rust-{}", &previous_sha[..16]);
    let previous_dir = scenario.root.join("runtime/generations").join(&previous_id);
    fs::create_dir_all(&previous_dir).unwrap();
    let previous_binary = previous_dir.join("herdr-mcp");
    fs::write(&previous_binary, previous_bytes).unwrap();
    state
        .stage_runtime_generation(
            &previous_id,
            previous_binary.to_str().unwrap(),
            &previous_sha,
            "service-install",
            10,
        )
        .unwrap();
    state.activate_runtime_generation(&previous_id, 11).unwrap();
    assert_eq!(scenario.run_auto().unwrap(), ExitCode::SUCCESS);
    let job = scenario.queued_job();
    scenario.wait_worker(&job);

    // Platform-service boundary: use the same durable transaction as install
    // and rollback, without launchd/systemd or installed-runtime mutations.
    let target_id = format!("rust-{}", &job.sha256[..16]);
    let target_dir = scenario.root.join("runtime/generations").join(&target_id);
    fs::create_dir_all(&target_dir).unwrap();
    let target_binary = target_dir.join("herdr-mcp");
    fs::copy(&job.binary_path, &target_binary).unwrap();
    verify_staged_file(&target_binary, &job.sha256).unwrap();
    state
        .stage_runtime_generation(
            &target_id,
            target_binary.to_str().unwrap(),
            &job.sha256,
            "service-install",
            20,
        )
        .unwrap();
    state
        .prepare_service_rollback(&ServiceRollbackRecord {
            rollback_id: "rb-auto-cross-epoch".to_owned(),
            source_kind: "rust".to_owned(),
            activated_generation_id: target_id.clone(),
            server_plist_backup: Some(
                scenario
                    .root
                    .join("server-backup.plist")
                    .to_string_lossy()
                    .into_owned(),
            ),
            watchdog_plist_backup: None,
            previous_current_target: Some(format!("generations/{previous_id}")),
            server_was_loaded: true,
            watchdog_was_loaded: false,
            created_at: 21,
            state: "prepared".to_owned(),
        })
        .unwrap();
    state
        .activate_runtime_generation_with_rollback(&target_id, Some("rb-auto-cross-epoch"), 22)
        .unwrap();
    let updates = UpdateStore::open(&RuntimePaths::discover().unwrap()).unwrap();
    updates
        .update_update_job(
            &job.job_id,
            "succeeded",
            Some("service install committed and health gate passed"),
            None,
            23,
        )
        .unwrap();
    assert_eq!(
        updates.latest_update_job().unwrap().unwrap().state,
        "succeeded"
    );
    drop(state);
    let mut state = StateStore::open_in_dir(&scenario.root, "state").unwrap();
    let rollback = state.latest_ready_service_rollback().unwrap().unwrap();
    assert_eq!(
        rollback.previous_current_target,
        Some(format!("generations/{previous_id}"))
    );
    assert_eq!(rollback.activated_generation_id, target_id);
    assert_eq!(
        state
            .active_runtime_generation()
            .unwrap()
            .unwrap()
            .generation_id,
        target_id
    );
    let claimed = state.begin_latest_service_rollback().unwrap().unwrap();
    state
        .complete_service_rollback(&claimed.rollback_id, &target_id, Some(&previous_id), 30)
        .unwrap();
    assert_eq!(
        state
            .active_runtime_generation()
            .unwrap()
            .unwrap()
            .generation_id,
        previous_id
    );
    assert_eq!(fs::read(previous_binary).unwrap(), previous_bytes);
    assert!(state.latest_ready_service_rollback().unwrap().is_none());
}
