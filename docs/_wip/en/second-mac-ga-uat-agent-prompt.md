# INTERNAL GA UAT — Second Mac Agent contract (not end-user install)

> **Audience:** Herdr-mcp maintainers running **G18 clean-machine UAT** on a second Mac (pi).  
> **Not for regular users.** End users install via [install.md](../../i18n/en/install.md) (Release binary → `herdr-mcp install` → `herdr-mcp doctor`). For agent-assisted first install on their own machine, use [agent-install.md](../../i18n/en/agent-install.md).

---

## Paste this to the pi coding Agent

```text
You received the second-Mac GA UAT contract. Read the full document at this URL and begin execution immediately — no preamble, no "shall I proceed?" unless you must pause for Cloudflare API Token creation or Chrome Load unpacked.

Goal: G18 default-instance clean-machine UAT on pi — independent Herdr, Release `herdr-mcp` runtime, independent Cloudflare Worker (workers.dev only), independent Link, extension + native-host; hand ChatGPT Connector URL to owner for OAuth.

References (read if blocked): agent-install.md, clean-machine-uat.md, chatgpt-connector.md
```

---

## Architecture constraints (short)

| Object | pi | Forbidden |
| --- | --- | --- |
| `workstation_id` | Unique; same as Edge `DEFAULT_WORKSTATION_ID` | Dogfood `prod-real-runtime` or any live dogfood ID |
| `LINK_SHARED_SECRET` | Fresh; Keychain + this Worker secret | Dogfood / `herdr-edge-prod` secret |
| Cloudflare Worker | Machine-specific `name`, `workers.dev` only | `herdr-edge-prod`, Custom Domain, DNS, Tunnel |
| Local runtime | GitHub Release → `herdr-mcp install` | `git clone` as runtime, `target/*`, `npm ci` for runtime |
| Edge sources | Temporary shallow clone; delete after deploy | Checkout on PATH or as runtime |

**Pause only for:** Cloudflare API Token (below); Chrome **Load unpacked**. ChatGPT OAuth is owner-only at the end.

---

## Execution contract

### 0. Agent rules

1. Execute automatable shell steps directly; pause only for Cloudflare Token / Account pick or Chrome Load unpacked.
2. Never `reset --hard`, `clean -fd`, or overwrite unrelated checkouts.
3. First install: `workers.dev` only — no Custom Domain, DNS, Tunnel, or zone mutation.
4. Never echo or persist `CLOUDFLARE_API_TOKEN`, `LINK_SHARED_SECRET`, or `HERDR_MCP_TOKEN` in repo, `.env`, logs, screenshots, or shell history. Prefer `export CLOUDFLARE_API_TOKEN=...` in the current process only.
5. Verify each mutation before continuing.

### 1. Preflight

6. Latest alpha from GitHub Releases (baseline `v0.4.0-alpha.16`; use newer prerelease if available). Record `TAG`.
7. macOS Apple Silicon; `launchctl list | awk '$3 ~ /herdr-mcp/'` empty; `:8772` free. Default instance — not dogfood `--instance uat`.
8. Do **not** connect to or configure `herdr-edge-prod` / `wss://herdr-edge-prod.*.workers.dev/ws`.

### 2. Herdr

9. If needed: `curl -fsSL https://herdr.dev/install.sh | sh`
10. `herdr api schema >/dev/null`; start headless server if socket missing (CI-style, no TUI).
11. `herdr workspace create --cwd "$HOME/herdr-uat-workspace" --label "uat" --focus`

### 3. Local runtime (Release only)

12. Download Release assets to `$HOME/herdr-mcp-clean-uat` (no `git clone` as runtime):

    ```bash
    TAG=v0.4.0-alpha.16
    REPO=whshang/herdr-mcp
    WORKDIR="${HOME}/herdr-mcp-clean-uat"
    mkdir -p "$WORKDIR/bin" "$WORKDIR/dl" && cd "$WORKDIR"
    gh release download "$TAG" -R "$REPO" -D dl \
      -p "herdr-mcp-*-aarch64-apple-darwin" \
      -p "release-manifest.json" \
      -p "herdr-mcp-extension-*.zip" \
      -p "herdr-mcp-extension-*.zip.sha256"
    install -m 755 dl/herdr-mcp-*-aarch64-apple-darwin bin/herdr-mcp
    export PATH="$WORKDIR/bin:$PATH"
    ```

13. `herdr-mcp install` → `doctor` → `status` → `update check`. Keep `update.channel = "preview"` during alpha.

### 4. Identities (memory only)

14. Generate in memory: `HERDR_MCP_TOKEN` (`openssl rand -hex 32`), `LINK_SHARED_SECRET`, `WORKSTATION_ID` (unique, `[A-Za-z0-9_.-]`, ≤64 chars), `HERDR_LINK_KEYCHAIN_SERVICE=herdr-edge-link-${WORKSTATION_ID}`.

### 5. PAUSE — Cloudflare API Token

**Tell the owner (copy-paste):**

```text
I need a Cloudflare API Token to deploy your private workers.dev Worker. Please create one now:

1. Open https://dash.cloudflare.com/profile/api-tokens
2. Click "Create Token"
3. Use template: "Edit Cloudflare Workers" (recommended)
4. Account Resources: select "All accounts" OR pick the one account you use for this UAT
5. Zone Resources: select "All zones" OR "All zones from an account" — workers.dev-only deploy does not need a specific zone; Account-scoped Workers permissions are what matter. Do NOT add DNS Write.
6. Continue → Create Token → copy the secret (shown once)

Deliver the token either:
  (A) paste into this chat's secret/private field when prompted, OR
  (B) save in your password manager and tell me you've set export CLOUDFLARE_API_TOKEN in a terminal I'll use (never commit to git, shell history, or screenshots)

To view or revoke tokens later: same page — https://dash.cloudflare.com/profile/api-tokens → Active tokens → Revoke.
```

15. After Token arrives: `export CLOUDFLARE_API_TOKEN=...` (never a CLI literal). Verify `GET https://api.cloudflare.com/client/v4/user/tokens/verify`, then `npx wrangler whoami`. One Account → auto-select; multiple → ask Account name only.

16. `export CLOUDFLARE_ACCOUNT_ID=...`. `GET /client/v4/accounts/<ACCOUNT_ID>/workers/subdomain` → `ACCOUNT_SUBDOMAIN`. Origin: `https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev`.

### 6. Edge deploy (temporary clone; delete after)

17. `EDGE_TMP=$(mktemp -d)` → shallow clone → `cd "$EDGE_TMP/edge/cloudflare"`.
18. `WORKER_NAME="$(node "$EDGE_TMP/scripts/cloudflare-worker-name.mjs" "$(hostname)")"`.
19. `cp wrangler.user.example.toml wrangler.user.toml` — set `name`, `DEFAULT_WORKSTATION_ID`, `OAUTH_ISSUER`; keep `workers_dev = true`, `routes = []`.
20. `npx wrangler deploy --config wrangler.user.toml`; `printf '%s' "$LINK_SHARED_SECRET" | npx wrangler secret put LINK_SHARED_SECRET --config wrangler.user.toml`.
21. Verify `/health`, OAuth discovery, `/mcp` (401 OK). `rm -rf "$EDGE_TMP"`.

### 7. Link (override dogfood defaults)

22. Keychain: `security add-generic-password -a "$USER" -s "$HERDR_LINK_KEYCHAIN_SERVICE" -w "$LINK_SHARED_SECRET" -U`
23. `herdr-mcp link install` → patch plist `HERDR_EDGE_URL`, `HERDR_WORKSTATION_ID`, `HERDR_LINK_KEYCHAIN_SERVICE` → `launchctl bootstrap`. **Do not** use default `herdr-edge-prod` URL.
24. `herdr-mcp link status`; `herdr-mcp doctor` (edge-reachable, 401 auth=not-sent). No `link cutover` / `link seal --execute` on pi.

### 8. Extension + native-host

25. Unzip extension from Release; **pause** — ask owner to open `chrome://extensions` → Developer mode → **Load unpacked** → `~/.config/herdr-mcp/extension`.
26. `herdr-mcp native-host install` → `native-host status` → `doctor`.

### 9. Close loop + report

27. Verify local + Edge + Link; confirm no Custom Domain / DNS / Tunnel.
28. `unset CLOUDFLARE_API_TOKEN CLOUDFLARE_ACCOUNT_ID`; delete temp token files; remind owner to revoke one-time Token in dashboard if desired.
29. Return non-secret report template (version, WORKER_NAME, workers.dev origin, WORKSTATION_ID, HERDR_EDGE_URL, MCP_URL, doctor/link summaries).
30. Give owner ChatGPT Connector URL: `https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev/mcp` — owner completes OAuth in a **new chat**; never paste `HERDR_MCP_TOKEN`.

## Non-goals

No multi-device Worker experiment, stable tag, dogfood mutation, or pi pointing at `herdr-edge-prod`.
