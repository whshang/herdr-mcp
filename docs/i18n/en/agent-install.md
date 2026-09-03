# Agent install

*End-to-end Agent installation contract, Cloudflare origin selection, and resilient Link setup.*

> **Executor: Agent.** This is the authoritative ordinary-workstation installation contract. It incorporates the former quick-install flow and the detailed security/operations contract. [Installation reference](install.md) is for manual/operator lookup.

The Agent reads and executes this page directly; the human does not need to wrap it in another product-specific prompt. Human participation is limited to actions that genuinely require personal authorization or choice, such as Cloudflare sign-in/API Token creation, an account choice that cannot be inferred safely, and ChatGPT Connector/OAuth. The Agent owns environment checks, published Release installation, Cloudflare Worker deployment, the outbound WSS Link, optional extension-channel selection, and verification.

> Current boundary: fully automated background-service setup is **macOS Apple Silicon first**. Windows may receive Release artifacts as preview. Do not invent unsupported Linux lifecycle packaging. Edge deploy may use Node/`wrangler` temporarily; the local MCP runtime itself must come from GitHub Releases, not `git clone` + `npm ci`.

## 0. Agent contract

1. Execute work directly; do not offload automatable shell steps to the human. Pause only for Cloudflare interactive login/API-Token creation, a genuinely ambiguous Account/zone choice, or the one-time choice to accept/decline the recommended Custom Domain when user intent is not already clear.
2. Preserve existing work. Never `reset --hard`, `clean -fd`, or overwrite user changes in an unrelated checkout.
3. Choose one canonical public origin during the first install and keep it consistent for Worker OAuth and MCP identity. `workers.dev` is the zero-domain **bootstrap** path, not the preferred long-lived production identity when the selected Cloudflare Account already has an active zone. Before registering the ChatGPT Connector/OAuth identity, discover usable zones and prefer a Custom Domain such as `herdr-mcp.example.com` for the stable production identity. If exactly one safe zone/hostname is obvious, present that as the recommended choice; if several zones are plausible, ask only which zone to use. If no active zone exists, the user declines, or the hostname conflicts with an existing record, continue on `workers.dev` without blocking installation. Cloudflare Custom Domains create their Worker DNS record and certificate automatically; do not request generic DNS Write for this path. The Link transport must not silently rewrite the chosen OAuth issuer later.
4. Treat the Cloudflare Token as a high-sensitivity credential. Never echo it or write it to the repo, `.env`, ordinary logs, screenshots, or shell history. Prefer process-environment injection; if a temporary file is unavoidable, use mode `0600` and delete it immediately after deployment.
5. Verify every mutation before continuing. On an error, determine whether the mutation already committed before retrying.
6. Do **not** install the local MCP runtime by cloning this repository or running `npm`/`cargo` unless the human explicitly asked for a contributor/from-source session.
7. If network, login state, or third-party availability blocks the requested path, stop and report the blocker. Do not build a proxy, switch network nodes, rewrite system proxy settings, or invent a bypass.

## 0.5 Fleet ownership gate — before any Cloudflare mutation

Resolve fleet intent before requesting a Cloudflare Token, naming a Worker, creating R2, deploying Edge, or creating another ChatGPT Connector. An empty local `~/.config/herdr-mcp` directory does **not** mean this is the user's first Herdr computer.

- If the user supplied a Herdr pairing address, this computer is joining an existing Worker. Install/verify the local runtime and macOS permission, then use `herdr-mcp worker connect "<pairing-address>"`. The CLI uses the macOS Computer Name by default; pass `--name` only when the user explicitly wants a different initial display name. Skip Worker/R2/Connector creation.
- If the user says another Herdr computer already exists but no pairing address is available, prefer creating the short-lived pairing directly in the already-authorized ChatGPT/Herdr conversation with Edge-local `herdr_mcp.device.pair`; it does not require an old workstation to be online. Show the pairing address, one-time 6-digit code, and exact expiry together. `herdr-mcp worker pair` on an authorized old computer remains the CLI fallback. Consume the pairing on the new computer with `worker connect` and reuse the existing public MCP/OAuth origin.
- If the user asks to permanently remove an old enrolled computer, prefer the same authorized ChatGPT/Herdr conversation: list devices with `herdr_devices`, select the immutable `device_id`, then call Edge-local `herdr_mcp.device.revoke` with `confirm=true`. Never revoke by display name. The old device does not need to be online. `herdr-mcp worker revoke <device-id> --confirm` remains the CLI fallback.
- If the user explicitly confirms this is the first Herdr computer/fleet, continue with the first-owner Cloudflare path below.
- If intent is unclear, ask exactly one ownership question: **create the first Herdr Worker, or join an existing Herdr Worker?** Do not infer the answer from missing local credentials.
- Pairing, old-Worker upgrade, hostname reachability, or permission failures stay on the existing-fleet repair path. Never fall back to creating a random-suffixed Worker, R2 bucket, or Connector unless the user explicitly changes the fleet intent.

## 1. Prerequisites

Run `herdr --version` and `herdr api schema >/dev/null`. Require a working `herdr` binary and the Herdr socket (default `~/.config/herdr/herdr.sock`, or an explicit `HERDR_SOCKET_PATH`). If Herdr is missing, install the official stable build directly:

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

On Windows use `powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"`. Re-run the checks afterward. If Herdr still does not become healthy, stop and report the blocker; herdr-mcp does not replace Herdr.

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

Treat a `doctor` permission result of `needs_setup`, `denied`, `unknown`, or `timeout` as a pause-and-fix point now, not as healthy. Completing Full Disk Access for the stable TCC broker once, up front, is what avoids repeated path-by-path permission prompts later during runtime, Herdr socket, and project access. Do not substitute `sudo` for the broker approval.

Node.js is required only for temporary Cloudflare Worker bootstrap (`npx wrangler`) and optional contributor tooling. It is **not** required to run the local MCP runtime.
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

Generate `WORKER_NAME` only through the repository helper when a temporary Edge checkout is available; the Agent must not invent its own hostname slug:

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
```

The helper lowercases the hostname, safely handles every character outside `[a-z0-9-]` (including `.`, `_`, whitespace, and non-ASCII input), collapses/trims `-`, and keeps the complete Worker name at or below 63 characters. The result must match `^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$`; for example `MacBook.local` becomes `herdr-edge-macbook-local`. `WORKER_NAME` (Cloudflare Worker naming) and the `dev_<ULID>` device identity intentionally use different grammars. Use strong randomness such as `openssl rand -hex 32` for secrets; never include secrets in the final report.

## 4. Cloudflare authorization pause

Open <https://dash.cloudflare.com/profile/api-tokens> when browser control is available; otherwise give the user that URL.

The simplest supported path is Cloudflare's current **Edit Cloudflare Workers** template, scoped to the single Account used for this install. It already includes the Worker script and **Workers Routes Write** permissions needed for the recommended Custom Domain route; it does not require generic DNS Write. **Core install does not require R2** and must not fail just because R2 is not enabled or the account has no payment method. Add **Account → Workers R2 Storage → Edit** only when the user explicitly enables the optional artifact relay (§6). Do not inflate permissions beyond what the chosen path needs.

For a tighter custom token, retain at least Account → **Workers Scripts → Write/Edit**, Account → **Account Settings → Read**, User → **Memberships → Read**, and User → **User Details → Read**. `Account Settings → Read` is required to read the account `workers.dev` subdomain. To discover and attach a Custom Domain, add Zone → **Zone → Read** and Zone → **Workers Routes → Write/Edit**, scoped to the chosen zone when practical. Do **not** add Zone → DNS Write for the normal Custom Domain path. If the user later enables the artifact relay, add Account → **Workers R2 Storage → Edit** at that point.

Tell the user the secret is shown once and ask them to paste it only into the current local-Agent session; prefer a dedicated secret-input channel when available.

## 5. Cloudflare preflight after the Token arrives

Inject it only as temporary `CLOUDFLARE_API_TOKEN`, never as a literal command-line argument. Verify `GET https://api.cloudflare.com/client/v4/user/tokens/verify`, then run `npx wrangler whoami` against a temporary Edge working directory.

- one Account → select automatically;
- multiple Accounts → ask only which Account name to use;
- invalid/under-scoped Token → stop mutations and name the exact missing permission.

A token can verify as **valid** (`/user/tokens/verify` returns active) and still get `403` on a specific call — that means a missing permission, not a bad token. Map the failing call to the permission instead of recreating a broader token blindly:

- `GET .../workers/subdomain` returns 403 → **Account Settings → Read** is missing;
- `wrangler deploy` / Workers Scripts calls fail → **Workers Scripts → Edit** is missing;
- R2 bucket provisioning fails → the optional **Workers R2 Storage → Edit** permission was not granted (expected on a core install; only an error if the user explicitly enabled the artifact relay).

Diagnose by permission, do not inflate scope speculatively, and never retry the mutation before the missing permission is granted.

**Existing-Worker detection before deploy.** With `ACCOUNT_ID`, list `GET /client/v4/accounts/<ACCOUNT_ID>/workers/scripts`. If an existing Herdr Worker is visible there — or the user has an existing ChatGPT Connector for Herdr — stop the deploy path and switch to the existing-fleet flow in [Multi-device control](existing-worker-connect.md) (`worker pair` on the owner, `worker connect` here). Deploy a new Worker only after the explicit first-fleet answer from §0.

After selection, keep the account ID only in the current deployment process environment:

```bash
export CLOUDFLARE_ACCOUNT_ID="$ACCOUNT_ID"
```

Do not write a personal `account_id` into tracked Wrangler config. Subsequent `wrangler deploy` and `wrangler secret put` inherit this temporary environment.

With `ACCOUNT_ID`, fetch `GET /client/v4/accounts/<ACCOUNT_ID>/workers/subdomain`. Reuse an existing account subdomain and **never rename it**. `ACCOUNT_SUBDOMAIN` is the selected Cloudflare Account's `workers.dev` subdomain returned by the Cloudflare API. The Worker origin always combines two independent values as `<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev`.

Also enumerate the active zones visible to the token before the public identity is finalized. This is discovery, not permission to mutate arbitrary DNS. If an active zone is available, prepare a collision-safe dedicated hostname (default suggestion: `herdr-mcp.<zone>`) and check that it is not already occupied by an incompatible DNS record/Worker. Prefer that Custom Domain for the final public MCP/OAuth identity. If no suitable zone exists or the user chooses not to use one, record `workers.dev` as the canonical origin and continue with no degraded-install warning: Relay fallback covers workstation transport, while the public MCP/OAuth endpoint remains the user's own Worker.

If no account subdomain exists, create one only because there is no old value; use `herdr-<short-account-id>` plus a random suffix on collision. Only after GET explicitly confirms that no subdomain exists, create one with:

```text
PUT /client/v4/accounts/<ACCOUNT_ID>/workers/subdomain
Content-Type: application/json

{"subdomain":"<candidate>"}
```

GET it again afterward and require the returned value to match before deploying the Worker.

## 6. Deploy Edge without requiring a permanent repo checkout

Obtain the Edge Worker sources needed for deploy (temporary shallow clone or Release-adjacent docs package is acceptable for this Edge step only). Generate ignored `wrangler.user.toml` from the published user example, then set `name` and `DEFAULT_WORKSTATION_ID`. Keep `workers_dev = true` so `https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev` remains the zero-domain bootstrap/diagnostic origin.

Deploy once on `workers.dev` and prove `/health` before binding the long-lived OAuth identity. Then finalize exactly one public origin **before** creating the ChatGPT Connector:

- **Recommended when an active Cloudflare zone is available:** add `[[routes]]`, set `pattern = "herdr-mcp.<zone>"` (or the user-selected dedicated hostname) and `custom_domain = true`, set `OAUTH_ISSUER=https://<custom-host>`, redeploy, and require `/health`, unauthenticated `/mcp`, and OAuth discovery to succeed on that hostname. Cloudflare creates the DNS record and certificate for a Custom Domain automatically.
- **No suitable zone / user declines:** leave `routes = []`, set `OAUTH_ISSUER=https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev`, and keep that `workers.dev` origin as the canonical public identity.

Do not register the Connector against the bootstrap hostname and then silently migrate it later. Once OAuth clients attach, public-origin changes are a deliberate migration operation.

**R2 is optional and off by default.** Keep the `[[r2_buckets]]` binding in `wrangler.user.toml` commented out for a core install; the published user example ships it that way. The Edge code treats `ARTIFACT_BUCKET` as optional, and the core deploy must succeed on Workers Free without R2 or a bound payment card. Only when the user explicitly enables the optional artifact relay: uncomment the binding, ensure the token has **Workers R2 Storage → Edit** (plus any Cloudflare R2 billing step), and run the provisioning step before deploy. `wrangler deploy` fails closed when a bound bucket is missing — which is exactly why the default path ships without the binding. When R2 stays disabled, skip provisioning and deploy directly:

```bash
npx wrangler deploy --config wrangler.user.toml
```

Artifact-relay enablement (optional path only):

```bash
node provision-r2.mjs --config wrangler.user.toml
npx wrangler deploy --config wrangler.user.toml
```

Never overwrite a pre-existing Worker unless this install can prove it owns it. If the intended name already exists, stop and determine whether it is the user's existing Herdr Worker; do **not** evade that ownership check by creating a random-suffixed Worker. Only an explicitly confirmed first-fleet install may choose a new unique Worker name after proving the existing Worker is unrelated. Then store the WSS shared secret as a Cloudflare Worker secret:

```bash
printf '%s' "$LINK_SHARED_SECRET" | npx wrangler secret put LINK_SHARED_SECRET --config wrangler.user.toml
```

No Zone/DNS mutation is required. A temporary checkout used only for Edge deploy must not become the production PATH for `herdr-mcp`.

## 7. macOS local MCP service ownership

Prefer the already-installed Release binary path:

```bash
herdr-mcp install
herdr-mcp status
herdr-mcp doctor
```

Do not recreate a repo-linked `~/.local/bin/herdr-mcp` bridge. Do not point LaunchAgent at a git checkout or `target/*/herdr-mcp`.

Browser extension / Native Messaging remains optional and is not required for the first ChatGPT closed loop. Extension channels are separate from Runtime DEV/PROD: **STORE / STANDALONE / DEV**.

- STORE: default ordinary-user path, with the fixed Chrome Web Store identity and Store updates.
- STANDALONE: v0.4.3+ GitHub/manual fixed-identity package, used when Store installation is unavailable or the user explicitly requests independent distribution.
- DEV: source development only, loaded unpacked from a repo/worktree `extension/` directory with a path-derived ID.

The Agent must inspect what the installed runtime actually supports. v0.4.2 has Store/DEV ownership only; do not invent standalone support. STANDALONE is a source-development-independent distribution channel, while DEV remains source-development only. On runtimes that expose it, select STANDALONE explicitly with `herdr-mcp native-host use standalone`. After selecting/installing a supported channel, run:

```bash
herdr-mcp native-host status
```

Status should identify the expected active channel/extension identity and confirm the Native Host runtime is consistent with the active runtime generation. See [Browser extension](extension.md) and [Browser continuity](browser-continuity.md).

## 8. macOS persistent Herdr Link

Store `LINK_SHARED_SECRET` in Keychain under `herdr-edge-link-<WORKSTATION_ID>`. The command text must reference the environment variable rather than a literal secret. Prefer the managed Link install path exposed by the installed `herdr-mcp` binary (`herdr-mcp link ...` / current stable product docs). Do not leave production Link ownership on a repository Bash wrapper.

The Agent should inspect network reachability instead of asking the user to choose a transport. Run `herdr-mcp doctor`, `herdr-mcp link status`, and bounded `/health` probes for the selected public origin / backing `workers.dev` origin. The Link reuses proxy settings that already exist in the user's environment; recognition precedence is `HERDR_LINK_PROXY` > `HTTPS_PROXY`/`https_proxy` > `HTTP_PROXY`/`http_proxy` > `ALL_PROXY`/`all_proxy`, and macOS also reads the existing `scutil --proxy` state (HTTPS, then HTTP, then SOCKS). `socks5://`/`socks5h://` URLs are supported with remote-DNS semantics, proxy authentication is not supported, and a macOS PAC configuration is detected but never evaluated.

Transport selection is intentionally automatic. With a configured Custom Domain, the Link uses that stable direct path and does not use the shared Relay Pool. Without a Custom Domain, it tries direct `workers.dev`, then an already-configured validated local proxy, then the built-in qualified Herdr Relay baseline (Deno-dominant with Supabase fallback); a newer valid signed Relay Pool cache overrides that baseline. The user never creates a Deno/Supabase account or configures a Relay URL. Do not change system proxy settings, network nodes, DNS, or the chosen public identity merely to make a probe pass.

## 9. Verify the closed loop

Verify local `server/discover`, `herdr-mcp status`, `herdr-mcp doctor`, Link status, Worker `/health`, public `/mcp`, and OAuth discovery. Record both the bootstrap `workers.dev` origin and the final canonical public origin. If a Custom Domain was selected, prove the Connector-facing hostname before registration; otherwise prove that the `workers.dev` public identity is healthy and that `link status` reports a usable automatic fallback ladder / Relay candidate pool. Doctor may probe Edge `/health`, OAuth metadata, and `/mcp` without sending tokens; never print tokens.

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
