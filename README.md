# herdr-mcp

**English** · [简体中文](README.zh.md) · [日本語](README.ja.md)

**Keep the brain in ChatGPT. Keep the work on your computers.**

Herdr-MCP lets ChatGPT and other Web AI inspect code, use Git, run commands and tests, and coordinate coding agents on your real development machines. [Herdr](https://herdr.dev/) keeps workspaces, terminals, services, repositories, worktrees, and agents alive across conversations, so long-running work does not disappear when a chat ends.

```text
ChatGPT / Web AI
       │ MCP + OAuth
       ▼
Cloudflare Edge
       │ authenticated outbound link
       ▼
   herdr-mcp
   ├─ files / Git / commands
   ├─ coding agents
   └─ Herdr workspaces / terminals / events
              ▲
              └─ optional Chrome extension: continuity / handoff / control center
```

The model keeps planning. Your computers keep the real state. Small tasks can run directly; larger tasks can be split across independent coding agents and machines while remaining observable and recoverable.

**[Documentation](https://whshang.github.io/herdr-mcp/)**

## Install

### Recommended: paste one sentence to your Agent

```text
Install and configure Herdr and herdr-mcp for me by following https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/en/agent-install.md end to end; use the current stable GitHub Release, configure Cloudflare and ChatGPT, prefer a dedicated Cloudflare Custom Domain when my account already has a suitable active zone, otherwise keep workers.dev, automatically verify the workstation network/fallback path, pause only when I must personally sign in or authorize access, and verify the complete connection before finishing.
```

The Agent checks the machine, installs Herdr and herdr-mcp, bootstraps the Worker on `workers.dev`, recommends/finalizes a Custom Domain before OAuth when your Cloudflare account has a suitable zone, starts the workstation connection, guides you through ChatGPT authorization, tests the actual network path, and proves the setup with a real MCP request. No domain is required: without one, the Link transparently falls back from direct `workers.dev` to an already-configured local proxy and then to the qualified shared Relay baseline when necessary.

### Manual installation

For step-by-step manual setup, use the [manual install guide](docs/i18n/en/install.md).

### ChatGPT configuration

Enable Developer Mode when required, then add the `herdr` app/Connector in **Settings → Apps** and complete OAuth.

[ChatGPT setup](docs/i18n/en/chatgpt-connector.md) · [OpenAI Developer Mode / MCP documentation](https://help.openai.com/en/articles/12584461)

### Cloudflare configuration

Cloudflare provides the stable public MCP/OAuth entry while every development computer connects outward, so you do not need to expose an inbound port on each machine.

[Cloudflare setup](docs/i18n/en/cloudflare-edge-deployment.md) · [Cloudflare Dashboard](https://dash.cloudflare.com/)

### Shared Relay is fallback transport, not your public endpoint

Herdr-MCP normally keeps the workstation Link direct. If you configured your own Cloudflare Custom Domain, the shared Relay Pool is not used. Without a Custom Domain, the Link tries the Worker `workers.dev` origin directly, then a validated local proxy when one exists; only when those paths are unavailable does it fall back automatically to the Herdr-operated Relay Pool. Fresh installs carry the exact Deno/Supabase baseline qualified by the v0.4.5 mainland-China no-proxy UAT; a newer valid signed pool cache can replace that baseline.

The Relay does not replace your MCP/OAuth address, terminate your device identity, or turn Herdr into a generic proxy. It only forwards the already-authenticated `herdr-link` WebSocket to your own `workers.dev` Worker. The production pool uses independent Deno and Supabase failure domains with sticky, capacity-weighted per-device selection and bounded failover. Deno carries the large majority of long-lived connections; Supabase receives a small capacity share and remains a full fallback because its hosted Edge Function lifetime and Free-plan invocation budget are tighter. Normal users do not create either provider account or configure a Relay URL.

## Control multiple computers

One Herdr Worker and one ChatGPT connection can control multiple enrolled computers. ChatGPT can discover the fleet with `herdr_devices`, see which machines are online, and route work to an explicitly named device.

A useful request looks like:

```text
List my Herdr devices. Use macbook-main for the backend task and macbook-lab for the independent test task. Keep the two working trees isolated and verify both results before reporting completion.
```

When several machines are eligible for a mutation and you do not name a target, Herdr fails with `device_ambiguous` instead of guessing. Device identity stays attached to follow-up operations and retries, and each computer has its own credential.

Web AI can also copy small non-secret UTF-8 text between enrolled computers through private workstation methods without adding another public MCP tool. The source is read with an integrity digest and the target write is bounded to HOME, regular non-symlink files, 256 KiB, explicit overwrite, default backup, and secret-like path/content rejection. Binary files, directory synchronization, and credentials are deliberately out of scope.

### Add another computer to the fleet

Preferred: ask in a Herdr WebChat that was explicitly approved by this Worker:

```text
Generate a Herdr pairing link for my new computer, valid for 10 minutes.
```

Herdr creates the pairing at the Worker control plane, so the operation does not need to route through a workstation. This still requires an existing fleet-admin principal: an explicitly approved WebChat, any already-enrolled computer, or a Worker operator credential. If the WebChat lacks that authority, run `herdr-mcp worker pair` on any already-enrolled Mac. Never run `worker pair` on the fresh computer as a discovery probe; if this is the first Worker, complete the Cloudflare bootstrap first. The pairing result includes the address, one-time 6-digit code, exact expiry, and the copyable `herdr-mcp worker connect "<pairing-address>"` command.

On the new computer, give its coding agent this one sentence:

```text
Connect this computer to my existing Herdr fleet by following https://github.com/whshang/herdr-mcp/blob/main/docs/i18n/en/existing-worker-connect.md; use this pairing address: <pairing-address>, ask me for the 6-digit verification code only when the CLI prompts for it, then verify this device appears online in the same Worker.
```

The new computer joins the existing Worker and ChatGPT connection. It does not create another Worker or copy a long-lived shared secret.

[Multi-device guide](docs/i18n/en/existing-worker-connect.md)

## Use it well

### Give the Web AI clear operating rules

For development work, a strong default prompt is:

```text
Inspect the live Herdr workspace and Git state before changing anything. Keep existing dirty worktrees isolated. Do deterministic reads, Git checks, patches, and bounded commands directly. Delegate independent or long-running work to available coding agents when that improves throughput. Verify the final diff and run the relevant tests before reporting completion.
```

For risky changes, state the target, safety constraints, and acceptance criteria. For investigation, explicitly request read-only work.

### Install at least one coding agent

Herdr-MCP can perform deterministic work directly. Coding agents are useful for long implementation loops, large refactors, test-fix cycles, and independent parallel modules. Herdr discovers the agents available on each computer, so the architecture does not depend on one vendor.

Good combinations:

| Workload | Suggested combination |
| --- | --- |
| Investigation, small patch, Git/test check | Web AI → direct Herdr-MCP tools |
| Medium implementation | Web AI plans → one coding agent executes → Web AI verifies |
| Large independent modules | Web AI decomposes → isolated agents/worktrees → cross-check + tests |
| Several computers | Web AI selects devices → independent tasks per machine → combined verification |
| Long unattended work | Add the Chrome extension for continuity and handoff |
| Human takeover | Open the same Herdr workspace/terminal and continue from the real state |

Avoid several agents editing the same working tree. Use isolated worktrees for parallel mutations.

For long tests and builds, the planner uses `herdr_exec_start` and resumes with `herdr_exec_read(session_id, offset=next_offset)` instead of treating terminal scrollback as completion evidence. Completed sessions keep bounded final output and exit evidence long enough to survive a runtime replacement; a running process is never assumed to have been safely taken over after a restart.

## Chrome extension

The browser extension is optional for the core ChatGPT → MCP → workstation connection. Install it when you want conversation continuity, queued next-turn messages, Browser Control Center, or supported ChatGPT artifact capture.

[Chrome Web Store](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp) · [Extension guide](docs/i18n/en/extension.md) · [Browser continuity](docs/i18n/en/browser-continuity.md)

## Common questions

### Why use Cloudflare?

ChatGPT runs on the public Internet while development machines are usually behind NAT, firewalls, changing networks, or corporate gateways. Herdr-MCP keeps those machines inbound-closed: each machine makes an authenticated outbound connection to a stable Cloudflare entry.

Cloudflare also provides the public MCP/OAuth endpoint, device routing, reconnect coordination, and the small amount of shared state needed for multi-device access.

### Can I use port forwarding, Tailscale, or another tunnel instead?

Another transport can work only if it provides the same properties: a public HTTPS MCP endpoint reachable by ChatGPT, trusted TLS, authentication/OAuth, safe device routing, reliable reconnect behavior, and unambiguous mutation delivery.

A private IP or Tailscale-only address is not directly reachable from ChatGPT's cloud service. Raw port forwarding increases exposure. Generic tunnels can publish an endpoint, but Cloudflare is the supported path because Herdr-MCP's routing, OAuth, multi-device, and recovery behavior is implemented and tested there.

### What do I do when I see `workstation_offline`?

It means Cloudflare could answer ChatGPT, but the selected computer did not have a validated live connection at that moment. Short interruptions get a reconnect grace period and the computer keeps reconnecting automatically.

Run:

```bash
herdr-mcp status
herdr-mcp doctor
```

If the error concerns a mutation, follow its delivery/retry metadata and do not blindly repeat an operation whose delivery is uncertain. See [Troubleshooting](docs/i18n/en/troubleshooting.md).

### Where can I see account or usage limits?

ChatGPT model limits belong to your ChatGPT plan/workspace. Check the usage or model-limit information exposed by ChatGPT for your account; some plans show a reset window rather than an exact remaining-token number.

Cloudflare usage is separate. Check **Workers & Pages → your Worker → Analytics & Logs** plus the account billing/usage pages for Worker and Durable Object consumption. Herdr-MCP keeps routine fleet activity bounded so idle devices do not continuously write coordination state.

### Do I need the Chrome extension?

No. The core connection works without it. Install it for browser continuity, handoff, Browser Control Center, and supported browser-side artifact capture.

### Does Herdr-MCP require a specific coding agent?

No. Deterministic work can run directly, and complex work can be delegated to whichever compatible agents are available on the selected computer.

## Related projects and acknowledgements

Herdr-MCP builds on ideas demonstrated by several open projects:

- [Herdr](https://github.com/herdrdev/herdr) — persistent workspace, terminal, and agent environment.
- [coding-tools-mcp](https://github.com/xyTom/coding-tools-mcp) — focused deterministic coding-MCP tools.
- [MCPX](https://github.com/opentokenz/mcpx) — durable remote MCP sessions and recovery ideas.
- [AgenticGPT](https://github.com/slhaf/AgenticGPT) — remote-worker architecture and managed jobs.
- [codex-with-chatgpt](https://github.com/XiaoDuoYa/codex-with-chatgpt) — Web planner / Codex executor collaboration.
- [codex-chatgpt-web](https://github.com/miuuyy/codex-chatgpt-web) — Codex harness with Web-model inference.
- [OpenAI tunnel-client](https://github.com/openai/tunnel-client) — secure exposure of MCP-compatible services to ChatGPT.

See [Ecosystem comparison](docs/i18n/en/herdr-vs-ecosystem.md) for more alternatives and architectural trade-offs.

## License

Herdr-MCP is released under the **MIT License**. Third-party projects, names, trademarks, code, and documentation remain subject to their own licenses and policies.
