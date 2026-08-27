# G5 Link production cutover (WIP)

Status: alpha in progress. This folder is excluded from the docs site build.
Developer-workstation **LaunchAgent cutover for `link-prod` landed 2026-08-27**
(alpha.13). `production_ready` remains **false** until an auditable seal exists.
"Dual verification" here means two independent observation passes by the same
operator session (not a second human).

Related: [#65](https://github.com/whshang/herdr-mcp/pull/65) staged Rust daemon,
[#96](https://github.com/whshang/herdr-mcp/pull/96) cutover execute,
[#97](https://github.com/whshang/herdr-mcp/pull/97) cutover harden + alpha.13,
[`docs/ga-release-gate.md`](../ga-release-gate.md) G5.

## Current ownership (read-only truth)

As of **alpha.13** on this developer workstation after live `link cutover --execute`
(2026-08-27T15:18:47Z UTC, independent Shell only):

| Layer | Owner today | Evidence |
| --- | --- | --- |
| MCP runtime service `dev.herdr-mcp.server` | Rust generation under `runtime/current` (**alpha.13** / `rust-5c7799b56a426855`) | `herdr-mcp service status` / `--version` |
| User CLI `~/.local/bin/herdr-mcp` | Symlink → `runtime/current/herdr-mcp` (G3 sealed) | `ls -l` / `readlink` |
| Production Link `dev.herdr-mcp.link-prod` | **Rust** `runtime/current/herdr-mcp link run` | ProgramArguments after execute; PID changed Node `10621` → Rust `4745`/`6131` |
| Prod runtime-control / status | Rust-compatible ids (desired still `rust-6e3f0b…` no-op migrate; status active observed `rust-5c7799…`) | same loopback `127.0.0.1:8772/mcp` |
| Dev/canary Link `dev.herdr-mcp.link` | **Node** unchanged | PID `3937` preserved through cutover |
| Rust candidate `dev.herdr-mcp.link-rust-candidate` | argv `runtime/current link run`; **Edge retargeted to edge-prod (epoch 2)**; soak PID online with MCP+Edge TCP | Do not treat candidate health as prod blocker once prod Edge is online |
| Health | `/health` has no `production_ready=true`; `link status` `production_ready_eligible=false` | seal still open |

### Live cutover evidence (developer workstation)

Preconditions unlocked by [#97](https://github.com/whshang/herdr-mcp/pull/97) / tag
`v0.4.0-alpha.13` (source `4fde2a8`): MUST-FIX backup non-overwrite + fail-closed
`launchctl print` probe + Edge `build_edge_url(workstation_id)`.

Independent Shell sequence (no `herdr_exec`, no `launchctl submit`):

1. `update apply` → `0.4.0-alpha.13` / `rust-5c7799b56a426855` healthy; Node `link-prod` still Node.
2. `link cutover --dry-run` → `execute_implemented=true`, `ready_for_execute=true` (dual UAT seal gate only `NO`).
3. Extra Node backup: `~/.config/herdr-mcp/backups/link-prod.plist.node-pre-cutover-20260827T230026`.
4. `HERDR_LINK_CUTOVER_I_UNDERSTAND=1 herdr-mcp link cutover --execute` → `ok=true`, `phase=VERIFY`, `production_ready=false`, backup `…/link-prod.plist.pre-rust-cutover` (Node bytes).

**Pass A** (2026-08-27T15:19:03Z): `link status` `production_owner=rust`; doctor `LAYER link owned production_owner=rust`; `/health` 200 alpha.13; prod argv managed; canary Node untouched.

**Pass B** (fresh shell 15:19:30Z, pid 5612): `tools/list` **18**; service healthy `rust-5c7799…`; prod Edge TCP `10.10.7.150→172.67.169.114:443` ESTABLISHED across 6 samples; kickstart reconnect kept Edge+MCP sockets.

Deliberate **rollback UAT** (restore Node plist + bootstrap) still **pending** — do not delete Node backups.

### P0-3 candidate soak deepen (same day, pre-cutover)

Against candidate only: reconnect + backoff bursts proven; generation activate + stale fail-closed on candidate `runtime-control.json` proven; heartbeat/cancel/long-request blocked until Edge URL fix (landed in alpha.13). Post-cutover candidate LaunchAgent currently exits `contract_rejected` after kickstart — separate follow-up, not a reason to undo prod cutover.

Read-only / candidate / cutover-plan commands:

```bash
herdr-mcp link status
herdr-mcp link run         # foreground candidate only; does not cut production
herdr-mcp link install     # candidate LaunchAgent only (dev.herdr-mcp.link-rust-candidate)
herdr-mcp link uninstall   # removes candidate LaunchAgent only; never Node link/link-prod
herdr-mcp link cutover     # default dry-run: plan + validate only (exit 2 if not ready)
herdr-mcp link cutover --dry-run
# herdr-mcp link cutover --execute  # requires HERDR_LINK_CUTOVER_I_UNDERSTAND=1; PREPARE/ACTIVATE/VERIFY + ROLLBACK for link-prod only; never auto-seals production_ready; independent Shell only
herdr-mcp link migrate-runtime-control           # default dry-run: plan Rust-compatible prod control
herdr-mcp link migrate-runtime-control --write-staging
# HERDR_LINK_MIGRATE_RUNTIME_CONTROL=1 herdr-mcp link migrate-runtime-control --apply
herdr-mcp doctor           # LAYER link shows production_owner=node|rust|...
```

### Dry-run cutover helper (landed; not a live cut)

`herdr-mcp link cutover` (default `--dry-run`) is the production cutover **planner**:

- Reads Node `dev.herdr-mcp.link-prod` + candidate `dev.herdr-mcp.link-rust-candidate`.
- Validates planned ProgramArguments must be `runtime/current/herdr-mcp link run` (never checkout / `target/`).
- Prints the exact cutover steps and Node-plist rollback steps that **would** run.
- Sets `ready_for_execute` from technical preconditions (managed runtime, candidate, ownership, rust control generation). `dual_verification_uat_recorded` is a **seal** blocker, not an execute blocker.
- **Never** bootouts/bootstraps launchd, never writes prod plists, never unloads Node `link` / `link-prod`.

### Execute cutover transaction (landed; do not run casually)

`herdr-mcp link cutover --execute` with `HERDR_LINK_CUTOVER_I_UNDERSTAND=1`:

- PREPARE: backup Node prod plist → `~/.config/herdr-mcp/backups/link-prod.plist.pre-rust-cutover`
- ACTIVATE: rewrite **only** `dev.herdr-mcp.link-prod` to `runtime/current/herdr-mcp link run`, bootout/bootstrap (never inferred launchd submission jobs)
- VERIFY: ProgramArguments + loaded; on failure ROLLBACK restores Node backup
- Leaves `dev.herdr-mcp.link` and `dev.herdr-mcp.link-rust-candidate` untouched
- Never flips `production_ready` / health seal
- Must run from an **independent Shell** (not managed `herdr_exec`)

### Runtime-control migration helper (landed; not a LaunchAgent cut)

`herdr-mcp link migrate-runtime-control` prepares a Rust-compatible
`runtime-control-prod.json` from the active managed generation
(`runtime/current` → `rust-*`), keeping the existing loopback MCP endpoint and
bumping `revision`. Modes:

| Mode | Effect |
| --- | --- |
| default / `--dry-run` | Plan + validate only; writes nothing |
| `--write-staging` | Writes `runtime-control-prod.rust-pending.json` only |
| `--apply` | Requires `HERDR_LINK_MIGRATE_RUNTIME_CONTROL=1`; backups then rewrites live control; **never** mutates LaunchAgents / `runtime/current` / status |

After `--apply`, Node `link-prod` (still production) must poll the new revision and
activate before `runtime_control_generation_rust_compatible` can pass. Dual UAT
and LaunchAgent ownership gates remain separate blockers for `ready_for_execute`.

**Safety note (developer workstation, 2026-08-27):** Node Link generation ids are
opaque labels (`^[A-Za-z0-9_.-]{1,64}$`) bound to a loopback MCP endpoint. Prod
was already proxying Rust MCP under `stable-0.3.32`. Applying `desired=rust-*`
with the same `http://127.0.0.1:8772/mcp` endpoint while LaunchAgent still runs
Node `dist/link` is a control-document label migration, not a binary cutover.
Failure mode keeps prior active generation (cannot remove active) and can restore
the control backup; success flips status `active_generation` without touching
launchd. Do **not** treat this as permission to cut Node LaunchAgents.

**Hard rule:** landing this dry-run helper does **not** equal production cutover, does **not** flip G5 to PASS, and does **not** set `production_ready=true`.

From a worktree (does not write prod plists):

```bash
cargo run -p herdr-mcp -- link cutover --dry-run
# or, once installed into a generation that includes this CLI:
"$HOME/.config/herdr-mcp/runtime/current/herdr-mcp" link cutover --dry-run
```

## Blockers (ordered)

1. **Production LaunchAgent still Node + repo checkout** — both `link` and `link-prod` ProgramArguments point at `/usr/local/bin/node` and `.../herdr-mcp/dist/link/macos-daemon.js`, violating AGENTS.md (no launchd-to-checkout). Candidate soak + runtime-control migrate do **not** clear this.
2. ~~**Runtime-control generation still Node-era**~~ — **cleared on this developer workstation** after alpha.12 gated `--apply`: desired/active=`rust-6e3f0b8685b89e66`, status outcome=`activated`. Other machines may still need `--apply`. Helper never mutates LaunchAgents.
3. **Health seal** — `production_ready` and `runtime=rust-candidate` must not flip until ownership + UAT gates pass. Live `link status` still has five gates false (`launchd_prod_program_is_rust_runtime`, `launchd_not_repo_checkout`, `health_runtime_not_candidate`, `node_link_not_required`, `dual_verification_uat`); `runtime_control_generation_rust_compatible` is now true on this machine.
4. **Production cutover execute missing** — dry-run / plan / validate helper landed (`herdr-mcp link cutover [--dry-run]`); real execute that bootouts Node prod, bootstraps Rust prod on `runtime/current`, and restores Node plist backup is **not implemented**. Dry-run ≠ cutover. `ready_for_execute` remains false.
5. **User CLI** — G3 sealed on this machine (`~/.local/bin/herdr-mcp` → `runtime/current`); cutover tooling must still use `runtime/current/herdr-mcp`, never a checkout/`target/` binary. Clean-machine seal remains a separate G3/G18 gap.
6. **Credentials** — Link secret stays in Keychain; MCP token stays in server plist env. `link run` loads both; cutover must preserve both; never commit secrets.
7. **Dual verification UAT** — Edge → Link → Rust runtime → Herdr smoke after cutover, plus rollback to Node Link if needed, from an **independent** Shell (not a managed `herdr_exec` session). Required before any production cutover; not part of candidate soak.

### Alpha.12 MCP update + runtime-control migrate evidence (developer workstation)

- Release: `v0.4.0-alpha.12` / source `94f5226` / generation `rust-6e3f0b8685b89e66` (from alpha.11 / `rust-30c3db71f6fa5a21`).
- Managed `update apply` succeeded; Node `dev.herdr-mcp.link` / `link-prod` ProgramArguments unchanged (still `node` + checkout `dist/link`); candidate remain loaded.
- Safety check before `--apply`: Node `RuntimeControlLoop` activates by validating loopback MCP health + contract; generation id is a label. Prod already served Rust MCP under `stable-0.3.32`.
- `link migrate-runtime-control --dry-run` / `--write-staging` then `HERDR_LINK_MIGRATE_RUNTIME_CONTROL=1 ... --apply`: backup under `~/.config/herdr-mcp/backups/`; live control rev 73 desired=`rust-6e3f0b8685b89e66`; status polled to `active=rust-6e3f0b8685b89e66` / outcome=`activated` within ~2s.
- `link status`: gates true = `rust_cli_link_run`, `runtime_control_generation_rust_compatible`, `user_cli_not_repo_bash_bridge`; `production_owner=node`, `production_ready_eligible=false`.
- `link cutover --dry-run`: `ready_for_execute=false`, `execute_implemented=false`; precondition `runtime_control_generation_rust_compatible` now true; dual UAT still false.

### Alpha.11 candidate soak evidence (developer workstation)

- Release: `v0.4.0-alpha.11` / source `0ae559b` / generation `rust-30c3db71f6fa5a21`.
- Managed `update apply` from alpha.10 succeeded; Node `dev.herdr-mcp.link` / `link-prod` PIDs and ProgramArguments unchanged.
- `herdr-mcp link install` → label `dev.herdr-mcp.link-rust-candidate`, ProgramArguments `[runtime/current/herdr-mcp, link, run]`, loaded=true, workstation `dev-rust-link-candidate`.
- Pre-alpha.12 `link status`: gates true only for `rust_cli_link_run` and `user_cli_not_repo_bash_bridge`.
- `doctor` LAYER link: `production_owner=node` / `production_ready_eligible=false`.
## Gates that must all be true before `production_ready=true`

These IDs are embedded in health metadata (`native_migration.link_cutover.requires_all`)
and evaluated by `herdr-mcp link status`:

| Gate ID | Meaning |
| --- | --- |
| `rust_cli_link_run` | Installed binary can run `herdr-mcp link run` (foreground) |
| `launchd_prod_program_is_rust_runtime` | `dev.herdr-mcp.link-prod` ProgramArguments[0] is `~/.config/herdr-mcp/runtime/current/herdr-mcp` with `link run` |
| `launchd_not_repo_checkout` | Prod plist does not point at repo/`worktree` `dist/link` |
| `runtime_control_generation_rust_compatible` | desired/active generation is Rust-era (not `stable-0.3.*`) |
| `health_runtime_not_candidate` | Health no longer stuck on `rust-candidate` after seal |
| `user_cli_not_repo_bash_bridge` | `~/.local/bin/herdr-mcp` → `runtime/current/herdr-mcp` |
| `node_link_not_required` | Production path does not require Node link binary |
| `dual_verification_uat` | Independent operator UAT recorded; never auto-flipped by code |

**Hard rule:** code must keep `production_ready=false` until every gate is true **and** an operator explicitly seals after dual verification. This prep slice does not flip the flag.

## Ordered cutover steps (operator Shell only)

Do **not** run these from a managed `herdr_exec` session. Do **not** use `launchctl submit`.

### 0. Preflight (read-only)

```bash
git -C /Users/qingxian/Documents/herdr-mcp status --short --branch
ls -l "$HOME/.local/bin/herdr-mcp"
readlink "$HOME/.config/herdr-mcp/runtime/current" || true
"$HOME/.config/herdr-mcp/runtime/current/herdr-mcp" --version
"$HOME/.config/herdr-mcp/runtime/current/herdr-mcp" service status
"$HOME/.config/herdr-mcp/runtime/current/herdr-mcp" link status
launchctl list | awk '$3 ~ /^dev\.herdr-mcp\.(server|link)/ { print }'
/usr/libexec/PlistBuddy -c 'Print :ProgramArguments' \
  "$HOME/Library/LaunchAgents/dev.herdr-mcp.link-prod.plist"
```

Stop if Link prod is already Rust, or if service is unhealthy.

### 1. Land remaining code (not tonight if unsafe)

Prerequisites still to implement after status + `link run`:

1. ~~`herdr-mcp link install|uninstall` for candidate~~ landed and **soaked on alpha.11** as `dev.herdr-mcp.link-rust-candidate` → `runtime/current/herdr-mcp link run` (never checkout/`target/`; never mutates Node `link`/`link-prod`).
2. ~~Generation fencing helper~~ landed as `herdr-mcp link migrate-runtime-control`; **live-applied on this developer workstation with alpha.12** (desired/active=`rust-6e3f0b8685b89e66`). **Not** equal to LaunchAgent cutover.
3. ~~Dry-run + execute~~ landed ([#96](https://github.com/whshang/herdr-mcp/pull/96)/[#97](https://github.com/whshang/herdr-mcp/pull/97)); **live developer-workstation execute recorded above** (dual self-observation passes). Seal / `production_ready` still open.

### 2. Candidate soak (safe)

```bash
# alpha.11 installed generation includes link install:
# 1) herdr-mcp link install   # bootstraps dev.herdr-mcp.link-rust-candidate only
# 2) Keep Node link + link-prod untouched
# 3) Soak reconnect / tool_result / cancel / generation activate on workstation id
#    dev-rust-link-candidate (distinct from Node canary)
# 4) herdr-mcp link uninstall when done
```

Developer workstation (2026-08-27): steps 1–2 completed on alpha.11; longer Edge soak / tool_result matrix still open before any prod cut.

### 3. Production cutover (dual self-observation)

Developer workstation: **done** on alpha.13 (see evidence above). Remaining:

1. Record dual verification timestamps/commands in this doc (Pass A/B above).
2. Do **not** flip `production_ready` until seal design + written evidence exist.
3. Keep Node backups; schedule deliberate rollback UAT later.
4. If execute fails mid-flight: restore Node backup, bootstrap Node link-prod, re-verify. Do not rebuild as rollback.

### 4. Post-cutover cleanup

- Remove repo-linked Node ProgramArguments from prod.
- Keep Node sources in repo for Edge/tests; remove user runtime dependency on Node link.
- Update `docs/ga-release-gate.md` G5 to PARTIAL/PASS with evidence SHAs.

## Explicit non-goals (still)

- No unload/replace of live Node `dev.herdr-mcp.link` canary.
- No `launchctl submit`.
- No epoch / 18-tool contract change.
- No pointing launchd at checkout, worktree, or `target/`.
- No credential printing or Keychain deletion.
- No `production_ready=true` without seal design + evidence.

## Remaining G5 gaps after this slice

- [x] `herdr-mcp link status` + gates (live on alpha.10+)
- [x] `herdr-mcp link run` + candidate install/uninstall
- [x] `link cutover` dry-run + **execute** (alpha.13; developer workstation live)
- [x] `migrate-runtime-control` gated `--apply` (control file only)
- [x] Dual self-observation Pass A/B recorded (this doc)
- [x] Candidate `contract_rejected` root-caused: edge-dev `/health` still publishes epoch 1 while Rust hello is epoch 2; candidate defaults + install probe retarget to edge-prod (epoch 2). Live soak: PID online, MCP `8772` + Edge TCP ESTABLISHED; link-prod untouched
- [ ] Deliberate Node rollback UAT via `link cutover --rollback` (alpha.14+) without deleting artifacts
- [ ] Auditable `production_ready` seal via `link seal` (alpha.14+; rollback clears seal)
- [ ] Longer candidate Edge soak matrix (heartbeat/cancel/long-request) on edge-prod
- [ ] Clean-machine confirmation of G3 user CLI seal (shared with G3/G18)

### Candidate contract fix evidence (2026-08-27)

Diagnosis (independent Shell):

- `herdr-edge-dev` `/health` → `contractEpoch:1` / epoch1 hash
- `herdr-edge-prod` `/health` → `contractEpoch:2` / public epoch2 hash
- Rust hello with epoch2 against edge-dev → `hello_ack` `contract_mismatch` → exit `contract_rejected`
- Same binary against edge-prod + prod Keychain + `dev-rust-link-candidate` → stays running with Edge+MCP TCP

Fix (alpha.14 source):

- Candidate / `link run` defaults: `wss://herdr-edge-prod.../ws` + `herdr-edge-prod-link-secret`
- `link install` and `link run` probe Edge `/health` and refuse epoch-1 Edges before bootstrap/connect
- hello_ack refusal logs `code` + `message` on stderr

Live reinstall (candidate only; ProgramArguments still `runtime/current`):

- `edge_contract_epoch=2`, `edge_service=herdr-edge-prod`
- LaunchAgent PID running; `localhost:8772` + Cloudflare HTTPS ESTABLISHED
- `dev.herdr-mcp.link-prod` PID unchanged through reinstall
