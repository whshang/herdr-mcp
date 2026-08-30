# Release model — Runtime, Extension, and Contract Compatibility

Status: **current runtime stable `v0.4.1` published** (2026-08-28). First-GA `v0.4.0` evidence remains historical and immutable. This file is the contributor-facing definition of how releases are sliced. GA gate status lives in [`ga-release-gate.md`](./ga-release-gate.md).

## Three release planes (independent lifecycles)

| Plane | What ships | Version source | Consumer update path | Bound to runtime tag? |
| --- | --- | --- | --- | --- |
| **Runtime Release** | `herdr-mcp` Rust binary, `release-manifest.json`, platform SHA256 sidecars | `crates/herdr-mcp/Cargo.toml` → Git tag `v*` | `herdr-mcp update check/apply` on `stable` or `preview` channel | **Yes** — manifest drives updater |
| **Browser Extension Release** | Chrome Web Store item | `extension/manifest.json` `version` | Chrome Web Store automatic update; `native-host install` binds the Store origin to the active runtime | **No** — independent Store lifecycle |
| **Contract Compatibility** | MCP epoch, tool catalog, state schema, Edge OAuth/MCP surface | Code + release manifest fields | Edge deploy + Link + runtime generation together | **Coupled by epoch**, not by zip filename |

**Critical rule:** Runtime GitHub Releases do **not** distribute the browser extension. The extension is a Chrome Web Store product with its own version and update cadence. Browser-extension `0.1.x` and runtime `0.4.x` evolve independently; proximity in release time does not create a semver or updater coupling between them.

An extension-only change must **not** force a Rust runtime version bump. Maintainers may use `scripts/pack-extension.mjs` to build a deterministic Store-upload / explicit unpacked-UAT package, but that zip is not an end-user Runtime Release asset.

## Runtime Release (authoritative product version)

- **Workflow:** `.github/workflows/rust-release.yml`; `workflow_dispatch` is an **attested qualification** path, while only a `push` of a `v*` tag can publish.
- **Verify gate:** Rust fmt/clippy/test, `npm` build/test/edge, extension smoke, site build, `git diff --check`.
- **Build:** cross-target binaries per `.github/rust-release-targets.json`.
- **macOS signing:** paid Apple Developer / Developer ID signing is optional distribution hardening. It is not required for the open-source runtime, qualification, or stable publication path. `scripts/sign-macos-release.sh` remains available for downstream/private distributors that deliberately opt into it.
- **Manifest:** `scripts/build-rust-release-manifest.mjs` — lists the complete Runtime Release binary set; the browser extension is not a Runtime Release asset.
- **Attestation:** both manual qualification and tag-push bundles use the pinned `actions/attest` path.
- **Publish:** **tag push only** (`event=push` + `refs/tags/v*`). `workflow_dispatch` never creates or overwrites a GitHub Release. Prerelease tags remain GitHub prereleases; plain semver tags are stable (current published stable: `v0.4.1`).
- **Recovery:** `.github/workflows/rust-release-recover.yml` accepts only an attested **tag-push** source run whose SHA/ref/manifest/attestation all match the immutable tag; a manual qualification run cannot be recovered into a Release.

**User-facing version** = Runtime Release version (`herdr-mcp --version`, README, install docs). `package.json` remains site/extension build tooling — **not** the runtime product version (G1).

### Pre-tag macOS protected-folder qualification

Herdr-MCP must remain usable without a paid Apple Developer account. macOS protected-folder reliability therefore uses an unsigned/open-source path: rotating runtime generations must not own the long-lived TCC permission boundary. The supported design is a stable, minimal local broker whose identity is independent of `runtime/current`; a user may grant that fixed broker macOS Files & Folders / Full Disk Access once when their projects require protected locations such as `~/Documents`.

Before creating the version tag:

1. manually dispatch **Rust Release** on the exact source ref intended for release; require verify → build → manifest → attest → qualification PASS and confirm the `publish` job is skipped;
2. run the macOS candidate through the real launchd-managed path and repeat the protected-folder probe that previously failed: a linked worktree outside `~/Documents` whose common Git dir is inside `~/Documents`, plus `herdr_git status`, `herdr_fs_read`, `herdr_fs_patch dry_run`, and bounded exec;
3. where direct launchd access is denied by TCC, verify the stable broker path completes the same managed-root operation while preserving all existing path/secret/dirty/busy gates;
4. apply a new runtime generation and repeat the probes without replacing the stable broker. Runtime generation churn must not require a new macOS privacy grant;
5. retain the qualification evidence, then create the immutable `v*` tag pointing to the exact `release_identity.source_commit` recorded by the qualified manifest. The tag-push run independently rebuilds and attests the same source and is the only path allowed to publish;
6. after publication, perform the normal stable-channel `update apply` / rollback dogfood and confirm broker/runtime/Native Host/service invariants before declaring patch-line closure.

Developer ID signing and notarization may still be used by a distributor that already participates in the Apple Developer Program, but neither is a Herdr-MCP runtime requirement or release gate.

A workflow-dispatch artifact is qualification evidence only. Its manifest preserves the actual branch/ref and commit provenance; it is not discoverable by the updater and must never be presented as a published stable or preview Release.

### Update channels

| Channel | Discovers | Typical use |
| --- | --- | --- |
| `stable` | Tags with empty semver prerelease (current: `0.4.1`) | Default |
| `preview` | Prerelease tags (`0.4.0-rc.1`, `0.4.0-alpha.19`) | Maintainer rehearsal |

Installed generations are content-addressed (`rust-<sha256-prefix>`). Update applies a new generation and switches `runtime/current`; rollback reactivates a prior generation without rebuild.

## Browser Extension Release

- **Store packaging:** maintainers run `node scripts/pack-extension.mjs` only for Chrome Web Store upload or explicit unpacked UAT.
- **Runtime release boundary:** `.github/workflows/rust-release.yml` never attaches an extension zip.
- **Distribution:** end users install the extension from the Chrome Web Store. A locally packed zip is maintainer-only Store-upload / explicit unpacked-UAT input, not a Runtime GitHub Release asset.
- **Updater separation:** `update apply` updates the Rust runtime only. After a Store install, `native-host install` binds the official Store origin to the **active runtime**; explicit unpacked development remains a maintainer override.

### Extension vs runtime version matrix (current snapshot)

| Artifact | Version | Notes |
| --- | --- | --- |
| Runtime binary | `0.4.1` published / `0.4.2` source-ready | `0.4.2` does not require paid Apple signing; protected-folder qualification must pass through the unsigned stable-broker path before tagging |
| Browser extension source | `0.1.77` | Current development source; Chrome Web Store published/review version may lag and must be checked independently |
| Native Messaging host | Managed by runtime generation | `native-host status` must show `runtime_matches_current=true` |

## Contract Compatibility (shared public surface)

These must stay aligned across Edge, Link, and runtime for a given **contract epoch**:

- MCP tool catalog (GA: epoch **2**, **18 tools**)
- `state_schema` in release manifest (currently **4**)
- OAuth issuer / MCP endpoint behavior on public Edge
- Link `production_ready` seal and health `runtime=rust` when production Link is Rust

Edge Worker deploy is a **separate operator action** (`wrangler` / install docs §6). A Runtime Release does not auto-deploy Edge; conversely Edge deploy does not change the installed runtime generation.

## Ownership boundaries (see also `AGENTS.md`)

| Identity | Location | Never confuse with |
| --- | --- | --- |
| Source checkout | git worktree | active runtime |
| Build artifact | `target/*/herdr-mcp` | installed generation |
| Installed generation | `~/.config/herdr-mcp/runtime/generations/rust-*` | repo `target/` |
| Active runtime | `~/.config/herdr-mcp/runtime/current/herdr-mcp` | git `HEAD` |
| User CLI | `~/.local/bin/herdr-mcp` → `runtime/current` | repo `bin/herdr-mcp` |

## Alpha release retention policy

Historical prerelease tags **`v0.4.0-alpha.17` through `v0.4.0-alpha.19`** and **`v0.4.0-rc.1`** remain on GitHub for:

- preview-channel update rehearsal evidence (G9/G10)
- rollback baselines recorded in service state
- audit trail for GA closure

They were **superseded** by the first stable `v0.4.0`; the current runtime stable is `v0.4.1`. Do **not** delete historical GitHub Releases/tags as part of cleanup.

## Post-GA workflow recommendations (docs only — not implemented here)

| Item | Recommendation |
| --- | --- |
| Dedicated `extension-release.yml` | Optional later automation: tag `extension-v*` → deterministic Store-upload / CI artifact → Chrome Web Store submission **without** runtime bump; do not create a second end-user GitHub extension channel |
| Extension version in manifest metadata | Optional non-updater field for docs automation only |
| Store / CRX pipeline | See `docs/_wip/browser-extension-development-and-store-release.md` |
| Windows runtime target | Already published as `x86_64-pc-windows-msvc`; keep support claims conservative until G19 Windows end-to-end UAT seals |

## Related

- [`AGENTS.md`](../AGENTS.md) — binary/runtime hard rules
- [`ga-release-gate.md`](./ga-release-gate.md) — GA SSOT
- [`ga-candidate-status.md`](./ga-candidate-status.md) — live stable snapshot
- [`history/ga/exit-alpha-checklist.md`](./history/ga/exit-alpha-checklist.md) — G1 unification runbook (archived)
- [`i18n/en/install.md`](./i18n/en/install.md) — user install path
