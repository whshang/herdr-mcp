# Agent install

*Minimal end-to-end execution contract for a coding agent. Human-facing details live in the [manual install guide](install.md); failure diagnosis lives in [troubleshooting](troubleshooting.md).*

> **Execution role: Agent.** Read this page once, then execute it. Do not recursively load every linked document up front; open a detailed guide only when its blocker actually occurs. The user owns only the steps that truly require a person: sign-in, system approval, Cloudflare Token creation/account or domain choice, and ChatGPT OAuth.

## 1. Execution rules

1. **Plan before calling tools.** Before the first mutation, internally resolve the current machine/fleet state, human-only boundaries, dependencies between steps, and final acceptance checks. Issue currently known independent reads as one wave. Put deterministic shell/Git work that shares the same project and safety boundary into one bounded execution call, including the local checks needed between those steps. Re-plan only when a result changes the next arguments or safety decision, a user action is required, or mutation delivery is uncertain. Do not run `status` after every command and do not poll merely to prove that nothing changed.
2. **Preserve existing work.** Never `reset --hard`, `clean -fd`, overwrite unrelated dirty files, or rebuild an existing fleet as an installation shortcut.
3. **Normal installation uses the current Stable GitHub Release only.** Do not clone this repository or use `npm`/`cargo` to build the workstation runtime unless the user explicitly asked for source development.
4. **Keep secrets ephemeral.** Never echo a Cloudflare Token or write it to Git, ordinary logs, or shell history. Pass it only through the current process environment or a hidden CLI prompt. Never put the local `HERDR_MCP_TOKEN` or Cloudflare Token into ChatGPT.
5. **Do not change the user's network environment.** Do not switch proxies, rewrite system DNS, or create a generic proxy. The only recovery exception is the verified single-Worker hosts entry in §6.
6. **Pause only at human boundaries.** Continue every step that can be determined and automated safely. Ask once when Cloudflare login/Token creation, a macOS permission approval, an ambiguous Account/zone choice, or ChatGPT OAuth actually requires the user.

## 2. Decide first: first Worker or existing fleet

Resolve this in order without repeatedly questioning the user:

- A pairing address was already supplied: this machine is **joining an existing Herdr Worker**; go to §5.
- This machine is already enrolled: preserve the existing fleet and repair/verify it instead of creating another Worker.
- Otherwise prepare for a **first Worker**. After a Cloudflare Token is available, list `GET /client/v4/accounts/<ACCOUNT_ID>/workers/scripts`; if that Account already contains a Herdr Worker, stop new-Worker mutation, require a pairing from an enrolled device, and continue at §5.
- Never run `herdr-mcp worker pair` on the computer currently being installed as a fleet-discovery probe.
- Never fall back to a random-suffixed Worker, second R2 bucket, or second Connector unless the user explicitly changes fleet intent.

## 3. Local installation phase

Check `herdr` first. If it is missing, install the official stable build:

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

On Windows use `install.ps1`; verify `herdr --version` and `herdr api schema`. Windows UAT uses matching PR CI `windows-uat`; do not use 0.4.8 or source builds.

Download the **Latest stable** platform binary from <https://github.com/whshang/herdr-mcp/releases>, place it on the user `PATH` (normally `~/.local/bin/herdr-mcp`), then run:

```bash
herdr-mcp --version
herdr-mcp install
herdr-mcp doctor
```

If `~/.local/bin/herdr-mcp` exists but the interactive shell cannot resolve it, classify this as `installed_but_not_on_shell_path`, repair the user's PATH, and verify a fresh shell. Do not reinstall or create a second PATH owner. Use [Troubleshooting](troubleshooting.md) only if the PATH repair is needed.

On macOS, run `herdr-mcp permissions status` before Cloudflare work. If it reports `needs_setup`, grant Full Disk Access once to the stable Herdr-MCP broker, then run `herdr-mcp permissions verify`; do not probe protected paths first or use `sudo`. The broker carries MCP/native Herdr TCC; use `permissions setup --upgrade-broker` only for migration. Linux uses the user-service/process backend. Windows uses a Startup-folder shortcut, user processes and Credential Manager without elevation; it can start installed `herdr server` if needed. Normal install needs no Node.js, Wrangler, npm or Cargo.

## 4. First Worker: Cloudflare + bootstrap

Cloudflare Workers Free is enough for Herdr and needs no payment method. If the user has no account, say registration is free and recommend Google sign-in. When a Token is required, open <https://dash.cloudflare.com/profile/api-tokens>. Prefer **Edit Cloudflare Workers** for the selected Account. A custom token needs **Account Settings → Read** and **Workers Scripts → Write/Edit**. If `workers/subdomain` returns 403, report the missing permission and do not inflate the token scope. **Core install does not require R2**; add Workers R2 Storage only for artifact relay.

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

The enrolled identity is an immutable `device_id`, for example `dev_01ARZ3NDEKTSV4RRFFQ69G5FAV`: `dev_` plus a 26-character ULID. Keep the display name separate from identity. Do **not** invent a `WORKSTATION_ID` from the hostname.

See [join an existing fleet](existing-worker-connect.md) for detail.

## 6. Link and network

`worker bootstrap` / `worker connect` creates a per-device credential and reconciles the production Link. The Agent only needs to verify:

```bash
herdr-mcp status
herdr-mcp doctor
herdr-mcp link status
```

Without a Custom Domain, try `workers.dev` directly first. On DNS failure, runtimes with this recovery query Cloudflare DNS then Google DNS; only a candidate passing the real TLS `/health` Herdr check may become a marked single-host system-hosts entry. On v0.4.8, the Agent performs the same verified hosts recovery from the manual install guide before rerunning bootstrap. Unix may require interactive `sudo`; Windows may require an elevated terminal. Retry direct Link next, then an existing local proxy; keep signed shared Relay last. Never change system DNS, network nodes, OAuth issuer, or the public MCP origin.

## 7. Final acceptance

Combine read-only checks into one final verification wave instead of repeating the same checks after every installation step. Installation is complete only when all of these are proven:

- `herdr-mcp status` / `doctor` are healthy;
- `herdr-mcp link status` shows the enrolled production Link online;
- the canonical public origin serves `/health` and OAuth discovery correctly;
- the machine has a canonical `dev_<ULID>` device identity;
- one real authenticated MCP request completes from the public origin to this workstation and back.

Then in ChatGPT enable Developer mode for Plugins, open **Plugins → Browse plugins**, and add `herdr` with the complete `https://…workers.dev/mcp` address, including `/mcp`; finish OAuth. Work inside a ChatGPT Project. In every new chat, use the `+` button in the first message to reference `herdr` so that chat enables the plugin.

The Chrome extension / Native Messaging path is optional and is not a prerequisite for the core Connector. Install it from the [extension guide](extension.md) only when the user wants browser continuity, handoff, or the Control Center; keep extension distribution/development details in that guide.

## 8. Cleanup and report

Unset `CLOUDFLARE_API_TOKEN` / `CLOUDFLARE_ACCOUNT_ID` and delete any temporary credential file. Report only non-secret facts: runtime version, device name/id (shortened for display if useful), Worker name, final public origin, Link status, `/health`, OAuth, and MCP E2E result.

If permissions, network, OAuth, existing-Worker ownership, or mutation-delivery uncertainty blocks progress, open the matching section of the [manual install guide](install.md) or [troubleshooting](troubleshooting.md). Do not copy the entire maintainer runbook into the execution context up front.