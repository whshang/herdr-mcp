# G5 Link production cutover (WIP)

Status: alpha in progress. This folder is excluded from the docs site build.
Live production cutover is **not** performed by this document alone. Independent
Shell dual verification is mandatory before flipping LaunchAgents.

Related: [#65](https://github.com/whshang/herdr-mcp/pull/65) staged Rust daemon,
[#50](https://github.com/whshang/herdr-mcp/pull/50) / [#51](https://github.com/whshang/herdr-mcp/pull/51)
generation + control, [`docs/ga-release-gate.md`](../ga-release-gate.md) G5 (P0 #1).

## Current ownership (read-only truth)

As of alpha.11 on developer workstations (G3 user CLI sealed to
`runtime/current`; production Link ownership unchanged — do not cut live Node Link):

| Layer | Owner today | Evidence |
| --- | --- | --- |
| MCP runtime service `dev.herdr-mcp.server` | Rust generation under `runtime/current` (alpha.11 / `rust-30c3db71f6fa5a21`) | `herdr-mcp service status` / `--version` |
| User CLI `~/.local/bin/herdr-mcp` | Symlink → `runtime/current/herdr-mcp` (G3 sealed) | `ls -l` / `readlink` |
| Production Link `dev.herdr-mcp.link-prod` | **Node** `node` + checkout `dist/link/macos-daemon.js` (desired=active=stable-0.3.32) | LaunchAgent ProgramArguments unchanged through alpha.11 apply |
| Dev/canary Link `dev.herdr-mcp.link` | **Node** same daemon path | LaunchAgent ProgramArguments unchanged |
| Rust candidate LaunchAgent `dev.herdr-mcp.link-rust-candidate` | **Live soak** via `link install` → `runtime/current/herdr-mcp link run` (workstation `dev-rust-link-candidate`) | Distinct from Node `link` / `link-prod`; never cuts them |
| Rust `link::daemon` | Staged; CLI `link run` + candidate install/uninstall live on alpha.11 | No production LaunchAgent cutover |
| Health | `runtime=rust-candidate`, `production_ready=false` | `/health` + `native_migration`; `link status` `production_ready_eligible=false` |

Read-only / candidate / cutover-plan commands:

```bash
herdr-mcp link status
herdr-mcp link run         # foreground candidate only; does not cut production
herdr-mcp link install     # candidate LaunchAgent only (dev.herdr-mcp.link-rust-candidate)
herdr-mcp link uninstall   # removes candidate LaunchAgent only; never Node link/link-prod
herdr-mcp link cutover     # default dry-run: plan + validate only (exit 2 if not ready)
herdr-mcp link cutover --dry-run
# herdr-mcp link cutover --execute  # gated stub; requires HERDR_LINK_CUTOVER_I_UNDERSTAND=1 and still no-ops
herdr-mcp doctor           # LAYER link shows production_owner=node|rust|...
```

### Dry-run cutover helper (landed; not a live cut)

`herdr-mcp link cutover` (default `--dry-run`) is the production cutover **planner**:

- Reads Node `dev.herdr-mcp.link-prod` + candidate `dev.herdr-mcp.link-rust-candidate`.
- Validates planned ProgramArguments must be `runtime/current/herdr-mcp link run` (never checkout / `target/`).
- Prints the exact cutover steps and Node-plist rollback steps that **would** run.
- Sets `ready_for_execute=false` and exits non-zero when preconditions fail (missing managed runtime, unhealthy candidate, Node-era runtime-control generation, missing dual UAT record, etc.).
- **Never** bootouts/bootstraps launchd, never writes prod plists, never unloads Node `link` / `link-prod`.
- `--execute` is intentionally not implemented: even with `HERDR_LINK_CUTOVER_I_UNDERSTAND=1` it no-ops and reports `cutover_performed=false`.

**Hard rule:** landing this dry-run helper does **not** equal production cutover, does **not** flip G5 to PASS, and does **not** set `production_ready=true`.

From a worktree (does not write prod plists):

```bash
cargo run -p herdr-mcp -- link cutover --dry-run
# or, once installed into a generation that includes this CLI:
"$HOME/.config/herdr-mcp/runtime/current/herdr-mcp" link cutover --dry-run
```

## Blockers (ordered)

1. **Production LaunchAgent still Node + repo checkout** — both `link` and `link-prod` ProgramArguments point at `/usr/local/bin/node` and `.../herdr-mcp/dist/link/macos-daemon.js`, violating AGENTS.md (no launchd-to-checkout). Candidate soak on alpha.11 does **not** clear this.
2. **Runtime-control generation still Node-era** — prod control/status commonly keep `desired_active` / `active_generation` like `stable-0.3.32` even while MCP service is Rust alpha.
3. **Health seal** — `production_ready` and `runtime=rust-candidate` must not flip until ownership + UAT gates pass. Live `link status` still has six gates false (`launchd_prod_program_is_rust_runtime`, `launchd_not_repo_checkout`, `runtime_control_generation_rust_compatible`, `health_runtime_not_candidate`, `node_link_not_required`, `dual_verification_uat`).
4. **Production cutover execute missing** — dry-run / plan / validate helper landed (`herdr-mcp link cutover [--dry-run]`); real execute that bootouts Node prod, bootstraps Rust prod on `runtime/current`, and restores Node plist backup is **not implemented**. Dry-run ≠ cutover.
5. **User CLI** — G3 sealed on this machine (`~/.local/bin/herdr-mcp` → `runtime/current`); cutover tooling must still use `runtime/current/herdr-mcp`, never a checkout/`target/` binary. Clean-machine seal remains a separate G3/G18 gap.
6. **Credentials** — Link secret stays in Keychain; MCP token stays in server plist env. `link run` loads both; cutover must preserve both; never commit secrets.
7. **Dual verification UAT** — Edge → Link → Rust runtime → Herdr smoke after cutover, plus rollback to Node Link if needed, from an **independent** Shell (not a managed `herdr_exec` session). Required before any production cutover; not part of candidate soak.

### Alpha.11 candidate soak evidence (developer workstation)

- Release: `v0.4.0-alpha.11` / source `0ae559b` / generation `rust-30c3db71f6fa5a21`.
- Managed `update apply` from alpha.10 succeeded; Node `dev.herdr-mcp.link` / `link-prod` PIDs and ProgramArguments unchanged.
- `herdr-mcp link install` → label `dev.herdr-mcp.link-rust-candidate`, ProgramArguments `[runtime/current/herdr-mcp, link, run]`, loaded=true, workstation `dev-rust-link-candidate`.
- `link status`: `production_owner=node`, `production_ready_eligible=false`, `cutover_performed=false`; gates true only for `rust_cli_link_run` and `user_cli_not_repo_bash_bridge`.
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
2. Generation fencing: prod control file generations must address Rust MCP endpoint/version, not `stable-0.3.32`.
3. ~~Dry-run cutover planner~~ landed as `herdr-mcp link cutover` (default dry-run). Real execute (write plist, bootout Node prod, bootstrap Rust prod, restore backup) is still a later slice and still requires human dual verification.

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

### 3. Production cutover (explicit dual verification)

Only when `herdr-mcp link status` shows all gates except `dual_verification_uat` true:

1. Operator A captures preflight + Node plist backup.
2. Operator B (or second terminal) runs the cutover helper / manual bootstrap against **`runtime/current`**, not a worktree binary.
3. Both verify: launchd ProgramArguments Rust, Edge online, `tools/list` 18/18 epoch 2, sample inspect/fs/exec, `link status` owner=rust.
4. Only then flip health seal (`production_ready` / non-candidate runtime label) in a dedicated release commit.
5. If anything fails: restore Node plist backup, bootstrap Node link-prod, re-verify. Do not rebuild as rollback.

### 4. Post-cutover cleanup

- Remove repo-linked Node ProgramArguments from prod.
- Keep Node sources in repo for Edge/tests; remove user runtime dependency on Node link.
- Update `docs/ga-release-gate.md` G5 to PARTIAL/PASS with evidence SHAs.

## Explicit non-goals for this prep slice

- No live cutover of `dev.herdr-mcp.link-prod`.
- No unload/replace of live Node `dev.herdr-mcp.link` (candidate uses `link-rust-candidate`).
- No `launchctl submit`.
- No epoch / 18-tool contract change.
- No pointing launchd at checkout, worktree, or `target/`.
- No credential printing or Keychain deletion.

## Remaining G5 gaps after this slice

- [x] `herdr-mcp link status` + 8 `production_ready` gates (live on alpha.10+)
- [x] `herdr-mcp link run` (foreground) with Keychain/plist credential load
- [x] Candidate `herdr-mcp link install|uninstall` for `dev.herdr-mcp.link-rust-candidate` → `runtime/current` only (does not touch Node jobs)
- [x] Install into managed alpha.11 generation and start candidate LaunchAgent soak (developer workstation)
- [x] `herdr-mcp link cutover` dry-run / plan / validate (default dry-run; `--execute` stub no-ops). **Dry-run landed ≠ cutover.**
- [ ] Longer candidate Edge soak (reconnect / tool_result / cancel / generation activate)
- [ ] Production LaunchAgent cutover **execute** for `link-prod` on `runtime/current` (still missing)
- [ ] Rust-compatible runtime-control generation cutover for prod control files
- [ ] Health seal commit (`production_ready` / runtime label) after UAT
- [ ] Independent dual-verification UAT record (mandatory before live cut)
- [ ] Clean-machine confirmation of G3 user CLI seal (shared with G3/G18)
