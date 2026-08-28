# Completed / Historical

# G18 Clean-Machine UAT Simulation — 2026-08-28

## Scope and honesty statement

This is a **clean-user simulation on the same physical Mac** (MacBookAir.local, arm64, macOS 26.5.2), **not** a second machine.

| Constraint | Status |
| --- | --- |
| Temporary `HOME` (`TMPHOME`) for herdr-mcp state | Used: `/tmp/g18-clean-home.LDadya` |
| Runtime binary from GitHub Release only (no repo checkout as install source) | Used: `v0.4.0-alpha.14` `aarch64-apple-darwin` |
| No production Link seal / cutover / tag cut | Honored — not run |
| No mutation of real `~/Library/LaunchAgents` for dogfood / link-prod | Honored — install aborted before launchd mutation |
| Not a second physical Mac | Explicitly documented |

Dogfood production Link/service remained untouched. Post-probe integrity: production LaunchAgents SHA-256 and `launchctl` label set unchanged vs pre-probe baseline.

---

## Verdict

**PARTIAL** for G18 clean-machine sim.

Enough signal for: release download → version → path isolation → doctor/status/update-check under TMPHOME (with optional Herdr socket bind).

Blocked for: full `herdr-mcp install` / service activation UAT on this host without colliding with dogfood launchd labels and loopback `:8772`.

**A second Mac (or a dedicated non-dogfood macOS user / VM with isolated launchd domain) is required** to complete install → service healthy → doctor green without production collision.

---

## Evidence

### 1. Isolated HOME

```text
TMPHOME=/tmp/g18-clean-home.LDadya
HOME=$TMPHOME
PATH=$TMPHOME/bin:...
REAL_HOME=/Users/qingxian  (read-only reference; not used as herdr-mcp config root)
```

RuntimePaths resolve via `$HOME` / optional `HERDR_SOCKET_PATH`:

| Probe | Result |
| --- | --- |
| `herdr-mcp config path` | `/tmp/g18-clean-home.LDadya/.config/herdr-mcp/config.toml` |
| Doctor INFO state | `/tmp/g18-clean-home.LDadya/.config/herdr-mcp` |
| Service status `plist` | `/tmp/g18-clean-home.LDadya/Library/LaunchAgents/dev.herdr-mcp.server.plist` |
| Link status prod plist path | under TMPHOME `Library/LaunchAgents/...` |

**Abort criterion "RuntimePaths resolve using real HOME despite TMPHOME": not triggered.** Paths stayed under TMPHOME.

### 2. Release binary (not repo)

| Field | Value |
| --- | --- |
| Repo | `whshang/herdr-mcp` |
| Tag | `v0.4.0-alpha.14` |
| Asset | `herdr-mcp-0.4.0-alpha.14-aarch64-apple-darwin` |
| Size | 17015728 |
| SHA-256 | `7c0ac0b73060aae07120410268d3cdb3afe409756c94b6e27af64069cf6066d9` |
| File | Mach-O 64-bit executable arm64 |
| Install location | `$TMPHOME/bin/herdr-mcp` |

```text
$ herdr-mcp --version
herdr-mcp 0.4.0-alpha.14
contract epoch 2 / 18 tools
state schema 4
```

### 3. Herdr transport strategy

Prefer **(b)** bind real Herdr socket for Herdr-only probes while keeping herdr-mcp state under TMPHOME:

```text
HERDR_SOCKET_PATH=/Users/qingxian/.config/herdr/herdr.sock
```

With bind — doctor Herdr layers PASS (transport, schema, RPC, snapshot, inspect, event cache).

Without bind (control contrast) — Herdr sock expected under TMPHOME and missing:

```text
LAYER herdr unowned unreachable sock=/tmp/g18-clean-home.LDadya/.config/herdr/herdr.sock
FAIL Herdr local transport / validated RPC / snapshot / inspect / event cache
```

No link seal, cutover, or production Link install was run.

### 4. Doctor / status / update (safe)

**With `HERDR_SOCKET_PATH` bind:**

```text
Herdr MCP doctor
PASS runtime endpoint
PASS Herdr local transport
PASS Herdr API schema
PASS validated Herdr RPC
PASS Herdr snapshot state
PASS Herdr inspect projection
PASS Herdr event cache
LAYER local-runtime unowned healthy http=401 port=8772 generation=missing
LAYER service absent ... loaded=true ... label=dev.herdr-mcp.server
LAYER link absent ... prod_loaded=true ... (plist paths under TMPHOME)
```

**status:** reports config under TMPHOME; `127.0.0.1:8772 healthy (HTTP 401)`; Herdr transport reachable; update channel stable.

**update check:**

```json
{
  "ok": true,
  "available": false,
  "current_version": "0.4.0-alpha.14",
  "release_version": "0.4.0-alpha.14",
  "tag": "v0.4.0-alpha.14",
  "asset": "herdr-mcp-0.4.0-alpha.14-aarch64-apple-darwin",
  "sha256": "7c0ac0b73060aae07120410268d3cdb3afe409756c94b6e27af64069cf6066d9",
  "provenance_verified": true,
  "repository": "whshang/herdr-mcp",
  "target": "aarch64-apple-darwin"
}
```

**update status:** `{"code":"update_status","job":null,"ok":true}`

**native-host status:** `extension_path_unconfigured` (expected on clean HOME).

### 5. Install — STOP (blocker)

`herdr-mcp install` maps to service install. Source behavior (`service_manager.rs`):

1. Plist path = `$HOME/Library/LaunchAgents/dev.herdr-mcp.server.plist` → under TMPHOME (file write would be isolated).
2. Launchd label is fixed: `dev.herdr-mcp.server` (also health-watchdog).
3. Install calls `is_loaded(SERVICE_LABEL)` then, if mutating, **`bootout` + `bootstrap`** on that label.
4. Same-active noop requires an existing **Rust** plist under the TMPHOME plist path — missing on clean TMPHOME — so install would **not** noop; it would treat production's loaded label as `server_was_loaded=true` and **bootout dogfood**.

Dogfood already owns:

```text
7356  0  dev.herdr-mcp.server   # listens 127.0.0.1:8772
80664 0  dev.herdr-mcp.link-prod
...   health-watchdog / link / link-rust-candidate
```

Additionally, doctor/status **false-positive** healthy runtime on `:8772` is the **production** process (`.../runtime/current/herdr-mcp candidate --port 8772`), not a TMPHOME generation.

**Decision:** did **not** run `herdr-mcp install` / `service install` / link install / seal / cutover.

Production LaunchAgents hashes and launchctl snapshot unchanged after simulation.

---

## What passed

1. Clean TMPHOME isolation for config/state paths.
2. GitHub Release `v0.4.0-alpha.14` darwin arm64 download + `chmod +x` (no repo binary as runtime source).
3. `--version` = `0.4.0-alpha.14`.
4. Doctor/status with Herdr socket bind (option b).
5. Doctor without bind documents expected Herdr unavailability (option a).
6. `update check` / `update status` against release provenance.
7. Production LaunchAgents / launchctl integrity preserved.

## What blocked

1. Full service install activation under TMPHOME on this Mac (shared user launchd labels + shared `:8772`).
2. Clean-machine proof that install creates and boots **its own** healthy generation without seeing dogfood HTTP 401.
3. Native messaging / extension path end-to-end on a truly clean user profile.
4. Link production ownership gates that require real Link install — intentionally out of scope; seal/cutover forbidden.

## Second Mac required?

**Yes, for a complete G18 install/service UAT** (or equivalent: separate macOS user account / VM where `dev.herdr-mcp.*` labels and port 8772 are free).

Same-hardware TMPHOME sim is valid for download → CLI → path isolation → doctor/update-check, but **cannot honestly complete install** without risking dogfood launchd mutation.

---

## Explicit non-actions

- No `herdr-mcp install` / `service install|start|stop|restart|uninstall`
- No `link seal` / `link cutover --execute` / `link install`
- No edits to `/Users/qingxian/Library/LaunchAgents/dev.herdr-mcp.link-prod.plist` (or any real LaunchAgents)
- No git tag cuts
- No use of repo `target/*/herdr-mcp` as the UAT runtime binary

## Simulation artifacts

| Item | Path |
| --- | --- |
| TMPHOME | `/tmp/g18-clean-home.LDadya` |
| Binary | `/tmp/g18-clean-home.LDadya/bin/herdr-mcp` |
| Env helper | `/tmp/g18-uat-env.sh` |
| This report | `docs/_wip/g18-clean-machine-sim-20260828.md` |

Recorded UTC: 2026-08-27T17:29:06Z

---

## Addendum — alpha.15 re-probe (2026-08-28)

Re-ran the same-Mac TMPHOME probe against GitHub Release `v0.4.0-alpha.15` only (no repo checkout, no `herdr-mcp install`).

| Check | Result |
| --- | --- |
| Download `herdr-mcp-0.4.0-alpha.15-aarch64-apple-darwin` + extension zip | OK |
| `--version` | `0.4.0-alpha.15` / epoch 2 / 18 tools |
| Config/state under TMPHOME | OK |
| `update check` provenance | OK (`available=false` at alpha.15) |
| Dogfood `launchctl` label set | unchanged |
| Dogfood `~/Library/LaunchAgents/dev.herdr-mcp.*.plist` SHA-256 | unchanged |
| Full `install` | **not run** (hardcoded label + `:8772`; TMPHOME `status`/`doctor` still saw dogfood `:8772` HTTP 401) |

**Verdict unchanged: PARTIAL.** Operator runbook for a second Mac: `docs/i18n/en/clean-machine-uat.md` (zh-CN twin updated). Dedicated second macOS user on the **same** host is **not** sufficient because port `8772` is machine-wide.
