# Release model — Runtime, Extension, and Contract Compatibility

Status: **current runtime stable `v0.4.2` published** (2026-08-31). First-GA `v0.4.0` evidence remains historical and immutable. This file is the contributor-facing definition of how releases are sliced. GA gate status lives in [`ga-release-gate.md`](./ga-release-gate.md).

## Three release planes (independent lifecycles)

| Plane | What ships | Version source | Consumer update path | Bound to runtime tag? |
| --- | --- | --- | --- | --- |
| **Runtime Release** | `herdr-mcp` Rust binary, `release-manifest.json`, platform SHA256 sidecars | `crates/herdr-mcp/Cargo.toml` → Git tag `v*` | `herdr-mcp update check/apply` on `stable` or `preview` channel | **Yes** — manifest drives updater |
| **Browser Extension Release** | STORE item and, from v0.4.3+, fixed-identity STANDALONE package | `extension/manifest.json` `version` | Store update or independent standalone package update; Native Host selects one supported extension owner | **No** — independent extension lifecycle |
| **Contract Compatibility** | MCP epoch, tool catalog, state schema, Edge OAuth/MCP surface | Code + release manifest fields | Edge deploy + Link + runtime generation together | **Coupled by epoch**, not by zip filename |

**Critical rule:** Runtime GitHub Releases do **not** own or silently bundle the browser-extension lifecycle. Browser-extension `0.1.x` and runtime `0.4.x` evolve independently; proximity in release time does not create a semver or updater coupling between them. STORE is the default ordinary-user distribution. v0.4.3 adds a separate fixed-identity STANDALONE distribution path for GitHub/manual installation; DEV remains source-development only and is not a release channel.

An extension-only change must **not** force a Rust runtime version bump. The current v0.4.2 packer produces the Store-upload / explicit unpacked-UAT package. The v0.4.3 standalone work extends that packaging surface with an explicit fixed-identity standalone mode without adding a fixed key to the DEV source manifest. Neither package becomes a Rust Runtime Release asset.

## Runtime Release (authoritative product version)

- **Workflow:** `.github/workflows/rust-release.yml`; `workflow_dispatch` is an **attested qualification** path, while only a `push` of a `v*` tag can publish.
- **Verify gate:** Rust fmt/clippy/test, `npm` build/test/edge, extension smoke, site build, `git diff --check`.
- **Build:** cross-target binaries per `.github/rust-release-targets.json`.
- **macOS distribution signing:** optional. The open-source runtime and stable release path do not require a paid Apple Developer account; `scripts/sign-macos-release.sh` remains available for distributors that opt into signing/notarization.
- **Manifest:** `scripts/build-rust-release-manifest.mjs` — lists the complete Runtime Release binary set; the browser extension is not a Runtime Release asset.
- **Attestation:** both manual qualification and tag-push bundles use the pinned `actions/attest` path.
- **Publish:** **tag push only** (`event=push` + `refs/tags/v*`). `workflow_dispatch` never creates or overwrites a GitHub Release. Prerelease tags remain GitHub prereleases; plain semver tags are stable (current published stable: `v0.4.2`).
- **Recovery:** `.github/workflows/rust-release-recover.yml` accepts only an attested **tag-push** source run whose SHA/ref/manifest/attestation all match the immutable tag; a manual qualification run cannot be recovered into a Release.

**User-facing version** = Runtime Release version (`herdr-mcp --version`, README, install docs). `package.json` remains site/extension build tooling — **not** the runtime product version (G1).

### macOS permissions broker (completed)

macOS protected-folder access uses a stable local TCC broker whose identity is independent of `runtime/current`. When a project lives in a protected location such as `~/Documents`, the user grants that fixed broker the required macOS privacy access once; rotating runtime generations reuse it. Cross-generation authorization reuse has been verified. Apple Developer ID signing and notarization remain **optional hardening**, not a v0.4.2 release gate.

### Stable release gate

`v0.4.2` is tagged and published as the current stable runtime; its pre-tag gate is closed. For subsequent stable releases, preserve the same ordering:

1. manually dispatch **Rust Release** on the **exact final source** intended for release; require verify → build → manifest → attest → qualification PASS and confirm the `publish` job is skipped;
2. retain the qualification evidence, then create the immutable `v*` tag pointing to the exact `release_identity.source_commit` recorded by the qualified manifest. The tag-push run independently rebuilds and attests the same source and is the only path allowed to publish;
3. after publication, perform the normal stable-channel `update apply` / rollback dogfood and confirm broker/runtime/Native Host/service invariants before declaring patch-line closure.

Closed for `v0.4.2`: generic Artifact Relay landed via PR #204; overlapping PR #199 was closed without merge, pane-session PR #200 is merged, `continuity.search` is integrated via PR #202, and the final production Artifact Relay/R2 deploy-upload → exact-source Rust import → `herdr_fs_image` read-back UAT passed on 2026-08-31. Evidence: [`history/v0.4.2/v0.4.2-artifact-relay-production-uat-20260831.md`](./history/v0.4.2/v0.4.2-artifact-relay-production-uat-20260831.md). These are no longer pre-tag TODOs.

A workflow-dispatch artifact is qualification evidence only. Its manifest preserves the actual branch/ref and commit provenance; it is not discoverable by the updater and must never be presented as a published stable or preview Release.

### Update channels

| Channel | Discovers | Typical use |
| --- | --- | --- |
| `stable` | Tags with empty semver prerelease (current: `0.4.2`) | Default |
| `preview` | Prerelease tags (`0.4.0-rc.1`, `0.4.0-alpha.19`) | Maintainer rehearsal |

Installed generations are content-addressed (`rust-<sha256-prefix>`). Update applies a new generation and switches `runtime/current`; rollback reactivates a prior generation without rebuild.

## Browser Extension Release

- **Three identities:** STORE = ordinary-user default; STANDALONE = v0.4.3+ fixed non-Store identity for independent/GitHub distribution; DEV = path-derived source-development identity.
- **Store packaging:** current maintainers can run `node scripts/pack-extension.mjs` for Chrome Web Store upload or explicit unpacked UAT.
- **Standalone packaging (v0.4.3 plan):** add an explicit deterministic mode that injects the public standalone manifest key into the packaged manifest without mutating `extension/manifest.json`; the resulting package has one stable non-Store Chromium ID on every machine/path.
- **Runtime release boundary:** `.github/workflows/rust-release.yml` never attaches an extension zip.
- **Distribution:** STORE remains the Chrome Web Store path. STANDALONE is published through an independently auditable extension release surface/manual GitHub artifact rather than being smuggled into the runtime manifest. DEV is never presented as the standalone fallback.
- **Updater separation:** `update apply` updates the Rust runtime only. Native Host ownership is `store | standalone | dev` only on runtimes that implement the three-channel contract; v0.4.2 remains Store/DEV and must not be described as standalone-capable.
- **Identity SSOT:** Store identity remains `contracts/browser-extension-store.json`; v0.4.3 standalone identity is `contracts/browser-extension-standalone.json`; the DEV source manifest stays unkeyed/path-derived.

### Extension vs runtime version matrix (current snapshot)

| Artifact | Version | Notes |
| --- | --- | --- |
| Runtime binary | `0.4.2` published stable | `v0.4.2` is tagged and published. Stable TCC broker cross-generation authorization and the final production Artifact Relay/R2 UAT are verified; Developer ID remains optional hardening |
| Browser extension source | `0.1.82` | Current development source; Chrome Web Store published/review version may lag and must be checked independently |
| Native Messaging host | Managed by runtime generation | `native-host status` must show `runtime_matches_current=true` |

## Contract Compatibility (shared public surface)

These must stay aligned across Edge, Link, and runtime for a given **contract epoch**:

- MCP tool catalog (GA: epoch **2**, **18 tools**)
- `state_schema` in release manifest (currently **5**)
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

They were **superseded** by the first stable `v0.4.0`; the current runtime stable is `v0.4.2`. Do **not** delete historical GitHub Releases/tags as part of cleanup.

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
