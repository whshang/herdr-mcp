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
(`mcp.rs:6157`, `mcp.rs:9469`) 不变。本设计里所有迁移都是**所有者显式执行**的写操作,
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
    (`mcp.rs:9469`, `{"ok":false,"code":"caller_grant_missing","retryable":false}`)
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

## Orphan listing scope (server-side)

`POST /connectors/page-assist/orphans` with `action: "list"` is always scoped to the
device_id of the authenticated owner device (device credential, or the registry record
already bound to the legacy default workstation). A client `device_id` (CLI
`--device-id`) is only an assertion: malformed -> 400 `orphans_invalid_device_id`;
different from the authenticated device -> 403 `orphans_device_mismatch`; no resolvable
device identity -> 403 `orphans_device_unresolved`. The DO internal route refuses
unscoped listing. Any future `--confirm` must go through a fresh-plan recheck
(`applyPlanIfFresh` / `apply_migration` -> `migrate_plan_stale` / `migrate_target_conflict`).

## 7. #630 `--confirm` write path (design, not implemented)

Status: design only. Today confirm is refused (`migrate_confirm_not_implemented`) and
every migration path is read-only. Nothing in this section relaxes a grant check:
`verifiedControlGrantSnapshot`, the `status === "active"` and exact
`{connector_id, grant_generation}` matching, owner-device auth, and the
provider-facing `caller_grant_missing` body all stay unchanged.

### 7.1 Flow

1. CLI `connector page-assist migrate --from <old> --to <new> --confirm
   [--idempotency-key <k>]`. Without `--confirm` it stays a dry run. The CLI first
   fetches the dry-run plan, prints it, and sends the confirm request with
   `plan_digest` (see 7.3). It never sends rows. The server only applies rows it
   recomputes itself.
2. Edge route `POST /connectors/page-assist/migrate` (new; the orphans route stays
   read-only):
   - Auth: `authenticateOwnerDeviceIdentity`. This needs a resolved registry
     `device_id`, otherwise 403 `migrate_device_unresolved`. 401 when no owner.
   - Body keys: exactly `{from, to, plan_digest, idempotency_key}`. Unknown keys
     get 400 `bad_request`, and malformed ids get 400 with a fixed code.
   - Calls the DO internal route `/internal/oauth/grant/page-assist-migrate` with
     `{from, to, plan_digest, idempotency_key, device_id: <authenticated>, by: "device:<id>"}`.
3. DO, inside **one** `storage.transaction`:
   1. Read the idempotency entry (7.3). On a hit, return the stored result with no
      further work.
   2. List every `grant:` record and normalize it.
   3. Run `applyPlanIfFresh(records, raw, plan)`. This recomputes `planMigration`
      on the records just read (target is current, record active, generation,
      matching rows, target conflicts). It rejects with `migrate_plan_stale`,
      `migrate_target_conflict`, or the fresh plan's own code.
   4. Require `digest(freshPlan) === plan_digest`, else `migrate_plan_stale`. This
      way the owner confirms exactly what was shown.
   5. **Device scope (decision):** apply only moves whose `device_id` equals the
      authenticated device. If the fresh plan holds rows of other devices, reject
      with `migrate_cross_device_rows` instead of a partial apply. The dry-run
      `plan` action gains the same server-side device filter and digest, so a
      plan shown for device A never covers device B. (Open question Q1.)
   6. Write back with `applyPlanToRawRecord` on the **raw** stored value, so
      unknown keys are preserved. Append the audit entry (7.5) and store the
      idempotency entry. All of this is one `put` per touched key inside the
      transaction.
4. The response holds a fixed shape: `{ok, action: "connector_page_assist_migrated", moved: n, audit_id}`
   or `{ok:false, code, invalid_records?}`.

### 7.2 Atomicity and rollback

- Every read, recheck, and write runs inside a single DO `storage.transaction`.
  A throw anywhere (including after the first `put`) rolls the whole thing back.
  The DO runs one request at a time, and the transaction gives all-or-nothing writes.
- Today a plan touches exactly one record (the single record whose current
  `connector_id === to`). The write is still built as "compute all new values,
  then put", so a later multi-record plan stays atomic.
- After the write, re-read inside the transaction and run `listOrphans` on the new
  record. If any planned row is still an orphan, or a row outside the plan
  changed, throw `migrate_postcondition_failed`, which causes a rollback.
- There is no compensating "undo" command in #630. Reversal is the normal
  `page-assist deny` / revoke, and it is audited.

### 7.3 Idempotency

- Key = client-supplied `idempotency_key` (`^[A-Za-z0-9_-]{16,64}$`). The CLI
  generates a random one per invocation and prints it. Without a key the request
  is rejected (`migrate_idempotency_key_required`).
- Stored at `migrate-idem:<device_id>:<key>` as `{plan_digest, result, at_ms}`,
  with a 24 h TTL, swept lazily and by alarm, and a bounded count (e.g. 256).
- Same key with the same digest returns the stored result and writes nothing.
  Same key with a different digest gets 409 `migrate_idempotency_conflict`.
- `plan_digest` = SHA-256 of the canonical JSON of the fresh plan (`from`, `to`,
  `grant_generation`, sorted `moves`).

### 7.4 Per-device scoping

- Identity always comes from `authenticateOwnerDeviceIdentity`. A client
  `device_id` is never trusted for the write; the route does not accept one.
- The legacy shared-secret owner path resolves the device only through the
  read-only registry lookup. With no record, the request fails closed.
- Audit `by` = `device:<device_id>`.

### 7.5 Audit

- Stored in the record's `audit[]` ring (64 entries, oldest dropped first; the
  drop itself is not audited). Shape is §1(d):
  `{at_ms, by, action: "migrate", from_connector_id, to_connector_id, rows:[{device_id, endpoint_ref, provider?, account_ref?}]}`
  plus `audit_id`.
- Written in the same transaction as the rows, so the rows and their audit entry
  always commit or roll back together.
- Rejected confirms are not stored. The route returns the fixed code and logs it,
  with no record write.
- `normalizeConnectorGrant` must not drop `audit` on unrelated write-backs. This
  overlaps with item 7.6(1).

### 7.6 Deferred review items 1-3

1. **Round-trip fidelity.** Today Edge `normalizeConnectorGrant` fills a missing
   `page_assist`/`webchat_control` with `[]` and drops explicit-null known keys.
   Rust `GrantRecord` likewise has `#[serde(default)] Vec` (missing becomes `[]`)
   and `Option` + `skip_serializing_if` (an explicit `null` disappears).
   Decision:
   - The write path never serializes a normalized record. It always patches the
     raw stored value (`applyPlanToRawRecord`), touches only the planned rows, and
     only adds `audit`.
   - A key absent in raw stays absent. An explicit `null` stays `null`, which then
     makes the record unplannable with `grant_record_missing_field`.
   - Rust: switch the known array/option fields to a tri-state (absent / null /
     value, e.g. `Option<Option<T>>` with `serde_with::rust::double_option`), or
     keep a `serde_json::Value` alongside the record and patch that. Acceptance:
     `parse -> serialize` is byte-identical for records with missing arrays,
     `null` arrays, `null` connector_id, and unknown keys.
2. **Check order for a missing `connector_id`.** Rust returns
   `grant_record_missing_field` (it checks status and fields before the target),
   while Edge returns `migrate_target_not_current` (records without
   `connector_id` never match the target).
   - Unified order for both: selectors present → selectors valid → `from != to`
     → target lookup (`connector_id` non-empty and `=== to`; zero matches give
     `migrate_target_not_current`, more than one gives `migrate_target_ambiguous`)
     → `status === "active"` → `grant_generation` valid (`grant_record_missing_field`)
     → matching rows → conflicts.
   - Rust therefore returns `migrate_target_not_current` for a missing or empty
     `connector_id`, and checks status after the target.
   - Add parity tests: one shared fixture table (JSON) consumed by both the Rust
     and Edge tests, asserting identical codes.
3. **`invalid_records` on plan failure.** In `oauth-store-do.ts` `pageAssistOrphans`,
   the plan branch returns `json(plan, 409)` without `invalid_records`. Decision:
   - Every plan response (ok or not) and every confirm failure includes
     `invalid_records: <count>`. That is only a count, with no ids and no contents.
   - When `invalid_records > 0` and the result is `migrate_target_not_current`,
     the owner can tell the target may be unreadable rather than absent.
   - Confirm itself still refuses whenever the target record is invalid.

### 7.7 Test plan

Unit/DO (StorageMock with a write counter and an injectable `put` failure):
- A happy path moves only the planned rows. The other connectors' rows and the
  unknown keys and nulls are byte-identical, and exactly 1 record and 1
  idempotency entry are written.
- Stale plan: a conflicting target row, an added or removed source row, a
  generation bump, or revoke between dry run and confirm each give a fixed code
  and zero writes.
- Digest mismatch gives `migrate_plan_stale`.
- Idempotency: a replay gives the same result and no new writes. Same key with a
  different digest gives 409. A missing or malformed key gives 400.
- **Rollback:** inject a throw on the 2nd `put` (the audit or idem entry). The
  storage snapshot must equal the pre-confirm state. Inject a postcondition
  failure and check the same.
- Device scope: device A confirming a plan containing B's rows gets
  `migrate_cross_device_rows` and zero writes. An unresolved legacy device gets
  403. A non-owner gets 401.
- Audit entries are appended, bounded at 64, and carry `by=device:<id>`.
- Round-trip and check-order parity fixtures (7.6) pass in both Rust and Edge.

Route level (`device-pairing-worker.test.mjs`): the auth matrix
(owner device / legacy / non-owner / generic bearer), unknown keys, and the
end-to-end dry run → confirm → `orphans` list is empty for the moved rows →
`verifiedControlGrantSnapshot` now resolves the moved row.

Mutation tests (must each fail ≥1 test):
- removing the `applyPlanIfFresh` call;
- removing the digest compare;
- removing the device filter;
- dropping the transaction wrapper (simulate it by using non-transactional puts);
- removing `status === "active"` / `grant_generation` checks in
  `verifiedControlGrantSnapshot`.

Real-usage verification (needed before enabling, on a staging Edge):
- Re-authorize a real connector (ChatGPT web) and confirm the orphans show up.
- Run the dry run and then confirm from the owner device. A Page Assist call
  through the new connector then succeeds where it previously returned
  `caller_grant_missing`.
- A second device cannot see or migrate the first device's rows.
- Replaying the same CLI command is a no-op.
- Revoke after confirm still fails closed.
- Check the audit entry via the owner diagnostics.

### 7.8 Open questions

- Q1: Should confirm be strictly per-device (as decided above) or allow an owner to
  migrate all devices' rows with an explicit `--all-devices` plus fleet-admin?
  The default is per-device only.
- Q2: Should the idempotency entries live in the grant record (one write) or as
  separate keys? Separate keys are proposed; both are inside the same transaction.
- Q3: Should we keep the two-step proposal model of §1(e), or does digest +
  idempotency key replace it? The proposal is that digest + key replaces stored
  proposals for #630, and §1(e) is deferred.
- Q4: Should audit entries for rejected confirms be persisted (that is a write on
  failure)? The proposal is no, logs only.
- Q5: For the Rust tri-state, should we use `serde_with` or a hand-written
  deserializer? This touches dependency policy.
