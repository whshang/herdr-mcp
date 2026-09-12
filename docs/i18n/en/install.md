# Manual install

*From one Herdr workstation to usable Web AI development.*

> **Role: manual/operator reference.** The primary herdr-mcp installation protocol is written directly for the executing Agent; see [Agent install](agent-install.md) and [Agent installation](agent-install.md). Use this page for manual inspection, troubleshooting, or understanding each stage. It no longer provides a product-specific prompt to copy into a named coding agent.

The goal is to connect a local workstation to ChatGPT / Web AI while keeping source code and real execution on the workstation.

## Before installation

### 1. Herdr must be available

```bash
herdr --version
herdr api schema >/dev/null
```

If Herdr is missing, use the official stable installer.

macOS / Linux:

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"
```

Then run `herdr --version` again. Herdr's own install behavior is authoritative at <https://herdr.dev/docs/install/>.

### 2. Decide which client path you need

- ChatGPT / another public Web AI → Cloudflare Edge + outbound Herdr Link;
- local MCP clients only → loopback runtime can be used without Cloudflare;
- browser extension → optional after the base Connector works, not a first-install prerequisite.

## Supported platform boundary

Use the GitHub `Latest` stable Release at <https://github.com/whshang/herdr-mcp/releases>. First-device `worker bootstrap` and existing-fleet `worker connect` are supported on macOS and Linux. The 1.0 Windows x86_64 candidate adds the same core enrollment path using Herdr named pipes, Windows Credential Manager, current-user Startup-folder shortcut autostart, and detached user processes; Windows remains candidate support until the native UAT gate passes. When that managed Windows runtime starts, it probes the configured Herdr API and best-effort starts an already-installed `herdr server` if needed; it does not install or remove Herdr itself. Browser Native Messaging, self-update, and product-level reinstall/uninstall are outside this Windows core UAT.

For an older installation, follow [Runtime self-upgrade](runtime-self-upgrade.md). Upgrade the runtime in place; do not recreate a healthy Worker, device relationship, or ChatGPT Connector just to move to the current release.

## Step 1: install the native herdr-mcp runtime

Download the newest stable `herdr-mcp` binary for this platform from <https://github.com/whshang/herdr-mcp/releases>, put it on `PATH`, then run:

```bash
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
```

`install` stages an immutable generation under `~/.config/herdr-mcp/runtime/` and points the user PATH entry at `runtime/current/herdr-mcp`. Normal users do not install the local runtime with a git clone, `npm`, or `cargo`.

On x86_64 Debian, use the static `x86_64-unknown-linux-musl` release asset. The installer prefers `systemd --user`; when no user systemd manager exists it uses the managed user-process backend. On macOS, the service is a user LaunchAgent and Full Disk Access is granted only to the stable macOS-only broker at `~/.config/herdr-mcp/tcc-broker/herdr-mcp-broker`; Linux and Windows do not use that TCC/FDA path. `sudo` does not replace macOS privacy permission.

On macOS, start with `herdr-mcp permissions status`. If it reports `needs_setup`, run `herdr-mcp permissions setup`, enable the exact stable broker in **System Settings → Privacy & Security → Full Disk Access**, then run `herdr-mcp permissions verify`. Do not probe `~/Documents` before setup just to trigger macOS prompts. The host-capable broker is the single TCC responsibility boundary for MCP and native Herdr panes/worktrees, so a normal first install should need one Full Disk Access approval instead of separate approvals per process. Ordinary runtime updates preserve this broker; use `permissions setup --upgrade-broker` only for an explicit compatibility migration. macOS may separately ask once for Keychain access by the stable `herdr-mcp-credential-helper`; approve that one-time prompt and keep the helper stable across updates. Platform details live in [CLI reference](cli-reference.md) and [Troubleshooting](troubleshooting.md). Do not add public Edge while the local doctor is unhealthy.

## Step 2: deploy the public Edge

Cloudflare Workers Free is sufficient for Herdr and does not require a payment method. If you do not have a Cloudflare account, create the free account on the sign-in page; Google sign-in is the recommended shortest path.

When ChatGPT needs to reach the workstation over the Internet, use a Cloudflare Worker as the stable OAuth/MCP entry point. Keep `workers.dev` enabled as the zero-domain bootstrap/diagnostic origin, but when the selected Cloudflare Account already has a suitable active zone, prefer a dedicated Custom Domain such as `herdr-mcp.example.com` for the long-lived OAuth/MCP identity before Connector authorization. If no suitable zone exists or the user declines, continue on `workers.dev` without blocking installation.

For automated installation, the executing Agent follows [Agent install](agent-install.md) / [Agent installation](agent-install.md) directly; those protocols own Token scoping, Worker naming, secret injection, account choice, and the boundary for network blockers.

For a manual/operator deployment, use the installed runtime too:

```bash
herdr-mcp worker bootstrap
```

This command owns Worker naming, release-artifact verification, direct Cloudflare API upload, secrets, first-device enrollment, and readiness verification. In runtimes containing the trusted-DNS recovery, if a new `workers.dev` hostname cannot resolve locally, bootstrap tries Cloudflare DNS then Google DNS, verifies the returned address with the real TLS `/health` contract, and only then attempts to persist a Herdr-marked mapping for that hostname in the system hosts file. Unix may request `sudo`; Windows requires an elevated terminal when the hosts file is not writable. Failure to persist the mapping does not invalidate an already verified in-process bootstrap path. Ordinary installation does not require a source checkout, Node.js, npm, Wrangler, or `wrangler.user.toml`.

Keep these constraints:

- Cloudflare API Token is an ephemeral process value, not a repository or log value;
- keep `workers.dev` as the zero-domain bootstrap origin; when a suitable active zone is available, finalize a Worker Custom Domain before Connector authorization rather than using generic DNS Write;
- keep `LINK_SHARED_SECRET` as a Worker secret;
- the workstation makes outbound authenticated WSS and does not expose a public local port.

See [Agent-assisted installation](agent-install.md) for the ordinary bootstrap contract. [Cloudflare Edge deployment](cloudflare-edge-deployment.md) retains the source/Wrangler workflow only as a maintainer and deep-operations reference.


### v0.4.8 `workers.dev` DNS recovery

v0.4.8 predates automatic direct trusted-DNS persistence. If bootstrap has already created the Worker but fails because its `workers.dev` hostname does not resolve locally, do this before rerunning the resumable bootstrap; do not switch to the public Relay for first enrollment.

```bash
EDGE_ORIGIN="https://<worker>.<account-subdomain>.workers.dev"
HOST="${EDGE_ORIGIN#https://}"; HOST="${HOST%%/*}"

# Try Cloudflare DoH first. If its hostname also fails locally, retry the same
# request with --resolve cloudflare-dns.com:443:1.1.1.1, then 1.0.0.1.
curl --noproxy '*' -fsS -H 'accept: application/dns-json'   "https://cloudflare-dns.com/dns-query?name=${HOST}&type=A"

# If Cloudflare DoH is unavailable, try Google DoH; the same pinned form can
# use dns.google:443:8.8.8.8 and then 8.8.4.4.
curl --noproxy '*' -fsS -H 'accept: application/dns-json'   "https://dns.google/resolve?name=${HOST}&type=A"
```

Choose one returned IPv4 `A` address as `IP`, then validate that address before touching hosts:

```bash
curl --noproxy '*' -fsS --resolve "${HOST}:443:${IP}" "${EDGE_ORIGIN}/health"
```

Proceed only when TLS hostname validation succeeds and `/health` identifies the expected Herdr Worker/contract. Check `/etc/hosts` first; if an unmanaged entry already names this hostname, stop instead of overwriting it. Otherwise add exactly one marked mapping, approve `sudo` yourself, and rerun bootstrap:

```bash
grep -n "${HOST}" /etc/hosts || true
printf '%s	%s	# herdr-mcp workers.dev %s
' "$IP" "$HOST" "$HOST" | sudo tee -a /etc/hosts >/dev/null
herdr-mcp worker bootstrap
```

This changes only one Worker hostname. It does not change the system DNS server, proxy, network node, OAuth issuer, or MCP public origin. A later runtime containing automatic recovery can refresh its own marked entry when that mapping becomes stale.

## Step 3: verify the Herdr Link

```bash
herdr-mcp doctor
herdr-mcp link status
```

If `workers.dev` direct access fails, first let `worker bootstrap` / `worker connect` repair DNS with the verified single-host mapping above and retry direct Link. Reuse an existing local proxy only if direct transport still fails; keep the built-in signed shared Relay as the final fallback. Do not redeploy the Worker or change system DNS just to repair this hostname. Verify with `doctor` and `link status`.

## Step 4: verify the public path

```bash
herdr-mcp doctor
herdr-mcp link status
curl -fsS "${EDGE_ORIGIN}/health"
curl -s -o /dev/null -w '%{http_code}\n' "${EDGE_ORIGIN}/mcp"
```

An unauthenticated `/mcp` response of `401` can be correct. The useful checks are: local runtime healthy, Link connected, Edge `/health` reachable, and OAuth metadata reachable.

## Step 5: add the herdr Connector in ChatGPT

This is a human step. The coding agent should pause and guide the user:

1. open ChatGPT settings and enable **Developer mode** for Plugins;
2. open **Plugins → Browse plugins** and add a custom plugin named `herdr`;
3. paste the complete deployed `${MCP_URL}`, including the final `/mcp`;
4. complete OAuth in the browser; the approval page selects Chinese, English, or Japanese from the browser language and asks you to run the Terminal approval command before entering the six-digit code;
5. create or open a **Project** and work there;
6. in the first message of every new chat, use the composer `+` button to reference `herdr`, so the plugin is enabled for that conversation.

Then do a read-only test:

```text
Inspect my Herdr projects. Read only; do not modify anything.
```

If `herdr_inspect` returns real workstation data, the base loop is usable.

See [ChatGPT Connector](chatgpt-connector.md).

## Step 6: add the browser extension only when continuity is needed

The browser extension adds Side Panel Control Center, workspace binding, long-conversation continuity, and queued next-turn messages. It is not required for the base MCP loop.

The extension has three identities: **STORE / STANDALONE / DEV**. The v0.4.2 Native Host supports Store/DEV ownership; v0.4.3+ adds the fixed-identity STANDALONE path for GitHub/manual distribution.

- STORE: default ordinary-user path, fixed Chrome Web Store identity and Store updates;
- STANDALONE: v0.4.3+, fixed non-Store identity for independent/GitHub distribution;
- DEV: source development only, Load unpacked from repo/worktree `extension/`, with a path-derived identity.

After installing/selecting a supported channel, run:

```bash
herdr-mcp native-host status
```

Require the active channel, extension identity, and Native Host runtime generation to match the intended installation. Do not use DEV as the ordinary-user fallback, and do not call a GitHub/manual fixed-identity package "dev".

See [Browser extension](extension.md) and [Browser Control Center](browser-control-center.md).

## What “installed” means

At minimum:

- `herdr --version` works;
- `herdr-mcp doctor` is healthy;
- Herdr Link is connected;
- Edge `/health` is reachable;
- ChatGPT OAuth is complete;
- a new conversation can call `herdr_inspect` against the real workstation;
- if the optional extension is installed, `herdr-mcp native-host status` is healthy and Control Center can see workspaces.

## Automated execution entry point

For automated installation, the Agent reads [Agent install](agent-install.md) directly. Use [Agent installation](agent-install.md) when the full permissions, security, and failure-boundary contract is needed.

Read deeper only when needed:

- [Troubleshooting](troubleshooting.md)
- [Architecture](architecture.md)
- [Runtime A/B](runtime-self-upgrade.md)
- [Cloudflare Edge deployment](cloudflare-edge-deployment.md)

Maintainer UAT, GA gates, and release evidence are intentionally outside the normal user installation flow.

## Repair, reinstall, and uninstall

On v0.4.3+, use the product-level lifecycle commands rather than manually deleting launchd files or runtime directories:

```bash
herdr-mcp reinstall
herdr-mcp uninstall
```

On macOS, `reinstall` repairs/replaces the managed Rust runtime while retaining configuration and credentials. On Linux, use `herdr-mcp install` for runtime repair and `herdr-mcp service uninstall` for explicit service removal; full product uninstall remains a macOS lifecycle integration. On macOS, generations follow normal service GC, preserving the active/rollback-safe set. `uninstall` removes strongly owned local herdr-mcp runtime/config state. The default instance covers its service, owned daily auto-update scheduler, Link/watchdogs, Native Messaging host, managed CLI link, and config root; a named instance removes only its own service/watchdogs/config. Product uninstall arms a tiny durable update-fence tombstone under the user cache outside the config root before teardown, so a queued silent updater cannot resurrect the removed service after the config directory is gone. That tombstone is cleared only by an explicit successful install/reinstall. It intentionally preserves Herdr itself (`herdr`, the Herdr service/socket/config), as well as separately managed browser/Cloudflare/Keychain/TCC authorization state. Run these lifecycle mutations from an independent terminal, not from a managed `herdr_exec` session that depends on the service being changed.
