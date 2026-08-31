# Release model — runtime, extension, and contract compatibility

This document is the long-lived contributor SSOT for Herdr release boundaries. It defines what is released, how each release plane is versioned, and what must remain compatible. Version-specific qualification records and UAT evidence do not belong here.

## Release planes

| Plane | Ships | Version source | Update path |
| --- | --- | --- | --- |
| Runtime | `herdr-mcp` native binaries, release manifest and checksums | `crates/herdr-mcp/Cargo.toml` + `v*` Git tag | `herdr-mcp update check/apply` |
| Browser extension | STORE and STANDALONE packages; DEV is source-only | `extension/manifest.json` plus channel-specific packaging identity | Store update or standalone distribution |
| Contract compatibility | MCP epoch/tool catalog, state schema, Edge OAuth/MCP behavior | source contracts + release manifest fields | coordinated compatibility, not a shared package version |

Runtime and browser-extension semver are independent. An extension-only change does not require a runtime version bump, and runtime GitHub Releases do not silently own or bundle the browser-extension lifecycle.

## Runtime publication

- `.github/workflows/rust-release.yml` is the runtime release workflow.
- `workflow_dispatch` is qualification only; it must not publish or overwrite a GitHub Release.
- only an immutable `v*` tag push may publish a Runtime Release.
- release manifests describe the runtime binary set and compatibility metadata.
- stable releases use plain semantic versions; prereleases use normal semver prerelease identifiers.
- installed generations are content-addressed; rollback reactivates an existing installed generation rather than rebuilding source.

### Retention

Stable Releases and stable Git tags are retained long term. After the corresponding stable release is published, superseded prerelease Releases and tags may be removed unless an active compatibility or rollback requirement explicitly depends on them. Historical qualification evidence should not be retained merely to justify keeping obsolete prerelease distribution artifacts.

## Browser extension distribution

Herdr uses three extension identities:

- **STORE** — ordinary-user default, installed and updated through the browser store.
- **STANDALONE** — fixed non-Store identity for auditable manual/GitHub distribution.
- **DEV** — developer-mode source checkout; path-derived and never treated as a release channel.

Native Messaging ownership must resolve to one supported extension owner at a time. Packaging for STORE or STANDALONE must not mutate the DEV source manifest into a fixed release identity. Runtime self-update does not update the extension.

## Contract compatibility

The runtime, Link and Edge surfaces must agree on the active contract epoch and compatibility-sensitive fields, including:

- MCP tool catalog / contract epoch;
- runtime state schema;
- OAuth issuer and public MCP endpoint behavior;
- Link production readiness and runtime ownership invariants.

An Edge deployment is an operator action independent of Runtime Release publication. A runtime update does not imply an Edge deploy, and an Edge deploy does not switch the local runtime generation.

## Ownership boundaries

| Identity | Location | Do not confuse with |
| --- | --- | --- |
| Source checkout | Git worktree | active runtime |
| Build artifact | `target/*/herdr-mcp` | installed generation |
| Installed generation | `~/.config/herdr-mcp/runtime/generations/rust-*` | repository build output |
| Active runtime | `~/.config/herdr-mcp/runtime/current/herdr-mcp` | Git `HEAD` |
| User CLI | `~/.local/bin/herdr-mcp` → `runtime/current` | repository wrapper scripts |

## Release discipline

For each stable runtime release:

1. qualify the exact final source without publishing;
2. create an immutable stable tag pointing to that exact qualified source;
3. let the tag-push workflow rebuild, attest and publish;
4. verify stable-channel update, service ownership, Native Messaging and rollback invariants;
5. remove superseded prerelease distribution artifacts once they no longer serve an active compatibility requirement.

Architecture and operating rules live in `docs/i18n/*/architecture.md`, `docs/herdr-architecture-roadmap.md`, browser-extension architecture docs, and `AGENTS.md`. Active implementation plans live in `docs/_wip/`; completed execution evidence is intentionally not kept as normal product documentation.
