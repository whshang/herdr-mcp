# GA Candidate Status — `0.4.0-rc.1` (GA release candidate)

Status: **rc.1 published** (2026-08-28). **Do not cut `v0.4.0` stable** until remaining vetoes in [`ga-release-gate.md`](./ga-release-gate.md) are honest PASS.

SSOT for gate rows: [`docs/ga-release-gate.md`](./ga-release-gate.md). Exit-alpha runbook: [`docs/exit-alpha-checklist.md`](./exit-alpha-checklist.md).

**FREEZE:** alpha.19 = final alpha candidate. **No alpha.20.** No delete release/branch/worktree.

## Current candidate

| Field | Value |
| --- | --- |
| Runtime version (dogfood) | `0.4.0-alpha.19` (post-rollback baseline) |
| rc.1 Release version | `0.4.0-rc.1` |
| Git tag | `v0.4.0-rc.1` |
| Tag commit | `0a4627ebfbd69aa7ff914e8c5d3aa76dbd643c40` (#138 + #139) |
| Generation (dogfood baseline) | `rust-3d2f685c636c3f3e` |
| rc.1 generation (rehearsal) | `rust-98dcc4100429554a` |
| Contract | epoch 2 / 18 tools / state schema 4 |
| Release URL | <https://github.com/whshang/herdr-mcp/releases/tag/v0.4.0-rc.1> |
| Tag-path workflow | [Rust Release run 33155520284](https://github.com/whshang/herdr-mcp/actions/runs/33155520284) — verify → build → manifest → attest → **publish** all PASS |
| Prior alpha candidate | `v0.4.0-alpha.19` @ `4690c13` — retained, not deleted |
| Connector URL (workers.dev) | `https://herdr-edge-prod.whshang.workers.dev/mcp` |
| Connector URL (custom domain) | `https://herdr-mcp.agentforme.cc.cd/mcp` |
| OAuth issuer | `https://herdr-mcp.agentforme.cc.cd` |
| Update channel (dogfood) | `preview` |

## Release identity (G2 seal — rc.1)

| Check | Result | Evidence |
| --- | --- | --- |
| Tag SHA == manifest `source_commit` | PASS | `0a4627e…` in `release-manifest.json` |
| Manifest `source_ref` | PASS | `refs/tags/v0.4.0-rc.1` |
| Binary SHA (darwin aarch64) | PASS | `98dcc4100429554a42e3fcd39fdebc3682393ba36a05fefc50386fec3d76d3f7` |
| Updater provenance | PASS | preview `update check` `provenance_verified=true` |
| Attestation | PASS | `actions/attest` job green on run 33155520284 |
| Extension zip excluded from manifest assets | PASS | manifest lists 2 platform binaries only |
| Prerelease flag | PASS | GitHub Release `isPrerelease=true` (`v*-*` policy) |

## G6/G7 dogfood public UAT (2026-08-28 · PASS · alpha.19)

Evidence (local, gitignored): `docs/_wip/g67-dogfood-public-uat-20260828.json`

Sealed on alpha.19 baseline; rc.1 does not change public MCP contract.

## G9/G10 preview-channel rc.1 rehearsal (2026-08-28 · PASS)

Evidence (local, gitignored): `docs/_wip/g910-rc1-stable-rehearsal-20260828.json`

| Step | Result | Notes |
| --- | --- | --- |
| Preflight `alpha.19` | PASS | generation `rust-3d2f685c636c3f3e` |
| `update check` (preview) | PASS | `0.4.0-rc.1` available, provenance verified |
| `update apply` | PASS | job `upd-1787906269602-5225-98dcc410` → `rust-98dcc4100429554a` |
| Post-update native-host | PASS | `runtime_matches_current=true`, version `0.4.0-rc.1` |
| `rollback` | PASS | `rb-1787906320335-rust-98dcc410`, guardian settled |
| Post-rollback `doctor` / native-host | PASS | restored `0.4.0-alpha.19` |

## G9/G10 stable-channel rehearsal (2026-08-28 · BLOCKED)

| Step | Result | Notes |
| --- | --- | --- |
| `update check` (stable) | **BLOCKED** | No non-prerelease release with manifest on GitHub |
| Policy | by design | `UpdateChannel::Stable` accepts `version.pre.is_empty()` only; `0.4.0-rc.1` excluded |
| GA implication | **BLOCKED** | Stable N→N+1 requires `v0.4.0` stable tag first |

**Minimal unblock path:** tag `v0.4.0` (non-prerelease) → stable channel discovers it → rehearsal stable `alpha.19 or rc.1` → `0.4.0` → rollback. Alternative owner policy: extend stable channel to accept `rc` prereleases (code change in `config.rs`).

## Remaining GA blockers (honest)

| Gate | Why still open |
| --- | --- |
| G1 | Dogfood baseline still alpha.19; no `v0.4.0` stable |
| G9 / G10 | Stable-channel rehearsal **BLOCKED** — no stable release exists yet |
| G20–G22 | Stable docs freeze (user paths still mention alpha) |
| G24 / G25 | Vetoes above; **do not tag stable** |

## Can enter `v0.4.0` formal release?

**No.** Missing: G1 exit-alpha, stable-channel G9/G10 (needs `v0.4.0` stable tag + rehearsal), G20–G22 docs freeze, G25 vetoes cleared.

## Next owner actions (fork — pick one)

**Owner decision required:** stable-channel G9/G10 is BLOCKED until one path is chosen. Do not tag `v0.4.0` stable or change `UpdateChannel` policy without explicit owner approval.

| | Option A — stable tag path | Option B — stable channel accepts rc |
| --- | --- | --- |
| **Action** | Tag `v0.4.0` (non-prerelease) after vetoes cleared | Code PR: extend `UpdateChannel::Stable` to accept `rc` prereleases |
| **Then** | Stable-channel G9/G10 rehearsal (`update check` → `apply` → `rollback`) | Stable-channel G9/G10 rehearsal against `0.4.0-rc.1` on stable channel |
| **Docs** | G20–G22 stable docs freeze | G20–G22 stable docs freeze (wording may differ) |
| **Risk / note** | Standard semver GA cut; stable users get `0.4.0` only | Policy change; needs owner approval + regression tests; no stable tag yet |

**After chosen path PASS:** G1 exit-alpha, G20–G22 docs freeze, second Mac clean install from stable or rc.1 Release.

## Related

- [`docs/ga-release-gate.md`](./ga-release-gate.md)
- [`docs/exit-alpha-checklist.md`](./exit-alpha-checklist.md)
- [`docs/i18n/en/clean-machine-uat.md`](./i18n/en/clean-machine-uat.md)
