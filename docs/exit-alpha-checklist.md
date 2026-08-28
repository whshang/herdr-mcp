# Exit-alpha checklist (G1) — `0.4.0` stable version unification

Docs-only planning runbook. **Do not cut `v0.4.0` (non-prerelease) until remaining GA vetoes are cleared.**

SSOT for gate status: [`docs/ga-release-gate.md`](./ga-release-gate.md). **Live candidate:** [`docs/ga-candidate-status.md`](./ga-candidate-status.md) (`0.4.0-rc.1` published; dogfood baseline `0.4.0-alpha.19`).

**FREEZE:** `0.4.0-alpha.19` = final alpha candidate. **No alpha.20.** No delete release/branch/worktree.

## Preconditions (all required before G1 / stable tag)

| Gate | Status (2026-08-28) | Notes |
| --- | --- | --- |
| G18 | **PASS** | Second Mac clean install + public MCP |
| G6 / G7 | **PASS** | Dogfood public matrix sealed 2026-08-28 (alpha.19) |
| G12 | **PASS** | Dogfood long-exec via public path |
| G2 | **PASS** | rc.1 tag-path publish — run `33155520284`; see [`ga-candidate-status.md`](./ga-candidate-status.md) |
| G15 | **PARTIAL** | Second Mac extension sealed; dogfood uat singleton |
| G17 | **PARTIAL** | Public path OK; full security matrix open |
| G9 / G10 | **PARTIAL** | Preview `alpha.19↔rc.1` PASS; **stable-channel BLOCKED** (no non-prerelease release) |
| G25 | **FAIL** | Vetoes remain |

## rc.1 path (executed 2026-08-28)

```text
1. Tag v0.4.0-rc.1 (#138 Cargo bump + #139 Cargo.lock) — DONE
2. Rust Release run 33155520284 — publish PASS
3. Preview: update check → apply → rc.1 — PASS
4. native-host / doctor post-apply — PASS
5. rollback → alpha.19 — PASS
6. Stable channel update check — BLOCKED (by design)
```

Evidence: `docs/_wip/g910-rc1-stable-rehearsal-20260828.json` (gitignored).

**Stable-channel blocker:** `UpdateChannel::Stable` only accepts `version.pre.is_empty()`. GitHub has no non-prerelease `0.4.x` release yet. Stable G9/G10 requires `v0.4.0` stable tag (or policy change to accept `rc` on stable channel).

## Version surfaces today vs stable target

| Surface | Live today | rc.1 (published) | Stable target |
| --- | --- | --- | --- |
| Rust runtime (dogfood) | `0.4.0-alpha.19` | `0.4.0-rc.1` (preview apply) | `0.4.0` |
| `crates/herdr-mcp/Cargo.toml` (main) | `0.4.0-rc.1` | `0.4.0-rc.1` | `0.4.0` |
| Git tag (latest rc) | — | `v0.4.0-rc.1` @ `0a4627e` | `v0.4.0` |
| Update channel (dogfood) | `preview` | `preview` | `stable` |
| `package.json` | `0.3.32` | unchanged | **not** runtime version |

## G1 unification assessment (2026-08-28)

| Surface | Can unify to `0.4.0-rc.1` now? | Gap for `0.4.0` stable |
| --- | --- | --- |
| Cargo.toml / binary | **Yes** — main @ `0.4.0-rc.1`, Release published | Bump to `0.4.0`, retag |
| Git tag / GitHub Release | **Yes** — `v0.4.0-rc.1` live | Need `v0.4.0` non-prerelease Release |
| Dogfood runtime | **Partial** — rehearsal proved apply/rollback; baseline restored to alpha.19 | Stable-channel apply to `0.4.0` |
| User docs / README | **No** — still alpha terminology (G20) | Stable docs freeze |
| `package.json` | **N/A** — not runtime version | Keep separate |

## Maintainer sequence (stable — after stable-channel G9/G10 unblocked)

```bash
# 1. Preflight (read-only, independent shell)
herdr-mcp --version
herdr-mcp service status
herdr-mcp link seal status

# 2. Bump Cargo.toml to 0.4.0 (no -alpha, no -rc)
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
# Do not forget: cargo generate-lockfile / commit Cargo.lock

# 3. Tag only when scorecard vetoes cleared
git tag -a v0.4.0 -m "herdr-mcp 0.4.0 stable"
git push origin v0.4.0

# 4. Post-release (independent shell)
herdr-mcp update check    # stable channel → 0.4.0
herdr-mcp update apply
herdr-mcp doctor
herdr-mcp --version       # 0.4.0, no alpha

# 5. Second Mac clean install regression from v0.4.0 Release
```

## Evidence to flip G1 → PASS

- `herdr-mcp --version` → `0.4.0` on dogfood after stable-channel `update apply`
- GitHub Release `v0.4.0` assets + manifest SHA verification
- Stable-channel G9/G10 rehearsal PASS (currently **BLOCKED**)
- Second Mac clean install from stable Release
- `package.json` explicitly **not** the runtime product version

## Explicit non-steps

- **Do not** tag `v0.4.0` while G9/G10 stable-channel remains BLOCKED.
- **Do not** tag `v0.4.0-alpha.20` (alpha.19 is final candidate).
- **Do not** rename `package.json` to `0.4.0` as a G1 shortcut.
- **Do not** delete/move historical tags (`v0.4.0-alpha.*`, `v0.4.0-rc.1`).

## Related

- [`docs/ga-candidate-status.md`](./ga-candidate-status.md)
- [`docs/ga-release-gate.md`](./ga-release-gate.md)
- [`docs/i18n/en/clean-machine-uat.md`](./i18n/en/clean-machine-uat.md)
- [`docs/i18n/zh-CN/clean-machine-uat.md`](./i18n/zh-CN/clean-machine-uat.md)
