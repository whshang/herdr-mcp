---
name: browser-adapter-author
description: Author, replay, and repair reusable Generic Web BrowserPage adapters as ordinary project Skills. Use when a task says "teach Herdr this website", asks for a browser/site adapter, or needs a saved site workflow replayed or repaired after page drift.
---

# Browser Adapter Authoring

Reference-only module. It owns no public tool and grants no browser authority.

An Adapter is an ordinary project Skill plus the existing BrowserPage kernel. Do not create an
Adapter registry, Adapter database, installer service, second browser executor, selector engine, or
site-specific Runtime subsystem. Learn the site through the existing BrowserPage methods, verify the
workflow on the live page, then persist only stable intent in a normal `SKILL.md`.

## 1. Ground authority and scope

1. Resolve the exact browser endpoint with `herdr_mcp.browser_endpoint.list` (CLI:
   `herdr-mcp webchat endpoints`). The selected endpoint must be the one the caller is authorized
   to control.
2. Resolve one exact http/https target origin with no embedded credentials. `open` and `claim`
   reject URLs outside `target_origin`.
3. Browser access is granted when the extension is installed/loaded through required `<all_urls>`
   host access. There is no Herdr per-site approval list or runtime permission prompt. BrowserPage
   still checks Chrome's live permission for the exact target origin before access. If Chrome later
   restricts that site, stop on `permission_required / host_permission_missing`; never try to
   widen browser permission from an Adapter.
4. Keep the Adapter limited to the site and task the user actually requested. Ambiguous origin,
   account, or write intent is a reason to stop and re-ground the task.

Skill text is not authorization. Runtime caller grants, Chrome host access, generation checks,
idempotency, delivery evidence, sensitive-field rules, and cleanup rules remain authoritative.

## 2. Explore through BrowserPage only

Use the existing private methods through `herdr_call` with `params` encoded as a JSON object
string. The local advanced CLI `herdr-mcp browser-page lifecycle|action` is only a wrapper over the
same methods.

```text
herdr_mcp.browser_page.lifecycle
  open|claim:
    { endpoint_ref, action, target_origin, url, idempotency_key }
  release|finalize:
    { endpoint_ref, action, page_ref }

herdr_mcp.browser_page.action
  observe:
    { endpoint_ref, page_ref, action:"observe", max_chars?, objective? }
  click:
    { endpoint_ref, page_ref, action:"click", generation, ref, idempotency_key }
  fill:
    { endpoint_ref, page_ref, action:"fill", generation, ref, value, idempotency_key }
  expect:
    { endpoint_ref, page_ref, action:"expect",
      condition:"document_ready"|"url_equals"|"text_present"|"text_absent",
      value?, timeout_ms? }
  screenshot:
    { endpoint_ref, page_ref, action:"screenshot" }
```

Rules:

- `open` creates one Herdr-owned page. `claim` binds one already-open exact canonical user tab.
  Both require `target_origin`, `url`, and an idempotency key. Neither accepts `page_ref`.
- `release` relinquishes a claimed page and never closes the user tab. `finalize` may close only
  a Herdr-owned page.
- `observe` returns bounded visible evidence plus a fresh element generation and opaque refs.
  `objective` may request Semantic/Jev advisory, but Adapter correctness cannot depend on it.
- `click` and `fill` require the fresh generation/ref from the current observation and one stable
  idempotency key for that intended mutation. Never replay after uncertain delivery.
- `expect` is read-only. `document_ready` takes no value; the other conditions require a bounded
  value. Use it to prove the deterministic postcondition when the page exposes one.
- `screenshot` is read-only visual evidence for the exact visible BrowserPage. It is optional
  authoring evidence, not a replacement for deterministic verification.
- After a successful mutation, observe again before resolving another element.

Do not drive the same task with Playwright, Selenium, AppleScript, injected page JavaScript,
CSS/XPath execution, cookies/storage access, or a second browser automation stack. Those paths do
not preserve BrowserPage identity and safety evidence.

## 3. Persist stable intent only

Store:

- exact allowed origin(s);
- task/command names and required inputs;
- stable target descriptions based on currently observable role/type, stable label/text, and nearby
  stable context;
- ordered BrowserPage operations;
- deterministic success conditions that current BrowserPage can verify;
- real repair notes for drift that was observed and fixed.

Prefer a role/type plus stable label/context. Treat recommendations, counters, hot-search text,
timestamps, generated class names, and position-only descriptions as volatile. If the target cannot
be uniquely re-resolved from stable observable semantics, record that the task is not safely
replayable yet.

Never persist:

- `page_ref`, raw tab id, BrowserPage generation, element generation/ref;
- cookies, tokens, auth headers, session ids, or credentials;
- user/private page data, raw observations, screenshots, or provider response bodies;
- CSS/XPath selectors or arbitrary executable JavaScript.

A captured trace is evidence used to write the Adapter. It is not the Adapter.

## 4. Replay with fresh observations

Every replay starts from live state:

1. `lifecycle open` or `claim` on the Adapter's exact origin.
2. `action observe`.
3. Re-resolve exactly one target from the current observation using the stored stable description.
4. Perform one intended `click` or `fill` with the fresh generation/ref and a new idempotency key.
5. Observe again after mutation, then continue with fresh refs.
6. Prove the result with `expect`, returned bounded evidence, or an artifact contract already owned
   by BrowserPage.
7. `finalize` a page this task opened; `release` a claimed user page.

Zero or multiple matches means drift. Stop instead of choosing a best guess.

## 5. Store Adapters as ordinary project Skills

Default path:

```text
<project-root>/.agents/skills/herdr-browser-<slug>/SKILL.md
```

Create `.agents/skills/herdr-browser-<slug>/` inside the managed project root before creating a new
file. `herdr_fs_write` does not create missing parent directories. Use the existing file mutation
authority: `herdr_fs_write` for a new Skill, then `herdr_fs_edit` or `herdr_fs_patch` for
repairs.

Do not add another manifest, `adapter.json`, database row, install record, activation record, or
runtime package. Existing Progressive SkillService discovery is the registry.

User-global promotion to `~/.agents/skills` is not part of this authoring path. Do not create a new
global write mechanism just to share an Adapter.

## 6. Prove discovery after writing

Writing the file is incomplete until the existing Skill surface can discover it with the same
`project_root`:

1. `herdr_mcp.skill.list` — confirm `herdr-browser-<slug>` is present and its
   `source_identity` starts with `project:`.
2. `herdr_mcp.skill.describe` — inspect the selected id and digest.
3. `herdr_mcp.skill.load` — load that exact id with `project_root` and confirm the body/digest.

A project Skill cannot shadow a builtin Skill id. Use the `herdr-browser-` prefix and a unique
project slug.

## 7. Minimal Adapter template

```markdown
---
name: herdr-browser-<slug>
description: Reusable BrowserPage workflow for <site>; use when the task needs <task>.
---

# <Site> BrowserPage workflow

## Scope
- Allowed origin: `https://<origin>`.
- Task: <one sentence>.
- This Skill grants no permission and bypasses no origin, generation, idempotency, delivery, or
  cleanup check.

## Inputs
- `<input>` — required; <meaning and bound>.

## Target semantics
- <target name>: role/type <...>, stable label/text <...>, near <stable context>.
- Re-observe and require exactly one match before every mutation.

## Execution
1. Open or claim the exact page.
2. Observe and resolve <target>.
3. Fill/click with the fresh generation/ref and one idempotency key.
4. Observe again before any next mutation.
5. Expect <document_ready | url_equals | text_present | text_absent> <value when required>.
6. Finalize only Herdr-owned pages; release claimed user pages.

## Verification
- Main path: <live verified result>.
- Critical failure/empty path: <live verified fail-closed result>.
- Missing or ambiguous target is drift; do not guess or retry a write blindly.

## Repair notes
- <date> — <observed drift> -> <smallest verified change>.

## Non-persisted runtime facts
- Never store page/tab refs, generations, element refs, credentials, private data, raw observations,
  screenshots, or provider bodies.
```

## 8. Authoring and repair workflow

For a new Adapter:

1. Explore the requested task with Generic Web.
2. Verify one main path on the live site.
3. Verify one critical failure/empty/not-found path when the workflow has one.
4. Distill only stable intent into the project Skill.
5. Prove `skill.list / describe / load`.
6. Replay once from the saved Skill with fresh BrowserPage state.

Do not write a permanent Adapter first and hope the site matches it later. Live evidence comes
before persistence.

For repair:

1. Load the existing project Skill and its current digest.
2. Observe the current live page.
3. Identify the smallest stable description or operation that drifted.
4. Re-run the main path and relevant failure path.
5. Edit the Skill only after the new behavior is verified.
6. Reload it and verify the new digest.

Use the project's normal Git/worktree/history for review and rollback. Do not invent an Adapter
activation database or force-replace stale content.

Never repair drift by weakening permission, generation/ref, idempotency, delivery, sensitive-data,
or cleanup checks.

## 9. Reference sites are platform UAT

Bilibili, X, and Doubao are reference scenarios used to discover missing BrowserPage primitives and
prove the platform. Do not hard-code those sites into the Runtime, this builtin Skill, or the
BrowserPage kernel. The product goal is that any permitted website can be taught through the same
project-Skill workflow.
