# Link production cutover contract

This is the durable safety contract for moving the production Herdr Link to the native runtime. Historical GA rehearsal logs and machine-specific evidence are intentionally not retained here.

## Production ownership invariants

Production readiness is derived from `herdr-mcp link status` and must remain fail-closed. The production path must not point to a repository checkout, build directory, or ad-hoc process. The active production LaunchAgent must execute the installed `runtime/current/herdr-mcp link run` path.

The required gates are:

| Gate | Requirement |
| --- | --- |
| `rust_cli_link_run` | installed runtime supports `herdr-mcp link run` |
| `launchd_prod_program_is_rust_runtime` | production LaunchAgent uses `runtime/current/herdr-mcp link run` |
| `launchd_not_repo_checkout` | production ownership never points into a checkout/worktree |
| `runtime_control_generation_rust_compatible` | desired and active generation use the supported native runtime contract |
| `health_runtime_not_candidate` | health metadata represents production ownership, not a candidate state |
| `user_cli_not_repo_bash_bridge` | user CLI resolves through the installed runtime |
| `node_link_not_required` | production path no longer depends on the legacy Node Link |
| `dual_verification_uat` | an independent operator verification has been explicitly recorded |

`production_ready` must remain false until every gate is true and the operator explicitly seals the cutover. Code must never infer the independent verification gate from process state alone.

## 1.2 Node Link and Server retirement

Starting with 1.2, the production Node Link and Node Server are retired. The
retired source surfaces are `src/server.ts`, `src/link/**`, `src/relay/**`, and
`bin/herdr-link`; PR-a records the boundary and ports the Node-only assertions,
while PR-b and PR-c remove the source and compatibility entrypoints. Within the
legacy Node runtime surface, only `bin/herdr-extension-host` remains retained.
Rust keeps the recognition and cleanup state machine for historical Node
production plists and `ServiceRollbackRecord` entries so old installation state
can be handled safely.

Production rollback moves between already installed Rust runtime generations
using recorded service state. It never rebuilds or starts `dist/` Node code and
never treats a source-tree Node process as a rollback target. This retirement
declaration does not unload, delete, rewrite, or otherwise mutate any existing
Node plist or daemon on a device.

The following table is the PR-a migration ledger. A row names the Node
assertions that depended on the retired implementation and the Rust or Edge
test that now owns the observable contract. Tests that only inspect a
retired wrapper or packaging path remain retirement canaries until PR-b/PR-c
remove that source.

| Node test and assertion group | Rust / Edge owner after PR-a |
| --- | --- |
| `tests/link-client.test.mjs`: canonical hello/tool result/tool error/cancel frames, request correlation, delivery states, queue bounds, duplicate IDs, frame limits, reconnect, fatal refusal, shutdown, foreign workstation filtering, and credential secrecy | `crates/herdr-mcp/src/link/socket_driver.rs` helper, handshake, frame, and channel tests; `crates/herdr-mcp/src/link/runner.rs` dispatch, cancel, timeout, generation, queue, foreign-workstation, and delivery-state tests; `crates/herdr-mcp/src/link/io_loop.rs` lifecycle and shutdown tests |
| `tests/link-local-mcp-transport.test.mjs`: loopback-only endpoint, epoch/hash validation, bearer handling, JSON/SSE parsing, ID correlation, timeout/cancel, malformed and oversized bodies, HTTP/JSON-RPC error sanitization, health probing, and token secrecy | `crates/herdr-mcp/src/link/local_mcp.rs::pure_helpers_match_node_oracle`, `construction_is_loopback_epoch_and_secret_fail_closed`, `dispatch_posts_exact_tools_call_and_preserves_full_result_envelope`, `jsonrpc_and_http_failures_are_sanitized`, `timeout_and_cancel_are_request_scoped`, `response_budget_and_health_version_cache_are_bounded` |
| `tests/link-daemon.test.mjs`: daemon defaults, missing credentials, WSS and contract-pair rejection, workstation ID validation, runtime identity, and frozen epoch-1 refusal/acceptance | `crates/herdr-mcp/src/link/daemon.rs` config, credential, contract, identity, and staged epoch-1 tests; `crates/herdr-mcp/src/link/edge_contract.rs::refuses_epoch1_dev_health` |
| `tests/link-lifecycle-policy-batch5.test.mjs`: phase set/order, hello-ack refusal classification, close-code policy, reconnect cap, heartbeat gate, and silence expiry/recycle | `crates/herdr-mcp/src/link/lifecycle.rs`, `policy.rs`, `heartbeat.rs`, and `io_loop.rs` shared batch-5 fixture tests |
| `tests/link-backoff.test.mjs` and `tests/link-reliability-batch4.test.mjs`: option sanitization, jitter/attempt/reset, runtime-generation ownership, drain, stale lease, and rollback-on-health-failure | `crates/herdr-mcp/src/link/backoff.rs`, `generation_fence.rs`, and `runtime_generation.rs` shared batch-4 and generation safety tests |
| `tests/runtime-generation.test.mjs`: candidate registration, exact contract/version checks, activation drain, stale lease fencing, and health failure rollback | `crates/herdr-mcp/src/link/runtime_generation.rs` candidate registration, activation, lease, managed-current, and rollback tests |
| `tests/link-relay-adapter-inbound.test.mjs`: canonical `tool_request` and `cancel` frames preserve every field when projected into the internal request shape | `crates/herdr-mcp/src/relay/wire.rs::shared_node_adapter_inbound_fixture_remains_valid_for_typed_decode`, including the exact internal projection from `tests/fixtures/relay-adapter-builders.json` |
| `tests/epoch1-runtime-profile.test.mjs`: frozen 17-tool count/hash and absence of `herdr_skill` | `edge/cloudflare/tests/mcp-handler.test.mjs::retired Node epoch1 profile keeps its frozen 17-tool contract identity`; Rust retains the supervised epoch-1 rollback identity checks in `link/daemon.rs` |
| `tests/epoch2-runtime-profile.test.mjs`: frozen 18-tool identity, `herdr_skill` presence, and all-tools incompatibility | Edge `mcp-handler.test.mjs` exercises all 18 epoch-2 tools; `crates/herdr-mcp/src/relay/contract.rs` pins the epoch-2 hash and `crates/herdr-mcp/src/contract.rs` pins the current 18-tool catalog. `HERDR_MCP_ALL_TOOLS` is a retired Node-only switch and has no Rust production equivalent. |
| `tests/self-update.test.mjs`: Node plist transformation and source-tree `dist/` exclusion assertions | Rust service-manager, updater, runtime-generation, and `ServiceRollbackRecord` tests own immutable installed-generation activation, rollback, legacy Node plist recognition/cleanup, and the prohibition on checkout or `dist/` ownership |
| `tests/agent-install-doc.test.mjs`, `tests/docs-command-contract.test.mjs`, `tests/release-surface.test.mjs`, and `tests/ci-test-selection.test.mjs`: legacy wrapper, documentation, packaging, and CI classification checks | The retirement and rollback boundary is declared above and in `AGENTS.md`; these tests remain source-removal and packaging canaries until PR-b/PR-c. They do not define a Rust or Edge runtime contract. |

## Cutover transaction

`herdr-mcp link cutover --execute` is a bounded ownership transaction:

1. validate all preconditions and prove the current production plist is an authentic Node rollback source before treating it as pre-Rust state;
2. back up that validated Node production Link LaunchAgent configuration without overwriting an existing unrelated/corrupt/Rust backup;
3. activate only the intended production owner using the installed runtime path;
4. verify ProgramArguments, launchd loaded state, runtime health and Link status;
5. on activation or verification failure, restore the previous production configuration;
6. keep unrelated development/candidate Link owners untouched;
7. do not mark `production_ready` merely because activation succeeded.

Cutover/rollback actions that affect production ownership must be initiated from an independent operator shell rather than from a Herdr-managed execution session whose connectivity depends on the path being changed.

`link cutover --dry-run` follows the same ownership rule as execute. A current Rust production plist is never described as a Node backup source and never receives a "Backup Node prod plist" step. When production is already managed Rust, dry-run points maintainers to either a real historical Node rollback backup or the explicit adoption path below.

## Runtime-control migration

Runtime-control migration and LaunchAgent ownership are separate concerns. Preparing or applying a compatible runtime-control generation does not by itself constitute a production cutover and must not set `production_ready`.

## Rollback

Rollback must restore the previous known-good production owner from the preserved backup, verify that it is loaded and reachable, and leave release generations intact. Never delete the active or rollback generation as part of cutover cleanup.

Before writing the production plist or mutating launchd, rollback validates the backup as a usable Node production plist: the label is exactly `dev.herdr-mcp.link-prod`, `ProgramArguments` classify as Node, the referenced Node executable exists, and the referenced Node Link daemon exists. A Rust, corrupt, stale, or incomplete backup fails closed while leaving the current production plist and launchd state untouched.

## Already-migrated Rust adoption

Some workstations may already have a healthy managed Rust production Link while the authentic historical Node rollback baseline is no longer recoverable. Do not reconstruct a historical backup and do not record a fake rollback UAT. Use the explicit migration evidence path only after independently proving the old baseline is unavailable:

```text
herdr-mcp link seal adopt-existing-rust --ack --reason "<operator reason>"
```

Adoption is guarded. It requires a loaded managed Rust production owner, healthy data-plane gates, aligned desired/active/loaded/current runtime generations, an explicit acknowledgement and non-empty operator reason, and the absence of a valid Node rollback backup. If a valid Node backup exists, adoption is refused.

The command records `irreversible-rust-adoption` evidence with `rollback_available=false`. It never writes `rollback-uat` evidence and never performs cutover, rollback, launchd, or seal mutation. `link seal status` and the final seal keep rollback UAT and irreversible adoption as distinct evidence classes. The seal requires independent dual-UAT evidence plus either a real rollback-UAT record or the explicit irreversible-adoption record.

## Secrets

Link credentials and runtime authentication material remain in their designated protected stores. Cutover tooling may preserve or reference them but must never print, migrate into repository files, or encode secrets into documentation/evidence.
