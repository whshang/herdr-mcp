# herdr-mcp

[简体中文](README.md) · **English** · [日本語](README.ja.md)

Let ChatGPT, Claude, Grok, and other Web AI work on your own computers: files, Git, commands, tests, local coding agents, multi-workstation routing, and closed-loop browser continuity.

**[Documentation](https://whshang.github.io/herdr-mcp/)**

![Herdr-MCP 1.1 Architecture](assets/architecture-en.svg)

## One-sentence install

Send this to a coding agent on the computer:

```text
Install and configure Herdr and herdr-mcp by following https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/en/agent-install.md; use the latest Stable GitHub Release, automate every safe step, and pause only when I must sign in or authorize Cloudflare, macOS Full Disk Access, or ChatGPT OAuth/Connector approval.
```

The agent checks the machine, installs Herdr and herdr-mcp, deploys the Worker, connects the workstation, and verifies the result.

Normal users do not need git clone, npm, Cargo, Wrangler, or a manual Runtime install.

## Cloudflare

- Cloudflare Workers Free is sufficient. No payment method is required.\
- The install agent runs `herdr-mcp worker bootstrap`.
- `workers.dev` works without a domain.
- If you already have a suitable domain, you can use a dedicated Custom Domain before ChatGPT authorization.
- The MCP URL given to ChatGPT must end in `/mcp`.

[Cloudflare setup](docs/i18n/en/cloudflare-edge-deployment.md)

## ChatGPT

1. Open ChatGPT **Settings → Apps & Connectors** (legacy UI: **Plugins → Browse plugins**) and enable **Developer mode**.
2. Create or add a custom MCP Connector. The recommended name is **`herdr`**.
3. Enter the full MCP URL, for example `https://herdr.example.com/mcp`.
4. Complete OAuth and approve the six-digit verification code in your local terminal.
5. Work inside a ChatGPT Project.
6. **On the first turn of every new conversation, select `@herdr` via the `+` menu.**

Put the local project folder in the ChatGPT Project instructions, for example:

```text
Local project: /Users/you/Documents/my-project
```

With the browser extension installed, Herdr Control Center can also sync device, workspace, and local-folder mappings into Project instructions. Live Herdr state remains authoritative before execution.

[ChatGPT setup](docs/i18n/en/chatgpt-connector.md)

## Authorization

On macOS, grant Full Disk Access to the stable broker only when needed:

```bash
herdr-mcp permissions status
herdr-mcp permissions setup
herdr-mcp permissions verify
```

Run setup only when status reports `needs_setup`, then authorize the broker in **System Settings → Privacy & Security → Full Disk Access**. A fixed broker revision can retain its existing file-access grant; Herdr TUI screen recording is a separate permission. If recording stops working after a Herdr update, see [macOS permission troubleshooting](docs/i18n/en/troubleshooting.md#macos-file-access-and-screen-recording-permissions).

During the first ChatGPT OAuth connection, the approval page shows the exact local approval command and a six-digit verification code. Complete approval in your own terminal. Never paste Cloudflare tokens, device credentials, or other secrets into chat.

[Install and authorization](docs/i18n/en/agent-install.md) · [Troubleshooting](docs/i18n/en/troubleshooting.md)

## What 1.1 gives you

- **State stays on your computers.** Herdr TUI workspaces, PTY terminals, Git worktrees, background servers, and long tasks survive chat boundaries.
- **Inbound-closed multi-workstation fleet.** One Cloudflare Worker enrolls multiple macOS / Linux / Windows workstations over outbound WSS Links with explicit `device_id` routing and zero guessed targets.
- **Strict multi-account & connector isolation.** Authorized Connectors, WebChat accounts, and browser endpoints keep independent identities; unauthorized or mismatched sessions fail closed.
- **Deterministic tools + any local coding agent.** Run file/patch/Git/test work directly across 18 MCP tools, or dispatch larger tasks to Codex, Claude Code, Pi, Cline, OpenCode, and other local agents with durable async inboxes.
- **Multi-provider WebChat orchestration (1.1).** Create, continue, stop, archive, queue next turns, and hand off across ChatGPT (reference planner), Claude, Grok, plus Doubao AI-creation image workflows.
- **Browser Adapter Platform & project Skills (1.1).** Actuate owned or claimed tabs safely with `page_generation`-fenced `browser-page` commands (`observe` / `click` / `fill` / `navigate`) and reusable `.agents/skills/herdr-browser-<slug>` Skills.
- **Explicit delivery safety & attested self-upgrade (1.1).** Mutations distinguish `applied`, `not_applied`, and `uncertain` without blind replay; `herdr-mcp update` verifies signed GitHub Release manifests and Sigstore attestations with rollback safety.

## Common usage

### One project

Store the local folder in ChatGPT Project instructions, then ask directly:

```text
Check this project's Git state, fix the current test failure, change only this project, and run the relevant tests.
```

### Multiple computers

```text
List my Herdr devices. Use macbook-main for the backend change and linux-lab for independent testing. Keep worktrees isolated and verify both results.
```

To add another computer, run on an enrolled device:

```bash
herdr-mcp worker pair
```

### Multiple accounts, one computer

One workstation can serve multiple authorized ChatGPT/WebChat accounts. Connector, account, and browser-session identity remain separate. Browser control only addresses sessions already discovered and authorized in the Registry.

### Local agents and multi-provider WebChat

Local coding agents can create, continue, and hand off supported WebChat sessions (ChatGPT, Claude, Grok, Doubao) or drive fenced `browser-page` tabs through herdr-mcp instead of building another brittle Playwright/DOM automation stack.

[Local agent ↔ WebChat](docs/i18n/en/local-agent-webchat-control.md)

## Browser extension (optional)

The core ChatGPT → MCP → workstation connection does not require the extension.

Install it from the Store—or stage the exact Runtime-matched unpacked build locally—when you want the Side Panel Control Center, Project binding, queued turns, ID-free handoff, multi-provider WebChat orchestration, or `browser-page` actuation:

```bash
herdr-mcp extension standalone install
```

[Chrome Web Store](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp) · [Extension guide](docs/i18n/en/extension.md) · [Control Center](docs/i18n/en/browser-control-center.md) · [Browser continuity](docs/i18n/en/browser-continuity.md)

## Check status & upgrade

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
herdr-mcp device list
herdr-mcp update
```

`herdr-mcp update` verifies the signed GitHub Release manifest and Sigstore build provenance before committing a rollback-safe Runtime update; run `herdr-mcp worker update` separately when `update status` reports Fleet Worker drift.

macOS Apple Silicon, Linux x86_64, and Linux ARM64 are Production. Windows x86_64 / ARM64 are currently Candidate. WSL is unsupported.

[Platform support](docs/i18n/en/platform-support-matrix.md) · [Self-upgrade](docs/i18n/en/runtime-self-upgrade.md) · [CLI reference](docs/i18n/en/cli-reference.md) · [Full documentation](https://whshang.github.io/herdr-mcp/)

## FAQ

### Why Cloudflare?

ChatGPT runs on the public Internet while development machines are usually behind NAT, firewalls, changing networks, or corporate gateways. Herdr-MCP keeps workstations inbound-closed: each device makes an authenticated outbound connection to a stable Cloudflare entry.

Cloudflare also provides the public MCP/OAuth endpoint, device routing, reconnect coordination, and the small amount of shared state needed for multi-device access.

### Can I use port forwarding, Tailscale, or another tunnel?

Another transport is a complete substitute only if it also provides a public HTTPS MCP endpoint reachable by ChatGPT, trusted TLS, authentication/OAuth, safe device routing, reliable reconnect behavior, and explicit mutation-delivery semantics.

Private IPs and Tailscale-only addresses are not directly reachable from ChatGPT's cloud service. Raw port forwarding increases exposure. Cloudflare is the currently supported and qualified path. Direct LAN / Tailscale connections are not supported in 1.1 and are under evaluation for a later release, not an existing capability and not a committed scope; their design would have to address authentication, encryption, device identity and authorization, network exposure, and delivery semantics. ChatGPT cloud would still need a reachable public endpoint.

### Which Herdr does herdr-mcp support?

Only the **Herdr TUI** (the `herdr` CLI and `herdr server`). CI pins Herdr TUI `0.9.5`. The new Herdr app and its herdr-core runtime (`hc`) are a separate runtime that does not share agents with the TUI, and herdr-mcp v1.1 does not support them. Herdr TUI documentation now lives at <https://herdr.dev/tui/docs/>. Details: [platform support matrix](docs/i18n/en/platform-support-matrix.md).

### What if the first install cannot find Herdr windows or the Herdr Server?

Starting with **v1.0.1**, `herdr-mcp install` discovers and verifies installed Herdr, attempts a stable Herdr update, invokes the official Herdr installer when Herdr is absent, and requires the local Herdr Server/API to become reachable before installation is considered complete.

If something still looks wrong, run:

```bash
herdr-mcp status
herdr-mcp doctor
```

Do not install a second Herdr copy just because the current shell PATH cannot resolve `herdr`; follow the repair path reported by `doctor`.

### What do I do when I see `workstation_offline`?

It means Edge can still answer, but the selected computer did not have a validated live connection at that moment. Short interruptions get an automatic reconnect grace period.

For mutations, follow the returned `delivery_state` / retry metadata and do not blindly repeat an operation whose delivery is uncertain. See [Troubleshooting](docs/i18n/en/troubleshooting.md).

### Do I need the Chrome extension?

No. The core ChatGPT → MCP → workstation connection works without it. Install the extension for Project binding, Control Center, browser continuity, queued next turns, multi-provider WebChat handoff, or fenced `browser-page` tasks.

### Does Herdr-MCP require a specific coding agent?

No. Deterministic work can run directly, and complex work can be delegated to whichever compatible agents are available on the selected computer.

## Related projects and acknowledgements

Herdr-MCP builds on ideas proven across several open projects:

- [Herdr](https://github.com/herdrdev/herdr) — persistent workspaces, terminals, and agent environments.
- [coding-tools-mcp](https://github.com/xyTom/coding-tools-mcp) — deterministic coding MCP tools.
- [MCPX](https://github.com/opentokenz/mcpx) — persistent remote MCP sessions and recovery.
- [AgenticGPT](https://github.com/slhaf/AgenticGPT) — remote Worker and managed jobs architecture.
- [codex-with-chatgpt](https://github.com/XiaoDuoYa/codex-with-chatgpt) — Web planner / Codex executor collaboration.
- [codex-chatgpt-web](https://github.com/miuuyy/codex-chatgpt-web) — Codex harness with Web-model inference.
- [OpenAI tunnel-client](https://github.com/openai/tunnel-client) — reference pattern for safely exposing MCP services to ChatGPT.

These references describe lineage and ecosystem context, not a dependency or endorsement by their authors. For architectural trade-offs, see [Ecosystem comparison](docs/i18n/en/herdr-vs-ecosystem.md).

## License

MIT
