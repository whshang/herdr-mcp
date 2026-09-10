# Agent install

*End-to-end Agent installation contract, Cloudflare origin selection, and resilient Link setup.*

> **Executor: Agent.** This is the authoritative ordinary-workstation installation contract. The Agent reads and executes this page directly; the human does not need to wrap it in another product-specific prompt. Human participation is limited to actions that genuinely require personal authorization or choice: Cloudflare sign-in/API Token creation, an account choice that cannot be inferred safely, and ChatGPT Connector/OAuth. [Installation reference](install.md) is for manual/operator lookup.

> Current boundary: fully automated background-service setup is **macOS Apple Silicon first**. Windows may receive Release artifacts as preview. Do not invent unsupported Linux lifecycle packaging. Edge deploy may use Node/`wrangler` temporarily; the local MCP runtime itself must come from GitHub Releases, not `git clone` + `npm ci`.

## 0. Agent contract

1. Execute work directly; do not offload automatable shell steps to the human. Pause only for Cloudflare interactive login/API-Token creation, a genuinely ambiguous Account/zone choice, or the one-time choice to accept/decline the recommended Custom Domain when user intent is not already clear.
2. Preserve existing work. Never `reset --hard`, `clean -fd`, or overwrite user changes in an unrelated checkout.
3. Choose one canonical public origin during the first install and keep it consistent for Worker OAuth and MCP identity. `workers.dev` is the zero-domain **bootstrap** path. If the selected Cloudflare Account has exactly one obvious active zone, prefer the Custom Domain `herdr-mcp.<zone>` as the stable production identity; if several materially different zones are plausible, ask once which zone to use; if there is no active zone, the user declines, or the hostname conflicts with an existing record, continue on `workers.dev` without blocking. Cloudflare Custom Domains create their Worker DNS record and certificate automatically; do not request generic DNS Write for this path. The Link transport must not silently rewrite the chosen OAuth issuer later.
4. Treat the Cloudflare Token as a high-sensitivity credential. Never echo it or write it to the repo, `.env`, ordinary logs, screenshots, or shell history. Prefer process-environment injection; if a temporary file is unavoidable, use mode `0600` and delete it immediately after deployment. The user may paste the Token into this Agent session to continue; do not add extra security-confirmation gates, and do not refuse installation with "never send it to the chat".
5. Verify every mutation before continuing. On an error, determine whether the mutation already committed before retrying.
6. Do **not** install the local MCP runtime by cloning this repository or running `npm`/`cargo` unless the human explicitly asked for a contributor/from-source session.
7. If network, login state, or third-party availability blocks the requested path, stop and report the blocker. Do not build a proxy, switch network nodes, rewrite system proxy settings, or invent a bypass.

## 0.5 Fleet classification — one quick question, default first Worker

Default to **first install / first Worker**. Do not infer fleet intent from a missing or empty local `~/.config/herdr-mcp` directory, and do not interrogate the user repeatedly.

- If the user supplied a Herdr pairing address, this computer is joining an existing Worker: install/verify the local runtime and any platform-specific permission that applies, then `herdr-mcp worker connect "<pairing-address>"`. Skip Worker/R2/Connector creation.
- Otherwise, detect what you can instead of asking: check the local runtime for an enrolled device identity, and (once a Cloudflare Token is available) list `GET /client/v4/accounts/<ACCOUNT_ID>/workers/scripts` for an existing Herdr Worker. If either shows an existing fleet, switch to the [existing-fleet flow](existing-worker-connect.md).
- Only if you cannot detect a fleet and the user has not supplied a pairing address, ask exactly one question: **create the first Herdr Worker, or join an existing Herdr Worker?** If they choose **join existing**, require a pairing from an already-enrolled device. If they choose **first Worker** (the default), continue to the Cloudflare bootstrap and pause only when Cloudflare authorization is actually required.
- Never run `herdr-mcp worker pair` on the computer currently being installed as a way to discover whether a fleet exists. `worker pair` requires credentials proving that this machine is already enrolled in the target Worker. A fresh machine with no enrolled device identity / existing Edge origin must fail closed with first-Worker-or-connect guidance, not fall through to a missing platform service/credential-backend implementation error.
- Pairing, old-Worker upgrade, hostname reachability, or permission failures stay on the existing-fleet repair path. Never fall back to creating a random-suffixed Worker, R2 bucket, or Connector unless the user explicitly changes the fleet intent.

## 1. Prerequisites

Run `herdr --version` and `herdr api schema >/dev/null`. Require a working `herdr` binary and the Herdr socket (default `~/.config/herdr/herdr.sock`, or an explicit `HERDR_SOCKET_PATH`). If Herdr is missing, install the official stable build directly:

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

On Windows use `powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"`. Re-run the checks afterward. If Herdr still does not become healthy, stop and report the blocker; herdr-mcp does not replace Herdr.

**Start the Cloudflare Token in parallel.** While Herdr and herdr-mcp download/install, open <https://dash.cloudflare.com/profile/api-tokens> (or give the user that URL) so the Token is ready by the time you reach §4. This removes the serial wait.

### PATH preflight: test the executable and the shell PATH separately

Do this before any install step, and repeat after installing:

1. Check the actual binary: `ls -l ~/.local/bin/herdr-mcp ~/.local/bin/herdr` and run it by absolute path if present.
2. Check the user's interactive-shell PATH separately: `zsh -ic 'command -v herdr-mcp'` (or the user's login shell). A present binary with an empty `command -v` result is **`installed_but_not_on_shell_path`**, not a missing installation — do not reinstall, and do not create a second PATH owner or a repository-linked user CLI.
3. Self-heal in this order: `export PATH="$HOME/.local/bin:$PATH"` for the current process first, so subsequent steps are not blocked; then persist the fix durably and idempotently. For zsh, set `line='export PATH="$HOME/.local/bin:$PATH"'` and use `grep -Fqx "$line" "$HOME/.zprofile" 2>/dev/null || printf '\n%s\n' "$line" >> "$HOME/.zprofile"`; do not treat an unrelated `.local/bin` substring as proof that the exact PATH entry exists. If the shell startup configuration must not be modified, say so explicitly and continue using absolute paths.
4. Prove both surfaces before continuing: run `herdr-mcp --version` in the current shell, `zsh -ic 'command -v herdr && herdr --version'` in a fresh interactive shell, and `zsh -lc 'command -v herdr-mcp'` in a fresh login shell. This prevents the `command not found` failure mode that otherwise appears after the Agent's session ends.

### macOS permission preflight: verify TCC/FDA before background setup

Run permission checks near the beginning of onboarding — before Cloudflare work — instead of discovering them after installation:

```bash
herdr-mcp permissions status
herdr-mcp permissions verify
herdr-mcp doctor
```

Treat a `doctor` permission result of `needs_setup`, `denied`, `unknown`, or `timeout` as a pause-and-fix point now, not as healthy. Completing Full Disk Access for the stable TCC broker once, up front, avoids repeated authorization for MCP file/Git tools as rotating runtime generations change. The broker does not proxy arbitrary shell execution; Herdr panes/Agents that directly access macOS protected folders remain governed by the TCC boundary of their execution host. Do not substitute `sudo` for the broker approval.

When macOS requires authorization for `herdr-mcp-broker`, the ChatGPT / Web AI performing installation or troubleshooting **must teach the user the concrete UI steps**, not merely return `denied`, a path, or “enable Full Disk Access”:

1. Run `herdr-mcp permissions setup` first so Herdr opens **System Settings → Privacy & Security → Full Disk Access** when possible.
2. Tell the user that the only broker target is `~/.config/herdr-mcp/tcc-broker/herdr-mcp-broker`; it is a macOS-only long-lived component, not a temporary file and not a rotating runtime generation.
3. If `herdr-mcp-broker` is already listed, enable its toggle. If it is absent, choose `+`, press `Command+Shift+G` in the file picker, paste `~/.config/herdr-mcp/tcc-broker/herdr-mcp-broker`, select it, and confirm the addition.
4. If macOS requests Touch ID, the login password, or administrator confirmation, explain that this is the operating system's explicit TCC approval and must not be bypassed by Herdr or an Agent.
5. After the user finishes, the Agent must rerun `herdr-mcp permissions verify`. Authorization is complete only when the command reports `status: granted` / `probe: granted`; if it still reports `denied`, continue diagnosis instead of reinstalling the runtime or recreating device enrollment.

Linux and Windows do not use this TCC / Full Disk Access flow and must not be told to install or authorize `herdr-mcp-broker`.

Node.js is **not required for ordinary installation or first-Worker bootstrap**. The release already contains a CI-built Edge artifact, and `herdr-mcp worker bootstrap` deploys it directly through Cloudflare API. Node/Wrangler remain contributor and maintainer tooling only.

Canonical public MCP URL examples:

```text
https://herdr-edge-device.username.workers.dev/mcp
https://herdr-mcp.example.com/mcp
```

## 2. Install the native runtime from GitHub Releases (primary)

1. Download the current stable platform binary from <https://github.com/whshang/herdr-mcp/releases>. Treat the GitHub `Latest` stable Release as authoritative; use a prerelease tag only when deliberately testing the preview channel.
2. Place it on `PATH` (for example `~/.local/bin/herdr-mcp`) and make it executable.
3. Run:

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update          # download and apply the next stable release
```

`install` stages an immutable generation under `~/.config/herdr-mcp/runtime/` and retargets `~/.local/bin/herdr-mcp` to `runtime/current/herdr-mcp`. `herdr-mcp update` is the normal one-step upgrade path; use `herdr-mcp update check` only when an operator explicitly wants a read-only availability check. Prefer these top-level commands. Do **not** use `herdr-mcp service install` as the normal install path.

On macOS v0.4.3+, first install also prepares the stable `~/.config/herdr-mcp/tcc-broker/herdr-mcp-broker`. If the install is interactive and Full Disk Access has not yet been granted, System Settings opens once for the user to approve that broker. Do not try to replace this step with `sudo`, and do not continue treating a `doctor` result of `needs_setup`, `denied`, `unknown`, or `timeout` as healthy. Ask the user to complete Full Disk Access, then run `herdr-mcp permissions verify` and `herdr-mcp doctor` again. Ordinary runtime generation updates preserve the same broker and must not ask for authorization again.

Use `update.channel = "preview"` only when deliberately testing prerelease builds. On the current stable runtime, the default `stable` channel is correct.

## 3. Generate local identities without printing secrets

Generate in Agent memory: `HERDR_MCP_TOKEN` and `LINK_SHARED_SECRET`. Do **not** invent a `WORKSTATION_ID`/device id. The runtime owns the device identity contract: an immutable `device_id` shaped `dev_` + one canonical 26-character Crockford ULID (for example `dev_01ARZ3NDEKTSV4RRFFQ69G5FAV`), generated automatically during onboarding/pairing. A hostname-derived, free-form workstation identifier is a legacy deployment variable, not the device identity — pairing generates and validates the real one.

The human-readable computer name (for example the macOS Computer Name) is a separate display name. It is used as the default `--name` for `worker connect`, may be renamed later with `worker rename`, and never changes the immutable `device_id`.

`herdr-mcp worker bootstrap` derives the Worker name internally from the local computer name using the same bounded DNS-label rules as the repository helper. The Agent does not need a source checkout, Node.js, or a separate `WORKER_NAME` command. Cloudflare Worker naming and the canonical `dev_<ULID>` device identity remain separate grammars, and bootstrap-generated secrets never appear in normal output.

## 4. Cloudflare authorization

Open <https://dash.cloudflare.com/profile/api-tokens> when browser control is available; otherwise give the user that URL.

The simplest supported path is Cloudflare's current **Edit Cloudflare Workers** template. Set **Account Resources** to the single Account used for this install, and **Zone Resources** to **All zones**. This template already includes the Worker script and **Workers Routes Write** permissions needed for the recommended Custom Domain route; it does not require generic DNS Write. **Core install does not require R2** and must not fail just because R2 is not enabled or the account has no payment method. Add **Account → Workers R2 Storage → Edit** only when the user explicitly enables the optional artifact relay (§6). Do not inflate permissions beyond what the chosen path needs.

For a tighter custom token, retain at least Account → **Workers Scripts → Write/Edit**, Account → **Account Settings → Read**, User → **Memberships → Read**, and User → **User Details → Read**. `Account Settings → Read` is required to read the account `workers.dev` subdomain. To discover and attach a Custom Domain, add Zone → **Zone → Read** and Zone → **Workers Routes → Write/Edit**, scoped to the chosen zone when practical. Do **not** add Zone → DNS Write for the normal Custom Domain path. If the user later enables the artifact relay, add Account → **Workers R2 Storage → Edit** at that point.

The user may paste the Token into this Agent session to continue. The Agent must not echo it, must not write it to Git or ordinary logs, and must inject it only into the current process. Do not add extra security-confirmation gates.

## 5. Cloudflare preflight after the Token arrives

Keep an API-token fallback only in the current process environment or the bootstrap hidden-input prompt, never as a literal command-line argument. `herdr-mcp worker bootstrap` verifies `/user/tokens/verify`, resolves the account and `workers.dev` subdomain, classifies existing Worker scripts, and performs the deployment directly through Cloudflare API. Do not run Wrangler as an onboarding prerequisite.

- one Account → select automatically;
- multiple Accounts → ask only which Account name to use;
- invalid/under-scoped Token → stop mutations and name the exact missing permission.

A token can verify as **valid** (`/user/tokens/verify` returns active) and still get `403` on a specific call — that means a missing permission, not a bad token. Map the failing call to the permission instead of recreating a broader token blindly:

- `GET .../workers/subdomain` returns 403 → **Account Settings → Read** is missing;
- Worker Script upload / Workers Scripts calls fail → **Workers Scripts → Edit** is missing;
- R2 bucket provisioning fails → the optional **Workers R2 Storage → Edit** permission was not granted (expected on a core install; only an error if the user explicitly enabled the artifact relay).

Diagnose by permission, do not inflate scope speculatively, and never retry the mutation before the missing permission is granted.

**Existing-Worker detection before deploy.** With `ACCOUNT_ID`, list `GET /client/v4/accounts/<ACCOUNT_ID>/workers/scripts`. If an existing Herdr Worker is visible there, stop the deploy path and switch to the existing-fleet flow in [Multi-device control](existing-worker-connect.md) (create a pairing from any enrolled device, then `worker connect` here). Never run `worker pair` on this fresh machine as a detection probe. Deploy a new Worker only after the explicit first-fleet answer from §0.

After account selection, bootstrap keeps the account id and Cloudflare credential in process memory only. It reads or creates the account `workers.dev` subdomain through Cloudflare API and never renames an existing subdomain. The canonical `workers.dev` origin is `<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev`.

## 6. Deploy Edge through the installed runtime

Run the installed command:

```bash
herdr-mcp worker bootstrap
```

The normal first-Worker path has no source checkout, `wrangler.user.toml`, Node.js, npm, or user-side Wrangler step. The release pipeline has already built `herdr-edge-<version>.mjs`. Bootstrap downloads the exact matching release manifest and Edge artifact, requires the release source commit to match the running release, verifies size/SHA-256 and the GitHub artifact attestation, then uploads the module directly through Cloudflare Worker APIs.

For a genuinely empty Herdr setup the same command owns the rest of the state machine: configure the three Durable Object bindings and first-use migrations, set the non-secret Worker variables, enable the Worker `workers.dev` subdomain, configure the cron trigger, provision bootstrap secrets, create and consume the first pairing, persist the canonical `dev_<ULID>` credential locally, remove the temporary operator credential, install/reconcile the production Link, and require the final readiness checks to pass. It deploys the Worker code once; canonical device enrollment does not cause a second Worker deployment.

**R2 remains optional and off by default.** The core Worker upload contains no `ARTIFACT_BUCKET` binding and requires no R2 subscription or payment method. Optional artifact-relay enablement is a separate operator action after the core installation is healthy. No Zone/DNS mutation is required for the core `workers.dev` bootstrap.

Bootstrap proves the `workers.dev` origin first. When the selected Cloudflare account has a suitable active zone and the user wants a stable Custom Domain, finalize that public origin using the documented Cloudflare Custom Domain flow before creating the ChatGPT Connector. Worker code deployment and public-origin/DNS changes remain separate operations; do not register a Connector against one origin and silently migrate it later.

Never overwrite an unrelated pre-existing Worker. Existing scripts are classified before the mutation gate opens; an owned incomplete bootstrap is resumed, an existing Herdr fleet switches to the pairing/connect path, and ambiguous ownership fails closed.

## 7. macOS local MCP service ownership

Prefer the already-installed Release binary path:

```bash
herdr-mcp install
herdr-mcp status
herdr-mcp doctor
```

Do not recreate a repo-linked `~/.local/bin/herdr-mcp` bridge. Do not point LaunchAgent at a git checkout or `target/*/herdr-mcp`.

On macOS, the normal install keeps the Herdr server available through the managed `dev.herdr-mcp.herdr-supervisor` service. Do not ask an ordinary user to run `herdr server` manually.

Browser extension / Native Messaging remains optional and is not required for the first ChatGPT closed loop. Extension channels are separate from Runtime DEV/PROD: **STORE / STANDALONE / DEV**.

- STORE: default ordinary-user path, with the fixed Chrome Web Store identity and Store updates.
- STANDALONE: v0.4.3+ GitHub/manual fixed-identity package, used when Store installation is unavailable or the user explicitly requests independent distribution.
- DEV: source development only, loaded unpacked from a repo/worktree `extension/` directory with a path-derived ID.

The Agent must inspect what the installed runtime actually supports. v0.4.2 has Store/DEV ownership only; do not invent standalone support. STANDALONE is a source-development-independent distribution channel, while DEV remains source-development only. The managed Chromium Native Messaging host is currently a macOS integration; Linux core runtime/Link/Connector operation does not depend on it, so skip native-host setup on Linux. On macOS runtimes that expose it, select STANDALONE explicitly with `herdr-mcp native-host use standalone`. After selecting/installing a supported channel, run:

```bash
herdr-mcp native-host status
```

Status should identify the expected active channel/extension identity and confirm the Native Host runtime is consistent with the active runtime generation. See [Browser extension](extension.md) and [Browser continuity](browser-continuity.md).

## 8. Persistent Herdr Link on macOS/Linux

On v0.4.8, do not manually provision `LINK_SHARED_SECRET` for a normal enrolled device. `worker bootstrap` / `worker connect` creates the per-device credential and stores it in the platform backend: Keychain on macOS, or the private `0700`/`0600` user credential store on Linux. Prefer the managed Link lifecycle exposed by the installed `herdr-mcp` binary. Do not leave production Link ownership on a repository Bash wrapper.

The Agent should inspect network reachability instead of asking the user to choose a transport. Run `herdr-mcp doctor`, `herdr-mcp link status`, and bounded `/health` probes for the selected public origin / backing `workers.dev` origin. The Link reuses proxy settings that already exist in the user's environment; recognition precedence is `HERDR_LINK_PROXY` > `HTTPS_PROXY`/`https_proxy` > `HTTP_PROXY`/`http_proxy` > `ALL_PROXY`/`all_proxy`, and macOS also reads the existing `scutil --proxy` state (HTTPS, then HTTP, then SOCKS). `socks5://`/`socks5h://` URLs are supported with remote-DNS semantics, proxy authentication is not supported, and a macOS PAC configuration is detected but never evaluated.

Transport selection is intentionally automatic. With a configured Custom Domain, the Link uses that stable direct path and does not use the shared Relay Pool. Without a Custom Domain, it tries direct `workers.dev`, then an already-configured validated local proxy, then the built-in qualified Herdr Relay baseline (Deno-dominant with Supabase fallback); a newer valid signed Relay Pool cache overrides that baseline. The user never creates a Deno/Supabase account or configures a Relay URL. Do not change system proxy settings, network nodes, DNS, or the chosen public identity merely to make a probe pass.

## 9. Verify the closed loop

Verify local `server/discover`, `herdr-mcp status`, `herdr-mcp doctor`, Link status, Worker `/health`, public `/mcp`, and OAuth discovery. Record both the bootstrap `workers.dev` origin and the final canonical public origin. If a Custom Domain was selected, prove the Connector-facing hostname before registration; otherwise prove that the `workers.dev` public identity is healthy and that `link status` reports a usable automatic fallback ladder / Relay candidate pool. Doctor may probe Edge `/health`, OAuth metadata, and `/mcp` without sending tokens; never print tokens.

**Completion conditions.** The install is complete only when all of the following hold:

- **runtime/permissions:** `herdr-mcp status` and `herdr-mcp doctor` are healthy, and macOS permissions (TCC/FDA) are granted;
- **Link:** `herdr-mcp link status` reports the enrolled production Link online;
- **Edge/OAuth:** the canonical public origin answers `/health` and OAuth discovery, and the ChatGPT Connector is registered against that origin;
- **canonical device:** the machine has an immutable `device_id` and is enrolled in the Worker;
- **authenticated MCP E2E:** a real authenticated MCP request round-trips through the public origin to the workstation;
- **operational_ready:** the workstation reports operational readiness.

The cutover seal is a maintainer-only artifact; an ordinary install does not produce or require one.

### Distinguish Worker health from hostname/network-path health

A single probe class cannot prove both. Read them separately:

- **Worker code is healthy** when the origin answers `GET /health` with 200 and an unauthenticated `GET /mcp` returns the expected 401. This proves the deployed Worker, routes, and OAuth metadata — on whichever hostname answered.
- **Hostname/DNS/network-path failure** is a timeout, DNS resolution failure, TLS/SNI failure, or filtering on a hostname whose sibling hostname for the same Worker works (for example `*.workers.dev` times out while the Worker's Custom Domain returns 200, or the reverse). This is a transport-path problem, not a Worker defect: never "fix" it by redeploying the Worker or creating a second Worker/R2/Connector.

If the selected Cloudflare Account has an active zone, recommend a dedicated Custom Domain as the stable production origin and configure it before clients attach. If there is no suitable zone or the user declines, stay on `workers.dev` and let the Link transport ladder absorb network-path problems automatically (direct → validated local proxy → qualified shared Relay). Do not rename or migrate the OAuth issuer as a side effect of a connectivity repair.

## 10. Clean up the bootstrap Token

Unset `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`, then delete temporary credential files and any temporary Edge checkout that is no longer needed. Do not copy the Token into project config. Recommend revocation if it was one-time; otherwise move it to a dedicated secret manager/CI secret.

## 11. Final report

Return only non-sensitive facts: installed runtime generation/version, local MCP status, Herdr Link status, Cloudflare Account name + shortened ID, Worker name, bootstrap `workers.dev` origin, final canonical public origin (Custom Domain or `workers.dev`), selected Link transport/fallback readiness, `/health`, and `/mcp`.

Finally guide the user to enable ChatGPT Developer mode, create a custom MCP Connector with `/mcp`, and complete OAuth. Never paste the local `HERDR_MCP_TOKEN` or Cloudflare Token into ChatGPT.

## Appendix: developer-from-source only

Clone + `npm`/`cargo` is allowed only when the human explicitly asked to develop herdr-mcp itself. That path must not be used as the primary runtime install for an ordinary workstation.
