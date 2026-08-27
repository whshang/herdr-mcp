# Clean-machine UAT checklist (G18)

Commands-only runbook for installing from a **GitHub Release binary** (no repo checkout as the runtime install source).

Do **not** cut a non-alpha stable tag from this checklist alone. Product remains alpha until G1 + G18 + remaining GA vetoes pass.

## Isolation honesty (same physical Mac)

Default production identities stay:

- LaunchAgent `dev.herdr-mcp.server`
- loopback port `8772`
- config root `~/.config/herdr-mcp`
- user CLI `~/.local/bin/herdr-mcp`

| Attempt | Safe on dogfood Mac? | Why |
| --- | --- | --- |
| TMPHOME / `HERDR_MCP_CONFIG_DIR` only | Partial probes only | Paths isolate, but default label/port still collide; `status`/`doctor` can report dogfood `:8772` |
| Same-user default `herdr-mcp install` | **No** | Mutates dogfood LaunchAgents / `~/.local/bin/herdr-mcp` |
| Same-user **named instance** (`--instance uat` / `HERDR_MCP_INSTANCE=uat`) | **Yes for local runtime UAT** | Distinct label `dev.herdr-mcp.uat.server`, non-8772 port, `~/.config/herdr-mcp-uat`; never rewrites default user CLI |
| Second Mac or VM (free `:8772`, empty default `dev.herdr-mcp.*`) | **Yes** | Full default-instance G18 including native-host / public ChatGPT path |

Named-instance evidence advances G18 local install/doctor/status on the dogfood Mac. It does **not** replace a second-Mac default-instance seal for native-host + public OAuth when those must share production identities.

Requires a Release that includes instance isolation (first: `v0.4.0-alpha.16` or newer). Do not claim named-instance UAT from `v0.4.0-alpha.15`.

## Platform under test

First-GA recommendation: **macOS Apple Silicon** only.

- Windows Release binary: preview / optional observation, not first-GA lifecycle seal.
- Linux lifecycle: not claimed.

## Preconditions

- Herdr installed per <https://herdr.dev>.
- No prior `herdr-mcp` checkout required for the runtime path.
- Network access to GitHub Releases and (for ChatGPT path) Cloudflare.
- For **default-instance** install: confirm `launchctl list | awk '$3 ~ /herdr-mcp/'` is empty and nothing listens on `:8772`.
- For **named-instance** install on a dogfood Mac: leave default `dev.herdr-mcp.server` / `:8772` alone; do not run Link cutover / `native-host install` against dogfood Chrome from the UAT binary.

## One-command operator bootstrap (second Mac, default instance)

Replace `TAG` if a newer prerelease is under test:

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

## Same-Mac named instance (when a second Mac is unavailable)

Use a downloaded Release binary only (never `target/*/herdr-mcp`). Keep dogfood on the default instance.

```bash
TAG=v0.4.0-alpha.16
REPO=whshang/herdr-mcp
WORKDIR="${HOME}/herdr-mcp-clean-uat"
mkdir -p "$WORKDIR/bin" "$WORKDIR/dl" && cd "$WORKDIR"
gh release download "$TAG" -R "$REPO" -D dl \
  -p "herdr-mcp-*-aarch64-apple-darwin" \
  -p "release-manifest.json"
install -m 755 dl/herdr-mcp-*-aarch64-apple-darwin bin/herdr-mcp
export PATH="$WORKDIR/bin:$PATH"
export HERDR_MCP_INSTANCE=uat

# Preflight: dogfood must stay untouched
readlink "$HOME/.config/herdr-mcp/runtime/current"
launchctl list | awk -v label='dev.herdr-mcp.server' '$3 == label { print $1, $2, $3 }'

herdr-mcp --version
herdr-mcp --instance uat install
herdr-mcp --instance uat doctor
herdr-mcp --instance uat status
herdr-mcp --instance uat update check
herdr-mcp --instance uat service status

# Expect isolated identities
test -x "$HOME/.config/herdr-mcp-uat/runtime/current/herdr-mcp"
launchctl list | awk -v label='dev.herdr-mcp.uat.server' '$3 == label { print $1, $2, $3 }'
# Dogfood still default:
readlink "$HOME/.config/herdr-mcp/runtime/current"
ls -l "$HOME/.local/bin/herdr-mcp"
launchctl list | awk -v label='dev.herdr-mcp.server' '$3 == label { print $1, $2, $3 }'
```

Cleanup when finished (does not touch dogfood):

```bash
export HERDR_MCP_INSTANCE=uat
herdr-mcp --instance uat uninstall
```

### Named-instance non-goals on the dogfood Mac

- Do not run `native-host install` / Chrome Load unpacked against dogfood profiles from this path.
- Do not run `link install` / `link cutover` / seal mutations for the UAT instance on the dogfood Mac.
- Public ChatGPT OAuth remains an **owner** step on a machine/Edge identity you intend to expose; stop at OAuth and record the exact blocked step.

## A. Local runtime only

```bash
herdr --version
herdr api schema >/dev/null

# Download the platform binary from GitHub Releases into PATH, then:
chmod +x "$(command -v herdr-mcp)"
herdr-mcp install          # default instance on a clean Mac
# or: herdr-mcp --instance uat install   # same Mac beside dogfood
herdr-mcp doctor
herdr-mcp status
herdr-mcp update check
```

Expect:

- `doctor` PASS on local Herdr / runtime / service layers for **that** instance
- default instance: PATH `herdr-mcp` may resolve through `~/.config/herdr-mcp/runtime/current`
- named instance: use `--instance` / `HERDR_MCP_INSTANCE` or the isolated `runtime/current` binary; `~/.local/bin/herdr-mcp` stays dogfood
- no requirement to `git clone` or `npm ci` for the local runtime
- named-instance `doctor` must report the UAT label/port/config root, not dogfood `:8772`

## B. Public ChatGPT path (owner action)

Follow [Installation](install.md) / [Agent-assisted installation](agent-install.md) using Release binary + temporary Edge bootstrap only, then [ChatGPT Connector](chatgpt-connector.md).

Prefer a **second Mac / default instance** for this section. Named-instance same-Mac UAT stops before mutating dogfood Link/Edge.

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

## C. Update / rollback (same clean machine / same instance)

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

For named instance, keep `HERDR_MCP_INSTANCE=uat` (or `--instance uat`) on every command.

## D. Explicit non-goals for this runbook

- Do not implement Browser Control Plane / true-steer work here (G16 post-GA boundary).
- Do not treat unpacked `extension/` from a git checkout as the sealed G15 path.
- Do not use `target/*/herdr-mcp` or a repo-linked `~/.local/bin/herdr-mcp` as production evidence.
- Do not mutate production Link on the dogfood Mac from the named-instance path.

## Evidence to record

Capture (non-secret) outputs of:

```bash
herdr-mcp --version
# default:
readlink "$HOME/.config/herdr-mcp/runtime/current" || true
launchctl list | awk -v label='dev.herdr-mcp.server' '$3 == label { print $1, $2, $3 }'
# named instance:
readlink "$HOME/.config/herdr-mcp-uat/runtime/current" || true
launchctl list | awk -v label='dev.herdr-mcp.uat.server' '$3 == label { print $1, $2, $3 }'
herdr-mcp --instance uat status
herdr-mcp --instance uat doctor
herdr-mcp --instance uat update check
ls -l "$HOME/.local/bin/herdr-mcp"
```

Attach those to the GA scorecard G18 row. Score named-instance same-Mac evidence as progress toward G18 local runtime, not as a full public-path PASS.
