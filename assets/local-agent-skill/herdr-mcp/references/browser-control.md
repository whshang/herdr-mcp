# Generic web control through BrowserPage

Use this for user-approved ordinary webpages (including logged-in sites) and for fast mechanical navigation with Jev. Use the installed `herdr-mcp` CLI: it attaches the trusted-local, exact-endpoint browser authority. The browser extension performs all actual page access. Do not use Playwright, raw CDP, injected JavaScript, cookies, or a parallel browser executor.

This is a separate contract from `webchat create/send/handoff`. A WebChat session ref is not a BrowserPage ref.

## Discover and own the exact page

```sh
herdr-mcp webchat endpoints --limit 5
# claim an existing exact user page
herdr-mcp browser-page lifecycle --params-json '{"endpoint_ref":"ENDPOINT_REF","action":"claim","target_origin":"https://example.com","url":"https://example.com/","idempotency_key":"ONE_STABLE_CLAIM_KEY"}'
# open an agent-owned, inactive page
herdr-mcp browser-page lifecycle --params-json '{"endpoint_ref":"ENDPOINT_REF","action":"open","target_origin":"https://example.com","url":"https://example.com/","idempotency_key":"ONE_STABLE_OPEN_KEY"}'
```

`target_origin` is a **required** parameter for both `claim` and `open`: pass the exact HTTP/HTTPS origin that `url` belongs to (`https://example.com` for any `https://example.com/...` page). It is normalized to an origin and must equal `url`'s origin; omitting it, or passing one that does not match `url`, fails with `invalid_params`. Do not pass `target_origin` (or `url`) for `release` or `finalize`; those two actions take only `endpoint_ref`, `action`, `page_ref` and an optional `idempotency_key`, and reject `target_origin`/`url`.

Choose a returned `endpoint_ref` on the intended device/profile. Use `claim` only for an existing exact user page. Use `open` for an agent-owned, inactive page. Never guess an endpoint, page ref, origin, account, or tab identity. Do not silently move between endpoints if one is unavailable. Record the returned opaque `page_ref` and `ownership`.

## Observe, act, and verify

```sh
herdr-mcp browser-page action --params-json '{"endpoint_ref":"ENDPOINT_REF","page_ref":"PAGE_REF","action":"observe","max_chars":4000}'
herdr-mcp browser-page action --params-json '{"endpoint_ref":"ENDPOINT_REF","page_ref":"PAGE_REF","action":"click","generation":"GENERATION_FROM_OBSERVE","ref":"ELEMENT_REF_FROM_OBSERVE","idempotency_key":"ONE_STABLE_ACTION_KEY"}'
herdr-mcp browser-page action --params-json '{"endpoint_ref":"ENDPOINT_REF","page_ref":"PAGE_REF","action":"expect","condition":"text_present","value":"Expected result"}'
```

Extension 0.1.214+ with a matching Runtime pages long observations through the read-only `browser_page.action` `action=observe`. Pass integer `element_offset` (0..4096, default 0) to skip that many eligible, visible controls in document order; each response still holds at most 128 refs and its non-null `next_element_offset` says where to continue (`null` means the end). Only eligible visible controls advance the cursor: password, file, hidden, disabled and sensitive fields never receive a ref and never consume a page slot. Every observe mints a fresh generation, so the previous page's refs go stale — page, re-observe, then act; never mix refs from two pages. The offset is a live-document position, not a stable cursor, so re-observe and reconcile if the page mutates between reads. Other actions reject `element_offset`. This pages element refs only: the page text snapshot, the bounded `expect element_present` scan and table extraction keep their existing caps.

Use only refs from the latest observation. Read the installed CLI/runtime error if it rejects a parameter; never emulate unsupported selectors, raw scripts, or arbitrary network calls. `fill` also requires a current generation, an observed nonsensitive ref, an explicit user-provided value, and a stable idempotency key. Prefer `expect` or a fresh `observe` to verify the actual result; a successful click alone is not task completion. Check the exact installed Runtime contract for supported `expect` condition fields before constructing one.

Extension 0.1.151+ also supports a visible **native single-choice `<select>`** through the existing `fill` action: `observe` exposes up to 24 enabled, visible option labels and the current `selected_option` (never raw DOM values), and the caller supplies an exact unique option value or displayed label as `value`. The Extension dispatches `input` and `change`, invalidates the old generation and returns normal mutation evidence. Missing, disabled, ambiguous, stale, previously unobserved and multi-select options fail closed without a click or event. A site's `change` handler might submit data or navigate, so reserve one idempotency key and inspect the outcome rather than blindly repeating the action. Custom JavaScript dropdowns still require ordinary observed refs and explicit clicks.

Extension 0.1.153+ additionally accepts the explicit string `"true"` or `"false"` in the same `fill` action for a visible native checkbox, and `"true"` for a native radio button. `observe` includes `checked` for native checkable inputs; always inspect that state first. If already in the desired state, the action rejects with `control_already_in_state` instead of toggling it. Radio uncheck and any ambiguous or unsupported state also fail closed before events. The normal generation/idempotency fences and post-action re-observation remain mandatory; do not blindly retry a mutation if its delivery status is unknown.

Extension 0.1.154+ and a matching Runtime add the **read-only** `expect` conditions `element_present` and `element_absent`. Pass an exact, trimmed visible control label in `value`, not a CSS/XPath selector, and use a bounded `timeout_ms` (at most 5000). The Extension checks only visible, enabled, nonsensitive controls with a fixed internal selector vocabulary; it never evaluates arbitrary page scripts. If more than 128 candidate controls exist and no label matched, `element_scan_truncated` prevents claiming that an unseen element is absent. A passing element wait proves only existence or absence at the time of inspection, not readiness to safely click; re-observe to obtain a fresh ref/generation before acting.

Extension 0.1.155+ and matching Runtime provide bounded **viewport** scrolling through the existing mutation-ledger-backed `browser_page.action` with `action=scroll`, `direction=up|down`, optional integer `amount` (1..1500 pixels, default 500), a current `generation` from `observe`, and a new stable `idempotency_key`. No element ref, arbitrary selector or script is permitted. The action returns `before_y`, `after_y`, `moved`, and `at_boundary`; `moved=false` means no viewport progress was observed, not that new content was fetched. Re-observe for current scroll coordinates and fresh generation. Do not repeat a scroll if delivery is unknown or replay it with a fresh key to probe the outcome. The operation affects the document viewport only, not arbitrary nested scroll containers.

Extension 0.1.156+ with a matching Runtime provides **read-only targeted text extraction** through `browser_page.action` `action=extract`, the latest `generation` and observed `ref`, and optional `max_chars` (1..8192, default 4000). It returns bounded visible text from one observed element, not form input values, page markup, raw attributes, credentials, scripts, or network payloads. Sensitive, hidden, disabled, detached, stale and cross-origin refs fail closed. Observe first to obtain a current ref; extraction does not consume an idempotency key or mutate the page. Older Runtimes may reject this action; do not silently replace it with raw scripts or a separate executor.

Extension 0.1.200+ with a matching Runtime adds bounded **single-key press** through the mutation-ledger-backed `browser_page.action` with `action=press`, a `key` from the fixed vocabulary (`Enter`, `Escape`, `Tab`, `Backspace`, `Delete`, `ArrowUp`, `ArrowDown`, `ArrowLeft`, `ArrowRight`, `Home`, `End`, `PageUp`, `PageDown`, `Space`), the current `generation`, an optional observed `ref` to focus first, and a stable `idempotency_key`. There is no text, modifier combination, raw key code, selector, or script input — typing belongs to `fill`. The Extension dispatches synthetic keydown/keypress/keyup events to the ref target or the current focused control; page JavaScript handlers receive them, but browser default actions (native form submit, native focus traversal) are not triggered by synthetic events. Press sends synthetic key events (isTrusted=false), so browser default actions such as Enter submitting a form are not guaranteed; use an explicit click for native submission. The trusted-input fix is tracked in [#617](https://github.com/whshang/herdr-mcp/issues/617). The press expires the observation generation: re-observe before any dependent action, and treat an uncertain delivery as reconciliation-only. A dispatch that cannot be confirmed reports `press_delivery_unknown` and leaves the observation generation current, so re-observe instead of replaying the key.

Extension 0.1.200+ with a matching Runtime adds **read-only table extraction** through `browser_page.action` with `action=extract_table`, the latest `generation`, an observed `ref` that must point to a visible `table` element (tables now appear in the `observe` element vocabulary), and optional `max_rows` (1..100, default 25). It returns bounded `headers` plus `rows` of visible cell text: each cell is clipped to 200 characters, at most 32 columns per row are returned, and `rows_truncated` states whether more rows exist. Cell text comes from rendered visible content only, through the same projection as `extract` — no raw `textContent` fallback, and no markup, attributes, input values, credentials, or scripts. A hidden or `aria-hidden` cell yields an empty value in its own column so positions stay aligned, and a row that is itself hidden is not returned. `rowspan`/`colspan` cells are flattened per occurrence rather than replicated, so rows can legitimately be shorter than the header. Hidden tables, non-table refs, stale generations, and cross-origin frames fail closed. Extraction does not consume an idempotency key or mutate the page; re-observe for fresh refs afterward.

Extension 0.1.217+ with a matching Runtime adds bounded **page navigation** through the mutation-ledger-backed `browser_page.action`:

- `navigate` takes the current `page_generation` from the page view, an explicit absolute `url` on the page's exact origin, and a stable `idempotency_key`.
- `reload` takes `page_generation` and an idempotency key, and is accepted only for task-owned pages.
- `back` / `forward` take `page_generation`, an explicit same-origin `expected_url`, and an idempotency key, and are accepted only for task-owned pages. History never guesses a destination.

Every navigation states its destination, is verified after dispatch, advances `page_generation`, and expires element generations, so re-observe before the next dependent action. A stale generation, cross-origin or unchanged destination, claimed-page reload/history request, or unsettled document fails closed before dispatch. A navigation that lands on another document reports `browser_page_navigation_mismatch` (or `browser_page_navigation_out_of_scope` when it leaves the origin) with `delivery_state=delivery_unknown`, and the page handle becomes stale: never retry it with the same key or treat it as applied. Reuse one idempotency key only for the identical request; changing `url` or `expected_url` under the same key yields `idempotency_key_conflict`.

Extension 0.1.212+ with a matching Runtime extends BrowserPage observation and assertions for ordinary Chrome/Ego pages (generation-fenced refs; no site-specific Feishu adapter required):

- **Hover**: mutation-ledger-backed `browser_page.action` with `action=hover`, current `generation`, observed `ref`, and a stable `idempotency_key`. Ownership and no-focus rules still apply; uncertain delivery is reconciliation-only (never replay to probe). Some refs may be marked hover-only — they are not clickable.
- **Click `expected_url`**: optional same-origin absolute `http(s)` navigation postcondition on `action=click` only. It is part of the mutation request hash: reusing an idempotency key with a different `expected_url` yields `idempotency_key_conflict`. Never treat an unverified destination as success; do not retry an uncertain navigation click.
- **Scoped `expect`**: in addition to label-based `element_present`/`element_absent`, ref-scoped conditions include `input_value_equals`, `input_value_contains`, `element_checked`, `element_selected`, `element_class_contains`, `element_visible`, and `element_enabled`. Text-only expect behavior is preserved. Observe first for a fresh ref/generation; long input values may appear truncated in observe previews (`text_truncated`) while scoped expect can still verify the exact value.
- **Control scan**: inert spans do not consume the bounded control budget; per-document generation nonces reject cross-document ref reuse.

After an explicit form-submit click, a full-page navigation may return `browser_page_stale`: the previous ref must no longer mutate the new document. Never replay the submit to probe the outcome. If the exact destination URL is established independently by the site's reviewed adapter or the user's explicit input, use `lifecycle claim` on that exact URL with a new claim idempotency key and observe the newly claimed ref. Release that claim before finalizing the originally owned page; the owned page retains the tab-cleanup identity even when its operation ref has become stale (extension 0.1.146+). If the new URL is unknown or the claim is ambiguous, stop and surface the uncertainty instead of guessing a route or opening another page.

Omit `objective` from a normal `observe` when only a DOM snapshot is needed: adding it requests a separate advisory semantic evaluation and adds latency.

## Bounded Jev fast path

When the task needs several **low-risk** clicks on current visible pages, prefer one bounded local fast path over several main-model round trips:

```sh
herdr-mcp browser-page fast-path --params-json '{"endpoint_ref":"ENDPOINT_REF","page_ref":"PAGE_REF","objective":"Reach the visibly verifiable destination","idempotency_key":"ONE_STABLE_RUN_KEY","max_steps":3}'
```

On a Runtime built with the 1.1 fast-path verification contract, add a caller-declared positive postcondition, for example `"verify":{"condition":"text_present","value":"Goal Complete"}` or `"verify":{"condition":"url_equals","value":"https://example.com/finished"}`. The Runtime checks this through its existing read-only BrowserPage `expect` after Jev reports `done`. An exact match returns `status=verified`, `requires_planner=false`, and the `verification` evidence. A mismatch returns `verification_required` with the failed evidence; it cannot silently promote a Jev guess to success. Omit `verify` on older installed Runtime versions until the feature is activated; an unsupported parameter must fail closed.

The Runtime re-observes each step, offers only Extension-classified low-risk link/reveal controls to Jev, validates the selected opaque ref, and routes any click through normal BrowserPage mutation reservation and delivery evidence. It executes at most eight clicks, stops on uncertain delivery, stale refs, login/challenge, human approval, or semantic uncertainty, and never guesses fill text or submits forms. If Jev is unavailable or uncertain, continue from the returned observation using the deterministic BrowserPage path; do not blindly retry an already attempted mutation.

Without `verify`, `status=verification_required` / `reason=done_candidate` remains advisory. Compare the returned final observation to a predeclared exact URL/text condition before reporting success; this needs no extra browser call when the returned evidence is sufficient. Use `expect` or fresh `observe` only when that evidence cannot settle the condition. `escalate`, `blocked`, and `budget_exhausted` are not proof of completion.

Speed rule: for one known action, use deterministic BrowserPage. Use Jev for multi-step, ambiguous mechanical choices when avoiding main-model round trips outweighs Jev model latency. The 2026-10-08 three-click localhost check measured about 0.5 s for deterministic observe/click/observe versus 2.53 s for one Jev fast path (including four semantic decisions); this is one fixture, not a product speed guarantee.

## Save reusable site work

After a successful live trial and readback, a stable site workflow can be recorded as a normal project Skill at `<project-root>/.agents/skills/herdr-browser-<slug>/SKILL.md`. Use the existing `browser-adapter-author` Skill and `skill.list/describe/load` path. Persist only stable origin, intent, user inputs, observable actions, verification, and drift notes; never persist refs, credentials, screenshots, page-private text, or a second execution state.

## Always return resources

```sh
herdr-mcp browser-page lifecycle --params-json '{"endpoint_ref":"ENDPOINT_REF","action":"release","page_ref":"PAGE_REF"}'
# For a page opened by this task, use "finalize" instead of "release".
```

After `release`, verify `released=true` and `tab_cleanup_verified=true` while the claimed user tab stays open. After `finalize`, verify `tab_cleanup_verified=true` and `tab_closed=true` for an owned page. If cleanup is uncertain, inspect ownership before further action; never close someone else's tab. BrowserPage control requires the exact caller grant; `caller_grant_missing` does not permit changing Connector permissions or bypassing the extension. For a remote connector caller it usually means the Page Assist grant is stale — a new `connector_id` after ChatGPT re-authorization or a new `endpoint_ref` after an extension identity change; the workstation owner re-allows the exact current endpoint (`herdr-mcp connector list` → `herdr-mcp webchat endpoints` → `herdr-mcp connector page-assist allow CONNECTOR_ID DEVICE_ID ENDPOINT_REF --confirm`) instead of widening anything.
