# Local-Agent install and `workers.dev` deployment

> **Role:** maintainer/automation implementation reference for a local coding Agent. End users should start from [Quick agent install](quick-agent-install.md) or [Installation](install.md).

This is an execution contract for a **local coding Agent**, not a list of commands for the human to copy. End-user one-sentence onboarding lives in [Quick agent install](quick-agent-install.md). The human should only need to perform Cloudflare interactive login/API-Token creation; the Agent owns environment checks, Release-binary install, Cloudflare Worker deployment, the outbound WSS Link, and verification.

> Current boundary: fully automated background-service setup is **macOS Apple Silicon first**. Windows may receive Release artifacts as preview. Do not invent unsupported Linux lifecycle packaging. Edge deploy may use Node/`wrangler` temporarily; the local MCP runtime itself must come from GitHub Releases, not `git clone` + `npm ci`.

## 0. Agent contract

1. Execute work directly; do not offload automatable shell steps to the human. Pause only for Cloudflare interactive login/API-Token creation or selection among multiple Cloudflare Accounts.
2. Preserve existing work. Never `reset --hard`, `clean -fd`, or overwrite user changes in an unrelated checkout.
3. First install uses `workers.dev` only. Do not create a Custom Domain, DNS record, Cloudflare Tunnel, or mutate an existing zone.
4. Treat the Cloudflare Token as a high-sensitivity credential. Never echo it or write it to the repo, `.env`, ordinary logs, screenshots, or shell history. Prefer process-environment injection; if a temporary file is unavoidable, use mode `0600` and delete it immediately after deployment.
5. Verify every mutation before continuing. On an error, determine whether the mutation already committed before retrying.
6. Do **not** install the local MCP runtime by cloning this repository or running `npm`/`cargo` unless the human explicitly asked for a contributor/from-source session.

## 1. Prerequisites

Run `herdr --version` and `herdr api schema >/dev/null`. Require a working `herdr` binary and the Herdr socket (default `~/.config/herdr/herdr.sock`, or an explicit `HERDR_SOCKET_PATH`). If Herdr itself is not installed/running, stop and direct the user to <https://herdr.dev>; herdr-mcp does not replace Herdr.

Node.js is required only for temporary Cloudflare Worker bootstrap (`npx wrangler`) and optional contributor tooling. It is **not** required to run the local MCP runtime.

## 2. Install the native runtime from GitHub Releases (primary)

1. Download the current stable platform binary from <https://github.com/whshang/herdr-mcp/releases> (current published stable runtime: `v0.4.1`; `v0.4.2` is merged but not yet tagged/published). Use a prerelease tag only when deliberately testing the preview channel.
2. Place it on `PATH` (for example `~/.local/bin/herdr-mcp`) and make it executable.
3. Run:

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update          # same as update check
```

`install` stages an immutable generation under `~/.config/herdr-mcp/runtime/` and retargets `~/.local/bin/herdr-mcp` to `runtime/current/herdr-mcp`. Prefer these top-level commands. Do **not** use `herdr-mcp service install` as the normal install path.

Use `update.channel = "preview"` only when deliberately testing prerelease builds. On the current stable runtime, the default `stable` channel is correct.

## 3. Generate local identities without printing secrets

Generate in Agent memory: `HERDR_MCP_TOKEN`, `LINK_SHARED_SECRET`, and a hostname-derived `WORKSTATION_ID` limited to `[A-Za-z0-9_.-]` and 64 chars. Generate `WORKER_NAME` only through the repository helper when a temporary Edge checkout is available; the Agent must not invent its own hostname slug:

```bash
WORKER_NAME="$(node scripts/cloudflare-worker-name.mjs "$(hostname)")"
```

`WORKER_NAME` and `WORKSTATION_ID` intentionally use different grammars. The helper lowercases the hostname, safely handles every character outside `[a-z0-9-]` (including `.`, `_`, whitespace, and non-ASCII input), collapses/trims `-`, and keeps the complete Worker name at or below 63 characters. The result must match `^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$`; for example `MacBook.local` becomes `herdr-edge-macbook-local`. Use strong randomness such as `openssl rand -hex 32` for secrets; never include secrets in the final report.

## 4. Only human pause: Cloudflare API Token

Open <https://dash.cloudflare.com/profile/api-tokens> when browser control is available; otherwise give the user that URL.

The simplest supported path is Cloudflare's current **Edit Cloudflare Workers** template, scoped to the single Account used for this install, **plus Account → Workers R2 Storage → Edit**. Do **not** add DNS Write. R2 write is required so the generated-image relay bucket can be created before Worker deploy.

For a tighter custom token, retain at least Account → **Workers Scripts → Write/Edit**, Account → **Workers R2 Storage → Edit**, Account → **Account Settings → Read**, User → **Memberships → Read**, and User → **User Details → Read**. `workers.dev` bootstrap does not need Zone/DNS permissions.

Tell the user the secret is shown once and ask them to paste it only into the current local-Agent session; prefer a dedicated secret-input channel when available.

## 5. Cloudflare preflight after the Token arrives

Inject it only as temporary `CLOUDFLARE_API_TOKEN`, never as a literal command-line argument. Verify `GET https://api.cloudflare.com/client/v4/user/tokens/verify`, then run `npx wrangler whoami` against a temporary Edge working directory.

- one Account → select automatically;
- multiple Accounts → ask only which Account name to use;
- invalid/under-scoped Token → stop mutations and explain the missing permission.

After selection, keep the account ID only in the current deployment process environment:

```bash
export CLOUDFLARE_ACCOUNT_ID="$ACCOUNT_ID"
```

Do not write a personal `account_id` into tracked Wrangler config. Subsequent `wrangler deploy` and `wrangler secret put` inherit this temporary environment.

With `ACCOUNT_ID`, fetch `GET /client/v4/accounts/<ACCOUNT_ID>/workers/subdomain`. Reuse an existing account subdomain and **never rename it**. `ACCOUNT_SUBDOMAIN` is the selected Cloudflare Account's `workers.dev` subdomain returned by the Cloudflare API. The Worker origin always combines two independent values as `<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev`.

If no account subdomain exists, create one only because there is no old value; use `herdr-<short-account-id>` plus a random suffix on collision. Only after GET explicitly confirms that no subdomain exists, create one with:

```text
PUT /client/v4/accounts/<ACCOUNT_ID>/workers/subdomain
Content-Type: application/json

{"subdomain":"<candidate>"}
```

GET it again afterward and require the returned value to match before deploying the Worker.

## 6. Deploy Edge without requiring a permanent repo checkout

Obtain the Edge Worker sources needed for deploy (temporary shallow clone or Release-adjacent docs package is acceptable for this Edge step only). Generate ignored `wrangler.user.toml` from the published user example, then set `name`, `DEFAULT_WORKSTATION_ID`, and `OAUTH_ISSUER=https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev`. Keep `workers_dev = true` and `routes = []`.

Create the private R2 bucket named in `wrangler.user.toml` (idempotent if it already exists), then deploy the Worker. Do not skip the provision step: `wrangler deploy` fails closed when the bound bucket is missing.

```bash
node provision-r2.mjs --config wrangler.user.toml
npx wrangler deploy --config wrangler.user.toml
```

Never overwrite a pre-existing Worker unless this install can prove it owns it; choose a machine-specific/random-suffixed name instead. Then store the WSS shared secret as a Cloudflare Worker secret:

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

Browser extension / Native Messaging remains optional and is not required for the first ChatGPT closed loop. If the human asks for continuity later, install the official **Herdr** extension from the Chrome Web Store. If the Store listing is not live yet, skip this optional step rather than using a local development build. After `herdr-mcp doctor` is healthy and the Store extension is installed:

```bash
herdr-mcp native-host install
herdr-mcp native-host status
```

See [Browser extension](extension.md) and [Browser continuity](browser-continuity.md).

## 8. macOS persistent Herdr Link

Store `LINK_SHARED_SECRET` in Keychain under `herdr-edge-link-<WORKSTATION_ID>`. The command text must reference the environment variable rather than a literal secret. Prefer the managed Link install path exposed by the installed `herdr-mcp` binary (`herdr-mcp link ...` / current stable product docs). Do not leave production Link ownership on a repository Bash wrapper.

When `workers.dev` is blocked (for example China SNI) or the machine already uses a system proxy for ChatGPT, configure Link WSS proxy or switch to a Custom Domain. Proxy precedence: `HERDR_LINK_PROXY` > `HTTPS_PROXY`/`https_proxy` > `HTTP_PROXY`/`http_proxy` > `ALL_PROXY`/`all_proxy`; macOS also reads `scutil --proxy`. See [Quick agent install](quick-agent-install.md) §5 for the full decision tree.

## 9. Verify the closed loop

Verify local `server/discover`, `herdr-mcp status`, `herdr-mcp doctor`, Link status, Worker `/health`, public `/mcp`, OAuth discovery, and that no Custom Domain/DNS/Tunnel was created. Doctor may probe Edge `/health`, OAuth metadata, and `/mcp` without sending tokens; never print tokens.

## 10. Clean up the bootstrap Token

Unset `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`, then delete temporary credential files and any temporary Edge checkout that is no longer needed. Do not copy the Token into project config. Recommend revocation if it was one-time; otherwise move it to a dedicated secret manager/CI secret.

## 11. Final report

Return only non-sensitive facts: installed runtime generation/version, local MCP status, Herdr Link status, Cloudflare Account name + shortened ID, Worker name, `workers.dev` origin, `/health`, and `/mcp`.

Finally guide the user to enable ChatGPT Developer mode, create a custom MCP Connector with `/mcp`, and complete OAuth. Never paste the local `HERDR_MCP_TOKEN` or Cloudflare Token into ChatGPT.

## Appendix: developer-from-source only

Clone + `npm`/`cargo` is allowed only when the human explicitly asked to develop herdr-mcp itself. That path must not be used as the primary runtime install for an ordinary workstation.
