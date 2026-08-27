# Clean-machine UAT checklist (G18)

Commands-only runbook for a **second machine** that has never used this repository as the runtime install path. Completing this checklist on a developer workstation that already owns production Link does **not** seal G18.

Do **not** cut a non-alpha stable tag from this checklist alone. Product remains alpha until G1 + G18 + remaining GA vetoes pass.

## Platform under test

First-GA recommendation: **macOS Apple Silicon** only.

- Windows Release binary: preview / optional observation, not first-GA lifecycle seal.
- Linux lifecycle: not claimed.

## Preconditions

- Fresh user account or VM with Herdr installed per <https://herdr.dev>.
- No prior `~/herdr-mcp` checkout required for the runtime path.
- Network access to GitHub Releases and (for ChatGPT path) Cloudflare.

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

## B. Optional public ChatGPT path

Follow [Installation](install.md) / [Agent-assisted installation](agent-install.md) using Release binary + temporary Edge bootstrap only:

```bash
herdr-mcp doctor
# After Edge + Link are configured:
# doctor should distinguish Edge configured / reachable / OAuth metadata / MCP endpoint
# without printing tokens
```

Then in a **new** ChatGPT conversation:

1. Connector OAuth succeeds
2. `tools/list` shows epoch 2 / 18 tools
3. One read-only tool call (`herdr_inspect` or equivalent)
4. One real bounded mutation the operator chooses
5. One long-task / streaming basics smoke if in scope for this UAT pass

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
