# Agent install

*Minimal end-to-end execution contract for a coding agent. Human-facing details live in the [manual install guide](install.md); failure diagnosis lives in [troubleshooting](troubleshooting.md).*

> **Execution role: Agent.** Read this page once, then execute it. Do not recursively load every linked document up front; open a detailed guide only when its blocker actually occurs. The user owns only the steps that truly require a person: sign-in, system approval, Cloudflare Token creation/account or domain choice, and ChatGPT OAuth.

## 1. Execution rules

1. **Plan before calling tools.** Before the first mutation, internally resolve the current machine/fleet state, human-only boundaries, dependencies between steps, and final acceptance checks. Issue currently known independent reads as one wave. Put deterministic shell/Git work that shares the same project and safety boundary into one bounded execution call, including the local checks needed between those steps. Re-plan only when a result changes the next arguments or safety decision, a user action is required, or mutation delivery is uncertain. Do not run `status` after every command and do not poll merely to prove that nothing changed.
2. **Preserve existing work.** Never `reset --hard`, `clean -fd`, overwrite unrelated dirty files, or rebuild an existing fleet as an installation shortcut.
3. **Normal installation uses the current Stable GitHub Release only.** Do not clone this repository or use `npm`/`cargo` to build the workstation runtime unless the user explicitly asked for source development.
4. **Keep secrets ephemeral.** Never echo a Cloudflare Token or write it to Git, ordinary logs, or shell history. Pass it only through the current process environment or a hidden CLI prompt. Never put the local `HERDR_MCP_TOKEN` or Cloudflare Token into ChatGPT.
5. **Do not change the user's network environment.** Existing proxy/network configuration may be reused, but do not switch proxies, rewrite system DNS, change network nodes, or create a generic proxy.
6. **Pause only at human boundaries.** Continue every step that can be determined and automated safely. Ask once when Cloudflare login/Token creation, a macOS permission approval, an ambiguous Account/zone choice, or ChatGPT OAuth actually requires the user.

## 2. Decide first: first Worker or existing fleet

Resolve this in order without repeatedly questioning the user:

- A pairing address was already supplied: this machine is **joining an existing Worker**; go to §5.
- This machine already has a valid Herdr device identity: preserve the existing fleet and repair/verify it instead of creating another Worker.
- Otherwise prepare for a **first Worker**. After a Cloudflare Token is available, bootstrap preflight must stop new-Worker mutation if that Account already contains a Herdr Worker; require a pairing from any enrolled device and continue at §5.
- **Never** run `herdr-mcp worker pair` on a fresh, unenrolled machine as a fleet-discovery probe.
- An existing-fleet repair failure must never fall back to a random-suffixed Worker, second R2 bucket, or second Connector.

## 3. Local installation phase

Check `herdr` first. If it is missing, install the official stable build:

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

On Windows use the official `install.ps1`. Then verify `herdr --version` and `herdr api schema`.

Download the **Latest stable** platform binary from <https://github.com/whshang/herdr-mcp/releases>, place it on the user `PATH` (normally `~/.local/bin/herdr-mcp`), then run:

```bash
herdr-mcp --version
herdr-mcp install
herdr-mcp doctor
```

If `~/.local/bin/herdr-mcp` already exists but the interactive shell cannot resolve it, fix PATH instead of reinstalling or creating a second CLI owner.

On macOS, if `permissions`/`doctor` explicitly requires Full Disk Access/TCC, let the user approve the system prompt and then verify again; do not substitute `sudo`. Linux uses the supported release user-service/process backend and must not inherit macOS launchd assumptions. Normal installation does not require Node.js, Wrangler, npm, or Cargo.

## 4. First Worker: Cloudflare + bootstrap

When a Token is required, open <https://dash.cloudflare.com/profile/api-tokens>. Prefer Cloudflare's **Edit Cloudflare Workers** template scoped to the Account being used; the core path requires Workers Scripts and related Worker-management permissions. **Workers R2 Storage is optional** and is added only when the user explicitly enables artifact relay. R2 must never block the core install.

Keep the Token only in the current process as `CLOUDFLARE_API_TOKEN` or provide it through the hidden `worker bootstrap` input. Do not put the Token in a command-line literal, repository config, or ordinary log.

Run:

```bash
herdr-mcp worker bootstrap
```

This command owns Cloudflare API preflight, Account and `workers.dev` subdomain resolution, existing-Herdr-Worker detection, release manifest plus `herdr-edge-<version>.mjs` artifact attestation, Worker/DO bootstrap, first canonical device enrollment, credential storage, and production Link reconciliation. The normal user path does not run Wrangler and does not require a source checkout.

Choose the public origin once, before Connector creation:

- If the Account has one clearly suitable active zone, prefer a dedicated Custom Domain such as `https://herdr-mcp.example.com/mcp`.
- If several materially different zones are suitable, ask the user once which one to use.
- If no suitable zone exists, the user does not want one, or the hostname conflicts, keep `workers.dev`, for example `https://herdr-edge-device.username.workers.dev/mcp`.

The normal Custom Domain path does not require generic DNS Write. Later network recovery must not silently change the selected OAuth/MCP public origin.

## 5. Join an existing Worker

An enrolled device creates the pairing; the fresh machine only consumes it:

```bash
herdr-mcp worker connect "<pairing-address>"
```

Ask for the six-digit verification code only when the CLI requests it. The display name defaults from the computer name; pass `--name` only when the user explicitly wants a different name. This path does not deploy another Worker, create another Connector, or copy a fleet-wide long-lived secret to the new machine.

See [join an existing fleet](existing-worker-connect.md) for detail.

## 6. Link and network

`worker bootstrap` / `worker connect` creates a per-device credential and reconciles the production Link. The Agent only needs to verify:

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
```

Without a Custom Domain, Link owns the supported `workers.dev` path selection: direct access first, then an already-configured local proxy, then the built-in signed shared Relay when needed. The Agent does not reconstruct that transport ladder or configure a Relay provider/URL. If this workstation cannot reach `workers.dev` directly, do not redeploy the Worker: a healthy Relay-selected Link plus a successful public-origin authenticated MCP round trip is valid acceptance evidence. With a Custom Domain, verify the already-selected origin; do not modify system networking just to make a probe succeed.

## 7. Final acceptance

Combine read-only checks into one final verification wave instead of repeating the same checks after every installation step. Installation is complete only when all of these are proven:

- `herdr-mcp status` / `doctor` are healthy;
- `herdr-mcp link status` shows the enrolled production Link online;
- the canonical public origin serves `/health` and OAuth discovery correctly;
- the machine has a canonical `dev_<ULID>` device identity;
- one real authenticated MCP request completes from the public origin to this workstation and back.

Then guide the user to enable ChatGPT Developer Mode when required, create the `herdr` Connector with the final `.../mcp` URL, and complete OAuth. ChatGPT authorization is the last user-owned boundary.

The Chrome extension / Native Messaging path is optional and is not a prerequisite for the core Connector. Install it from the [extension guide](extension.md) only when the user wants browser continuity, handoff, or the Control Center; keep extension distribution/development details in that guide.

## 8. Cleanup and report

Unset `CLOUDFLARE_API_TOKEN` / `CLOUDFLARE_ACCOUNT_ID` and delete any temporary credential file. Report only non-secret facts: runtime version, device name/id (shortened for display if useful), Worker name, final public origin, Link status, `/health`, OAuth, and MCP E2E result.

If permissions, network, OAuth, existing-Worker ownership, or mutation-delivery uncertainty blocks progress, open the matching section of the [manual install guide](install.md) or [troubleshooting](troubleshooting.md). Do not copy the entire maintainer runbook into the execution context up front.