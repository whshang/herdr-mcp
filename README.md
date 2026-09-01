# herdr-mcp

**Connect ChatGPT and other Web AI to a real, persistent development workstation.**

Herdr-MCP keeps the Web model as the primary planner, gives it bounded local tools through MCP, and uses [Herdr](https://herdr.dev/) as the persistent worksite where terminals, services, repositories, worktrees, and coding agents keep running across individual chat turns.

```text
ChatGPT / Web AI
       │ MCP + OAuth
       ▼
Cloudflare Edge
       │ outbound authenticated link
       ▼
   herdr-mcp
   ├─ files / Git / exec
   ├─ local coding agents
   └─ Herdr workspace / pane / PTY / events
              ▲
              └─ optional Chrome extension: continuity / handoff / control center
```

The value is the combination: reuse the reasoning capacity and interface you already have in Web AI, keep long-running development state on your own machine, delegate complex work to replaceable local agents when useful, and keep the whole worksite observable and recoverable for human takeover.

**Documentation:** https://whshang.github.io/herdr-mcp/

**Chrome Web Store:** https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp

**GitHub:** https://github.com/whshang/herdr-mcp

Languages: **English** · [简体中文](README.zh.md) · [日本語](README.ja.md)

## Install

### Recommended: give one sentence to a coding agent

Paste this into Codex, Claude Code, Cursor, Pi, OpenCode, Cline, or another execution-capable local coding agent:

```text
Install and configure Herdr and herdr-mcp for me. Read and follow https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/en/agent-install.md end to end. Use GitHub Releases for the local herdr-mcp runtime, not a git clone. Pause only for Cloudflare sign-in/API-token steps and the ChatGPT herdr app/Connector authorization that require me personally; automate and verify everything else.
```

The Agent protocol covers Herdr, the native herdr-mcp runtime, Cloudflare Edge, the workstation Link, ChatGPT MCP/OAuth configuration, `herdr-mcp doctor`, and a real end-to-end smoke test.

[Agent install guide](docs/i18n/en/agent-install.md)

### Manual installation

Use the manual guide when you want to control every step yourself:

[Manual install](docs/i18n/en/install.md)

Normal runtime use does not require Node.js or npm. The published herdr-mcp runtime is native; Node/Wrangler may only be needed temporarily during Cloudflare bootstrap.

### Add a second computer

On v0.4.3+, an already-authorized machine can create a short-lived pairing session:

```bash
herdr-mcp worker pair
```

Then give the new computer's coding agent this prompt with the printed pairing address and 6-digit code:

```text
Read and follow https://github.com/whshang/herdr-mcp/blob/main/docs/i18n/en/existing-worker-connect.md to connect this computer to my existing Herdr Worker. Pairing address: <pairing-address> Verification code: <code>
```

This joins the existing Worker. It should not create another Worker, OAuth client, Connector, or copy a long-lived shared secret.

[Second-computer guide](docs/i18n/en/existing-worker-connect.md)

### ChatGPT configuration

Current ChatGPT custom MCP configuration lives under **Settings → Apps → Create** after Developer Mode has been enabled for the account/workspace. Business and Enterprise/Edu policy can control who is allowed to create or publish custom MCP apps, so the exact controls depend on the workspace.

[ChatGPT Connector guide](docs/i18n/en/chatgpt-connector.md) · [OpenAI Developer Mode / MCP documentation](https://help.openai.com/en/articles/12584461)

### Cloudflare configuration

The supported public entry is a Cloudflare Worker + Durable Object with an outbound authenticated workstation Link. Start from the Cloudflare dashboard and let the Agent protocol create only the resources and least-privilege credentials it needs.

[Cloudflare Edge guide](docs/i18n/en/cloudflare-edge-deployment.md) · [Cloudflare Dashboard](https://dash.cloudflare.com/)

## First real test

Open a new ChatGPT conversation with the `herdr` app/Connector enabled and send:

```text
Inspect my Herdr projects. Read only; do not modify anything.
```

A healthy setup lets ChatGPT observe real Herdr workspaces, panes, agents, Git state, and project files through MCP.

## How to use it well

### Prompt for evidence, boundaries, and acceptance criteria

Herdr-MCP works best when the Web model owns planning and uses the workstation as a source of facts and execution. A useful development prompt is:

```text
Inspect the live Herdr workspace and Git state before changing anything. Keep existing dirty worktrees isolated. Do deterministic reads, Git checks, patches, and bounded commands directly. Delegate independent or long-running work to local coding agents when that improves throughput. Verify the final diff and run the relevant tests before reporting completion.
```

For risky mutations, also state the target, safety constraints, and what counts as success. For investigation, explicitly request read-only work.

### Install at least one local coding agent

The Web model can handle many small operations directly through Herdr-MCP. A local coding agent becomes valuable for long implementation loops, large refactors, test-fix cycles, or independent parallel work. Herdr can discover available agents with evidence-backed local scanning instead of requiring one specific vendor.

```bash
herdr-mcp scan --json
```

Use the agent you already trust. Herdr is designed to keep agent choice replaceable.

### Good combinations

| Workload | Recommended combination |
| --- | --- |
| Read-only investigation, small patch, Git/test check | Web AI → Herdr-MCP direct tools |
| Medium implementation | Web AI plans → one local coding agent executes → Web AI verifies |
| Large independent modules | Web AI decomposes → multiple isolated local agents/worktrees → cross-check + tests |
| Long unattended work | Above + Chrome extension for continuity, wake/handoff, and Browser Control Center |
| Human takeover | Attach to the same Herdr workspace/pane and continue from the real terminal state |

Avoid opening several agents on the same mutable working tree. Prefer one task per isolated worktree when parallel edits are necessary.

## Browser extension

The Chrome extension is optional. The base ChatGPT → MCP → workstation path works without it.

Install it when you want long-conversation continuity, workspace binding, queued next-turn messages, Browser Control Center, or browser-side capture of supported ChatGPT-generated artifacts.

[Chrome Web Store](https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp) · [Extension guide](docs/i18n/en/extension.md) · [Browser continuity](docs/i18n/en/browser-continuity.md)

## Common questions

### Why does the supported setup use Cloudflare?

ChatGPT runs outside your private network and needs a stable HTTPS MCP/OAuth endpoint. The development workstation is usually behind NAT, a firewall, or a corporate network. Herdr-MCP therefore keeps the workstation inbound-closed and establishes an authenticated **outbound** Link to Cloudflare Edge.

The Edge layer also owns public routing, OAuth-facing endpoints, multi-device selection, reconnect semantics, and bounded coordination state. That gives the Web client one stable public address while local machines can disconnect, reconnect, or change networks.

### Can I use direct intranet penetration / port forwarding instead?

Technically, another transport can replace Cloudflare if it provides the same security and protocol properties: a publicly reachable HTTPS MCP endpoint, trusted TLS, OAuth/authentication, safe device routing, outbound-friendly workstation connectivity, reconnect handling, and no ambiguous mutation replay.

A plain private IP, Tailscale-only address, raw port forward, or SSH tunnel is not reachable from ChatGPT's cloud runtime by itself. Generic public tunnels can expose a machine, but the current supported Herdr-MCP product path is Cloudflare Edge because the routing and recovery semantics are implemented and tested there.

### What should I do when I see `workstation_offline`?

`workstation_offline` means the public MCP/Edge path was alive enough to answer, but Edge did not have a validated Link WebSocket to the selected workstation. The browser extension does not determine this state.

Start with:

```bash
herdr-mcp status
herdr-mcp doctor
```

On v0.4.3+, short Link interruptions receive a bounded reconnect grace and the local Link uses reconnect/backoff plus prolonged-offline recovery. For mutations, follow the returned delivery/retry metadata; do not blindly replay an operation whose delivery is uncertain.

[Troubleshooting `workstation_offline`](docs/i18n/en/troubleshooting.md)

### Where can I see account or usage limits?

Herdr-MCP itself does not purchase or meter model tokens. Web-model limits come from the ChatGPT plan/workspace and can change by model and plan. Check the plan/model usage controls available in your ChatGPT account; some limits are shown as reset windows rather than an exact remaining-token counter.

Cloudflare consumption is separate. Check **Workers & Pages → your Worker → Analytics & Logs** and the account billing/usage pages for Worker, Durable Object, and related resource usage. Herdr-MCP keeps routine activity bounded to avoid turning idle presence into unnecessary Durable Object writes.

### Do I need the Chrome extension?

No for the first connection. Yes when you want browser continuity, local-to-Web wake/handoff, Browser Control Center, or supported ChatGPT artifact capture.

### Does Herdr-MCP require a specific coding agent?

No. Small deterministic work can run directly. Complex work can be delegated to whichever locally available agent fits the task.

## CLI essentials

Most users can let their coding agent operate these commands:

```bash
herdr-mcp install
herdr-mcp status
herdr-mcp doctor
herdr-mcp update check
herdr-mcp update apply
herdr-mcp update auto
herdr-mcp update status
herdr-mcp rollback
herdr-mcp reinstall
herdr-mcp uninstall
```

Source developers on v0.4.3+ also have a separate DEV runtime plane:

```bash
herdr-mcp dev status
herdr-mcp dev sync
herdr-mcp dev rollback
```

Runtime DEV/PROD identity and browser-extension DEV/STANDALONE/STORE identity are separate concepts.

## Related projects and acknowledgements

Herdr-MCP exists because several open projects established useful pieces of this problem. The architecture intentionally reuses those ideas rather than rebuilding every layer.

- [Herdr](https://github.com/herdrdev/herdr) — persistent terminal/workspace/agent runtime used as the local source of truth.
- [coding-tools-mcp](https://github.com/xyTom/coding-tools-mcp) — strong reference for a narrow deterministic coding-MCP tool surface.
- [MCPX](https://github.com/opentokenz/mcpx) — useful reference for durable remote MCP sessions and recovery semantics.
- [AgenticGPT](https://github.com/slhaf/AgenticGPT) — remote-worker architecture with managed jobs and a broader service surface.
- [codex-with-chatgpt](https://github.com/XiaoDuoYa/codex-with-chatgpt) — explicit Web-planner / Codex-executor collaboration.
- [codex-chatgpt-web](https://github.com/miuuyy/codex-chatgpt-web) — the inverse shape: Codex as the harness with Web-model inference behind it.
- [OpenAI tunnel-client](https://github.com/openai/tunnel-client) — reference for securely exposing MCP-compatible services to ChatGPT.

See [Herdr-MCP and the ecosystem](docs/i18n/en/herdr-vs-ecosystem.md) for the architectural comparison, including tmux, cmux, ACP, remote workers, coding-MCP runtimes, and Codex-first bridges.

## License and copyright

Herdr-MCP is released under the **MIT License**. Copyright remains with the project contributors and respective copyright holders of third-party projects. Third-party names, trademarks, code, and documentation remain subject to their own licenses and policies.

The authoritative package license is declared in [`crates/herdr-mcp/Cargo.toml`](crates/herdr-mcp/Cargo.toml).
