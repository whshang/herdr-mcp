# G5 Link production cutover (WIP)

Status: alpha in progress. This folder is excluded from the docs site build.
Live production cutover is **not** performed by this document alone. Independent
Shell dual verification is mandatory before flipping LaunchAgents.

Related: [#65](https://github.com/whshang/herdr-mcp/pull/65) staged Rust daemon,
[#50](https://github.com/whshang/herdr-mcp/pull/50) / [#51](https://github.com/whshang/herdr-mcp/pull/51)
generation + control, [`docs/ga-release-gate.md`](../ga-release-gate.md) G5 (P0 #1).

## Current ownership (read-only truth)

As of the G5 prep slice on developer workstations (runtime already on
`0.4.0-alpha.9` with CLI aliases + doctor LAYER; Link ownership unchanged):

| Layer | Owner today | Evidence |
| --- | --- | --- |
| MCP runtime service `dev.herdr-mcp.server` | Rust generation under `runtime/current` (alpha.9) | `herdr-mcp service status` / `--version` |
| Production Link `dev.herdr-mcp.link-prod` | **Node** `node` + checkout `dist/link/macos-daemon.js` | LaunchAgent ProgramArguments |
| Dev/canary Link `dev.herdr-mcp.link` | **Node** same daemon path | LaunchAgent ProgramArguments |
| Rust `link::daemon` | Staged candidate assembly only | No production LaunchAgent; no `link run` yet |
| Health | `runtime=rust-candidate`, `production_ready=false` | `/health` + `native_migration` |

Read-only command (this PR):

```bash
herdr-mcp link status
herdr-mcp doctor   # LAYER link shows production_owner=node|rust|...
```

## Blockers (ordered)

1. **No Rust CLI `link run` / install lifecycle** — binary help previously promised link commands "as implementations land"; this slice adds **status only**. Foreground `link run` + managed LaunchAgent install still missing.
2. **Production LaunchAgent still Node + repo checkout** — both `link` and `link-prod` ProgramArguments point at `/usr/local/bin/node` and `.../herdr-mcp/dist/link/macos-daemon.js`, violating AGENTS.md (no launchd-to-checkout).
3. **Runtime-control generation still Node-era** — prod control/status commonly keep `desired_active` / `active_generation` like `stable-0.3.32` even while MCP service is Rust alpha.
4. **Health seal** — `production_ready` and `runtime=rust-candidate` must not flip until ownership + UAT gates pass.
5. **User CLI bridge** — `~/.local/bin/herdr-mcp` may still symlink to repo Bash wrapper (G3); cutover tooling must use `runtime/current/herdr-mcp`, not the checkout.
6. **Credentials** — Link secret stays in Keychain; MCP token stays in server plist env. Cutover must preserve both; never commit secrets.
7. **Dual verification UAT** — Edge → Link → Rust runtime → Herdr smoke after cutover, plus rollback to Node Link if needed, from an **independent** Shell (not a managed `herdr_exec` session).

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

Prerequisites still to implement after this status/gates slice:

1. `herdr-mcp link run` — foreground staged daemon; macOS Keychain + server-plist token load parity with Node `macos-daemon.ts`.
2. `herdr-mcp link install|status|uninstall` for **candidate** label first (`dev.herdr-mcp.link`), ProgramArguments = `runtime/current/herdr-mcp link run`, never checkout/`target/`.
3. Generation fencing: prod control file generations must address Rust MCP endpoint/version, not `stable-0.3.32`.
4. One-command cutover helper that: writes new plist beside old, bootouts Node prod, bootstraps Rust prod, verifies health/gates, and can revert to Node plist backup. Still requires human dual verification.

### 2. Candidate soak (safe)

```bash
# After link run/install exist on an installed generation:
# 1) Point only the non-prod label at Rust canary Edge/workstation IDs
# 2) Keep link-prod on Node
# 3) Soak reconnect / tool_result / cancel / generation activate
```

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
- No `launchctl submit`.
- No epoch / 18-tool contract change.
- No pointing launchd at checkout, worktree, or `target/`.
- No credential printing or Keychain deletion.

## Remaining G5 gaps after this slice

- [ ] `herdr-mcp link run` (foreground) with Keychain/plist credential load
- [ ] Candidate then production LaunchAgent install/uninstall on `runtime/current`
- [ ] Rust-compatible runtime-control generation cutover for prod control files
- [ ] Health seal commit (`production_ready` / runtime label) after UAT
- [ ] Independent dual-verification UAT record
- [ ] User CLI migration off Bash bridge (shared with G3)
