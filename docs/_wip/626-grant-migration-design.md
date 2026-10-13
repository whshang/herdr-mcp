# WIP design: Page Assist / WebChat Control grant migration across ChatGPT re-authorization

- Issue: whshang/herdr-mcp#626 (跟踪项 1:「重新授权后的 grant 迁移」)
- Status: design only. No code in this change implements this document.
- Revision: rev 2, responds to the PI review on PR #630 (comment 6145101428).
  Source line references are against `origin/main` d193380a.
- Incident (2026-10-13, qingxian-macbookair): ChatGPT `@herdr` 调用持续返回 `caller_grant_missing`。两个叠加原因之一是:重新授权 ChatGPT 生成了新的 `connector_id`,前任 connector 名下的 grant 行仍在记录里,但匹配不上新 connector,于是静默失效。所有者没有任何提示,需要重新手动 `connector page-assist allow`。

## 0. Invariant

**Caller grant 检查不放宽、不绕过、不替代。** `verifiedControlGrantSnapshot`
(`oauth-store-do.ts:1616-1640`) 继续只返回 `grant.status === "active"` 记录中
`connector_id` 和 `grant_generation` 都精确匹配的行。Rust 侧的 fail-closed 检查
(`mcp.rs:6157`, `mcp.rs:9419`) 不变。本设计里所有迁移都是**所有者显式执行**的写操作,
走和 `connector page-assist allow --confirm` 同等的所有者设备认证;不存在任何
自动推断、自动迁移或自动扩权的路径。

## 1. Revision: answers to the PI review (rev 1 → rev 2)

Rev 1 had three factual or design errors:

1. It inferred "same subject" on re-authorization. The store has no subject:
   grants are keyed only by `GRANT_PREFIX + client_id` (`oauth-store-do.ts:52`,
   `:494`, `:514`), and `:372-375` explicitly warns that ChatGPT shares one
   application `client_id` across user accounts. A trigger keyed on
   "new connector for an existing client_id" could therefore propose moving
   account A's grants to account B.
2. It auto-superseded the predecessor's rows on approve, which would cut off
   other accounts' live grants under the same `client_id`.
3. It described a `grant_generation` bump on re-auth. That bump does not exist:
   approve hard-codes `grant_generation: 1` (`:488`, `:507`), and the approve path
   (`:493-514`) **already copies** the previous active record's
   `page_assist[]` / `webchat_control[]` rows into the new record. Those rows
   keep their old `connector_id`, so they no longer match the new connector.

All three are removed from the design below. Each review item is answered here
as Conclusion / Change / Acceptance.

### (a) No inferred account link

- **Conclusion:** No migration may be proposed or applied based on an inferred
  provider-account link. Nothing today captures a verifiable provider account
  id, so nothing can be inferred.
- **Change:** Default design is **owner-picked orphan rows**:
  `herdr-mcp connector page-assist orphans` lists rows whose `connector_id` is
  not the record's current `connector_id`. `migrate` requires the owner to name
  **both** `--from <conn_old>` and `--to <conn_new>` explicitly. Ambiguous
  input fails closed (unknown or missing source/target, zero matching rows,
  target not the current connector, or more than one candidate when a selector
  was left implicit). Nothing is proposed automatically.
  Possible later work, designed separately: capture a stable, verifiable
  provider account id at approve time, and only then consider proposals.
- **Acceptance:** With no captured account id, no code path creates a proposal
  without explicit owner `--from`/`--to`. Another ChatGPT account under the same
  `client_id` never receives a proposal or a migration. Ambiguous or missing
  selectors are rejected with a stable error code and leave the record unchanged.

### (b) No auto-supersede

- **Conclusion:** Approve must not mark, delete, or rewrite any other
  connector's rows.
- **Change:** Remove rev 1 §3.2 entirely. Approve keeps today's carry-forward
  behaviour (`:493-514`). Those carried-forward rows whose `connector_id` is not
  the current connector **are the orphan set**, and they are the only source
  of truth for migration. There is no `superseded` status written at approve
  time.
- **Acceptance:** A test approves a second connector under the same `client_id`
  and checks that every pre-existing row is byte-identical afterwards, including
  rows of a still-active other connector, and that no proposal record exists.

### (c) Explicit confirmation and an apply-time recheck

- **Conclusion:** Migration is a privileged write and must follow the
  `connector revoke <id> --confirm` pattern (`cli.rs:1354-1361`).
- **Change:** `herdr-mcp connector page-assist migrate --from <conn_old> --to <conn_new> [--device <dev>] [--endpoint <ref>] --confirm`.
  Without the literal `--confirm`, the command is a dry run that only prints what
  it would do. Edge verifies that the caller is the owner device
  (`connector_owner_device_required` otherwise). Inside the single DO
  transaction it re-checks: grant record `status === "active"`, not revoked;
  `--to` is the record's current `connector_id` and rows are written with the
  record's **current** `grant_generation`; the source rows still exist and
  were not revoked. Any failure rejects the whole apply with no partial write.
- **Acceptance:** Tests cover a missing `--confirm` (no write), a non-owner
  device (rejected), revoke-then-confirm (rejected), a target that is not current
  (rejected), and a stale generation (rejected).

### (d) Stored audit records, bounded orphans, and the 32-row cap

- **Conclusion:** Owner actions must leave a stored audit trail. Orphan rows
  must not grow without bound or silently consume active capacity.
- **Change:**
  - Fix the dropped `changed_by`: today `setWebchatControlGrant` and
    `setPageAssistGrant` discard it (`void changedBy;` at `:854` / `:930`).
    Persist it in a bounded audit log (`audit[]`, ring buffer, e.g. 64 entries).
  - Migration confirm, decline, expiry, and revoke each append
    `{ at_ms, by, action, proposal_id?, from_connector_id, to_connector_id, rows: [{device_id, endpoint_ref, provider?, account_ref?}] }`.
  - Cap semantics: today the 32-row check (`:831`, `:912`) counts every row,
    including dead orphan rows. Change it so the 32 cap counts only rows of
    the current `connector_id`. Orphan rows get their own cap (e.g. 32) and a
    retention period (e.g. 30 days since the record's `approved_at_ms`). When
    the orphan cap is exceeded, the oldest rows are dropped first and each drop is audited.
  - Explicit owner cleanup: `herdr-mcp connector page-assist orphans prune --from <conn_old> --confirm`
    (owner device, audited).
- **Acceptance:** `changed_by` is present in the audit after allow, deny,
  migrate, and prune. With 32 orphan rows plus 32 active rows, allow still
  succeeds. The orphan cap and retention are enforced, and every drop is audited.

### (e) Proposals: bounded, keyed, expiring, and revoke-aware

- **Conclusion:** If a two-step flow is used (`migrate` creates a proposal and
  `migrate confirm` applies it), proposals need a defined lifecycle.
- **Change:** Proposals live in a bounded map keyed by
  `(from_connector_id, to_connector_id, device_id, endpoint_ref)`, capped at
  e.g. 16 per `client_id`. A new proposal for the same key replaces the old one.
  Each proposal has a TTL (e.g. 24 h). Expiry is checked **lazily on every access**
  (list, confirm, approve, revoke), and an optional DO alarm sweeps expired
  proposals. Each expiry is audited. Proposals are single-use: after confirm or
  decline they are removed. `revokeGrant` (`:942-985`) sets the whole record to
  `status: "revoked"`, so it must also invalidate all proposals for that
  `client_id` and audit it. Confirm after revoke always fails.
- **Acceptance:** Expired, replayed, or post-revoke proposals are rejected. The
  map never exceeds its cap. Expiry produces an audit entry.

### (f) endpoint_ref changes

- **Conclusion:** No row is ever moved to a different `endpoint_ref`.
- **Change:** If an orphan row's `endpoint_ref` is no longer live, `orphans` and
  `migrate` (dry run) show **info-only** suggestions of live candidate
  endpoints on the same `device_id`. The owner runs the normal
  `connector page-assist allow ... --confirm` for the endpoint they choose.
  Migrate skips that row and reports it.
- **Acceptance:** A row with a vanished endpoint is never written under a
  different endpoint. The suggestion output lists only same-device live endpoints.

### (g) Test plan additions

- **Conclusion:** The negative and mutation tests from the review are required
  before implementation merges.
- **Change and acceptance:** the test list:
  - Another account under the same `client_id` gets no proposal and no migration.
  - Two connectors active concurrently do not affect each other's rows.
  - Confirm from the wrong or non-owner device is rejected.
  - Confirm after revoke is rejected.
  - Extra rows injected into a proposal or request are rejected.
    A migration only applies rows that exist under `--from`.
  - A proposal replayed to a different target is rejected.
  - Mutation tests: removing the `status === "active"` filter or the
    `grant_generation` filter in `verifiedControlGrantSnapshot` must fail a test.
  - Old-Edge write-back: an old Edge version that rewrites the record preserves
    the new fields (audit and proposals), or the new Edge detects the loss and
    fails closed. Test it with a record written by the old normalize path.
  - Snapshot test: the provider-facing `caller_grant_missing` body
    (`mcp.rs:9419`, `{"ok":false,"code":"caller_grant_missing","retryable":false}`)
    stays byte-identical.

## 2. Problem

A Page Assist / WebChat Control grant row is
`{ connector_id, grant_generation, device_id, endpoint_ref[, provider, account_ref] }`
stored on the Edge OAuth Store DO under `GRANT_PREFIX + client_id`, in
`page_assist[]` / `webchat_control[]`, and resolved by exact
`{connector_id, grant_generation}` match on an active record.

On re-authorization, the Edge mints a new `connector_id`, keeps `grant_generation: 1`,
and carries the previous rows forward unchanged. Those rows keep the old
`connector_id`, so they become **orphans**. They still take capacity, but they
no longer authorize anything. The runtime correctly fails closed with
`caller_grant_missing`, but nothing tells the owner why.

Requirement: **不能静默丢掉,也不能自动扩权。** Make orphans visible, and let
the owner explicitly re-attach them.

## 3. Non-goals

- 不放宽、不绕过、不替代 caller grant 检查(见 §0)。
- 不引入第二套 grant 存储(Edge OAuth Store DO 仍是唯一事实)。
- 不推断「同一账号」或「同一 subject」;存储里没有这个信息。
- 不自动迁移、不自动 supersede、不碰其他 connector 的行。

## 4. Design summary

- `connector page-assist orphans [list|prune --from <id> --confirm]`: makes
  orphan rows visible, with info-only endpoint suggestions (f).
- `connector page-assist migrate --from <old> --to <new> [...] --confirm`:
  owner-picked. It rewrites only the selected existing orphan rows to
  `{connector_id: <new>, grant_generation: <current>}` in one DO transaction,
  with the (c) rechecks. The row scope is a strict subset of what `--from` held.
- Owner-only diagnostics: `connector list` shows an orphan count per old
  connector. The workstation-side `caller_grant_missing` diagnostic can suggest
  `connector page-assist orphans`. The provider-facing body never changes.
- Compatibility: absent new fields normalize to empty. No schema epoch bump.
  Old-Edge write-back is covered by (g).

## 5. Security review checklist

- [ ] No proposal or migration without explicit owner `--from`/`--to` (a).
- [ ] Approve never mutates other connectors' rows (b).
- [ ] `--confirm`, owner device, and apply-time active/generation/revoke rechecks (c).
- [ ] `changed_by` and audit records are stored. Orphan cap and retention exist,
      and the active-row 32 cap excludes orphans (d).
- [ ] Proposals are bounded, keyed, expiring, single-use, and invalidated by revoke (e).
- [ ] No endpoint remapping; suggestions are info only (f).
- [ ] `caller_grant_missing` provider-facing body is byte-identical (snapshot) (g).
