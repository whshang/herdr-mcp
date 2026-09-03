# Troubleshooting

*Locate the failing layer before restarting everything.*

herdr-mcp spans Herdr, the local runtime, workstation link, Cloudflare Edge, OAuth/MCP and browser continuity. The fastest diagnosis is to find the broken layer first.

Use this order:

```text
Herdr
  ↓
local herdr-mcp runtime
  ↓
workstation link
  ↓
Cloudflare Edge
  ↓
OAuth / MCP
  ↓
ChatGPT tool snapshot
  ↓
browser continuity
```

If one layer is broken, do not start by reconfiguring a later one.

## 30-second triage

### Is the local HTTP runtime listening?

```bash
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8772/
```

`200` or `401` means a process is listening. Connection failure means the runtime is down, on another port or otherwise unreachable.

On macOS LaunchAgent installations:

```bash
herdr-mcp status
herdr-mcp logs
herdr-mcp permissions status
```

If file/git tools return `macos_tcc_access_blocked`, run `herdr-mcp permissions setup`, grant access to `herdr-mcp-broker`, then `herdr-mcp permissions verify`. Ordinary `not_found` errors are unrelated.

### Is Herdr itself available?

```bash
herdr --version
herdr api schema >/dev/null
```

If local HTTP works but `herdr_inspect` cannot see real workspaces, investigate the Herdr daemon/socket before touching Cloudflare.

### Is the binary installed but off the shell PATH?

```bash
ls -l ~/.local/bin/herdr-mcp
zsh -ic 'command -v herdr-mcp'
```

A present binary with an empty `command -v` result is `installed_but_not_on_shell_path`, not a missing installation. Export `~/.local/bin` for the current process, persist the same line idempotently in the shell startup file, and do not reinstall or create a second PATH owner.

### Can Edge see the workstation?

OAuth may succeed while the workstation is offline. Public login success does not prove the local development machine is connected.

Check Edge health/status and the workstation link.

### Is the link blocked by the local network?

The Link reuses proxies that already exist in the environment. Resolution order:

```text
HERDR_LINK_PROXY > HTTPS_PROXY/https_proxy > HTTP_PROXY/http_proxy > ALL_PROXY/all_proxy
  > macOS system proxy (scutil --proxy: HTTPS, then HTTP, then SOCKS)
```

Details that matter when `workers.dev` is unreachable:

- `socks5://` and `socks5h://` are supported. SOCKS5 dials use remote-DNS semantics: the hostname is sent to the proxy unresolved, so a locally polluted DNS resolver cannot break `workers.dev` connectivity.
- Proxy authentication (HTTP Basic or SOCKS5 username/password) is not supported; proxy URLs with embedded credentials are rejected, and credentials never appear in status or error output.
- On macOS, a PAC configuration is detected but never evaluated. Link does not fetch or execute PAC scripts; with only a PAC configured, the Link connects directly.
- Do not "fix" link connectivity by disabling TLS, rewriting system proxy settings, or turning the Link into a general-purpose forwarder.

### Does a new ChatGPT conversation get the current catalog?

The current public ChatGPT contract is **epoch 3 / 19 actions**. Workstation execution remains **epoch 2 / 18 tools**, including `herdr_skill`; the extra public action is Edge-local `herdr_devices`.

Old conversations may retain an older `tools/list` snapshot. Before reinstalling anything, verify the server and open a new conversation.

## Symptom: Cloudflare API returns 403 while the token verifies as valid

`/user/tokens/verify` only proves the token is active. A 403 on a specific call names a missing permission:

- `GET .../accounts/<id>/workers/subdomain` fails → **Account Settings Read** is missing;
- Worker script deploy calls fail → **Workers Scripts Edit** is missing;
- R2 provisioning fails → the optional **Workers R2 Storage Edit** was never granted. The core install does not need it; this is an error only when the user explicitly enabled the artifact relay.

Grant exactly the missing permission and retry. Do not recreate a broader token blindly and do not report it as a generic deployment failure.

## Symptom: one hostname fails while another hostname of the same Worker works

If `*.workers.dev` times out or fails DNS/TLS while the same Worker's Custom Domain returns `/health` 200 (or the reverse), the Worker code is healthy and the failure is hostname/DNS/network-path specific. Do not redeploy the Worker and do not create another Worker/R2/Connector. Prefer a Custom Domain as the stable production origin when the user owns a domain; otherwise keep `workers.dev` and let the Link transport fallback (direct → validated local proxy → shared relay) handle the network path.

## Symptom: Connector cannot be added or OAuth loops

Check:

- MCP URL is `https://<stable-origin>/mcp`;
- public base URL / OAuth issuer use the same origin without `/mcp`;
- protected-resource and authorization-server metadata are reachable;
- the Worker deployment is the intended one;
- the ChatGPT Workspace permits Developer/custom MCP apps.

If an OAuth token is successfully issued and the next step fails, you are probably past authentication and into MCP discovery/routing.

See [ChatGPT Connector](chatgpt-connector.md).

## Symptom: Connector says connected, but the chat has 0 tools

Connector installation and conversation tool acceptance are different steps.

Check in this order:

1. open a new conversation;
2. confirm current public contract/version;
3. verify `tools/list` succeeds;
4. check whether one incompatible `inputSchema` is causing the catalog to be rejected;
5. distinguish a stale conversation snapshot from a server problem.

If new conversations see 18 tools and an old one sees 17, the server is usually fine.

## Symptom: tools are visible but `herdr_inspect` reports workstation offline

This is no longer a ChatGPT schema problem. If ChatGPT received a structured `workstation_offline` result, the ChatGPT → MCP Edge path was alive enough to return that result; Edge did not have a usable Link WebSocket for the selected workstation. The browser extension does not make this decision.

On v0.4.3+, recovery is layered rather than "restart everything":

1. A recently connected workstation gets up to **2 seconds** of process-local reconnect grace at Edge. A validated Link `hello` wakes the pending request immediately. This grace does not write Durable Object storage or alarms.
2. If the workstation is still unavailable, Edge returns machine-readable recovery metadata: `retryable=true`, `delivery_state=not_delivered`, `retry_after_ms=5000`, and a read-only `herdr_inspect` probe policy with 5s / 10s / 20s backoff.
3. The local Link keeps its normal reconnect/backoff loop. A successful Online transition clears the prolonged-offline timer.
4. If the Link cannot become Online continuously for **300 seconds**, it exits with diagnostic evidence so launchd `KeepAlive` can start a fresh `dev.herdr-mcp.link-prod` process.
5. The server health watchdog remains responsible only for an actually unhealthy local server. `workstation_offline` by itself is not a reason to restart a healthy `dev.herdr-mcp.server`.

The replay rule is deliberately stricter than the retry hint:

- `delivery_state=not_delivered`: the request did not reach the workstation; after recovery it may be sent again. Reuse an idempotency key when the operation has one.
- `delivery_state=delivery_unknown`: a read may be retried when explicitly marked retryable, but a mutation must be reconciled against state/evidence before any replay.
- `delivery_state=delivered` or no proof of non-delivery: do not replay a mutation merely because the connection later failed.

Check the workstation in this order:

```bash
herdr-mcp status
herdr-mcp link status
launchctl print gui/$(id -u)/dev.herdr-mcp.link-prod
tail -n 100 ~/.config/herdr-mcp/link-prod.launchd.err.log
```

Then confirm workstation identity, active runtime generation health, and Edge's recent Link state. Do not delete/recreate the Connector to fix a workstation-link problem, and do not turn a Link-only failure into a global `herdr-mcp service restart` unless local server health is also bad.

## Symptom: inspect works, but file operations fail

Common gates:

- path is outside a managed Git root;
- filename matches a secret-ish path rule;
- read-only mode is enabled;
- target root is not in the write allowlist;
- the file is already dirty and explicit acknowledgement is required;
- another worker is active in the project and the busy gate rejects concurrent writes.

Read the structured error first. Do not make shell bypass the default answer to every file gate.

`herdr_exec` is intentionally a stronger boundary than `herdr_fs_*` and does not provide the same secret-path filtering.

## Symptom: transient TaskGroup / ExceptionGroup control-plane errors

You may see a failed snapshot/pane operation even though the agent or repository is fine.

herdr-mcp can degrade some read paths to narrower evidence sources such as:

- list APIs instead of one large snapshot;
- deterministic Git state;
- direct managed-root file operations.

Re-run `herdr_inspect` / `herdr_since` to obtain current facts. Do not treat one control-plane blip as proof the Git project is unusable.

## Symptom: prompt or exec timed out and you do not know whether it ran

The rule is **do not blindly retry a mutation**.

### `herdr_prompt`

If the failure happened during post-submit status waiting, the agent may already have received the prompt. Inspect agent state/output first. Reuse an `idempotency_key` for a repeated intent.

### `herdr_exec`

If the command was already delivered to a visible pane, a later control-plane timeout is not permission to send the command again. Inspect the pane, Git state, files and tests.

“No success response reached the client” does not mean “nothing happened.”

## Symptom: local agent finished but ChatGPT did not continue

This is often not an MCP failure.

MCP provides:

```text
ChatGPT → workstation
```

A local task finishing later does not create a new ChatGPT turn automatically. For:

```text
workstation → ChatGPT
```

use browser continuity:

- Native Messaging host is installed;
- the current conversation is bound to the correct workspace;
- the relevant Auto scope is enabled, or use a manual HUD action.

See [Browser continuity](browser-continuity.md).

## Symptom: HUD shows the wrong workspace name

Binding identity is the `workspace_id`; the label is display data.

If the ID is correct but the label is stale, the extension should refresh it from the live workspace catalog. Do not remove a correct binding merely to fix display text.

If the ID itself is wrong, bind to the correct workspace.

## Symptom: Browser Control Center will not open, shows no workspaces, or stays on Runtime unavailable

Separate a **Side Panel UI problem**, a **Native Messaging identity problem**, and a **runtime problem**:

1. Click the Herdr toolbar icon and verify Chrome opens the Control Center Side Panel directly; do not navigate to `control-center.html` as a normal web page.
2. `herdr-mcp status` / `herdr-mcp doctor` should first prove the local runtime is healthy.
3. `herdr-mcp native-host status` should report the Native Messaging host registered.
4. If Chrome just updated the Store extension, refresh the affected web page (or restart Chrome if needed) so the current content script is loaded.
5. `herdr-mcp native-host status` should report the extension identity/channel you intentionally selected and a Native Host runtime consistent with the active runtime generation. On v0.4.2 the supported ownership channels are Store/DEV; v0.4.3+ may also report STANDALONE. Do not repair an origin mismatch by guessing another channel: inspect the installed runtime's supported commands and the actual Chrome extension identity first.
6. `Runtime healthy · event stream reconnecting` means the panel still has a snapshot while incremental events recover; it does not mean the whole local runtime is down. Use Refresh for an authoritative reconciliation.

`Send instruction` executes through the trusted local control route; `Adjust current task` returns an exact provider capability/outcome and never silently becomes Prompt. Terminal-only panes can run a command through the fenced `pane.send_input + Enter` path. Arbitrary `Herdr API` remains Preview-only. If any mutation reports `uncertain`, inspect live state before retrying. If Steer reports `session_not_resolved`, the selected provider session lacks a verifiable control endpoint/thread/active-turn mapping; that is a capability result, not a transport failure.

See [Browser Control Center](browser-control-center.md).

## Symptom: ChatGPT response is partial, disconnected or shows send timeout

Do not immediately resend the original task. Tool mutations may already have occurred.

Continuity recovery is evidence-first:

1. inspect same-origin conversation state when available;
2. if server state is ahead of the DOM, refresh/synchronize the view;
3. retry only when evidence says the request was not accepted;
4. fail closed on uncertain delivery;
5. consider handoff only after normal recovery is exhausted.

If automatic recovery cannot obtain trustworthy evidence, refresh manually and use **Herdr monitor** to re-read local state before continuing.

See [Wake, recovery and handoff](browser-continuity.md).

## Symptom: ChatGPT Queue did not send immediately or queued content is still pending

Queue is intentionally **not an immediate send**. While the assistant turn is live, content should remain in the current conversation's durable queue and send only after the turn settles, before generic auto-continue.

Check:

- the page is ChatGPT; other sites do not currently expose the same Queue UI;
- whether the assistant is still generating, using tools, or waiting on a permission card; a live turn must not be interrupted by Queue;
- a `turn-in-progress` or uncertain submit is not ACKed or dropped;
- clicking Queue with an empty composer can retry a still-pending batch;
- right-clicking Queue explicitly clears the current conversation queue;
- after confirmed handoff, pending entries move to the new conversation and should not replay from the source.

Content disappearing without confirmed delivery is the actual reliability failure. Capture the conversation, current turn state, and browser console evidence before filing an issue.

## Symptom: Manual handoff is unavailable

Use **Handoff** in the in-page HUD. If it is unavailable or disabled, verify:

- the current site/conversation type supports handoff;
- a workspace is bound;
- the workspace has no active working agent;
- no transfer is already active.

The current scope may be **Auto on or Auto off**. Where handoff is supported, the target conversation inherits the source Auto state and source automatic wakes pause during transfer.

Handoff must create the packet, create the new conversation, verify the seed, and only then move the binding. If the transfer is recoverable/uncertain, keep the old binding as the safety anchor instead of manually unbinding it.

## Symptom: z.ai / DeepSeek stops after printing a JSON tool call

That is the JSON→MCP bridge, not the ChatGPT Connector.

Check:

- Native Messaging host;
- local MCP tool catalog;
- stable conversation identity;
- whether the last real assistant message is still a tool-call JSON object;
- whether a `TOOL_RESULT` was returned;
- whether enough bridge context survived a page reload to resume safely.

Do not treat internal tool JSON as the final natural-language answer.

See [JSON → MCP bridge](extension.md).

## Symptom: Chromium asks for local-device / loopback permission

Some Chrome/Chromium profiles apply a separate permission gate to local loopback access.

Check the extension site/local-device permission in the browser's extension settings. Native Messaging is the primary trusted path, but diagnostics/compatibility paths can still surface loopback permissions.

Do not rotate Herdr credentials just because a browser permission is pending.

## Symptom: Cloudflare deployment fails

Separate these cases:

- credential/identity failure;
- build/test failure;
- Worker deployed but health/routing failed;
- workers.dev works but Custom Domain/DNS fails.

Use least-privilege credentials and fix the specific layer. Do not turn a route problem into an account-admin token.

See [Cloudflare Edge credentials](cloudflare-edge-token.md) and [Cloudflare Edge deployment](cloudflare-edge-deployment.md).

## Symptom: runtime upgrade broke local behavior

For an implementation change inside the same contract epoch:

```bash
bin/herdr-runtime-generation status
```

Inspect active/previous generations and use the Runtime A/B rollback path when appropriate.

If the tool catalog/schema changed, this is a contract migration, not an ordinary runtime A/B problem. Do not use `herdr-self-update` to sneak across an epoch boundary.

See [Runtime A/B](runtime-self-upgrade.md).

## Useful evidence for an issue

Useful, non-secret diagnostics include:

- `boot_id`;
- runtime version / contract epoch;
- workstation id;
- failing tool;
- failure phase / delivery state;
- whether Git/pane/agent state changed around the failure;
- Edge health/workstation status;
- whether the ChatGPT conversation was new or old;
- bound workspace identity.

Remove bearer tokens, OAuth JWTs, Cloudflare secrets and sensitive project content before sharing logs.

## Restart last, not first

Restarting can restore service, but it can also erase the evidence that explains the root cause.

Prefer:

1. capture current state;
2. identify the failing layer;
3. restart only the relevant component;
4. verify that layer and the next layer afterward.

That turns “it started working again” into an actual diagnosis.
