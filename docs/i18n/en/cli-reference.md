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
herdr-mcp update status
herdr-mcp rollback
herdr-mcp uninstall
```

`service ...`, `link ...`, `native-host ...`, `candidate` and `dev` are advanced/internal commands. Do not use a repository checkout, Node.js, npm or `service install` as the normal runtime installation path.

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

Static evidence is kept in a bounded capability inventory under the herdr-mcp config directory. Live status, cwd, project, pane, workspace and session state always come from Herdr/EventCache and are not replaced by the inventory. `herdr_inspect.capability_inventory.available_agents` exposes only locally available kinds permitted by `HERDR_MCP_AGENT_ALLOW`, so discovery improves delegation without bypassing the existing worker/auditor visibility policy.

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
| `HERDR_MCP_AGENT_ALLOW` | default workers/auditors | control agent visibility in inspect/since |
| `HERDR_MCP_ALL_TOOLS` | off | expose advanced/compatibility tools |
| `HERDR_SKILL_NETWORK` | on | `0` uses bundled skill only |

## Quick command map

| Goal | Command |
|---|---|
| runtime status | `herdr-mcp status` |
| follow logs | `herdr-mcp logs -f` |
| rebuild local runtime | `npm run build && herdr-mcp restart` |
| install browser bridge | `herdr-mcp native-host install` |
| preflight Cloudflare permissions | `bin/herdr-cloudflare-token ... --dry-run` |
| inspect A/B state | `bin/herdr-runtime-generation status` |
| rollback runtime | `bin/herdr-runtime-generation rollback` |
| inspect Custom Domain | `bin/herdr-cloudflare-domain status` |
