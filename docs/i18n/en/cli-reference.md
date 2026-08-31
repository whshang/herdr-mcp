# herdr-mcp CLI: local management and operations

This page covers **herdr-mcp** commands only. For Herdr workspace, pane, agent and session commands, use the official [Herdr CLI reference](https://herdr.dev/docs/cli-reference/).

## Daily runtime management

The normal user path is the native Rust CLI installed from a GitHub Release:

```bash
herdr-mcp install
herdr-mcp status
herdr-mcp doctor
herdr-mcp permissions status
herdr-mcp update check
herdr-mcp update apply
herdr-mcp update auto
herdr-mcp update status
herdr-mcp rollback
herdr-mcp reinstall
herdr-mcp uninstall
```

`update auto` is the scheduler entrypoint. On the default macOS production instance, `service install` reconciles the owned `dev.herdr-mcp.auto-update` LaunchAgent. It runs on load and then daily. Automatic installation is intentionally **PROD-runtime + Stable-release only**: a compiled DEV runtime, `[update] check = false`, named instances, or `preview` all skip before network access. When a strictly newer Stable Release exists, the command reuses the normal provenance-verified detached update transaction; it does not introduce a second downloader or bypass rollback gates. `service uninstall` first arms an owned durable update fence and removes the scheduler; detached workers re-check that fence before activation, so service removal cannot be undone by a queued silent update. An explicit successful install clears the fence.

`reinstall` is the product repair/replacement path. It re-applies the managed Rust service lifecycle while preserving configuration and credentials; runtime generations continue to follow normal service GC and retain the active/rollback-safe set. `uninstall` performs complete cleanup of **strongly owned herdr-mcp runtime/config state**. The default instance covers its service, owned auto-update scheduler, Link/watchdogs, Native Messaging host, managed user CLI, and config root; a named instance is intentionally limited to its own service/watchdogs/config and never takes ownership of default scheduler/Link/Native Host/user-CLI state. The default product uninstall intentionally leaves one small update-fence tombstone in the user cache after config deletion so a previously detached updater cannot resurrect the service; only an explicit successful install/reinstall clears it. Both deliberately do **not** uninstall or mutate the independent `herdr` executable, Herdr service/socket/config, browser extension account state, Cloudflare resources, macOS Keychain entries, or TCC authorization. `service uninstall` remains the narrower advanced service primitive.

`service ...`, `link ...`, `native-host ...` and `candidate` are advanced/internal commands. `dev` is an advanced **source-development** surface described below. Do not use a repository checkout, Node.js, npm or `service install` as the normal runtime installation path.

## Source-development runtime: DEV / PROD

v0.4.3+ has one explicit path for dogfooding herdr-mcp source without losing a stable recovery source:

```bash
herdr-mcp dev status
herdr-mcp dev sync --dry-run
herdr-mcp dev sync
herdr-mcp dev rollback
```

- `dev status` is read-only. It reports current runtime channel, active/dev/prod generations, source repo/branch/commit/dirty provenance, whether `runtime/current` matches recorded state, and whether the pinned PROD snapshot validates.
- `dev sync --dry-run` shows the intended transaction without building or switching runtime state.
- `dev sync` requires a clean source checkout by default, builds a DEV identity such as `0.4.3-dev`, pins the pre-existing PROD binary and SHA-256 evidence under `~/.config/herdr-mcp/runtime/channels/prod/`, then reuses the normal transactional service install path. Server, Native Host and `dev.herdr-mcp.link-prod` must reconcile to the same managed generation before activation is accepted.
- `dev sync --allow-dirty` is an explicit provenance override for deliberate local experiments. Do not make it the default.
- `dev rollback` verifies and reinstalls the pinned PROD binary. Repeated DEV syncs preserve that fixed PROD recovery source rather than treating the previous DEV generation as PROD.

DEV/PROD switching is local runtime lifecycle only. It does not deploy Cloudflare Edge, change DNS/OAuth, or create a third persistent test environment. Runtime DEV/PROD is independent from extension DEV/STANDALONE/STORE identity.

## macOS permissions

```bash
herdr-mcp permissions status
herdr-mcp permissions setup
herdr-mcp permissions verify
```

`status` is `granted`, `denied`, `needs_setup`, `unknown`, or `timeout`. `setup` may open Privacy & Security and does not grant access. `verify` checks a protected path. If file/git tools return `macos_tcc_access_blocked`, grant Files and Folders or Full Disk Access to `herdr-mcp-broker`, then verify again.

## Capability discovery: `scan`

`doctor` answers **“is this installation healthy?”**. `scan` answers **“which local agent capabilities are actually evidenced on this workstation?”**.

```bash
herdr-mcp scan
herdr-mcp scan --json
herdr-mcp scan --probe
herdr-mcp scan --refresh --probe
```

The scan intentionally does **not** reimplement Herdr's live-agent detection. It combines three evidence sources owned by the installed Herdr/runtime stack:

- `agent.list` is authoritative for live agent instances, status, pane/workspace and cwd;
- `server.agent_manifests` is authoritative for the detection manifests loaded by Herdr;
- the installed `herdr agent start --help` declaration is used to discover the agent kinds that this Herdr build says it can start.

herdr-mcp takes the bounded union of those kinds, looks for the corresponding executable on `PATH`, and records `herdr_startable`, `executable_available`, and the derived `available_for_start`. A kind is considered available for a new delegation only when the installed Herdr declares it startable **and** the executable is present. This avoids copying a stale hard-coded Agent-kind list into herdr-mcp while still recording workstation-local availability.

The default scan records bounded `--version` evidence only for explicitly allowlisted self-description adapters that have passed side-effect smoke tests. `--probe` additionally runs bounded, non-interactive `--help` adapters for capabilities that can be proven from the agent's own CLI. A discovered binary with no trusted adapter remains installed-but-unprobed. `--refresh` explicitly reloads Herdr's agent manifests and bypasses cached probe evidence.

Probe subprocesses receive no stdin, have a three-second timeout and a bounded output capture, and start from a cleared environment with only non-secret runtime variables restored. API keys, bearer tokens and provider credentials are not inherited. Unsupported or ambiguous traits remain unknown; herdr-mcp does not infer provider, model, vision, reasoning quality or code-edit support from an agent name.

Static evidence is kept in a bounded capability inventory under the herdr-mcp config directory. Live status, cwd, project, pane, workspace and session state always come from Herdr/EventCache and are not replaced by the inventory. `herdr_inspect.capability_inventory.available_agents` exposes every locally available discovered kind by default; `HERDR_MCP_AGENT_ALLOW` is an explicit operator restriction when a narrower view is required. Availability does not assign a role or require delegation: the Web planner decides from task structure, live load, verified capabilities and resource state, while unknown quality/cost/latency traits remain unknown.

### Dynamic planning advice for the Web planner

v0.4.3 keeps the 18-tool public MCP contract and does not add a dedicated planning tool. The progressive `herdr_skill` bootstrap advertises a read-only local method routed through the existing `herdr_call` tool:

```text
herdr_call(
  method="herdr_mcp.planning.advise",
  params={
    "project_root":"/path/to/project",
    "requires_code_edit":true,
    "requires_shell":true,
    "independent_units":2,
    "ownership_isolated":true
  }
)
```

The result separates evidence from the decision: live compatible/rejected workers, scan-proven startable-but-not-running Agent kinds, a direct deterministic option, a parallelism opportunity, and workspace/pane/worktree/utility-pane resource facts. The method never starts an Agent, creates a worktree, or auto-selects a worker. Explicit user targets are preserved; required capabilities fail closed when evidence is missing; optional quality/cost/latency traits remain unknown when unverified.

The Web planner can then choose direct execution, reuse an existing Agent, create one new lane, or parallelize only when work is genuinely independent and mutation ownership is isolated. Existing idle/done Agents, worktrees, and duplicate utility panes are exposed as reuse signals; cleanup remains planner-owned after completion rather than becoming a background cleanup daemon.

## Connector information

```bash
herdr-mcp connector
```

Use this to inspect Connector/public-entry information. Local static bearer credentials and ChatGPT OAuth are separate boundaries; `HERDR_MCP_TOKEN` does not belong in the ChatGPT Connector UI.

See [ChatGPT Connector](chatgpt-connector.md).

## Health and logs

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
herdr-mcp status
herdr-mcp logs
```

A local HTTP `200` or `401` proves that the runtime is listening. A connection error indicates a process/port problem.

## Watchdog

macOS can install the runtime watchdog:

```bash
herdr-mcp watchdog install
herdr-mcp watchdog status
```

The watchdog protects herdr-mcp availability. It does not treat every transient Herdr TaskGroup/ExceptionGroup error as a reason to restart the Herdr daemon.

## UI language

```bash
herdr-mcp lang en
herdr-mcp lang zh
herdr-mcp lang ja
```

The browser extension also supports English, Simplified Chinese and Japanese.

## Browser Native Messaging host

```bash
herdr-mcp native-host install
herdr-mcp native-host status
```

Primary path:

```text
Chrome extension
  ↓ Native Messaging
native host
  ↓ local Unix socket
herdr-mcp runtime
```

The browser does not need to store the Herdr bearer. See [Browser extension](extension.md).

## Cloudflare Edge credentials

```bash
bin/herdr-cloudflare-token --zone example.com --dry-run
bin/herdr-cloudflare-token --zone example.com
bin/herdr-cloudflare-token --zone example.com --verify-only
bin/herdr-cloudflare-token --zone example.com --rotate
```

See [Cloudflare Edge credentials](cloudflare-edge-token.md) for least-privilege handling.

## Custom Domain operations

```bash
bin/herdr-cloudflare-domain preflight
bin/herdr-cloudflare-domain status
bin/herdr-cloudflare-domain attach
bin/herdr-cloudflare-domain watch
bin/herdr-cloudflare-domain detach
```

Legacy CNAME/Tunnel migration only:

```bash
bin/herdr-custom-domain-cutover preflight
bin/herdr-custom-domain-cutover run
```

New installations do not need the cutover path. See [Cloudflare Edge deployment](cloudflare-edge-deployment.md).

## Runtime A/B

```bash
bin/herdr-runtime-generation status

bin/herdr-runtime-generation register \
  --generation <id> \
  --endpoint http://127.0.0.1:8773/mcp \
  --runtime-version <version>

bin/herdr-runtime-generation activate --generation <id>
bin/herdr-runtime-generation rollback
bin/herdr-runtime-generation remove --generation <id>
```

Generation management is for implementation changes within the same public contract epoch: start candidate → verify health/contract → activate → drain old generation. See [Runtime A/B](runtime-self-upgrade.md).

## Self update

```bash
bin/herdr-self-update
```

The supervised updater reuses generation activation and must not be used to silently cross a public contract epoch.

## Workstation link

`bin/herdr-link` maintains the workstation's outbound authenticated WSS connection to Edge. It carries workstation identity and routes requests to the current active runtime generation. It is normally service-managed rather than run manually.

## Common environment variables

| Variable | Default | Purpose |
|---|---|---|
| `HERDR_MCP_PORT` | `8772` | local runtime HTTP port |
| `HERDR_MCP_TOKEN` | empty | local curl/Cursor bearer; not ChatGPT |
| `HERDR_MCP_BASE_URL` | empty | public OAuth/MCP origin, without `/mcp` |
| `HERDR_SOCKET_PATH` | `~/.config/herdr/herdr.sock` | Herdr Socket API |
| `HERDR_MCP_READONLY` | off | disable mutation |
| `HERDR_MCP_WRITE_ROOTS` | managed roots | restrict writable projects |
| `HERDR_MCP_AGENT_ALLOW` | all discovered agents | optionally restrict agent visibility in inspect/since |
| `HERDR_MCP_ALL_TOOLS` | off | expose advanced/compatibility tools |
| `HERDR_SKILL_NETWORK` | on | `0` uses bundled skill only |

## Quick command map

| Goal | Command |
|---|---|
| runtime status | `herdr-mcp status` |
| follow logs | `herdr-mcp logs -f` |
| dogfood current source as DEV runtime | `herdr-mcp dev sync` |
| inspect DEV/PROD provenance | `herdr-mcp dev status` |
| return from DEV to pinned PROD | `herdr-mcp dev rollback` |
| install browser bridge | `herdr-mcp native-host install` |
| preflight Cloudflare permissions | `bin/herdr-cloudflare-token ... --dry-run` |
| inspect A/B state | `bin/herdr-runtime-generation status` |
| rollback runtime | `bin/herdr-runtime-generation rollback` |
| inspect Custom Domain | `bin/herdr-cloudflare-domain status` |
