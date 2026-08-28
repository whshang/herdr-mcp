# GA Candidate Status — `0.4.0-alpha.19` (last alpha)

Status: **alpha candidate frozen** (2026-08-28). **Do not cut `v0.4.0` stable** until remaining vetoes in [`ga-release-gate.md`](./ga-release-gate.md) are honest PASS.

SSOT for gate rows: [`docs/ga-release-gate.md`](./ga-release-gate.md). Exit-alpha runbook: [`docs/exit-alpha-checklist.md`](./exit-alpha-checklist.md).

## Current candidate

| Field | Value |
| --- | --- |
| Runtime version | `0.4.0-alpha.19` |
| Git tag | `v0.4.0-alpha.19` |
| Tag commit | `4690c13d9e7304852f9f762cb783d1326f7a5e12` (#134) |
| Generation (dogfood) | `rust-3d2f685c636c3f3e` |
| Contract | epoch 2 / 18 tools / state schema 4 |
| Release URL | <https://github.com/whshang/herdr-mcp/releases/tag/v0.4.0-alpha.19> |
| Tag-path workflow | [Rust Release run 33152207094](https://github.com/whshang/herdr-mcp/actions/runs/33152207094) — verify → build → manifest → attest → **publish** all PASS |
| Recovery-only? | **No** — first end-to-end tag-path publish after #133 extension-glob fix |

## Release identity (G2 seal)

| Check | Result | Evidence |
| --- | --- | --- |
| Tag SHA == manifest `source_commit` | PASS | `4690c13…` in `release-manifest.json` |
| Manifest `source_ref` | PASS | `refs/tags/v0.4.0-alpha.19` |
| Binary SHA (darwin aarch64) | PASS | `3d2f685c636c3f3e2c4720c9a63c703a818307af0507a68ac953268f8a009a60` |
| Updater provenance | PASS | dogfood `update check` `provenance_verified=true` |
| Attestation | PASS | `actions/attest` job green on run 33152207094 |
| Extension zip excluded from manifest assets | PASS | manifest lists 2 platform binaries only; bundle has 5 files (binaries + extension zip + sha256 + manifest) |
| Fail-closed duplicate publish | PASS | alpha.18 tag run `33150060112` failed identity verify before publish; publish step refuses existing release (`refusing publish overwrite`) |

## Dogfood cut (default instance)

| Step | Result |
| --- | --- |
| `update apply` alpha.18 → alpha.19 | PASS (`upd-1787903368810-67219-3d2f685c`) |
| `--version` | `0.4.0-alpha.19` |
| `native-host` / `doctor` | `runtime_matches_current=true`, `version_consistent=true` |
| `link seal status` | `production_ready=true` (unchanged) |

## G6/G7 local MCP smoke (dogfood, 2026-08-28)

Not a substitute for owner ChatGPT UAT; records native tool path only.

| Tool / flow | Result | Notes |
| --- | --- | --- |
| `herdr_inspect` | PASS | epoch 2 / 18 tools; `production_ready=true` |
| `herdr_fs_grep` | PASS | `G2` hits in `docs/` |
| `herdr_git status` | PASS | clean tree on `herdr-mcp` |
| `herdr_fs_write` (bounded) | PASS | created `docs/_wip/ga-uat-local-mutation-evidence-20260828.txt` (gitignored) |
| `herdr_exec_start` → `herdr_exec_read` | PASS | single session `es_107ef-…`; completed; no duplicate start |

Public ChatGPT Connector matrix (OAuth, fresh `tools/list`, cross-turn) remains **owner-only** — see Owner ChatGPT UAT pack in `ga-release-gate.md`.

## Remaining GA blockers (honest)

| Gate | Why still open |
| --- | --- |
| G1 | Still alpha semver; no `v0.4.0` stable |
| G9 / G10 | No stable-channel N→N+1 update/rollback rehearsal (blocked until stable tag) |
| G6 / G7 | Dogfood local smoke only; full public matrix / soak not sealed |
| G24 / G25 | Vetoes above; **do not tag stable** |

## Stable candidate rehearsal design (`v0.4.0-rc.1` or `v0.4.0`)

**Not executed.** Rehearsal sequence for G9/G10 evidence after stable tag exists:

```text
install (or already on N)
  → update check (stable channel)
  → update apply
  → doctor
  → native-host status   # runtime_matches_current=true
  → link seal status     # production_ready=true
  → rollback
  → doctor
  → native-host status
  → runtime identity unchanged / prior generation restored
```

Optional preview-channel rehearsal before stable tag (does **not** flip G9/G10 to PASS):

```bash
herdr-mcp update check --manifest <pinned-manifest-url>
herdr-mcp update apply --manifest <pinned-manifest-url>
herdr-mcp doctor
herdr-mcp native-host status
herdr-mcp rollback
herdr-mcp doctor
```

Record job ids, generation paths, and non-secret `doctor` layer summaries in scorecard.

## Next owner actions

1. Review G6/G7 Owner ChatGPT UAT pack on dogfood maintenance window or second Mac (if not already fully sealed).
2. When vetoes clear: bump `Cargo.toml` to `0.4.0`, refresh docs, tag `v0.4.0`, run stable-channel G9/G10 rehearsal.
3. **Do not** create `alpha.20+` unless a blocking defect requires a new prerelease.

## Related

- [`docs/ga-release-gate.md`](./ga-release-gate.md) — G1–G25 scorecard
- [`docs/exit-alpha-checklist.md`](./exit-alpha-checklist.md) — G1 version unification
- [`docs/i18n/en/clean-machine-uat.md`](./i18n/en/clean-machine-uat.md) — second Mac canonical path
