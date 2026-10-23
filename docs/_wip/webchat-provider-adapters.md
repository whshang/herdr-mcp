# WebChat provider adapters — diagnosis and decoupling plan

> Status: DRAFT / diagnosis only — no implementation in this PR
>
> Date: 2026-10-23
>
> Author: pi (`pi@agent`)
>
> Base: `origin/main` @ `2cec2c49`
>
> Authority: [`v1.1-browser-adapter-platform.md`](v1.1-browser-adapter-platform.md) §1, §3.1;
> [`../releases/1.1-feature-list.md`](../releases/1.1-feature-list.md) §2, §4 (gate 8, gate 10)
>
> Related open work: PR [#666](https://github.com/whshang/herdr-mcp/pull/666) (browser
> security/dispatch qualification), issues
> [#623](https://github.com/whshang/herdr-mcp/issues/623) (archive without tab),
> [#624](https://github.com/whshang/herdr-mcp/issues/624) (title projection),
> [#686](https://github.com/whshang/herdr-mcp/pull/686) (1.1 feature list).

## 0. Verdict

ChatGPT, Claude and Grok are implemented **twice**: once as `*Adapter` classes inside the
extension, and again as provider-string branches in the Rust runtime. The browser-side
adapter seam exists but is narrower than it looks: the page installs **one** adapter
singleton (`window.__H2W_ADAPTER__`, `extension/content/wake.js:58`) and the shared engine
is ChatGPT-shaped — dozens of `ADAPTER.name === "chatgpt"` branches in `wake.js` for
submit, settlement, queueing, artifacts, archive, HUD and recovery. On the other side,
Bilibili / X / Doubao are **not** registered adapters either: they are bespoke
`if (operation === "herdr_mcp.…")` handlers in `extension/background.js:3693`, `:3716`,
`:3750`, `:3784` that happen to operate on BrowserPage records.

The decoupling is therefore mostly **deletion and re-pointing**, not new machinery: the
runtime's reservation/idempotency/delivery/grant authority is already provider-neutral
(§1.4), and the target model is the one already written down in the platform doc:

> "strengthen ChatGPT / Grok / Claude control **without creating a second WebChat state
> machine**" — `docs/_wip/v1.1-browser-adapter-platform.md:17`

> "An Adapter is never a second authority. The extension executes one typed browser
> operation and returns evidence; it does not interpret Skill prose, own retries, or keep a
> second wait/delivery state machine." — `docs/_wip/v1.1-browser-adapter-platform.md:173`

## 1. Current hardcoding inventory

### 1.1 Extension (browser side)

Adapter-shaped, but a single-adapter singleton rather than a registry:

| Surface | Evidence |
| --- | --- |
| Adapter base contract (capabilities + policy + per-site declarations) | `extension/content/base.js:1-50` (`BaseAdapter`, `supportsCapability`, `getConversationKey`, `getInputEl`) |
| Registration | `extension/content/injector/chatgpt.js:470`, `claude.js:367`, `grok.js:332` (`registerH2WAdapter(new …Adapter())`) |
| One adapter per page | `extension/content/wake.js:58` (`const ADAPTER = window.__H2W_ADAPTER__`) |
| Declared capabilities | `chatgpt.js:5-19` (`browserActuation`, `stopGeneration`, `sessionCreate`, `sessionOpen`, `titleProjection`, `chatModeGuard`), `claude.js:4`, `grok.js:4` |
| Shared engine is ChatGPT-shaped | `extension/content/wake.js:488`, `:1041-1065`, `:1172`, `:1196`, `:1333-1392`, `:2533-2637`, `:2625-2720`, `:2962`, `:3191`, `:3624-3661`, `:5411`, `:6375` (dozens of `ADAPTER.name === "chatgpt"` branches) |

Bilibili / X / Doubao are **not** adapters; they are bespoke handlers over BrowserPage
records in the background worker:

| Site | Evidence |
| --- | --- |
| Bilibili transcript | `extension/background.js:3693`, `:10437-10613` (`bilibili_*` error vocabulary, `/x/player/wbi/v2`, subtitle host checks) |
| Doubao image generate/status | `extension/background.js:3716`, `:3750`, `:9515-10055` (`doubao_*` delivery/artifact vocabulary) |
| X search/thread read | `extension/background.js:3784` |

So today there are two extension-side patterns and neither is the target: provider
injectors (single adapter, ChatGPT-biased engine) and bespoke site RPC handlers.

Remaining registry/background branches on provider name:

| Coupling | Evidence |
| --- | --- |
| Only ChatGPT + Claude are "core"; Grok is a second-class "dynamic" site | `extension/background.js:65` (`CORE_TAB_URLS`), `:76-77` (`SUPPORTED_DYNAMIC_TAB_URLS`, `SUPPORTED_SITE_PERMISSION_PATTERNS`) |
| Per-provider content-script list | `extension/background.js:121-125` (grok entry), `extension/manifest.json:47-55`, `:59-86` |
| provider → origin pattern switch | `extension/background.js:321-325` |
| Site derivation special-cases ChatGPT | `extension/background.js:450-452` |
| Project/launch URL builders branch per site | `extension/background.js:776-786`, `:790-823` |
| Conversation-URL parsers duplicated per provider | `extension/background.js:1894-1912` (`claudeConversationInfo`, `grokConversationInfo`), `:1947-1950` |
| ChatGPT-only dispatch path | `extension/background.js:3011` (`if (site === "chatgpt") return sendChatGptTabMessage(...)`) |
| Continuity/handoff limited to ChatGPT | `extension/background.js:7896-7902`, `:11009` |
| ChatGPT-only artifacts/perf/migration machinery | `extension/content/chatgpt-artifact-core.js:161` (`/backend-api/estuary/content`), `extension/content/chatgpt-perf-main.js`, `extension/background.js:64`, `:989-1040`, `:2392`, `extension/continuity-core.js:14` |

### 1.2 Rust runtime

The MCP method catalog is already provider-neutral — the coupling is in the handlers:

`crates/herdr-mcp/src/progressive_skills.rs:52-65`
(`browser_session.create|open|archive|archive_status|inspect`,
`browser_message.append`, `browser_dispatch.submit|status|stop`,
`browser_composer.set_reasoning|set_apps`, `browser_page.lifecycle|action|fast_path`).

Two facts keep the Rust side cheap to decouple:

- there is **no provider enum, selector table or per-provider config**: `provider` is an
  opaque bounded string (`state_store.rs:3093`, `:5569`), and the literal `"chatgpt"` is
  the only hardcoded provider, used as a fail-closed gate in roughly ten places;
- webchat operations already ride **one generic adapter-command surface** —
  `BrowserActuator` → `BrowserActuationBroker` → SSE envelope `herdr-browser-actuation/v1`
  → extension `handleBrowserActuation` — with provider-specific work in the extension.

So the Rust-side work is to delete the `"chatgpt"` gates, not to introduce an adapter
transport.

| Coupling | Evidence |
| --- | --- |
| Provisional session identity rule only for `chatgpt` | `crates/herdr-mcp/src/mcp_http.rs:954-960` |
| `source_turn.observe` / `archive.claim` require `https://chatgpt.com/` | `mcp_http.rs:1069`, `:1107` |
| Source session must be `chatgpt` | `mcp_http.rs:1083` |
| Provider detected by name/openai substring | `mcp_http.rs:2817` |
| Opaque connector session only for `chatgpt` | `mcp_http.rs:3031`, `crates/herdr-mcp/src/link/local_mcp.rs:480` |
| Task parent binding only for `chatgpt` sessions | `crates/herdr-mcp/src/mcp.rs:265`, `:284` |
| ChatGPT composer rewrites conversation URLs to ids | `mcp.rs:826-848` |
| Per-host conversation URL parsing (chatgpt.com / claude.ai / grok.com) — a second copy of the extension parsers | `mcp.rs:884-928` |
| Continuity resume only for `chatgpt` | `mcp.rs:1216` |
| Claude-only diagnostic counters | `mcp.rs:2723` |
| Session-URL lookup pinned to `r.provider = 'chatgpt'` | `crates/herdr-mcp/src/state_store.rs:3437` |
| Session ops assert `kind`/`provider` constants | `state_store.rs:3740` |
| Alpha-4 support matrix requires provider `chatgpt` for create | `mcp.rs:5800-5821` |
| `session.open` / `session.archive` / `archive_status` chatgpt-only gates | `mcp.rs:5967-5974`, `:6089`, `:6710-6713` |
| Archive idempotency asymmetry: `session.open` uses the generic `operations` ledger, direct `session.archive` rides only the actuation envelope; the durable `browser_session_archive_intents` ledger covers deferred self-archive only | `state_store.rs:882/954`, `mcp.rs:5172` |
| Grant check keyed on `(endpoint_ref, provider)` — neutral shape, provider string is the join key | `mcp.rs:306-308`, `:5148-5150`, `:6863-6865` |

### 1.3 Edge / link grants

| Coupling | Evidence |
| --- | --- |
| WebChat control grant header is `{endpoint_ref, provider, account_ref}` | `crates/herdr-mcp/src/link/local_mcp.rs:338-350` (`webchat_control_grants_header`), `:834-836` |
| Trusted edge grants admission | `mcp_http.rs:2854` (`trusted_edge_webchat_control_grants`) |
| Grant migration / legacy records | `crates/herdr-mcp/src/grant_migration.rs`, `docs/_wip/626-grant-migration-design.md` |
| Wildcard grant already supported (forward-compatible for new adapter ids) | `link/local_mcp.rs:1943-1961` |

The grant shape survives the migration unchanged if `provider` is redefined as
`adapter_id`: the wildcard path already proves the runtime does not need to know the
provider set in advance.

### 1.4 What is already provider-neutral (and must stay that way)

- One reservation/idempotency/delivery authority and mutation ledger (`state_store.rs`,
  `mcp.rs` `BrowserOperation` dispatch at `:6058-6105`).
- Page lifecycle, generation/ref fencing, owned/claimed tab rules
  (`extension/content/browser-page-kernel.js`, `page-assist-core.js`).
- Grant/capability/generation checks (`mcp.rs:306-308`) and the method catalog.
- Artifact capture/import and the operations ledger.

This is why the migration is a re-pointing exercise: the authority already exists in one
place, and the adapters are supposed to be thin.

## 2. Target model

One BrowserPage authority; provider behaviour becomes adapter-declared commands:

```text
MCP caller
  -> Runtime method (unchanged names: browser_session.create, browser_dispatch.submit,
     browser_dispatch.stop, browser_session.archive, …)
  -> adapter registry lookup by adapter_id (chatgpt | claude | grok | bilibili | x | doubao)
  -> typed command with declared capabilities  (session.create/send/stop/archive/status)
  -> BrowserPage kernel executes; extension adapter maps DOM -> evidence
  -> Runtime settles delivery from evidence (one ledger, one state machine)
```

Rules:

1. **Adapter identity, not provider prose.** `adapter_id` + origin allowlist +
   capability set + URL canonicalizer + digest, reviewed like a Skill. Unknown adapter
   fails closed with one stable code (`adapter_unsupported`).
2. **No second state machine.** `session.create/send/stop/archive/settle` remain
   *commands* over BrowserPage; they reuse the existing reservation, idempotency,
   `applied|uncertain|not_applied` delivery, generation fence and human-boundary policy
   (`v1.1-browser-adapter-platform.md:173`, `:819`, `:887-889`).
3. **Canonicalization lives with the adapter, once.** Today the same URL parsing exists in
   both `extension/background.js:1894-1912` and `crates/herdr-mcp/src/mcp.rs:884-928`;
   the adapter manifest should be the single declaration, with the runtime validating
   rather than re-deriving.
4. **Thin control plane stays in Runtime**: grants, idempotency, delivery states, archive
   ledger, artifact cache, continuity/work-memory. Adapters never own retries or
   settlement.
5. **Compatibility is a naming shim, not a second path.** `provider` remains accepted as
   an alias of `adapter_id`; no method is renamed or removed
   (`v1.1-browser-adapter-platform.md:889`).

## 3. Migration plan

| Phase | Scope | Exit criterion |
| --- | --- | --- |
| 0 — Registry seam | Adapter manifest table in Runtime + `provider → adapter_id` resolver; session ops consult it, behaviour identical | Existing session tests and UAT codes unchanged |
| 1 — ChatGPT reference | Move `chatgpt.js` URL/DOM declarations into the manifest; delete `mcp_http.rs:954-960/1069/1083/1107` and `mcp.rs:826-848` special cases in favour of generic adapter validation; keep `chatgpt-artifact-core.js` behaviour behind a capability flag | Full ChatGPT create → dispatch → settle → archive path still green; no host literal left in Rust outside the manifest |
| 2 — Claude | Register Claude in the same manifest; delete the duplicated parser branch `mcp.rs:903-908` and the Claude-only counters (`mcp.rs:2723`) once ported to capability facts | Claude stop + create pass; grant and delivery codes unchanged |
| 3 — Grok | Promote Grok out of "dynamic site" (`background.js:76-77`) into a first-class adapter; drop `EXPERIMENTAL_TAB_URLS` special-casing and the separate project parser | Grok create/stop pass with the same adapter contract as ChatGPT |

Split of ownership:

| Concern | Stays in Runtime | Moves to adapter |
| --- | --- | --- |
| grants, idempotency, delivery, reservation | ✔ | |
| archive ledger, artifact cache, continuity/work memory | ✔ | |
| method names/params, fail-closed codes | ✔ | |
| host matching, DOM selectors, composer/stop heuristics | | ✔ |
| conversation-URL canonicalization, project/session identity | | ✔ (runtime validates) |
| capability flags (`sessionCreate`, `stopGeneration`, …) | | ✔ (already declared) |

Compatibility shims:

- `provider` accepted as alias of `adapter_id`; wildcard grants unchanged.
- Legacy injectors keep working until the extension manifest carries adapter entries.
- Existing fail-closed codes preserved (`browser_session_identity_provisional`,
  `browser_canonical_url_invalid`, `caller_grant_missing`, …) so UAT transcripts stay
  comparable.

Risks:

- **#666 collision**: its browser security/dispatch gates touch `mcp_http.rs` and
  `extension/background.js`, the two files this work rewrites. Sequence phases after #666
  merges, and do not edit those files in the diagnosis PR.
- **Live UAT re-baseline**: extension 0.1.213 evidence (create/dispatch/settle,
  second-account scope refusal) must be re-run once adapters own canonicalization.
- **Provider-specific behaviours that must survive**: archive-without-tab
  ([#623](https://github.com/whshang/herdr-mcp/issues/623)), title projection after the
  first assistant turn ([#624](https://github.com/whshang/herdr-mcp/issues/624)),
  ChatGPT composer URL rewriting (`mcp.rs:826-848`) and Claude's MAIN-world insert
  behaviour.
- **One-way door avoided**: no Event/wire schema change in phases 0-3; adapter manifest is
  additive.

## 4. First PR slice (recommended)

Smallest RED → GREEN that proves one provider op through the adapter surface, with no
extension release and no behaviour change:

**Slice: conversation-URL identity resolved by the adapter registry.**

- Move the per-host match in `crates/herdr-mcp/src/mcp.rs:884-928`
  (`chatgpt.com` / `claude.ai` / `grok.com`) behind one adapter lookup that returns
  `(adapter_id, conversation_id, project_id)`.
- RED: `conversation_url_ref_for_grok_returns_adapter_id` fails today because the
  provider is a hardcoded match arm; `conversation_url_ref_for_unknown_host` today returns
  `None` while the target contract requires `adapter_unsupported`.
- GREEN: both come from the registry; a fourth adapter needs a manifest row, not a match
  arm.
- Why this one: purely runtime, no DOM, no delivery semantics, no new state; it is the
  exact seam phases 1-3 consume, and it is reviewable in one sitting.

Alternative if a browser-side proof is preferred over a runtime seam: route
`dispatch.stop` for Grok through the declared `stopGeneration` capability instead of a
provider branch (`grok.js:4` already declares it), asserting capability dispatch rather
than provider name.

## 5. Acceptance signals for the whole effort

- No provider host literal in Rust production code except the adapter manifest.
- Adding a fourth provider touches one manifest row + one adapter file, no runtime match
  arm.
- The runtime still has exactly one reservation/idempotency/delivery state machine.
- Session-op method names, params, and fail-closed codes are unchanged; #666 gates and the
  existing ChatGPT/Claude/Grok UAT transcripts remain valid.
