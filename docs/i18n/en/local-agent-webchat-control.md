# Local agent WebChat control

*How a local coding agent uses herdr-mcp to create, continue, hand off, and observe supported WebChat sessions.*

This page is for the **local agent** side: Pi, Codex, Claude, or any other coding agent that runs on this workstation and needs ChatGPT/WebChat to take part in the work. It answers one question:

> I am a local agent. When should I drive a web conversation through herdr-mcp, what can I actually do today, and how do I do it without breaking identity, idempotency, or delivery evidence?

Web AI is not the only caller of Herdr's browser capability, and this page exists so a local agent does not have to be told individual API names by the user.

## 1. What this is

Herdr-MCP has two directions, and they are different contracts:

```text
Web AI (planner)                      Local coding agent (planner/executor)
      │                                          │
      │ MCP + OAuth Connector                    │ herdr-mcp CLI
      ▼                                          ▼
                    herdr-mcp  (control + state boundary)
                        ├─ Continuity / Work Memory
                        └─ Browser / WebChat control
                                    │ trusted local path
                                    │ (Native Messaging → mode-0600 socket)
                                    ▼
                       Chrome extension / registered browser endpoint
                                    │
                                    ▼
                            ChatGPT / supported WebChat
```

- **Web AI → herdr-mcp → workstation.** The Web AI plans and herdr-mcp gives it tools on this machine.
- **Local agent → herdr-mcp → WebChat/browser.** The local agent plans and executes, and herdr-mcp gives it a controlled way to act on a supported web conversation.

Both directions meet in the same runtime, the same Continuity journal, and the same browser registry. Nothing here is a new message bus and nothing here is a second task-state authority: the local agent reuses the existing Continuity journal, Work Memory partitions, browser resources, and browser control plane.

| Component | Responsibility |
| --- | --- |
| Local coding agent | Decide *why* and *when* web work is needed; compose the task; verify the result |
| herdr-mcp runtime | Control and state boundary: identity, consent/capability gating, idempotency, delivery evidence, Continuity, Work Memory |
| Chrome extension | Browser-side execution, binding, wake, and observation boundary; performs the page operations and reports observations |
| Browser endpoint | A registered, consented browser that can be a control target |
| ChatGPT / supported WebChat | The remote conversation. It is **not** a local shell and it is not a terminal |

A web conversation is a *turn-based remote collaborator*. Herdr-MCP carries a bounded message into it, records whether that message was delivered, and keeps the durable task state in Continuity — not in the page.

## 2. When to use it

Typical local-agent scenarios:

- start a new conversation in the same ChatGPT Project so the work stays in one place;
- continue work in a WebChat conversation that is already bound to this machine;
- dispatch one more message into an existing, already-bound session;
- check whether a browser/WebChat session is still alive before relying on it;
- do a canonical handoff when the local context is nearly full or the task should continue on the Web AI side;
- resume a bounded amount of prior work through Continuity/Work Memory and then continue in a conversation;
- let a Web AI planner take over after the local agent finished a code change.

Multi-WebChat / multi-account use is only as wide as the returned browser registry: control exactly the `session_ref` values that `herdr-mcp webchat resources` returns for the endpoint you inspected. Anything not returned there is not addressable, and nothing is "experimental" unless this page says so (see [Current boundaries](#11-current-boundaries)).

## 3. When not to use it

- Scraping ordinary websites, or any page that is not a supported WebChat surface.
- Downloading files, or moving data between machines (the extension is not a file transport).
- Replacing Playwright/Selenium/AppleScript as a general browser automation stack.
- Bypassing account authorization, consent prompts, or the extension's control switch.
- Guessing Project, account, conversation, or session identifiers.
- Reading a user's private ChatGPT history out of the page. If it was never captured into the local journal, it cannot be pulled out through MCP.
- Driving the user's normal browsing session for unrelated work.

## 4. Prerequisites

Everything below is a real local precondition, not a formality:

1. **A healthy herdr-mcp runtime on this machine.**
   ```bash
   herdr-mcp status
   herdr-mcp doctor
   ```
   If this checkout is a source-development runtime, check the channel first (`herdr-mcp dev status`) so you know which runtime binary you are actually talking to. The CLI always speaks to the *active* runtime, never to a repository build artifact.
2. **The browser extension installed and connected to that runtime.**
   ```bash
   herdr-mcp native-host status
   herdr-mcp extension standalone status
   ```
   Browser control is one of the places where the extension is **not** optional: it is the browser-side execution boundary. (The core Web-AI → workstation path works without the extension; this one does not.)
3. **A registered endpoint with WebChat control consent.**
   ```bash
   herdr-mcp webchat endpoints
   ```
   The endpoint must report `consent.webchat_control: true`. `consent.tool_bridge` and `consent.tool_bridge_workstation_mutation` are separate switches for a different feature (Web AI tool calls through the page) and are not required here.
4. **A signed-in provider account that the extension has observed.** Accounts, ChatGPT Projects (`space` resources), and conversations (`session` resources) appear as resources under that endpoint.
5. **No manual credential handling.** The `herdr-mcp` CLI is a first-party local client: it attaches the trusted local caller grant itself. Never print, copy, or ask the user for the runtime bearer, browser cookies, or connector credentials.

The ChatGPT Connector (OAuth MCP) is the *other* direction. A local agent does not need the Connector to control a bound WebChat session, and installing the Connector does not by itself grant browser control.

`HERDR_ENV` is unrelated to this surface: it governs direct native Herdr pane/tab/workspace operation, not `webchat`, `continuity`, or `memory`.

## 5. Capability discovery

Never start from a hardcoded API name or a remembered `*_ref`. Discover the current capability and identity set first:

```bash
# 1. Which browsers are registered and consented?
herdr-mcp webchat endpoints --limit 5

# 2. What is reachable under that endpoint?
herdr-mcp webchat resources --kind account
herdr-mcp webchat resources --kind space
herdr-mcp webchat resources --kind session --parent-ref SPACE_REF

# 3. What is this exact object, and is it currently actionable?
herdr-mcp webchat inspect SESSION_REF
```

Read the results as follows:

| Field | Meaning |
| --- | --- |
| `endpoint_ref` | The consented browser endpoint. Refs are opaque hashes; never synthesize or transform them |
| `resource_ref` | The exact account / `space` (Project) / `session` (conversation) identity |
| `parent_ref` | The hierarchy edge: account → space → session |
| `observation_generation` | The generation the extension reported for that resource; pass it as `--expected-generation` |
| `consent.webchat_control` | Whether this endpoint may be driven |
| `actuation_available` / `actuation_reason` (from `inspect`) | The capability of *that inspect call*, not a generic mutation preflight. For mutations, trust the operation's returned delivery state |

`herdr-mcp webchat resources` returns `actuation_evaluated: false` with `actuation_reason: "not_evaluated_by_list"`; a list result is never a health or consent signal.

The private method `herdr_mcp.browser_endpoint.inspect` additionally exposes the extension's declared capability operations (`capabilities.operations`) and the input contract (text-only messages, size limits, no attachments). Use it when you need machine-readable capability detail; use `herdr-mcp webchat inspect` for the resource-level answer.

**Supported browser mutations today** (the runtime fails closed with `code: "unsupported"` for everything else):

| Operation | CLI | State |
| --- | --- | --- |
| `browser_session.create` (ChatGPT, no reasoning effort, no required apps) | `herdr-mcp webchat create` | supported |
| `browser_dispatch.submit` (plain message) | `herdr-mcp webchat send` | supported |
| `browser_dispatch.status` | `herdr-mcp webchat dispatch-status` | supported (read-only) |
| `browser_session.archive` | `herdr-mcp webchat archive` | supported |
| `browser_session.open` | — | supported private method, no CLI wrapper |
| `browser_dispatch.stop` | — | supported private method, no CLI wrapper |
| `browser_message.append` | — | not supported |
| `browser_composer.set_reasoning` / `set_apps` | — | not supported |
| `browser_space.create` / `browser_space.open` | — | not supported |
| `dispatch.submit` with `reasoning_effort` or `required_apps` | — | not supported (the plain call is) |
| `herdr_mcp.browser_handoff.prepare` | `herdr-mcp webchat handoff` | supported (canonical packet, optional automatic delivery) |

When a capability is missing, report the honest outcome; do not silently fall back to another automation stack.

## 6. Basic workflows

### Workflow A — find an existing WebChat/browser session

```bash
herdr-mcp webchat endpoints --limit 5
herdr-mcp webchat resources --kind space
herdr-mcp webchat resources --kind session --parent-ref SPACE_REF --limit 10
herdr-mcp webchat inspect SESSION_REF
```

Report the identity chain you actually used: endpoint → account → Project (`space`) → session, plus `observation_generation`. Session refs and Project refs are machine-bound (`br_*`/`bep_*` values observed on this device); they are not portable names, and they are never guessed from a label.

If a related Work Memory partition exists, its locator (`project_ref`, `repo_id`, `work_chain_id`) comes from `herdr-mcp memory` / Continuity results — never from the browser ref, and never synthesized from the repository path.

### Workflow B — create a new conversation inside an existing Project

```bash
herdr-mcp webchat create \
  --endpoint-ref ENDPOINT_REF \
  --provider chatgpt \
  --account-ref ACCOUNT_REF \
  --space-ref SPACE_REF \
  --display-label "short task label" \
  --message "first message for the new conversation" \
  --expected-generation OBSERVATION_GENERATION \
  --idempotency-key ONE_STABLE_KEY \
  [--work-chain-id WORK_CHAIN_ID]
```

Notes:

- `--space-ref` is the ChatGPT Project. Omit it only when the account-level default conversation is genuinely intended.
- `--display-label` is a label, not an identity; identity comes from the returned session ref.
- `--expected-generation` must be the generation you observed for the target scope. A stale generation is rejected instead of being applied to the wrong target.
- `--work-chain-id` links the new conversation to an existing Work Memory chain. Do not invent one.
- One intended mutation = one idempotency key. Store it before you send, and reuse it for any retry decision.
- Arguments for capabilities that are not supported today (`--space-ref` creation flows via `browser_space.create`, composer reasoning/app selection) must not be emulated with another tool.

Success returns `ok: true` plus the created resource/evidence and the delivery state. Report the returned `session_ref` — not a URL you constructed yourself.

### Workflow C — dispatch a message and observe the outcome

```bash
herdr-mcp webchat send \
  --session-ref SESSION_REF \
  --message "next instruction" \
  --expected-generation OBSERVATION_GENERATION \
  --idempotency-key ONE_STABLE_KEY \
  [--work-chain-id WORK_CHAIN_ID]

herdr-mcp webchat dispatch-status DISPATCH_ID
```

`send` returns a dispatch object:

| Field | Meaning |
| --- | --- |
| `dispatch.dispatch_id` | Durable operation identity; use it for every later read |
| `dispatch.delivery_state` | `applied`, `not_applied`, `uncertain`, `rejected`, `browser_offline`, `resource_unavailable`, `stopped` |
| `dispatch.execution_state` | Whether the target turn is still running |
| `dispatch.result.settled` / `assistant_message_ref` / `evidence_id` | Durable settlement evidence for the turn |
| `replayed` | `true` when the runtime returned an already-recorded dispatch for the same idempotency key |

Resuming after a timeout is **not** resending. Read `dispatch-status` for the same `dispatch_id`; if the state is `uncertain`, re-observe the session (`herdr-mcp webchat inspect SESSION_REF`) and the live page before deciding anything. `browser_offline` / `resource_unavailable` mean the target is not currently reachable — wait and re-observe, and never change the idempotency key just to probe liveness.

Stopping a running turn is a separate private operation (`herdr_mcp.browser_dispatch.stop`) with no CLI wrapper today. If you need it and only have the CLI, report that boundary instead of faking a stop.

### Workflow D — canonical handoff

The canonical handoff preparation is the read-only private method `herdr_mcp.browser_handoff.prepare`:

```text
continuity_id  (durable task state)
   └─ herdr_mcp.browser_handoff.prepare { continuity_id, source_url, [objective], [work_chain_id], [handoff_id] }
        ├─ handoff.message                                   (one canonical message)
        ├─ automatic_delivery.params = { source_url, message, work_chain_id }
        ├─ manual_delivery.copy_prompt  == handoff.message    (byte-for-byte identical)
        └─ herdr_mcp.browser_session.create (pass automatic_delivery.params unchanged)
             └─ the target conversation's first step is continuity.resume <continuity_id>
```

Rules that make this safe:

- `prepare` is **read-only** and derives its target route (account/Project) from the already-registered source session; it never enumerates endpoints, accounts, Projects, generations, or device ids first.
- Automatic delivery and the manual **Copy Prompt** use the *same* canonical message. Never compose a second handoff message, encode/obfuscate it, switch transport, or recursively wrap a rejected payload.
- The target conversation must begin by resuming the **existing** `continuity_id`. Handoff never creates a second Continuity chain, and the page never becomes the task-state authority.
- After the target resumes, it re-checks live workspace / Git / runtime state; the journal is history, not live truth.
- If automatic delivery returns no Herdr execution or result fields, no workstation execution can be inferred from that outcome; use the already-prepared Copy Prompt or re-observe the exact dispatch when one exists.
- An uncertain delivery does not expose the Copy Prompt path until reconciliation proves the first attempt did not apply, so a second conversation cannot be created by guessing.

**The supported way to run it:**

```bash
herdr-mcp webchat handoff \
  --continuity-id hc:... \
  --source-url 'https://chatgpt.com/g/g-p-.../c/...' \
  [--objective TEXT] [--work-chain-id ID] [--handoff-id ID] \
  [--idempotency-key KEY] [--prepare-only]
```

`webchat handoff` is a thin local wrapper around the canonical implementation above, not a second handoff implementation:

- it asks the runtime for the canonical packet (`herdr_mcp.browser_handoff.prepare`) exactly like the Web planner and the extension HUD do;
- it then hands that packet's own `automatic_delivery.params` to the source-anchored `browser_session.create`, unchanged — the CLI never composes, rewrites, or re-encodes the message;
- `--source-url` is the audit anchor and the only route input: the runtime resolves the existing WebChat route (endpoint / account / Project) from the registered conversation, so no routing ids are passed on the command line;
- `--objective` / `--work-chain-id` / `--handoff-id` map to the same `prepare` inputs; `--handoff-id` also gives the packet a stable identity.

Output is the canonical packet plus the delivery evidence:

| Field | Meaning |
| --- | --- |
| `handoff` | The canonical packet (continuity_id, source_url, message, work_chain_id, target_context) |
| `automatic_delivery.params` | The canonical create params, byte-identical to the packet |
| `automatic_delivery.attempted` / `completed` | Whether a delivery was sent, and whether it reached `applied` |
| `automatic_delivery.delivery_state` / `reason` / `replayed` | The runtime's own delivery vocabulary and replay flag |
| `automatic_delivery.session_ref` / `dispatch_id` / `result` | The created conversation and its dispatch evidence |
| `manual_delivery.copy_prompt` | The same canonical message, for manual continuation |
| `instruction` | Plain-language statement of what actually happened |

Idempotency: one logical handoff keeps one key. Without `--idempotency-key` the CLI reuses the canonical `handoff_id`. Do not rotate the key to probe delivery state; use `automatic_delivery` and, when a dispatch exists, `dispatch-status`. The CLI never retries an uncertain delivery for you.

`--prepare-only` skips delivery and returns the packet (with `automatic_delivery.attempted=false`, `reason="prepare_only"`).

**Prepared is not delivered.** When the source conversation is not currently registered, or browser control is unavailable, the packet and `manual_delivery.copy_prompt` are still returned with `automatic_delivery.attempted=false` and the runtime's reason. That is a usable result for manual continuation, and it is **not** a completed handoff: only `automatic_delivery.completed=true` (that is, `delivery_state=applied`) means a new conversation was actually created. An `uncertain` delivery is reported as-is and never retried automatically.

**Still true:** `herdr-mcp webchat create` does not accept `source_url`. Source-anchored delivery stays inside this handoff path so the ordinary create interface keeps requiring explicit routing ids.

## 7. Local agent example

A local agent task might read:

> "Create a new conversation in the bound ChatGPT Project, carry durable continuity `hc:...` over to it, have the target resume first, and continue the current task there."

The agent should proceed in this order:

1. **Capability discovery** — `herdr-mcp webchat endpoints`, `herdr-mcp webchat resources --kind space`, `herdr-mcp webchat inspect SPACE_REF`. Confirm `consent.webchat_control` is true and capture `observation_generation`.
2. **Resolve identity** — pick the exact `endpoint_ref` / `account_ref` / `space_ref` from the returned resources. Do not guess, do not reuse a ref from another machine or another Project.
3. **Resolve durable state** — `herdr-mcp continuity resume hc:...` (or a bounded `herdr-mcp continuity search ... --project-path <checkout>` first, honoring `confirmation_required`). This is the only source of the task's durable state.
4. **Run the canonical handoff** — `herdr-mcp webchat handoff --continuity-id hc:... --source-url '<exact conversation URL>'` (add `--work-chain-id` when the chain already has one). This performs canonical preparation *and* the automatic delivery in one step; `--prepare-only` returns just the packet.
5. **Verify delivery** — read `automatic_delivery.completed` / `delivery_state`. If `completed=false`, nothing was created: use `manual_delivery.copy_prompt`, or `herdr-mcp webchat dispatch-status <dispatch_id>` when a dispatch exists. Never retry with a new `--idempotency-key`.
6. **Report** — return the exact `session_ref`, the delivery state, and what the target was asked to do. State explicitly that `continuity.resume` runs on the target side, and that a prepared-only result is not a completed handoff.

Never place real account ids, tokens, or production secrets in a plan, a message, or a report. Refs returned by the CLI are opaque identifiers; they are safe to pass back to the CLI and to report to the user, but they are not credentials.

## 8. Delivery and retry semantics

| Situation | Correct behavior |
| --- | --- |
| No Herdr execution/result evidence | Do not infer local execution; use the canonical manual path, or re-observe the exact dispatch when one exists |
| `applied` | The mutation is durable; continue, and use the dispatch/evidence identity for follow-ups |
| `not_applied` | Nothing was delivered; a new attempt needs a deliberate decision, not an automatic loop |
| `uncertain` | Re-observe (`webchat inspect`, `dispatch-status`); never blind-retry the mutation |
| `browser_offline` / `resource_unavailable` | The target is not reachable now; wait, re-observe, then retry the original intent — do not rotate the idempotency key to probe |
| `rejected` | Treat the attempt as refused; report that result and stop automatic retries |
| `stopped` | The turn was stopped deliberately; treat it as an outcome, not a failure to retry |

Additional rules:

- **One idempotency key per intended mutation.** Replaying the same key returns the recorded dispatch (`replayed: true`) instead of acting twice.
- **Never synthesize identity.** Account, Project, session, generation, and Continuity/work-chain identifiers must all come from returned results.
- **Delivery evidence beats optimism.** A timeout is not a delivery; a missing error is not a delivery; terminal or conversational scrollback is not settlement evidence.
- **Mutations are serialized per account scope** by the runtime; do not try to parallelize conflicting mutations.
- **Do not create a second state authority.** The page, the Project, and the conversation are not substitutes for Continuity/Work Memory.

## 9. Continuity vs Work Memory vs Browser session

| Concept | Owns | Not to be confused with |
| --- | --- | --- |
| **Continuity** | Durable cross-conversation task state; the authoritative journal for one `continuity_id` | A live browser tab; a Work Memory partition |
| **Work Memory** | A more precise partition of history: `project_ref` + `repo_id` + `work_chain_id` | A browser session; a repository path by itself |
| **Browser / WebChat session** | The current execution carrier for a web conversation (`session_ref`) | Durable task state; a repo/work chain |
| **Browser endpoint** | A registered, consented browser that may be a control target | A user account or a Project |
| **Browser extension** | Browser-side execution, binding, wake, and observation | A file transport, an agent runtime, a general RPA layer |
| **Herdr workspace** | The local development environment (panes, agents, terminals, worktrees) | A web conversation |

The practical failure mode is collapsing these into one "session id". `continuity_id`, `work_chain_id`, `session_ref`, `space_ref`, `endpoint_ref`, and Herdr pane/workspace ids are different namespaces with different lifetimes.

## 10. Troubleshooting

| Symptom | What to check |
| --- | --- |
| No endpoint at all | `herdr-mcp native-host status`, `herdr-mcp extension standalone status`, `herdr-mcp doctor` (the `standalone-extension-load` advisories if you use the STANDALONE channel) |
| Endpoint present, `consent.webchat_control: false` | The extension's control switch/consent has not been granted on this endpoint; this is a browser-side action, not a CLI flag |
| Account or Project ambiguous | List again and choose by returned refs; never resolve ambiguity by guessing or by "most recent" |
| `browser_resource_not_found` / stale session | Re-run `herdr-mcp webchat resources`; the conversation may have been closed, archived, or replaced by a newer observation |
| The same conversation was observed by two browser endpoints | The canonical URL resolves to the **newest** observation, so a browser-profile or extension-identity switch does not strand an otherwise fresh conversation. Only two distinct sessions sharing that newest timestamp fail closed as `browser_canonical_url_ambiguous` |
| Dispatch timeout | Read `herdr-mcp webchat dispatch-status DISPATCH_ID`; then observe the session; only then decide |
| Mutation delivery uncertain | Re-observe first. Do not resend with a new idempotency key |
| `code: "caller_grant_missing"` | You are not on the trusted local path (for example a raw TCP MCP client). Use the `herdr-mcp` CLI, which attaches the local grant itself; grants cannot be asserted over TCP |
| `code: "unsupported"` | The operation is outside today's supported matrix (see [Capability discovery](#5-capability-discovery)); report it instead of emulating it |
| Continuity found but the browser session is gone | Resolve the durable state first (`continuity resume`), then create/open a target session; do not treat the missing page as lost history |
| Session exists but `work_memory` is `null` | Stay at the Continuity level. There is no `project_ref`/`repo_id`/`work_chain_id` to use, and none may be synthesized |
| Connector confusion | The ChatGPT Connector is the Web-AI → workstation path. Local WebChat control goes extension → local IPC → runtime, and does not need the Connector |

## 11. Current boundaries

This section is deliberately explicit so nobody documents or builds against a capability that does not exist yet:

- **Unsupported browser operations** (runtime returns `code: "unsupported"`): `browser_space.create`, `browser_space.open`, `browser_message.append`, `browser_composer.set_reasoning`, `browser_composer.set_apps`, and `browser_dispatch.submit` with `reasoning_effort` or `required_apps`.
- **Supported but without a CLI wrapper today:** `browser_session.open`, `browser_dispatch.stop`, `browser_endpoint.inspect`, `browser_space.inspect`. They are reachable as private methods through the runtime MCP boundary; the local CLI does not expose a subcommand for them.
- **Handoff:** the canonical preparation path is `herdr_mcp.browser_handoff.prepare`, exposed to local agents as `herdr-mcp webchat handoff` (which reuses it and then performs the source-anchored delivery). The Web planner and the extension HUD keep calling the private method directly. There is still no `herdr-mcp webchat handoff`-style flag on `webchat create`, and `webchat create` does not accept `source_url`.
- **`ego-browser`** is development/UAT infrastructure, never a user dependency and never a substitute for this control plane.
- **Not exposed at all:** reading a user's private ChatGPT history body, attachments through the dispatch contract, arbitrary DOM access, and any provider other than the ones the registry actually reports.

Treat everything in this section as implementation truth, not as a roadmap promise: verify against the installed runtime before relying on it.

## Related documentation

- [Browser continuity](browser-continuity.md) — continuity, Auto/Queue, manual handoff and Copy Prompt from the page side.
- [Browser Control Center](browser-control-center.md) — Chrome Side Panel surface for workspace/pane/binding state.
- [Browser extension](extension.md) — extension identities, local security boundary, and the JSON → MCP bridge.
- [CLI reference](cli-reference.md) — the full `herdr-mcp` command surface.
- [Troubleshooting](troubleshooting.md) — runtime, link, and browser diagnostics.
