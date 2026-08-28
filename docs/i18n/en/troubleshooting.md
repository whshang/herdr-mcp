# Troubleshooting: locate the failing layer before restarting everything

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
```

### Is Herdr itself available?

```bash
herdr --version
herdr api schema >/dev/null
```

If local HTTP works but `herdr_inspect` cannot see real workspaces, investigate the Herdr daemon/socket before touching Cloudflare.

### Can Edge see the workstation?

OAuth may succeed while the workstation is offline. Public login success does not prove the local development machine is connected.

Check Edge health/status and the workstation link.

### Does a new ChatGPT conversation get the current catalog?

The current production public contract is **epoch 2 / 18 tools**, including `herdr_skill`.

Old conversations may retain an older `tools/list` snapshot. Before reinstalling anything, verify the server and open a new conversation.

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

This is no longer a ChatGPT schema problem.

Check:

- local runtime health;
- `herdr-link` status;
- workstation identity;
- active runtime generation health;
- recent heartbeat on Edge.

Do not delete/recreate the Connector to fix a workstation-link problem.

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
4. If the extension was just updated, reload it in `chrome://extensions`.
5. If a developer changes the absolute unpacked-extension path, Chromium may assign a different extension ID. A Native Messaging `allowed_origins` entry for the old ID can then produce `Access to the specified native messaging host is forbidden`. Re-register the Native Host for the current development identity rather than copying a bearer into browser storage.
6. `Runtime healthy · event stream reconnecting` means the panel still has a snapshot while incremental events recover; it does not mean the whole local runtime is down. Use Refresh for an authoritative reconciliation.

Prompt Agent / Steer Session / Herdr API / Terminal Input are intentionally Preview-only in the current Control Center. That is not a failure. The executable actions today are `Inspect state` and the bounded `Read output tail`.

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

See [Wake, recovery and handoff](extension-wake.md).

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

Open **Browser Control Center → Current page** first. Manual handoff is intentionally not duplicated in the HUD. Then verify:

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

See [JSON → MCP bridge](extension-bridge.md).

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
