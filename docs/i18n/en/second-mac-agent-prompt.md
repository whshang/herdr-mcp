# Second Mac (pi) UAT — local Agent execution contract

Copy the entire block below to a coding Agent on the second Mac (Codex, Cursor, Claude Code, etc.). This is an **executable contract**, not a human copy-paste command list.

**Goal:** Complete G18 default-instance clean-machine UAT on pi — independent Herdr, independent `herdr-mcp` Release runtime, independent Cloudflare Worker (`workers.dev` only), independent Link, extension + native-host, then hand the ChatGPT Connector URL to the owner for OAuth.

**Authoritative references:** [Agent-assisted installation](agent-install.md) · [Clean-machine UAT](clean-machine-uat.md) · [ChatGPT Connector](chatgpt-connector.md)

---

## Architecture constraints (must match exactly)

| Object | pi requirement | Forbidden |
| --- | --- | --- |
| `workstation_id` | Unique to this machine; same as Edge `DEFAULT_WORKSTATION_ID` | Reuse dogfood `prod-real-runtime` or any live dogfood ID |
| `LINK_SHARED_SECRET` | Fresh on this machine; Keychain + this Worker secret | Reuse dogfood / `herdr-edge-prod` secret |
| Cloudflare Worker | Machine-specific `name`, `workers.dev` only | Connect to `herdr-edge-prod`, Custom Domain, DNS, Tunnel |
| ChatGPT Connector | Points at pi Worker `/mcp` | Reuse dogfood Connector URL |
| Local runtime | GitHub Release binary → `herdr-mcp install` | `git clone` as runtime, `target/*`, `npm ci` for runtime |
| Edge sources | Temporary shallow clone; delete after deploy | Leave checkout on PATH or as runtime |
| Multi-device Worker | Out of scope for this run | Any one-Worker-multi-machine experiment |

Dogfood example: `herdr-edge-prod.<account>.workers.dev`  
pi example: `herdr-edge-macbook-uat.<account>.workers.dev` (actual name comes from the hostname helper)

**Pause only for:** Cloudflare login / API Token / Account pick; Chrome **Load unpacked** (you prepare the extension path). ChatGPT OAuth is owner-only at the end — do not click it for them.

---

## Execution contract (34 items)

### 0. Agent contract

1. Execute automatable shell steps directly; pause only for Cloudflare interactive login / API Token creation, multi-Account selection, or Chrome Load unpacked.
2. Preserve existing work. Never `reset --hard`, `clean -fd`, or overwrite user changes in an unrelated checkout.
3. First install uses `workers.dev` only. Do not create Custom Domain, DNS, Cloudflare Tunnel, or mutate an existing zone.
4. Cloudflare Token, `LINK_SHARED_SECRET`, and `HERDR_MCP_TOKEN` are high-sensitivity credentials. Never echo them or write them to the repo, `.env`, ordinary logs, screenshots, or shell history. Prefer process-environment injection; if a temporary file is unavoidable, use mode `0600` and delete immediately after use.
5. Verify every mutation before continuing. On error, determine whether the mutation already committed before retrying.
6. Do **not** install the local MCP runtime by cloning this repository or running `npm`/`cargo` (Edge temporary deploy excepted).

### 1. Preflight: Release and machine identity

7. Query GitHub Releases for the latest alpha tag (baseline `v0.4.0-alpha.16`; use a newer prerelease if available). Record `TAG` and the version `herdr-mcp --version` will report.
8. Confirm **macOS Apple Silicon**, `launchctl list | awk '$3 ~ /herdr-mcp/'` is empty, and `:8772` is free. This is a default-instance clean machine, not dogfood `--instance uat`.
9. Confirm this task must **not** connect to, probe, or configure dogfood Worker `herdr-edge-prod` / `wss://herdr-edge-prod.*.workers.dev/ws`.

### 2. Herdr

10. If `herdr --version` fails, install Herdr from the official script:

    ```bash
    curl -fsSL https://herdr.dev/install.sh | sh
    ```

11. Verify `herdr api schema >/dev/null`. If the socket is missing or the server is not running, start a headless server in the background (CI-style; no TUI):

    ```bash
  SOCKET="${HERDR_SOCKET_PATH:-$HOME/.config/herdr/herdr.sock}"
  mkdir -p "$(dirname "$SOCKET")"
  HERDR_SOCKET_PATH="$SOCKET" herdr server </dev/null >>"$HOME/.config/herdr/headless-server.log" 2>&1 &
  # Poll up to 60s until herdr status server --json reports running:true
    ```

12. Create and focus a UAT workspace (path is yours, e.g. `~/herdr-uat-workspace`):

    ```bash
  herdr workspace create --cwd "$HOME/herdr-uat-workspace" --label "uat" --focus
    ```

### 3. Local runtime (Release binary only)

13. Download Release assets into a temp workdir (**no** `git clone` as runtime):

    ```bash
  TAG=v0.4.0-alpha.16   # replace with latest alpha from step 7
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

14. Install and verify the default instance:

    ```bash
  herdr-mcp --version
  herdr-mcp install
  herdr-mcp doctor
  herdr-mcp status
  herdr-mcp update check
    ```

15. While alpha, keep `update.channel = "preview"`. Alpha binaries usually default to preview when `config.toml` is absent; if config exists with `stable`, switch to preview or remove the field and re-run `update check`.

### 4. Generate identities in memory (never print secrets)

16. Generate and keep only in memory:
    - `HERDR_MCP_TOKEN` — `openssl rand -hex 32` (`install` writes server plist; never echo)
    - `LINK_SHARED_SECRET` — `openssl rand -hex 32`
    - `WORKSTATION_ID` — unique to this machine, `[A-Za-z0-9_.-]`, max 64 chars (e.g. `pi-uat-$(date +%Y%m%d)` or hostname-derived)
    - `HERDR_LINK_KEYCHAIN_SERVICE` — `herdr-edge-link-${WORKSTATION_ID}`

17. **Pause — Cloudflare API Token (credential pause #1)**

    Open <https://dash.cloudflare.com/profile/api-tokens> (open yourself when browser control is available; otherwise give the URL to the owner).

    - Recommended template: **Edit Cloudflare Workers**, scoped to one Account
    - Do **not** add DNS Write / Zone permissions
    - Custom token at minimum: Account → Workers Scripts Write/Edit; Account Settings Read; Memberships Read; User Details Read
    - Tell the owner the secret is shown once and must be pasted only into this Agent session's secret channel; **never** echo it in logs

18. After the Token arrives: inject only as `export CLOUDFLARE_API_TOKEN=...` (not a command-line literal). Verify `GET https://api.cloudflare.com/client/v4/user/tokens/verify`, then `npx wrangler whoami`. One Account → auto-select; multiple → ask Account name only; on failure stop mutations.

19. After `CLOUDFLARE_ACCOUNT_ID` is known, `export` it in the current shell. `GET /client/v4/accounts/<ACCOUNT_ID>/workers/subdomain` yields `ACCOUNT_SUBDOMAIN` (reuse existing; never rename). Public Worker origin: `https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev`.

### 5. Edge deploy (temporary shallow clone; delete after)

20. Obtain Edge sources temporarily (**deploy only**; must not become runtime PATH):

    ```bash
  EDGE_TMP="$(mktemp -d)"
  git clone --depth 1 https://github.com/whshang/herdr-mcp.git "$EDGE_TMP"
  cd "$EDGE_TMP/edge/cloudflare"
    ```

21. Generate Worker name via the repository helper (**do not** invent slugs):

    ```bash
  WORKER_NAME="$(node "$EDGE_TMP/scripts/cloudflare-worker-name.mjs" "$(hostname)")"
    ```

22. Generate local Wrangler config from the published example:

    ```bash
  cp wrangler.user.example.toml wrangler.user.toml
    ```

    Edit `wrangler.user.toml`:
    - `name = "<WORKER_NAME>"`
    - `DEFAULT_WORKSTATION_ID = "<WORKSTATION_ID>"` (same as step 16)
    - `OAUTH_ISSUER = "https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev"`
    - Keep `workers_dev = true`, `routes = []`

23. Deploy and store the Link secret:

    ```bash
  npx wrangler deploy --config wrangler.user.toml
  printf '%s' "$LINK_SHARED_SECRET" | npx wrangler secret put LINK_SHARED_SECRET --config wrangler.user.toml
    ```

    Record: `EDGE_ORIGIN="https://${WORKER_NAME}.${ACCOUNT_SUBDOMAIN}.workers.dev"`, `HERDR_EDGE_URL="wss://${WORKER_NAME}.${ACCOUNT_SUBDOMAIN}.workers.dev/ws"`, `MCP_URL="${EDGE_ORIGIN}/mcp"`.

24. Verify Edge (no tokens): `curl -fsS "${EDGE_ORIGIN}/health"`; OAuth discovery and `/mcp` reachable (401 acceptable). Then **delete** `$EDGE_TMP` (`rm -rf "$EDGE_TMP"`). Do not leave a checkout at `~/Documents/herdr-mcp` as the install source.

### 6. Local Link (must override dogfood defaults)

`herdr-mcp link install` writes a candidate plist that defaults to dogfood `herdr-edge-prod`. **pi must patch the plist after install** — never use the default Edge URL.

25. Store `LINK_SHARED_SECRET` in Keychain (service name from step 16):

    ```bash
  security add-generic-password -a "$USER" -s "$HERDR_LINK_KEYCHAIN_SERVICE" -w "$LINK_SHARED_SECRET" -U
    ```

26. Install the Link candidate LaunchAgent, patch env vars, and relaunch:

    ```bash
  herdr-mcp link install
  PLIST="$HOME/Library/LaunchAgents/dev.herdr-mcp.link-rust-candidate.plist"
  launchctl bootout "gui/$(id -u)/dev.herdr-mcp.link-rust-candidate" 2>/dev/null || true
  /usr/libexec/PlistBuddy -c "Set :EnvironmentVariables:HERDR_EDGE_URL ${HERDR_EDGE_URL}" "$PLIST"
  /usr/libexec/PlistBuddy -c "Set :EnvironmentVariables:HERDR_WORKSTATION_ID ${WORKSTATION_ID}" "$PLIST"
  /usr/libexec/PlistBuddy -c "Set :EnvironmentVariables:HERDR_LINK_KEYCHAIN_SERVICE ${HERDR_LINK_KEYCHAIN_SERVICE}" "$PLIST"
  launchctl bootstrap "gui/$(id -u)" "$PLIST"
    ```

27. Verify Link: `herdr-mcp link status`; `herdr-mcp doctor` should show edge-reachable / oauth-metadata / mcp-endpoint (401 auth=not-sent). Do **not** run `link cutover` / `link seal --execute` on pi (dogfood seal actions).

### 7. Extension + native-host

28. Install extension from Release zip (downloaded in step 13):

    ```bash
  shasum -a 256 -c "$WORKDIR/dl"/herdr-mcp-extension-*.zip.sha256
  mkdir -p "$HOME/.config/herdr-mcp/extension"
  unzip -o "$WORKDIR/dl"/herdr-mcp-extension-*.zip -d "$HOME/.config/herdr-mcp/extension"
    ```

29. **Pause — Chrome Load unpacked (UI pause #2)**

    Ask the owner: open `chrome://extensions` → enable Developer mode → **Load unpacked** → select `~/.config/herdr-mcp/extension`. Do not complete ChatGPT OAuth or Connector setup for them.

30. Install native-host and verify:

    ```bash
  herdr-mcp native-host install
  herdr-mcp native-host status
  herdr-mcp doctor
    ```

### 8. Closed-loop verification and cleanup

31. Verify the closed loop (never print tokens):
    - Local: `herdr-mcp status`, `herdr-mcp doctor` (Herdr / runtime / service / link / edge layers)
    - Edge: `${EDGE_ORIGIN}/health`, OAuth metadata, `${MCP_URL}` (401)
    - Link: `herdr-mcp link status` shows this machine's `HERDR_EDGE_URL` and `WORKSTATION_ID`, **not** `herdr-edge-prod`
    - Confirm no Custom Domain / DNS / Tunnel was created

32. Clean up bootstrap credentials: `unset CLOUDFLARE_API_TOKEN CLOUDFLARE_ACCOUNT_ID`; delete any temporary token files; recommend revoking one-time Tokens.

### 9. Final report (non-secret fields only)

33. Return this template to the owner (**no** `HERDR_MCP_TOKEN`, `LINK_SHARED_SECRET`, or Cloudflare Token):

    ```text
    === pi UAT install report ===
    herdr-mcp version:
    runtime generation:
    launchd server label: dev.herdr-mcp.server
    loopback port: 8772
    config root: ~/.config/herdr-mcp
    Herdr workspace label: uat
    Cloudflare account (name + shortened id):
    WORKER_NAME:
    workers.dev origin:
    WORKSTATION_ID:
    HERDR_EDGE_URL: (full wss://... URL)
    MCP_URL: (https://.../mcp)
    /health:
    herdr-mcp doctor summary:
    herdr-mcp link status summary:
    native-host status:
    extension path: ~/.config/herdr-mcp/extension
    update check:
    ```

34. **ChatGPT Connector URL for owner OAuth**

    ```text
    ChatGPT → Settings → Connectors → Add MCP App
    MCP URL: https://<WORKER_NAME>.<ACCOUNT_SUBDOMAIN>.workers.dev/mcp
    ```

    Remind the owner:
    - Enable Developer mode; start a **new chat** to validate `tools/list` (epoch 2 / 18 tools)
    - **Never** paste `HERDR_MCP_TOKEN` into ChatGPT
    - Read smoke: `herdr_inspect`; then one bounded write and long exec `herdr_exec_start` → `herdr_exec_read`
    - See [ChatGPT Connector](chatgpt-connector.md) and [clean-machine UAT §B](clean-machine-uat.md)

---

## Explicit non-goals

- Do not implement or validate multi-device Worker control plane
- Do not cut a stable tag
- Do not mutate dogfood live state
- Do not `reset --hard` / `clean -fd` dirty worktrees
- Do not point pi at `herdr-edge-prod` or dogfood `workstation_id`
