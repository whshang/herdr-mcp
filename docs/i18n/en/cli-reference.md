# herdr-mcp CLI: local management and operations

This page covers **herdr-mcp** commands only. For Herdr workspace, pane, agent and session commands, use the official [Herdr CLI reference](https://herdr.dev/docs/cli-reference/).

## Daily runtime management

On macOS, the main management CLI controls the LaunchAgent-backed runtime:

```bash
herdr-mcp status
herdr-mcp start
herdr-mcp stop
herdr-mcp restart
herdr-mcp logs
herdr-mcp logs -f
```

Typical development loop:

```bash
npm run build
herdr-mcp restart
herdr-mcp status
```

For temporary development, run the Node process directly:

```bash
npm run dev
# or
node dist/server.js
```

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
bin/herdr-extension-host install
bin/herdr-extension-host status
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
| install browser bridge | `bin/herdr-extension-host install` |
| preflight Cloudflare permissions | `bin/herdr-cloudflare-token ... --dry-run` |
| inspect A/B state | `bin/herdr-runtime-generation status` |
| rollback runtime | `bin/herdr-runtime-generation rollback` |
| inspect Custom Domain | `bin/herdr-cloudflare-domain status` |
