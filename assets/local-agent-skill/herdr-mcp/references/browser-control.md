# Generic web control through BrowserPage

Use this for user-approved ordinary webpages (including logged-in sites) and for fast mechanical navigation with Jev. Use the installed `herdr-mcp` CLI: it attaches the trusted-local, exact-endpoint browser authority. The browser extension performs all actual page access. Do not use Playwright, raw CDP, injected JavaScript, cookies, or a parallel browser executor.

This is a separate contract from `webchat create/send/handoff`. A WebChat session ref is not a BrowserPage ref.

## Discover and own the exact page

```sh
herdr-mcp webchat endpoints --limit 5
herdr-mcp browser-page lifecycle --params-json '{"endpoint_ref":"ENDPOINT_REF","action":"claim","target_origin":"https://example.com","url":"https://example.com/","idempotency_key":"ONE_STABLE_CLAIM_KEY"}'
```

Choose a returned `endpoint_ref` on the intended device/profile. Use `claim` only for an existing exact user page. Use `open` with the same parameters for an agent-owned, inactive page. Never guess an endpoint, page ref, origin, account, or tab identity. Do not silently move between endpoints if one is unavailable. Record the returned opaque `page_ref` and `ownership`.

## Observe, act, and verify

```sh
herdr-mcp browser-page action --params-json '{"endpoint_ref":"ENDPOINT_REF","page_ref":"PAGE_REF","action":"observe","max_chars":4000}'
herdr-mcp browser-page action --params-json '{"endpoint_ref":"ENDPOINT_REF","page_ref":"PAGE_REF","action":"click","generation":"GENERATION_FROM_OBSERVE","ref":"ELEMENT_REF_FROM_OBSERVE","idempotency_key":"ONE_STABLE_ACTION_KEY"}'
herdr-mcp browser-page action --params-json '{"endpoint_ref":"ENDPOINT_REF","page_ref":"PAGE_REF","action":"expect","condition":"text_present","value":"Expected result"}'
```

Use only refs from the latest observation. Read the installed CLI/runtime error if it rejects a parameter; never emulate unsupported selectors, raw scripts, or arbitrary network calls. `fill` also requires a current generation, an observed nonsensitive ref, an explicit user-provided value, and a stable idempotency key. Prefer `expect` or a fresh `observe` to verify the actual result; a successful click alone is not task completion. Check the exact installed Runtime contract for supported `expect` condition fields before constructing one.

Extension 0.1.151+ also supports a visible **native single-choice `<select>`** through the existing `fill` action: `observe` exposes up to 24 enabled, visible option labels and the current `selected_option` (never raw DOM values), and the caller supplies an exact unique option value or displayed label as `value`. The Extension dispatches `input` and `change`, invalidates the old generation and returns normal mutation evidence. Missing, disabled, ambiguous, stale, previously unobserved and multi-select options fail closed without a click or event. A site's `change` handler might submit data or navigate, so reserve one idempotency key and inspect the outcome rather than blindly repeating the action. Custom JavaScript dropdowns still require ordinary observed refs and explicit clicks.

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

After `release`, verify `released=true` and `tab_cleanup_verified=true` while the claimed user tab stays open. After `finalize`, verify `tab_cleanup_verified=true` and `tab_closed=true` for an owned page. If cleanup is uncertain, inspect ownership before further action; never close someone else's tab. BrowserPage control requires the exact caller grant; `caller_grant_missing` does not permit changing Connector permissions or bypassing the extension.
