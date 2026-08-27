# Clean-machine UAT checklist (G18)

Commands-only runbook for a **second Mac / VM** that has never used this repository as the runtime install path. Completing this checklist on the dogfood workstation that already owns production Link / `:8772` / `dev.herdr-mcp.*` does **not** seal G18.

Do **not** cut a non-alpha stable tag from this checklist alone. Product remains alpha until G1 + G18 + remaining GA vetoes pass.

## Isolation honesty (same physical Mac)

`herdr-mcp` service install hardcodes LaunchAgent label `dev.herdr-mcp.server` and loopback port `8772`. Product does **not** currently expose overrides for those identities.

| Attempt | Safe on dogfood Mac? | Why |
| --- | --- | --- |
| TMPHOME / `HERDR_MCP_CONFIG_DIR` only | Partial probes only | Paths isolate, but `launchctl` still sees the same user domain; `status`/`doctor` can report the **dogfood** `:8772` as healthy |
| Same-user `herdr-mcp install` | **No** | Would mutate dogfood LaunchAgents / service |
| Second macOS user on the **same** host | **No** for full install | Labels are per-user, but `:8772` is machine-wide; starting a second service collides with dogfood |
| Second Mac or VM (free `:8772`, empty `dev.herdr-mcp.*`) | **Yes** | Required for honest G18 PASS |

TMPHOME evidence remains PARTIAL only (`docs/_wip/g18-clean-machine-sim-20260828.md` + alpha.15 re-probe). Do **not** mark G18 PASS from TMPHOME-on-same-daemon.

## Platform under test

First-GA recommendation: **macOS Apple Silicon** only.

- Windows Release binary: preview / optional observation, not first-GA lifecycle seal.
- Linux lifecycle: not claimed.

## Preconditions

- Fresh Mac / VM with Herdr installed per <https://herdr.dev>.
- No prior `herdr-mcp` checkout required for the runtime path.
- Network access to GitHub Releases and (for ChatGPT path) Cloudflare.
- Confirm `launchctl list | awk '$3 ~ /herdr-mcp/'` is empty and nothing listens on `:8772` before install.

## One-command operator bootstrap (second Mac)

Replace `TAG` if a newer prerelease is under test (example uses the first Release that ships the extension zip):

```bash
TAG=v0.4.0-alpha.15
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
herdr --version
herdr api schema >/dev/null
herdr-mcp --version
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update check
# Extension (optional for G15 residual on the clean machine):
shasum -a 256 -c dl/herdr-mcp-extension-*.zip.sha256
mkdir -p ~/.config/herdr-mcp/extension
unzip -o dl/herdr-mcp-extension-*.zip -d ~/.config/herdr-mcp/extension
# Chrome: Load unpacked -> ~/.config/herdr-mcp/extension
herdr-mcp native-host install
herdr-mcp native-host status
```

No `git clone`. No `npm ci` for the runtime path.

## A. Local runtime only

```bash
herdr --version
herdr api schema >/dev/null

# Download the platform binary from GitHub Releases into PATH, then:
chmod +x "$(command -v herdr-mcp)"
herdr-mcp install
herdr-mcp doctor
herdr-mcp status
herdr-mcp update check
```

Expect:

- `doctor` PASS on local Herdr / runtime / service layers
- PATH `herdr-mcp` resolves through `~/.config/herdr-mcp/runtime/current`
- no requirement to `git clone` or `npm ci` for the local runtime
- `doctor` must **not** be explaining another machine's already-running `:8772`

## B. Public ChatGPT path (owner action)

Follow [Installation](install.md) / [Agent-assisted installation](agent-install.md) using Release binary + temporary Edge bootstrap only, then [ChatGPT Connector](chatgpt-connector.md).

```bash
herdr-mcp doctor
# After Edge + Link are configured on THIS clean machine:
# expect Edge configured + edge-reachable + oauth-metadata + mcp-endpoint (401 auth=not-sent)
# without printing tokens
```

Then the **human operator** (not an unattended agent) in a **new** ChatGPT conversation:

1. Settings → Connectors → add custom MCP App / Connector with the public Edge URL from install docs
2. Complete OAuth in the browser (never paste `HERDR_MCP_TOKEN` into ChatGPT)
3. Confirm OAuth success, then start a **new** chat so `tools/list` is fresh
4. Verify epoch 2 / 18 tools
5. One read-only tool call (`herdr_inspect` or equivalent)
6. One real bounded mutation the operator chooses
7. One long-task / streaming basics smoke if in scope for this UAT pass

If the operator cannot complete OAuth in this session, leave G7/G18 public path as open and record the exact step blocked (connector add / authorize / tools/list / tool call).

## C. Update / rollback (same clean machine)

```bash
herdr-mcp update check
herdr-mcp update apply
herdr-mcp update status
herdr-mcp doctor
# Controlled rollback only when a previous managed generation exists:
herdr-mcp rollback
herdr-mcp doctor
herdr-mcp status
```

## D. Explicit non-goals for this runbook

- Do not implement Browser Control Plane / true-steer work here (G16 post-GA boundary).
- Do not treat unpacked `extension/` from a git checkout as the sealed G15 path.
- Do not use `target/*/herdr-mcp` or a repo-linked `~/.local/bin/herdr-mcp` as production evidence.
- Do not mutate production Link on the dogfood Mac from this checklist.

## Evidence to record

Capture (non-secret) outputs of:

```bash
herdr-mcp --version
readlink "$HOME/.config/herdr-mcp/runtime/current" || true
herdr-mcp status
herdr-mcp doctor
herdr-mcp update check
launchctl list | awk -v label='dev.herdr-mcp.server' '$3 == label { print $1, $2, $3 }'
```

Attach those to the GA scorecard G18 row when the clean machine actually passes.
