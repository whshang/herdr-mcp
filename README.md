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

### Platform status

| Platform / architecture | Status | Qualification |
| --- | --- | --- |
| macOS Apple Silicon | Production | Physical-machine runtime, TCC, Link, Connector and browser-extension UAT |
| Linux x86_64 | Production | Physical Debian installation/lifecycle + static release qualification |
| Linux ARM64 / aarch64 | Production | Physical NanoPi R5C Debian 11 install/Link UAT + native ARM64 static release qualification |
| Windows x86_64 | Candidate | Hosted Windows CI/release build; physical UAT not completed |
| Windows ARM64 / aarch64 | Candidate | Native `windows-11-arm` CI/release build; physical UAT not completed |
| WSL | Unsupported | No qualified host/guest filesystem or lifecycle boundary |

See the [platform support matrix](docs/i18n/en/platform-support-matrix.md) for the exact tested/not-yet-tested boundary.

## Install

### Recommended: paste one sentence to your Agent

```text
Install Herdr and herdr-mcp by following https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/en/agent-install.md: plan dependencies first, combine all currently automatable work into as few safe execution steps as possible, use the current Stable GitHub Release, and pause only when I must personally sign in, authorize, or choose a Cloudflare Account/domain.
```

The Agent checks the machine, installs Herdr and herdr-mcp, bootstraps the Worker, configures the final public origin, starts the workstation Link, guides ChatGPT authorization, and proves the setup with a real MCP request. A domain is optional. If this computer cannot reach `workers.dev` directly, the Link can use an already-configured local proxy or the built-in shared Relay automatically.

### Manual installation

For step-by-step manual setup, use the [manual install guide](docs/i18n/en/install.md).

### ChatGPT configuration

Enable Developer Mode for Plugins, open **Plugins → Browse plugins**, then add `herdr` with the complete Worker URL ending in `/mcp` and complete OAuth. Work in a ChatGPT Project; in the first message of each new chat, use the composer `+` button to reference `herdr` so that conversation enables the plugin.

[ChatGPT setup](docs/i18n/en/chatgpt-connector.md) · [OpenAI Developer Mode / MCP documentation](https://help.openai.com/en/articles/12584461)

### Cloudflare configuration

Cloudflare provides the stable public MCP/OAuth entry while every development computer connects outward, so you do not need to expose an inbound port on each machine. Workers Free is sufficient and needs no payment method; if you do not have an account, registration is free and Google sign-in is the shortest setup path.

[Cloudflare setup](docs/i18n/en/cloudflare-edge-deployment.md) · [Cloudflare Dashboard](https://dash.cloudflare.com/)

### Link network fallback

Herdr-MCP prefers a direct workstation Link. When the selected path uses `workers.dev` and the local network cannot reach it, Link can reuse an existing local proxy and then the built-in signed shared Relay. Selection is automatic; normal users do not configure a Relay provider or Relay URL.

Relay carries only the authenticated workstation Link to your own Worker. Your MCP/OAuth URL and device identity stay unchanged. Use `herdr-mcp doctor` and `herdr-mcp link status` to verify the selected path; see [Troubleshooting](docs/i18n/en/troubleshooting.md) for network-specific diagnosis.

## Control multiple computers

One Herdr Worker and one ChatGPT connection can control multiple enrolled computers. ChatGPT can discover the fleet with `herdr_devices`, see which machines are online, and route work to an explicitly named device.

A useful request looks like:

```text
List my Herdr devices. Use macbook-main for the backend task and macbook-lab for the independent test task. Keep the two working trees isolated and verify both results before reporting completion.
```

When several machines are eligible for a mutation and you do not name a target, Herdr fails with `device_ambiguous` instead of guessing. Device identity stays attached to follow-up operations and retries, and each computer has its own credential.

Web AI can also copy small non-secret UTF-8 text between enrolled computers through private workstation methods without adding another public MCP tool. The source is read with an integrity digest and the target write is bounded to HOME, regular non-symlink files, 256 KiB, explicit overwrite, default backup, and secret-like path/content rejection. Binary files, directory synchronization, and credentials are deliberately out of scope.

### Add another computer to the fleet

On any computer already enrolled in the fleet, run:

```text
herdr-mcp worker pair
```

Herdr creates the pairing at the Worker control plane, so the operation does not need to route through a workstation. Pairing is a device/operator fleet action: run it on any already-enrolled computer through `herdr-mcp worker pair`. Never run `worker pair` on the fresh computer as a discovery probe; if this is the first Worker, complete the Cloudflare bootstrap first. The pairing result includes the address, one-time 6-digit code, exact expiry, and the copyable `herdr-mcp worker connect "<pairing-address>"` command.

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
Before changing anything, inspect only the live Herdr/Git state needed for this task and form a short dependency plan. Keep unrelated dirty work isolated. Finish one lane before adding agents and load only the Skills the task needs. Batch independent reads and same-boundary deterministic commands; do not use status checks or polling as thinking steps. Re-plan only when new evidence changes the next decision. Make the smallest sufficient change, then verify the relevant diff, tests, and real boundary.
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

### Herdr 0.9.1 multi-machine + Herdr-MCP devices

Herdr 0.9.1 can save SSH machines, show several Herdr servers in one TUI, and route API commands directly with `herdr --machine <label-or-id> <command>`. Herdr-MCP keeps its own Edge device fleet for ChatGPT/Web-AI routing. The same physical computer may use both paths at once: if both paths reach the same Herdr session, they see the same live workspace, pane, agent, terminal, Git, and filesystem state because they are talking to the same Herdr server — not because two copies are synchronized.

The identities stay separate. A Herdr saved machine is addressed by its machine profile + SSH target + Herdr session; an Edge device is addressed by its immutable `device_id` and device-bound `herdr_ref_*` references. Never treat a bare `w1` or `w1:p1` as globally unique across machines, and never merge the two identities just because labels or hostnames look similar.

For maintenance/UAT, first resolve the saved profile with `herdr machine list --json`, then use `herdr --machine <label-or-id> workspace ...`, `pane ...`, `agent ...`, or another supported API command. Both ends should run Herdr 0.9.1 before CLI forwarding is relied on, and failed remote commands never fall back to Local. `herdr --remote <target>` remains the interactive remote-TUI attach path. ChatGPT operations continue to prefer the Edge device path; SSH/Herdr-machine control is never a transparent retry for an Edge mutation with uncertain delivery.

See [Herdr 0.9.1 multi-machine and dual-path control](docs/i18n/en/multi-machine-control.md) for the routing rules and verification procedure.

## Local agents can control supported WebChat sessions

Web AI is not the only caller of Herdr's browser capability. A local coding agent (Pi, Codex, Claude, or whatever you run on this machine) can drive the same controlled browser/WebChat control plane through herdr-mcp, instead of starting its own automation stack.

Typical uses:

- create a new ChatGPT/WebChat conversation inside an existing Project;
- continue work in a WebChat conversation that is already bound to this machine;
- dispatch the next message into an existing session and read its delivery state;
- check whether a browser/WebChat session is still alive;
- hand the task to a WebChat planner through the canonical handoff path;
- continue a long task into a new conversation when the context approaches its limit.

The browser extension supplies the browser-side execution, binding, wake, and observation boundary, and the supported local entrypoint is the `herdr-mcp` CLI. It attaches the trusted local grant itself, so a local agent never handles browser credentials and never synthesizes account, Project, or session identifiers.

Do not bypass Herdr with your own Playwright, AppleScript, or DOM automation against ChatGPT: those paths cannot preserve identity, idempotency, or delivery evidence.

```bash
herdr-mcp webchat endpoints
herdr-mcp webchat resources --kind session
herdr-mcp webchat handoff --continuity-id hc:... --source-url 'https://chatgpt.com/g/g-p-.../c/...'
```

[Local agent WebChat control](docs/i18n/en/local-agent-webchat-control.md) · [Browser continuity](docs/i18n/en/browser-continuity.md) · [Extension guide](docs/i18n/en/extension.md)

## Chrome extension

The browser extension is optional for the core ChatGPT → MCP → workstation connection. Install it when you want conversation continuity, queued next-turn messages, Browser Control Center, or supported ChatGPT artifact capture.

1.0 browser support covers ChatGPT, Claude, and Grok WebChat sessions. ChatGPT exposes the full surface — conversation create, dispatch, archive, and self-handoff — while Claude and Grok expose signed-in session dispatch with settled results and reload recovery. Gemini remains opt-in experimental and is not part of the 1.0 acceptance boundary.

For ChatGPT Auto, deterministic browser/runtime safety gates stay authoritative. Ordinary post-turn semantic judgment uses a fixed progressive chain: **typed evaluation routes (Jev) → chat routes (LLM) → bounded script fallback**. Goal-aware automation additionally gives the existing LLM Goal Supervisor one bounded Jev semantic prior, while Work Memory/TODO evidence remains the completion authority. The extension stores no semantic-provider endpoint, model, or API key; it asks the local Herdr Runtime for typed evaluation or chat and keeps semantic policy, probability boundaries, judge prompts, and completion tokens product-owned.

The native runtime owns one provider-neutral semantic route pool shared by browser Auto, planning Skill/method relevance, and Work Memory relevance. Configuration has exactly two layers: mode-`0600` `~/.config/herdr-mcp/config.json` for one workstation, and the authenticated Cloudflare Worker route pool for all enrolled workstations. Local typed/chat routes take precedence over Worker routes for the same derived mode. The extension never stores provider configuration and the runtime does not read semantic provider values from shell or process environment. The `config.toml` used by released 0.4.x runtimes is migrated once to `config.json` and retained only as `config.toml.migrated`; unreleased 1.0 semantic TOML sections are intentionally not a migration format.

Work Memory search keeps strict FTS5 hits and the frozen cursor authoritative. When typed semantic evaluation is configured and a strict page has spare capacity, the runtime may fetch a bounded relaxed FTS candidate set from the same frozen Work Memory partition and include those candidates only in `semantic_ranking`; they never change persisted hit order or cursor contents. With no semantic route, or when a provider times out, errors, exhausts its pool, or returns invalid output, Work Memory search continues on the strict deterministic path unchanged.

Every route is a provider-neutral object with an arbitrary `name`, one wire `protocol` (`decision`, `decision-vercel`, or `openai-chat`), and explicit `url`, `model`, and `api_key`. The protocol determines the route mode: `decision` and `decision-vercel` are typed evaluation routes, while `openai-chat` is a chat route. TypeSafe and OpenRouter therefore share the provider-neutral `decision` adapter; Vercel AI Gateway uses its vendor-specific Evaluation v4 wire through `decision-vercel`; OpenAI-compatible LLMs share `openai-chat`. Typed Jev routes and chat/LLM routes use the same rotating-start, bounded-attempt, deadline, cooldown, and failover executor.

Example `config.json` fragment:

```json
{
  "semantic": {
    "routes": [
      {
        "name": "fast_primary",
        "protocol": "decision",
        "url": "https://api.typesafe.ai/v1/systemone",
        "model": "jev-latest",
        "api_key": "<key>"
      },
      {
        "name": "fast_backup",
        "protocol": "decision",
        "url": "https://openrouter.ai/api/alpha/decisions",
        "model": "~typesafe/jev-latest",
        "api_key": "<key>"
      },
      {
        "name": "fast_gateway",
        "protocol": "decision-vercel",
        "url": "https://ai-gateway.vercel.sh/v4/ai/evaluation-model",
        "model": "typesafe-ai/jev",
        "api_key": "<key>"
      },
      {
        "name": "chat_primary",
        "protocol": "openai-chat",
        "url": "https://example-provider.invalid/v1/chat/completions",
        "model": "<model>",
        "api_key": "<key>"
      }
    ]
  }
}
```

Cloudflare uses the same route objects in the single Worker-wide `HERDR_SEMANTIC_ROUTES` JSON secret; only the outer `semantic.routes` wrapper is omitted. Provider credentials never appear in status/capability responses.

If you use the macOS STANDALONE channel, `herdr-mcp doctor` also checks whether Google Chrome is actually loading the fixed Herdr standalone ID from the managed `~/.config/herdr-mcp/extensions/standalone/current` path. A `standalone-extension-load state=drift` warning means Chrome is still using another Load-unpacked directory; reload the Herdr extension from the `expected` path shown by `doctor`.

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
