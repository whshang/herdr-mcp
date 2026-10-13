# Page Assist / WebChat provider-wildcard control decision

## Status

Accepted, re-landed via #595.

## Date

2026-10-11 (decision); re-landed 2026-10-12 via #595.

## Context

WebChat control grants from remote MCP connectors were provider-scoped: a grant was needed
before `webchat send` / `dispatch-status` / `browser_dispatch.stop` would work through a
connector. Page Assist grants are endpoint-scoped rather than provider-scoped, so an owner
who authorized a device/endpoint for Page Assist still needed an explicit per-provider
WebChat control grant for each provider. PR #593 states the intended effect: `connector page-assist allow` also authorizes WebChat send/status/stop for every provider on that endpoint, and existing Page Assist grants work with no migration.

PR #589 (`b0e695d6`) first had Edge forward Page Assist caller grants for all page-scoped
methods. PR #593 (`f2e78045`) then changed the grant match to allow a provider wildcard and
had Edge derive a wildcard grant from each Page Assist endpoint grant.

## Decision

- A WebChat control grant matches on exact `endpoint_ref` plus either an exact `provider` or
  the wildcard `provider: "*"`. `account_ref` no longer participates in the match.
- Edge derives a `{endpoint_ref, provider: "*"}` grant at forward time from each Page Assist
  endpoint grant on the routed device.
- `connector page-assist allow` therefore also authorizes WebChat send/status/stop for every
  provider on that endpoint. No migration is required; `page-assist deny` removes the derived
  grant; explicit per-provider grants stay until denied.
- Unchanged: caller grant, device/endpoint exact match, tab ownership, origin, and
  sensitive-field checks.
- Re-land safety (from #595): the link layer accepts `"*"`; the runtime advertises
  `webchat_control.any_provider` in its hello message; and the Workstation DO strips `"*"`
  grants for runtimes that do not advertise it, so older runtimes keep explicit per-provider
  grants.

## Consequences

- The initial wildcard change (#593) was reverted by #594: after deploy, Edge forwarded the
  derived `"*"` grant, but the runtime link layer (`link/local_mcp.rs`, `valid_grant_provider`)
  and the shipped runtimes rejected it. Every browser private method routed to a device that
  had a Page Assist grant failed with `invalid trusted caller grant context`. Production Edge
  was rolled back to `b0e695d6` (version `fd437f6a`).
- The safe re-land (#595) gates the wildcard on a runtime capability so newer runtimes accept
  `"*"` while older runtimes continue to receive explicit per-provider grants. This removes
  the compatibility hazard that caused the rollback, at the cost of a capability handshake.
- The decision was cherry-picked as `681f4044` into PR #601.

## Timeline

| PR | Date (merged, +0800) | Commit | Change |
| --- | --- | --- | --- |
| #589 | 2026-10-11 08:28 | `b0e695d6` | Edge forwards Page Assist caller grants for all page-scoped methods. |
| #593 | 2026-10-11 23:53 | `f2e78045` | WebChat control grants accept provider `"*"`, `account_ref` no longer participates; Edge derives a `{endpoint_ref, provider: "*"}` grant from each Page Assist endpoint grant. No migration. |
| #594 | 2026-10-12 00:28 | `a6febe0f` | Reverts #593. After deploy, the runtime link layer (`link/local_mcp.rs`, `valid_grant_provider`) and shipped runtimes rejected the derived `"*"` grant, so every browser private method to a device with a Page Assist grant failed with `invalid trusted caller grant context`. Production Edge rolled back to `b0e695d6` (version `fd437f6a`). |
| #595 | 2026-10-12 07:59 | `1895e2e3` | "WebChat provider wildcard grants, gated on runtime capability (re-land #593 safely)". Link layer accepts `"*"`; runtime advertises `webchat_control.any_provider` in hello; Workstation DO strips `"*"` grants for runtimes not advertising it, so older runtimes keep explicit per-provider grants. Cherry-picked as `681f4044` into PR #601. |

## References

- PR #589: https://github.com/whshang/herdr-mcp/pull/589
- PR #593: https://github.com/whshang/herdr-mcp/pull/593
- PR #594: https://github.com/whshang/herdr-mcp/pull/594
- PR #595: https://github.com/whshang/herdr-mcp/pull/595
- PR #601: https://github.com/whshang/herdr-mcp/pull/601
- `docs/history/architecture/v1.0-alpha8-page-assist.md`
- `assets/local-agent-skill/herdr-mcp/references/webchat-control.md`

## Rationale and alternatives (STUB)

> STUB: rationale and alternatives considered were not found in the repo, PR bodies or issues; owner to fill in.
