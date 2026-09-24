# herdr-mcp

[简体中文](README.md) · **English** · [日本語](README.ja.md)

Let ChatGPT and other Web AI work on your own computers: files, Git, commands, tests, coding agents, and multiple workstations.

**[Documentation](https://whshang.github.io/herdr-mcp/)**

## One-sentence install

Send this to a coding agent on the computer:

```text
Install and configure Herdr and herdr-mcp by following https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/en/agent-install.md; use the latest Stable GitHub Release, automate every safe step, and pause only when I must sign in or authorize Cloudflare, macOS Full Disk Access, or ChatGPT OAuth/Connector approval.
```

The agent checks the machine, installs Herdr and herdr-mcp, deploys the Worker, connects the workstation, and verifies the result.

Normal users do not need git clone, npm, Cargo, Wrangler, or a manual Runtime install.

## Cloudflare

- Cloudflare Workers Free is sufficient. No payment method is required.
- The install agent runs `herdr-mcp worker bootstrap`.
- `workers.dev` works without a domain.
- If you already have a suitable domain, you can use a dedicated Custom Domain before ChatGPT authorization.
- The MCP URL given to ChatGPT must end in `/mcp`.

[Cloudflare setup](docs/i18n/en/cloudflare-edge-deployment.md)

## ChatGPT

1. Enable **Developer mode** for Plugins.
2. Open **Plugins → Browse plugins**.
3. Add the Herdr Connector. The recommended name is **`herdr`**.
4. Enter the full MCP URL, for example `https://herdr.example.com/mcp`.
5. Complete OAuth.
6. Work inside a ChatGPT Project.
7. **On the first turn of every new conversation, manually select or @ `herdr`.**

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

Run setup only when status reports `needs_setup`, then authorize the broker in **System Settings → Privacy & Security → Full Disk Access**.

During the first ChatGPT OAuth connection, the approval page shows the exact local approval command and a six-digit verification code. Complete approval in your own terminal. Never paste Cloudflare tokens, device credentials, or other secrets into chat.

[Install and authorization](docs/i18n/en/agent-install.md) · [Troubleshooting](docs/i18n/en/troubleshooting.md)

## What it gives you

- **State stays on your computers.** Workspaces, terminals, Git, worktrees, agents, and long tasks survive chat boundaries.
- **One ChatGPT can control multiple computers.** One Worker can enroll multiple workstations with explicit device routing.
- **Multiple accounts can use one computer.** Authorized Connectors/WebChat accounts keep separate identities; browser control is isolated by provider, account, and session.
- **Use the coding agents you already have.** Small work can run directly; larger work can be delegated to available agents.
- **Retries stay safe.** Delivered, not-delivered, and uncertain mutations are explicit.
- **Browser continuity is optional.** The Chrome extension adds Project binding, Control Center, queued next turns, handoff, and supported WebChat control.

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

### Local agents and ChatGPT

Local coding agents can create, continue, and hand off supported WebChat sessions through herdr-mcp instead of building another Playwright/DOM automation stack.

[Local agent ↔ WebChat](docs/i18n/en/local-agent-webchat-control.md)

## Browser extension (optional)

The core ChatGPT → MCP → workstation connection does not require the extension.

Install it when you want Project binding, Control Center, browser continuity, queued next turns, or WebChat handoff:

[Chrome Web Store](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp) · [Extension guide](docs/i18n/en/extension.md) · [Browser continuity](docs/i18n/en/browser-continuity.md)

## Check status

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
herdr-mcp device list
```

macOS Apple Silicon, Linux x86_64, and Linux ARM64 are Production. Windows x86_64 / ARM64 are currently Candidate. WSL is unsupported.

[Platform support](docs/i18n/en/platform-support-matrix.md) · [CLI reference](docs/i18n/en/cli-reference.md) · [Full documentation](https://whshang.github.io/herdr-mcp/)

## FAQ

### Why Cloudflare?

ChatGPT runs on the public Internet while development machines are usually behind NAT, firewalls, changing networks, or corporate gateways. Herdr-MCP keeps workstations inbound-closed: each device makes an authenticated outbound connection to a stable Cloudflare entry.

Cloudflare also provides the public MCP/OAuth endpoint, device routing, reconnect coordination, and the small amount of shared state needed for multi-device access.

### Can I use port forwarding, Tailscale, or another tunnel?

Another transport is a complete substitute only if it also provides a public HTTPS MCP endpoint reachable by ChatGPT, trusted TLS, authentication/OAuth, safe device routing, reliable reconnect behavior, and explicit mutation-delivery semantics.

Private IPs and Tailscale-only addresses are not directly reachable from ChatGPT's cloud service. Raw port forwarding increases exposure. Cloudflare is the currently supported and qualified path.

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

No. The core ChatGPT → MCP → workstation connection works without it. Install the extension for Project binding, Control Center, browser continuity, queued next turns, or WebChat handoff.

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

These references acknowledge sources of ideas and ecosystem context; they do not imply dependency on Herdr-MCP or endorsement by the listed projects or authors. See [Ecosystem comparison](docs/i18n/en/herdr-vs-ecosystem.md) for architectural trade-offs.

## License

MIT
