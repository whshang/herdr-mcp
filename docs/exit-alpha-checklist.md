# Exit-alpha checklist (G1) — `0.4.0` stable version unification

Docs-only planning runbook. **Do not cut `v0.4.0` (non-prerelease) until remaining GA vetoes are cleared.**

SSOT for gate status: [`docs/ga-release-gate.md`](./ga-release-gate.md). **Live candidate:** [`docs/ga-candidate-status.md`](./ga-candidate-status.md) (`0.4.0-alpha.19`).

**FREEZE:** `0.4.0-alpha.19` = final GA candidate. **No alpha.20.** No delete release/branch/worktree.

## Preconditions (all required before G1 / stable tag)

| Gate | Status (2026-08-28) | Notes |
| --- | --- | --- |
| G18 | **PASS** | Second Mac clean install + public MCP |
| G6 / G7 | **PASS** | Dogfood public matrix sealed 2026-08-28 |
| G12 | **PASS** | Dogfood long-exec via public path |
| G2 | **PASS** | alpha.19 tag-path publish — see [`ga-candidate-status.md`](./ga-candidate-status.md) |
| G15 | **PARTIAL** | Second Mac extension sealed; dogfood uat singleton |
| G17 | **PARTIAL** | Public path OK; full security matrix open |
| G9 / G10 | **PARTIAL** | Preview `alpha.18↔alpha.19` rehearsal PASS; **stable-channel blocked until rc.1/stable tag** |
| G25 | **FAIL** | Vetoes remain |

## rc.1 path (before stable)

Execute **after** owner approves rc.1 tag PR. **Do not skip to `v0.4.0` stable.**

```text
1. Tag v0.4.0-rc.1 (Cargo.toml bump; rust-release.yml publishes)
2. Dogfood or second Mac: install from rc.1 Release OR update apply (preview)
3. herdr-mcp doctor
4. herdr-mcp --version          # expect 0.4.0-rc.1
5. herdr-mcp native-host status # runtime_matches_current=true
6. herdr-mcp link status        # production_ready, Rust link-prod
7. herdr-mcp rollback           # verify recovery
8. herdr-mcp doctor
9. Flip update.channel=stable (if policy ready) → update check → apply → rollback
10. Record evidence in ga-release-gate.md
```

**2026-08-28 rehearsal (preview channel, no rc.1 tag):** `alpha.19` → rollback → `alpha.18` → update apply → `alpha.19` PASS. Evidence: `docs/_wip/g910-rc1-rehearsal-20260828.json` (gitignored).

**Equivalence limit:** preview-channel alpha rehearsal ≠ stable-channel G9/G10 PASS.

## Version surfaces today vs stable target

| Surface | Live today | rc.1 target | Stable target |
| --- | --- | --- | --- |
| Rust runtime | `0.4.0-alpha.19` | `0.4.0-rc.1` | `0.4.0` |
| `crates/herdr-mcp/Cargo.toml` | `0.4.0-alpha.19` | `0.4.0-rc.1` | `0.4.0` |
| Git tag | `v0.4.0-alpha.19` | `v0.4.0-rc.1` | `v0.4.0` |
| Update channel (dogfood) | `preview` | `preview` then `stable` soak | `stable` |
| `package.json` | `0.3.32` | unchanged | **not** runtime version |

## Maintainer sequence (stable — after rc.1 + stable-channel G9/G10)

```bash
# 1. Preflight (read-only, independent shell)
herdr-mcp --version
herdr-mcp service status
herdr-mcp link seal status

# 2. Bump Cargo.toml to 0.4.0 (no -alpha, no -rc)
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace

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
- Stable-channel G9/G10 rehearsal PASS
- Second Mac clean install from stable Release
- `package.json` explicitly **not** the runtime product version

## Explicit non-steps

- **Do not** tag `v0.4.0` while G9/G10 stable-channel remains PARTIAL.
- **Do not** tag `v0.4.0-alpha.20` (alpha.19 is final candidate).
- **Do not** rename `package.json` to `0.4.0` as a G1 shortcut.

## Related

- [`docs/ga-candidate-status.md`](./ga-candidate-status.md)
- [`docs/ga-release-gate.md`](./ga-release-gate.md)
- [`docs/i18n/en/clean-machine-uat.md`](./i18n/en/clean-machine-uat.md)
- [`docs/i18n/zh-CN/clean-machine-uat.md`](./i18n/zh-CN/clean-machine-uat.md)
