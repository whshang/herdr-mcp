# WIP design: Page Assist / WebChat Control grant migration across ChatGPT re-authorization

- Issue: whshang/herdr-mcp#626 (跟踪项 1:「重新授权后的 grant 迁移」)
- Status: design only. No code in this change implements this document.
- Incident (2026-10-13, qingxian-macbookair): ChatGPT `@herdr` 调用持续返回 `caller_grant_missing`。两个叠加原因之一是:重新授权 ChatGPT 生成了新的 `connector_id`(`conn_uGpFPdf84BEd1zeNfLRXO1` 的前任 grant 随之静默失效),所有者没有任何提示,需要重新手动 `connector page-assist allow`。

## 1. Problem

A Page Assist / WebChat Control grant is a tuple:

```
{ client_id, connector_id, grant_generation, device_id, endpoint_ref, allowed }
```

stored on the Cloudflare Edge OAuth Store DO (keyed `GRANT_PREFIX + client_id`,
fields `page_assist[]` / `webchat_control[]`), and resolved at request time by
`verifiedControlGrantSnapshot(clientId, connectorId, grantGeneration)`, which
filters **exact** `{connector_id, grant_generation}` matches.

When the same user re-authorizes the same provider app (ChatGPT), the Edge mints
a **new** `connector_id` (and/or bumps `grant_generation`) for the same
`client_id`. The old grant rows do not match the new connector, so:

- every previously-issued grant silently stops authorizing anything;
- the runtime reports `caller_grant_missing` (fail-closed, correct) but with no
  hint that a healthy grant was just orphaned by re-authorization;
- the owner discovers it only by manual triage (`connector list` →
  `webchat endpoints` → `connector page-assist allow` again).

Hard requirement from the issue: **不能静默丢掉,也不能自动扩权** — migration
must be owner-confirmed and must never widen authority beyond what the previous
connector already had.

## 2. Non-goals

- 不放宽、不绕过、不替代 caller grant 检查本身(`caller_grant_missing` 的
  fail-closed 语义保持不变)。
- 不引入第二套 grant 存储/授权来源(Edge OAuth Store DO 仍是唯一事实)。
- 不做跨用户的 grant 迁移:迁移只考虑同一 `client_id`、同一 subject 的
  前任 connector。

## 3. Design

### 3.1 Grant record gains explicit lifecycle

Extend the stored grant row (both `page_assist` and `webchat_control`):

```
{
  connector_id, grant_generation, device_id, endpoint_ref,   // unchanged
  status: "active" | "superseded",                           // new, default "active"
  superseded_by_connector_id?: string,                        // new
  superseded_at?: number,                                     // new (epoch sec)
  migrated_from_connector_id?: string,                        // new (provenance)
}
```

`status: "superseded"` rows are **kept** (audit), but `verifiedControlGrantSnapshot`
only returns `active` rows — resolution semantics are unchanged for existing
connectors.

### 3.2 Re-authorization marks the predecessor superseded and opens a migration proposal

When the Edge creates a new connector for a `client_id` that already has an
active connector (same subject, i.e. the normal ChatGPT re-auth path), inside
the same DO transaction:

1. Mark the old connector's grant rows `status: "superseded"`,
   `superseded_by_connector_id = <new connector_id>`.
   (Nothing is deleted; capacity accounting counts only `active` rows, so
   migration cannot be blocked by a full 32-slot quota.)
2. Create a **migration proposal** record on the grant record:

```
migration_proposal: {
  proposal_id,                       // server-minted, idempotency anchor
  from_connector_id, to_connector_id,
  device_id,                         // the owner workstation that must confirm
  grant_kind: "page_assist" | "webchat_control",
  rows: [{ device_id, endpoint_ref, provider?, account_ref? }],  // exact copy, no widening
  created_at, expires_at             // e.g. 14-day TTL, then auto-expire
  state: "pending" | "applied" | "declined" | "expired"
}
```

The proposal is a **copy of exactly the superseded rows' scopes** — same
`device_id`, `endpoint_ref` (and `provider`/`account_ref` for WebChat Control).
It never adds scopes, never crosses users, and never crosses `client_id`.

### 3.3 Owner confirmation is the only path to re-attach authority

New workstation CLI (owner-device authenticated, same auth as
`connector page-assist allow`):

```
herdr-mcp connector page-assist migrate list     # pending proposals + orphaned grant rows
herdr-mcp connector page-assist migrate confirm --proposal-id <id> [--device <dev>]
herdr-mcp connector page-assist migrate decline --proposal-id <id>
```

- `confirm` re-validates against **live** state at apply time: the target
  `endpoint_ref` must still exist in the local registry (`browser_endpoint.list`
  view) and, for WebChat Control, the provider/account pair must still resolve.
  Rows whose targets no longer exist are skipped and reported, never
  auto-mapped to a different endpoint/account.
- `confirm` is idempotent on `proposal_id`; the apply transaction is the same DO
  transaction pattern as `setPageAssistGrant` (normalize → capacity check on
  active rows → put).
- `changed_by` records `device:<ownerDevice>` plus
  `migrate:<proposal_id>` for the audit trail.
- Decline/expiry leaves the grants superseded — the "not silent" requirement is
  satisfied by §3.4's notices, not by auto-reissuing.

### 3.4 No-silent-failure notices

- `connector list` output gains an `orphaned_grants` section: superseded rows
  grouped by `superseded_by_connector_id`, with the owning user's new
  connector and a hint that a migration proposal is pending (or expired).
- The runtime's `caller_grant_missing` diagnostic path gains an advisory hint
  (workstation-owner surfaces only, never in the provider-facing error body):
  "this device previously held grants under connector <old>, superseded by
  <new> at <ts>; run `herdr-mcp connector page-assist migrate list`".
  The provider-facing `caller_grant_missing` error code and retryable=false
  contract stay byte-identical.

### 3.5 Compatibility

- Grant resolution contract (`{connector_id, grant_generation}` exact-match,
  fail-closed) is untouched; old runtime versions simply ignore the new fields.
- The DO store migrates lazily on read (`normalizePageAssistGrants` /
  `normalizeConnectorGrant` treat absent `status` as `"active"`), no schema
  epoch bump required.
- Capacity (32) continues to apply to active rows only.

## 4. Security review checklist

- [ ] Proposal rows are a strict subset of the superseded rows (no new scope).
- [ ] Only the owner device of the exact `device_id` can confirm a proposal.
- [ ] Confirm re-checks endpoint/account liveness at apply time.
- [ ] No path mutates grants outside the DO transaction + normalize gate.
- [ ] `caller_grant_missing` provider-facing error body unchanged.
- [ ] Proposals expire; expiry is reported, never silently renewed.

## 5. Test plan (when implemented)

- Edge (TS, `edge/cloudflare`): re-auth mints new connector → old rows
  superseded + proposal created with exact scope copy; confirm applies within
  capacity and is idempotent; confirm skips vanished endpoint refs; decline and
  TTL expiry leave state superseded; normalize treats legacy rows as active.
- Rust contract tests: `connector list` surfaces `orphaned_grants`;
  owner-diagnostic hint text present on the workstation surface only.
- Incident repro: grant under `conn_old`, re-auth to `conn_new`, no confirm →
  `caller_grant_missing` (unchanged), `migrate list` shows the proposal;
  `confirm` → same `@herdr` call succeeds with no other change.