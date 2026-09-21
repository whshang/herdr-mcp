# Browser extension development and Store release

> Status: **active 1.0 release plan**
>
> Updated: 2026-09-19.
>
> This file contains only the current DEV / STANDALONE / STORE release boundary. Historical rollout notes for the 0.1.7x/0.1.9x candidate series were moved intact to [the historical Store rollout record](../history/architecture/browser-extension-development-and-store-release-20260829.md).

## Current authority

The browser extension is an independent release plane from the Rust Runtime, Cloudflare Edge, and ChatGPT Workspace App action snapshot.

Machine-readable identity authority:

- STORE: `contracts/browser-extension-store.json`
- STANDALONE: `contracts/browser-extension-standalone.json`
- DEV: path-derived source-development identity

User-facing behavior belongs in the maintained locale docs:

- [English extension guide](../i18n/en/extension.md)
- [简体中文扩展指南](../i18n/zh-CN/extension.md)
- [日本語拡張ガイド](../i18n/ja/extension.md)
- Browser Continuity and Browser Control Center in the same locale trees
- privacy policy in the same locale trees

This WIP owns packaging, channel qualification, Store submission, and release evidence only. It must not become a second product-behavior SSOT.

## Current source state

At the current `main` baseline used to open this closeout lane:

- extension manifest version: **0.1.101**;
- Runtime package version: **1.0.0-alpha.9**;
- final stable 1.0 source is **not frozen yet**;
- open extension-changing work may advance the manifest version before the final package is qualified.

The semantic-judgment policy is now on `main` at extension 0.1.103. The provider-cleanup closeout lane advances its exact candidate to 0.1.113 for supported Grok optional-site access, direct Chrome-permission authority, provider-scoped bounded recovery, historical-turn Grok settlement recovery, endpoint-affine browser actuation when multiple Extension hosts are online, and final Claude/Grok signed-in qualification. Treat 0.1.113 as a candidate until that lane is merged. Any later extension-changing PR has the same rule.

The successful package workflow run `35325140968` qualified the 0.1.101 package at source `4e28c285`. That is valid historical evidence for that source, but main has advanced, so it is not the final stable-1.0 package identity.

Never publish or document a historical candidate such as 0.1.75, 0.1.76, 0.1.89, 0.1.90, 0.1.91, or 0.1.101 as the final 1.0 Store build merely because an older package or Store submission exists.

## Distribution channels

### STORE

Ordinary-user distribution uses the fixed Chrome Web Store identity.

A Store release is complete only when:

1. final source is frozen;
2. the exact Store ZIP is produced by `.github/workflows/extension-store.yml`;
3. package source identity and SHA-256 evidence are recorded;
4. the exact package is uploaded/published through the Store release process;
5. the installed Store item is verified under the Store extension ID;
6. Native Messaging owner/origin matches STORE;
7. supported provider pages are refreshed and real page smoke passes.

A successful workflow, an unpacked package, an older Unlisted Store item, or source-level tests alone do not satisfy the Store-install gate.

### STANDALONE

STANDALONE is the fixed non-Store identity used for independent/manual distribution and release UAT.

Canonical preparation:

```bash
herdr-mcp extension standalone install --ref <exact-release-commit>
herdr-mcp extension standalone status
herdr-mcp native-host use standalone
herdr-mcp native-host status
herdr-mcp doctor
```

The exact loaded Chrome path must match `standalone status.chrome.load_unpacked_path`. Loading a Store ZIP manually or loading an arbitrary repo path under the fixed STANDALONE identity is invalid.

### DEV

DEV is for source development only. Its identity is path-derived and it is not a substitute for STORE/STANDALONE release acceptance.

Use the supported Native Host DEV lifecycle for the actual checkout being tested. After extension source changes, explicitly reload the unpacked extension and refresh provider pages before claiming browser evidence.

## Final 1.0 extension gate

Before stable 1.0 publication, all of the following must be true on the exact final source:

- extension manifest/version is intentional and unique for the candidate;
- extension Store workflow passes;
- Store ZIP and Standalone ZIP/sidecar hashes are retained as release evidence;
- fixed STORE and STANDALONE IDs match their contract files;
- manifest permissions and optional-host behavior pass the maintained privacy/security tests;
- Native Messaging status reports the intended channel and `runtime_matches_current=true`;
- Control Center, HUD, binding, manual continue/handoff, queue, archive/close, and the supported provider adapters pass the applicable source-level smoke/regression gates;
- real provider-page acceptance required by issue #399 is not replaced by mocks;
- Store-ID + Native Messaging smoke is performed with the actual Store-installed build before the Store plane is marked complete;
- old pages are refreshed after extension updates so content-script evidence comes from the candidate being qualified.

If final source changes after package qualification, rerun the package gate. Do not carry forward an old package's source marker as evidence for the new source.

## Release ordering

For stable 1.0 closeout:

1. finish and merge approved source changes;
2. freeze the final source commit;
3. run Runtime/Edge qualification required by the release model;
4. run the exact-source extension Store/Standalone package workflow;
5. perform required real-browser acceptance;
6. publish the chosen Store build only after its exact identity/evidence is known;
7. record final evidence in the release closeout issue/notes;
8. move completed WIP evidence into `docs/history/` and leave only reusable user guidance in `docs/i18n/`.

Runtime tag/Release and Chrome Web Store publication remain independent actions. Neither implies the other.

## Cleanup rules

After a successful extension release:

- remove superseded local unpacked candidate directories owned by the release task;
- retire merged temporary branches/worktrees after reachability proof;
- keep immutable release artifacts, hashes, Store identity contracts, and release notes;
- keep historical rollout evidence under `docs/history/`;
- do not delete compatibility fixtures or migration comments merely because their version number is old;
- do not leave a dated candidate described as “current” in maintained user docs or active WIP.
