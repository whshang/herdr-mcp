# Runtime/Extension old<->new compatibility and rollback check (#586)

Checklist row covered: *"Verify Runtime/Extension old↔new capability negotiation and
rollback"* in [#586](https://github.com/whshang/herdr-mcp/issues/586).

Automated, repeatable entry point:

```bash
node scripts/check-compat-rollback.mjs            # full check (includes the cargo lane)
node scripts/check-compat-rollback.mjs --skip-cargo   # source + Extension lanes only
node scripts/check-compat-rollback.mjs --tag=v1.1.0-rc.9   # compare against another release plane
```

Exit code `0` = every covered check passed; `1` = a check failed; `2` = the script
could not resolve a real repository root.

## What the peer is, and why it is not a fixture

The "old Runtime" is not hand-copied into a fixture. Everything the check knows
about the older plane is read out of the tag's own source with `git show <tag>:<path>`:

| Peer fact | Read from |
| --- | --- |
| accepted fields per registry operation | `crates/herdr-mcp/src/mcp_http.rs` (`browser_registry_allow_fields`) |
| capability vocabulary | `crates/herdr-mcp/src/state_store.rs` (`ALLOWED_BROWSER_CAPABILITY_OPERATIONS`) |
| durable state schema version / migration count | `crates/herdr-mcp/src/state_store.rs` (`SCHEMA_VERSION`), `state_store/schema.rs` (`MIGRATIONS`) |

`scripts/compat/registry-contract.mjs` is the parser. If a released plane renames
or drops a field, the check fails instead of a stale fixture silently agreeing
with the new code.

## Isolation

* The check creates a throwaway tree with `mkdtemp` and sets `HOME`,
  `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME`,
  `HERDR_MCP_CONFIG_DIR`, `HERDR_MCP_DEV_STATE_DIR`, `HERDR_MCP_INSTANCE`,
  `HERDR_SOCKET_PATH`, and `TMPDIR` inside it.
* `env-isolation-contract` re-reads `crates/herdr-mcp/src/paths.rs` on every run
  and fails if the Runtime stops honouring one of those variables, so the
  isolation claim cannot rot into a comment.
* `isolation-real-config-untouched` snapshots every path under
  `~/.config/herdr-mcp` before and after and fails if a path was created or
  removed. Volatile subtrees a live production Runtime owns (`exec-spools/`,
  `logs/`, `sessions/`, `events/`, `*.spool`, `*.log`, SQLite `-wal`/`-shm`,
  sockets) are excluded from the *content* comparison because a running service
  rewrites them continuously; path identity is still compared.
* Forbidden in every lane: the `herdr-mcp update`, `update apply`,
  `update major-rollback`, `worker update`, `--with-worker` CLI paths,
  `launchctl`, `osascript`, and any network peer other than the none this check
  uses. The only spawned processes are `git`, `node --test`, and `cargo test`
  (updater/rollback logic is exercised through the Rust unit tests only).

## Result matrix (2026-11-02, peer `v1.1.0-rc.10` = `7f287ae19e0809cc5842a6b960cc3c959dca2bfd`, local `origin/main` `71ce6bda`)

| Item | Scenario | Coverage | Evidence |
| --- | --- | --- | --- |
| A | new Extension → rc.10 Runtime: source-turn observe with `profile_seed`/`endpoint_ref` provenance (#815) | **NEW** | `A-peer-source-turn-shape`, `A-extension-legacy-fallback` (`tests/compat-runtime-extension-negotiation.test.mjs`, 4 tests) |
| A | same, at the shipped-Extension level incl. late replay (#816) | EXISTING | `tests/manual/background_bind_test.mjs` N-1 section, now pinned to the tag's real allow-list by the NEW parser |
| A | new Extension → rc.10 Runtime: continuity/handoff durability probe (#812) | **NEW (source-level)** | `B-runtime-accepts-peer-payloads` shows no registry/route field was added for it; the probe's `resolveContinuityAvailability` degrades to `unknown`/`unavailable` on any unexpected peer answer |
| A | new Extension declares provider capabilities the rc.10 Runtime would reject | **NEW** | 4th node test: every declared operation is already in rc.10's `ALLOWED_BROWSER_CAPABILITY_OPERATIONS` |
| B | rc.10 Extension payloads → current Runtime (`endpoint.register`, `endpoint.consent`, `provider.observe` with the rc.10 capabilities document, legacy four-field `source_turn.observe`) | **NEW** | `B-runtime-accepts-peer-payloads` + `mcp_http::tests::compat_rc10_extension_payloads_stay_accepted_by_the_current_runtime` |
| B | current Runtime → rc.10 caller/MCP-side registry (`browser_registry_call_with_grants`) | NOT COVERED | The Extension never speaks that surface; it is a caller-tool registry with its own contract tests. Out of scope for extension↔runtime negotiation |
| C | capability vocabulary drift | **NEW** | `C-capability-vocabulary` (identical 27 operations on both planes) |
| C | newer peer protocol admitted, capability snapshot replaced | **NEW** | same Rust test (`adapter_protocol_version` 1 → 2, `observation_generation` bump) |
| C | stale/replayed capability generation rejected | EXISTING | `state_store` provider-observation tests (`stale_capability_generation`) |
| D | durable state written by this Runtime readable by rc.10 | **NEW (structural)** | `D-rollback-state-schema`: `SCHEMA_VERSION = 16` and 16 migrations on both planes, so the rc.10 binary opens a store written here |
| D | fail-closed refusal of a *newer* store instead of a silent downgrade | EXISTING, run by the NEW script | `state_store::tests::future_schema_version_fails_closed`, `schema_eight_binary_refuses_v12_and_requires_compatible_rollback_state` |
| D | rollback restores the previous schema before starting the old binary | EXISTING, run by the NEW script | `updater::tests::rollback_restores_schema_fifteen_before_starting_the_old_binary` |
| D | scheduled/auto update refuses an undeclared schema jump | EXISTING, run by the NEW script | `updater::scheduled_tests::auto_refuses_state_schema_migration_and_points_to_explicit_update` |
| D | end-to-end `herdr-mcp update` / `major-rollback` on a real generation | NOT COVERED (by design) | Forbidden in this check (service mutation, launchd, PROD). Live rollback evidence stays with [#728](https://github.com/whshang/herdr-mcp/issues/728) / #800 |
| D | Windows/Linux service-manager rollback | NOT COVERED | Platform lanes own their own service tests; this check is macOS-local and touches no service manager |

## Failure semantics worth knowing

* `D-rollback-state-schema` fails the moment the current tree adds a schema
  migration (state schema 17). That is deliberate: from then on a store written
  by this Runtime is *refused* by the rc.10 binary (fail-closed downgrade
  guard), so rolling back requires the `update major-rollback` backup path and an
  explicit release-note statement. Silence there would be the bug.
* `B-runtime-accepts-peer-payloads` fails when a new registry field is added
  without registering it in `ADDITIVE_REGISTRY_FIELDS`. Adding a field is
  compatible only if the Extension keeps working against a peer that rejects it;
  the registry entry is where that fallback is recorded.
* The check needs the compared tag in the local checkout (`git fetch --tags` in a
  shallow clone). A missing tag is a FAIL, not a skip, so a green run always
  means a real peer was compared.

## Not covered here, and where it lives

* Real Chrome + real connected WebChat regression (Extension packaging,
  MV3/service-worker restart, uncertain delivery): `tests/manual/extension_smoke.mjs`,
  `tests/manual/background_bind_test.mjs`, and live UAT on #586.
* Semantic provider configured/unconfigured/provider-error parity: `docs/semantic-routing.md`
  and the `semantic*` Rust tests.
* Page Assist / Connector grant boundaries: `webchat_grants` tests and #713.
