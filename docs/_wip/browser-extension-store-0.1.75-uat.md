# Browser extension 0.1.75 Store and long-chat UAT

Status: active release-candidate evidence  
Date: 2026-08-29 (UTC+8)

## Candidate

- Extension product name: `Herdr — AI Workspace Bridge`
- Manifest version: `0.1.75`
- Chrome Web Store extension id: `kpcengcaammanfnbclapecdgahdmhanp`
- Store item: `https://chromewebstore.google.com/detail/kpcengcaammanfnbclapecdgahdmhanp`
- Candidate package SHA-256: `5b3908961a005c26edc6e0e85849d456e73a5f87ea9269f6332b2cf30edbfda3`

## Store visibility baseline

Before uploading the 0.1.74 predecessor, the Developer Dashboard still showed the previous `0.1.70` package with status `Draft`. Distribution was configured as `Public` / `All regions`, but the item had not been published.

Real Chrome Web Store checks in the logged-in Chrome profile therefore produced:

- search `herdr`: `No search results`;
- search `herdr-mcp`: `No search results`;
- direct item id: `This item is not available`.

This is a publication-state blocker, not evidence of a Native Messaging failure or search-index delay. A real Store-install → Native Messaging UAT must use an installable Store build after the Dashboard accepts/publishes the candidate, preferably through a trusted-tester/private path before public release when available.

## Store host-permission UAT

The first real 0.1.74 submission preflight exposed **Publishing will be delayed → Broad Host Permissions** because the manifest carried `<all_urls>` as an always-on host permission. The submission was deliberately cancelled rather than accepted blindly.

0.1.75 removes always-on `<all_urls>` and narrows required host access to the local Herdr endpoint plus the documented ChatGPT/Claude surfaces. z.ai/DeepSeek and a user-configured OpenAI-compatible endpoint use `optional_host_permissions`; Options asks only for the exact origin after an explicit user action. The rebuilt 0.1.75 package was accepted by the Developer Dashboard and the Draft version was verified as `0.1.75`.

A second real Store preflight still reports **Broad Host Permissions** because supporting an arbitrary user-configured OpenAI-compatible endpoint requires broad optional `http://*/*` / `https://*/*` declarations even though runtime grants are exact-origin. Removing that warning completely would require either constraining supported LLM providers or introducing a new native/runtime external-proxy protocol. The latter would make the Store build depend on a runtime newer than current 0.4.1 and would couple this release line to concurrent 0.4.2 Rust work, so the current candidate keeps the optional capability and accepts Chrome's in-depth review.

The item remains **Unlisted** during review, so it is not discoverable in Chrome Web Store listing/search; the direct item URL is the only intended pre-public installation path. After the synchronized privacy justification was saved, the 0.1.75 draft was submitted. The Developer Dashboard confirmed **Status: Pending review**, showed **Your extension was submitted for review**, and emitted **Item submitted.** Public visibility was not enabled.

## Experimental site boundary

0.1.75 keeps `chat.z.ai` and `chat.deepseek.com` as explicit experimental integrations and tightens their host-access boundary:

- both default OFF;
- each has its own switch in **Herdr Settings → Experimental features**;
- when OFF, the site content script is not statically declared in `manifest.json`;
- enabling an experimental site requests Chrome access to that exact site; only after the permission is granted does the service worker dynamically register its packaged content scripts; disabling it unregisters the scripts and removes stale permission where applicable;
- background JSON→MCP, automation registration, and z.ai handoff paths fail closed with `experimental-site-disabled` while the site is OFF;
- already-open pages must be reloaded after changing a switch so their injected-script lifecycle matches the saved setting.

## Local connection guidance

The Options diagnostics path now links failed local-runtime self-tests to the Herdr GitHub beginner setup guide. The link is localized for English and Simplified Chinese; Japanese falls back to the Japanese README entry path.

## Historical long-chat performance UAT

Conversation used:

`https://chatgpt.com/g/g-p-6a89c078669481918c8eb70fdfd3d978-herdr-mcp/c/6a8d570f-b4d8-83ea-8971-43d634178d53`

The existing historical conversation was lengthened with eight pure-text UAT turns. Every prompt explicitly prohibited tool use, external access, and file/project mutation. No code was changed through the conversation. Approximate settle times were 14s, 30s, 26s, 30s, 26s, 22s, 21s, and 27s.

### Steady-state baseline and growth

| Sample | DOM elements | Visible turn wrappers | JS heap (`performance.memory`) | CDP nodes | JS event listeners | 100ms timer drift avg/max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Historical-chat load | ~4,590 | ~5 | ~232.7 MB | 20,027 | 1,402 | ~3.24 / 4.8 ms |
| After 4 added turns, bottom | 1,342 | 5 | ~214.7 MB | 11,309 | 1,320 | ~1.74 / 2.6 ms |
| After 8 added turns, bottom | 1,338 | 5 | ~221.4 MB | 11,375 | 1,383 | ~1.88 / 2.8 ms |

Observed result: adding eight substantial turns did not produce linear DOM, listener, heap, or event-loop growth. ChatGPT kept only a small active turn window mounted at the bottom.

### History-scroll stress

Explicitly scrolling to the old-history top temporarily materialized more history:

- DOM elements: 1,889;
- JS heap: ~284.0 MB;
- CDP nodes: 15,890;
- listeners: 1,976.

Immediately returning to the bottom restored the active DOM to roughly 1,348 elements, although V8 heap reclamation lagged and briefly reached ~305.6 MB. After 30 seconds quiescent at the bottom:

- DOM elements: 1,348;
- visible turns: 5;
- JS heap: ~229.9 MB;
- CDP heap: ~200.8 MB;
- CDP nodes: 11,325;
- listeners: 1,337;
- timer drift: ~1.68 ms average / 2.5 ms max.

The same page navigation/time origin remained in place through the test; no reload loop occurred and no automatic resubmission of the UAT prompts was observed.

## Performance conclusion

The tested long-chat path is practically bounded rather than linearly growing. The main transient pressure comes from intentionally revisiting old virtualized history; returning to the bottom and remaining quiescent allows DOM/listener counts and JS heap to recover near the steady-state baseline.

The extension should therefore continue avoiding invasive deletion of ChatGPT-owned DOM. The current strategy remains appropriate: bounded extension state, coalesced observers, hidden-page suspension, 429 backoff, evidence-first recovery, reload cooldown, and safe handoff/rollover when pressure is sustained.

## Automated regression evidence

After the experimental-site and Options changes, the targeted extension suites passed:

- `tests/options-i18n.test.mjs`;
- `tests/manual/extension_smoke.mjs`;
- `tests/manual/background_bind_test.mjs`;
- `tests/browser-control-plane.test.mjs`;
- `tests/extension-recovery.test.mjs`;
- `tests/extension-native-host.test.mjs`;
- `tests/pack-extension.test.mjs`;
- `tests/browser-extension-store-contract.test.mjs`.

A full TypeScript/npm gate was not used as positive evidence in this worktree because the local TypeScript process entered an idle 0%-CPU hang when using the shared dependency tree. CI on the PR remains the authoritative clean-build gate.

## Remaining real Store UAT gate

1. Submit the accepted 0.1.75 Unlisted draft for Chrome Web Store in-depth review with the synchronized listing/privacy justification.
2. After the Unlisted review passes, use the direct Store item URL for the real Store-hosted installation UAT.
3. Install the extension from the actual Chrome Web Store origin, not unpacked developer mode.
4. Run `herdr-mcp native-host install` and `herdr-mcp native-host status`; verify the frozen Store origin/id is accepted.
5. Open the installed Store extension Control Center and verify local runtime/workspace state, explicit binding, and one harmless trusted local action.
6. Only after a public listing is actually live, repeat Store searches for `herdr` and `herdr-mcp` and record discovery/indexing behavior.
