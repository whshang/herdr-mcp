# GA Candidate Status — `0.4.0` stable

Status: **`v0.4.0` stable published** (2026-08-28). Stable-channel G9/G10 **PASS**. **Full GA not declared** — G20–G22 docs freeze and remaining vetoes in [`ga-release-gate.md`](./ga-release-gate.md).

SSOT for gate rows: [`docs/ga-release-gate.md`](./ga-release-gate.md). Exit-alpha runbook: [`docs/exit-alpha-checklist.md`](./exit-alpha-checklist.md).

**FREEZE:** alpha.19 = final alpha candidate. **No alpha.20.** No delete release/branch/worktree. Prior tags `v0.4.0-alpha.19`, `v0.4.0-rc.1` retained.

## Stable shipped

| Field | Value |
| --- | --- |
| Stable Release version | `0.4.0` |
| Git tag | `v0.4.0` |
| Tag commit | `19fc6a41a7e35d850981f2c66119035f5a2c467d` (#142) |
| Runtime version (dogfood post G9/G10) | `0.4.0-alpha.19` (rollback baseline) |
| Generation (dogfood baseline) | `rust-3d2f685c636c3f3e` |
| Stable apply generation (rehearsal) | `rust-621d74d268b5299a` |
| Contract | epoch 2 / 18 tools / state schema 4 |
| Release URL | <https://github.com/whshang/herdr-mcp/releases/tag/v0.4.0> |
| Tag-path workflow | [Rust Release run 33157370273](https://github.com/whshang/herdr-mcp/actions/runs/33157370273) — verify → build → manifest → attest → **publish** all PASS |
| Prior rc candidate | `v0.4.0-rc.1` @ `0a4627e` — retained, not deleted |
| Prior alpha candidate | `v0.4.0-alpha.19` @ `4690c13` — retained, not deleted |
| Connector URL (workers.dev) | `https://herdr-edge-prod.whshang.workers.dev/mcp` |
| Connector URL (custom domain) | `https://herdr-mcp.agentforme.cc.cd/mcp` |
| OAuth issuer | `https://herdr-mcp.agentforme.cc.cd` |
| Update channel (dogfood) | `stable` (config.toml) |

## Release identity (G2 seal — v0.4.0 stable)

| Check | Result | Evidence |
| --- | --- | --- |
| Tag SHA == manifest `source_commit` | PASS | `19fc6a4…` in `release-manifest.json` |
| Manifest `source_ref` | PASS | `refs/tags/v0.4.0` |
| Binary SHA (darwin aarch64) | PASS | `621d74d268b5299a7141e67710e107e38efd87f16594dfb8ff54ce66097e29c5` |
| Updater provenance | PASS | stable `update check` `provenance_verified=true` |
| Attestation | PASS | `actions/attest` job green on run 33157370273 |
| Extension zip excluded from manifest assets | PASS | manifest lists 2 platform binaries only |
| Prerelease flag | PASS | GitHub Release `isPrerelease=false` |

## G6/G7 dogfood public UAT (2026-08-28 · PASS · alpha.19)

Evidence (local, gitignored): `docs/_wip/g67-dogfood-public-uat-20260828.json`

Sealed on alpha.19 baseline; v0.4.0 stable does not change public MCP contract.

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

## G9/G10 stable-channel v0.4.0 rehearsal (2026-08-28 · PASS)

Evidence (local, gitignored): `docs/_wip/g910-stable-v040-20260828.json`

| Step | Result | Notes |
| --- | --- | --- |
| `update.channel=stable` | PASS | config.toml created/verified |
| `update check` (stable) | PASS | `0.4.0` available, provenance verified |
| `update apply` alpha.19→0.4.0 | PASS | job `upd-1787907966241-37421-621d74d2` → `rust-621d74d268b5299a` |
| Post-update native-host | PASS | `runtime_matches_current=true`, version `0.4.0` |
| Post-update link | PASS | prod Link Rust, loaded |
| `rollback` 0.4.0→alpha.19 | PASS | `rb-1787907991968-rust-621d74d2`, guardian settled |
| Post-rollback `doctor` / native-host | PASS | restored `0.4.0-alpha.19` |

## Remaining GA blockers (honest)

| Gate | Why still open |
| --- | --- |
| G1 | Dogfood rolled back to `alpha.19` after stable G9/G10 rehearsal; optional stable apply pending |
| G4 | Second Mac clean install from `v0.4.0` stable Release not yet sealed (prior G18 used `alpha.17`) |
| G24 / G25 | G4 UAT open; G1 dogfood version not yet unified to `0.4.0` |

## Can declare GA stable?

**Not yet.** `v0.4.0` stable Release exists; stable-channel G9/G10 PASS; user-facing docs now reference `v0.4.0` stable. Missing: G4 second-Mac stable clean install UAT, G1 dogfood optional stable apply, G25 veto #8 until G4 seals.

## Next owner actions

1. **G4 — second Mac clean install** from [`v0.4.0` Release](https://github.com/whshang/herdr-mcp/releases/tag/v0.4.0) binary (see [clean-machine-uat §One-command bootstrap](i18n/en/clean-machine-uat.md#one-command-operator-bootstrap-second-mac-default-instance); prior G18 used `alpha.17`).
2. **G1 — exit alpha** (optionally `update apply` on dogfood to leave `0.4.0` resident after G4 PASS).
3. Re-run scorecard; if G24/G25 clear → declare GA.

## Related

- [`docs/ga-release-gate.md`](./ga-release-gate.md)
- [`docs/exit-alpha-checklist.md`](./exit-alpha-checklist.md)
- [`docs/i18n/en/clean-machine-uat.md`](./i18n/en/clean-machine-uat.md)
