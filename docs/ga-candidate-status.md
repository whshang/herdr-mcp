# GA Candidate Status — `0.4.0` stable

Status: **`v0.4.0` stable published** (2026-08-28). Stable-channel G9/G10 **PASS**. G1 dogfood stable apply **PASS**. G4 second-Mac stable clean install **PASS** (pi-ga-20260828). **Full GA not declared** — G25 remains **PARTIAL** (G14/G15 veto-adjacent rows; see [`ga-release-gate.md`](./ga-release-gate.md)).

SSOT for gate rows: [`docs/ga-release-gate.md`](./ga-release-gate.md). Exit-alpha runbook (archived): [`docs/history/ga/exit-alpha-checklist.md`](./history/ga/exit-alpha-checklist.md). Release planes: [`docs/release-model.md`](./release-model.md). **Patch-line note:** current published Rust runtime stable remains `v0.4.1`; `v0.4.2` is an untagged source candidate (`state_schema` **5** in current source; the `v0.4.0` snapshot below remains schema 4). The stable TCC broker has completed cross-generation authorization verification; Developer ID signing remains optional hardening. Remaining before tag: exact-final-source Rust Release qualification and final production Artifact Relay/R2 deploy-upload → Rust import → read-back UAT. Generic relay convergence is complete via PR #204; PR #199 was closed without merge after its overlapping relay work was superseded, pane-session PR #200 is merged, and `continuity.search` is integrated via PR #202. Rust Release manual dispatch builds and attests a pre-tag qualification bundle and cannot publish. This file otherwise preserves the first-GA `v0.4.0` closure snapshot.

**Historical alpha freeze (completed):** alpha.19 was the final alpha candidate and there was no alpha.20. Immutable historical tags/Releases remain retained per [`release-model.md`](./release-model.md#alpha-release-retention-policy). Merged/obsolete development branches and worktrees may now be cleaned after live safety checks.

## Stable shipped

| Field | Value |
| --- | --- |
| Stable Release version | `0.4.0` |
| Git tag | `v0.4.0` |
| Tag commit | `19fc6a41a7e35d850981f2c66119035f5a2c467d` (#142) |
| Runtime version (dogfood) | `0.4.0` |
| Generation (dogfood) | `rust-621d74d268b5299a` |
| Prior rollback baseline | `0.4.0-alpha.19` / `rust-3d2f685c636c3f3e` |
| Stable apply job (GA closure) | `upd-1787908596603-46525-621d74d2` |
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
| Extension zip excluded from manifest assets | PASS | manifest lists 2 platform binaries only; zip `0.1.68` bundled separately |
| Prerelease flag | PASS | GitHub Release `isPrerelease=false` |

## G4 second Mac stable clean install (2026-08-28 · PASS)

Evidence: [`docs/history/ga/g4-second-mac-stable-v040-uat-20260828.md`](./history/ga/g4-second-mac-stable-v040-uat-20260828.md)

| Check | Result | Notes |
| --- | --- | --- |
| Release binary | PASS | `v0.4.0` stable — not alpha.17/alpha.19 |
| `install` / `doctor` | PASS | 7/7 layers on pi |
| Independent Worker | PASS | `herdr-edge-yitaidiannao-local-ga` (retained UAT env) |
| Link | PASS | via operator proxy |
| Extension + native-host | PASS | extension `0.1.68`; 4 manifests |
| Stable channel at head | PASS | `update check` `available=false` |

## G6/G7 dogfood public UAT (2026-08-28 · PASS · alpha.19)

Evidence (archived, tracked): [`history/ga/g67-dogfood-public-uat-20260828.json`](./history/ga/g67-dogfood-public-uat-20260828.json)

Sealed on alpha.19 baseline; v0.4.0 stable does not change public MCP contract.

## G9/G10 preview-channel rc.1 rehearsal (2026-08-28 · PASS)

Evidence (archived, tracked): [`history/ga/g910-rc1-stable-rehearsal-20260828.json`](./history/ga/g910-rc1-stable-rehearsal-20260828.json)

| Step | Result | Notes |
| --- | --- | --- |
| Preflight `alpha.19` | PASS | generation `rust-3d2f685c636c3f3e` |
| `update check` (preview) | PASS | `0.4.0-rc.1` available, provenance verified |
| `update apply` | PASS | job `upd-1787906269602-5225-98dcc410` → `rust-98dcc4100429554a` |
| Post-update native-host | PASS | `runtime_matches_current=true`, version `0.4.0-rc.1` |
| `rollback` | PASS | `rb-1787906320335-rust-98dcc410`, guardian settled |
| Post-rollback `doctor` / native-host | PASS | restored `0.4.0-alpha.19` |

## G9/G10 stable-channel v0.4.0 rehearsal (2026-08-28 · PASS)

Evidence: the one-off local JSON was never tracked and is no longer retained; authoritative retained evidence is the stable-channel section in [`ga-release-gate.md`](./ga-release-gate.md) plus Rust Release run [`33157370273`](https://github.com/whshang/herdr-mcp/actions/runs/33157370273).

| Step | Result | Notes |
| --- | --- | --- |
| `update.channel=stable` | PASS | config.toml created/verified |
| `update check` (stable) | PASS | `0.4.0` available, provenance verified |
| `update apply` alpha.19→0.4.0 | PASS | job `upd-1787907966241-37421-621d74d2` → `rust-621d74d268b5299a` |
| Post-update native-host | PASS | `runtime_matches_current=true`, version `0.4.0` |
| Post-update link | PASS | prod Link Rust, loaded |
| `rollback` 0.4.0→alpha.19 | PASS | `rb-1787907991968-rust-621d74d2`, guardian settled |
| Post-rollback `doctor` / native-host | PASS | restored `0.4.0-alpha.19` |

### Dogfood GA-closure stable apply (2026-08-28 · PASS)

| Step | Result | Notes |
| --- | --- | --- |
| Preflight `alpha.19` | PASS | generation `rust-3d2f685c636c3f3e` |
| `update check` (stable) | PASS | `0.4.0` available |
| `update apply` alpha.19→0.4.0 | PASS | job `upd-1787908596603-46525-621d74d2` → `rust-621d74d268b5299a` |
| Post-apply native-host | PASS | `runtime_matches_current=true`, version `0.4.0` |
| Post-apply link | PASS | prod Link Rust, `production_ready=true` |
| Post-apply `doctor` | PASS | all layers green |

## Remaining before GA declare (honest)

| Gate | Why still open |
| --- | --- |
| G25 | **PARTIAL** — G14/G15 (and related veto #5/#7) not fully sealed; G4 install path **PASS** |
| G8 / G11 / G14 / G15 / G17 / G19 / G23 | **PARTIAL** — see scorecard; not all are veto-level |

## Can declare GA stable?

**Not yet.** `v0.4.0` stable Release exists; G4 second-Mac stable clean install PASS; dogfood `0.4.0`; user docs reference stable. Remaining: honest G25 / partial rows per [`ga-release-gate.md`](./ga-release-gate.md) — do not declare GA until vetoes clear or are explicitly DEFERRED post-GA.

## Next owner actions

1. Re-run scorecard when G14/G15 (or agreed post-GA DEFERRED) positions are updated.
2. Optional: dedicated `extension-release.yml` workflow (see [`release-model.md`](./release-model.md)).

## Related

- [`docs/ga-release-gate.md`](./ga-release-gate.md)
- [`docs/history/ga/exit-alpha-checklist.md`](./history/ga/exit-alpha-checklist.md)
- [`docs/history/ga/README.md`](./history/ga/README.md)
- [`docs/i18n/en/clean-machine-uat.md`](./i18n/en/clean-machine-uat.md)
