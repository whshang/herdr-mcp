# herdr-mcp — Cloudflare stable edge

This subtree implements the Cloudflare Worker + Durable Object edge used between
ChatGPT and the workstation `herdr-link`. Relay v1, frozen public contract
**epoch 2 / 18 tools**, MCP transport and OAuth compatibility are implemented
and exercised by the Edge Gate. Epoch 1 remains frozen only as the immediately
previous rollback/old-session compatibility baseline.

Routing is deliberately independent from Worker deployment. `workers.dev` is a
fully supported default and requires no user-owned domain. A Cloudflare Custom
Domain is recommended only when the operator wants a long-lived stable MCP/OAuth
origin. See [`../../docs/i18n/en/cloudflare-edge-deployment.md`](../../docs/i18n/en/cloudflare-edge-deployment.md).

## Status vs. plan phases

| Plan phase | Edge state |
| --- | --- |
| Phase 1 relay protocol / contract | Complete: canonical Relay v1 plus frozen epoch-2 18-tool public contract; epoch 1 remains a legacy compatibility baseline. |
| Phase 2 `herdr-link` | Implemented in `src/link/**`; workstation sidecar connects outbound over authenticated WSS. |
| Phase 3 Cloudflare Edge | Implemented: Worker/DO/Hibernation WSS, MCP transport, correlation, offline semantics, payload bounds and redacted logging. |
| Phase 4 OAuth Edge | Implemented and migrated for the production issuer, including DCR, PKCE, refresh rotation and signing-key continuity. |
| Phase 5 production cutover | Complete for the reference deployment: `herdr-mcp.agentforme.cc.cd` is a Worker Custom Domain; the legacy Tunnel is retired. Open-source installs may remain on `workers.dev`. |
| Phase 6 self-upgrade | Implemented and production-proven: exact-contract candidate validation, atomic A/B switching, drain/rollback, and heartbeat runtime-status convergence behind the stable Link/Edge boundary. |

## Layout

```text
edge/cloudflare/
├── README.md                 ← Edge architecture and operations
├── wrangler.toml             ← dev config: herdr-edge-dev, workers.dev, no routes
├── wrangler.prod.toml        ← production candidate: still validates on workers.dev first
├── wrangler.user.example.toml
├── provision-r2.mjs          ← idempotent R2 bucket create before wrangler deploy
├── tsconfig.json             ← edge-local type check + compile (outDir: dist/)
├── .dev.vars.example         ← copy to .dev.vars, change the secret
├── .gitignore                ← ignores dist/ (build output)
├── src/
│   ├── index.ts              ← Worker entry: /health /info /status /ws /mcp /artifacts / OAuth
│   ├── artifact-relay.ts     ← private R2 generic artifact relay (not an MCP tool)
│   ├── workstation-do.ts     ← Durable Object per workstation_id
│   ├── relay-adapter.ts      ← ★ SOLE Relay Protocol v1 wire boundary
│   ├── canonical-imports.ts   ← canonical v1 type/validation port (isolated build)
│   ├── env.ts                ← typed bindings
│   ├── version.ts            ← Edge identity / current public contract epoch + hash
│   ├── contracts/            ← frozen epoch1/epoch2 catalogs + public contract pointer
│   ├── limits.ts             ← capacities, timeouts, frame budgets, op classification
│   ├── errors.ts             ← error taxonomy + retry/ambiguity classification
│   ├── pending.ts            ← bounded pending-request registry + request ids
│   ├── payload.ts            ← frame/body size checks
│   ├── auth.ts               ← Link/shared-secret + static-bearer auth primitives
│   ├── state.ts              ← persisted session schema + sanitized summaries
│   ├── logger.ts             ← redacting structured logger
│   ├── mcp-handler.ts        ← public initialize/discover/tools/list/tools/call handler
│   └── mcp-chatgpt-transport.ts ← ChatGPT/OpenAI stateless framing helpers
└── tests/
    ├── *.test.mjs            ← pure-logic unit tests (node --test, no wrangler)
    └── manual/dev-link-smoke.mjs ← optional end-to-end smoke (needs wrangler dev)
```

## What's implemented (boundaries)

- **`POST /artifacts`, `GET|DELETE /artifacts/:id`** — private R2 generic short-lived artifact relay. Existing MCP/OAuth or `LINK_SHARED_SECRET` for upload; object capability for download/delete. 8 MiB, conservative MIME allowlist (images + inert text/docs + archives + `application/octet-stream`), strict magic for recognized images, 15-minute expiry, random IDs, attachment/nosniff downloads. Not a public bucket and not a nineteenth MCP tool.
- **`GET /health`** — stable edge role: service, version, contract epoch/hash marker. No DO dependency.
- **`GET /info`** — route + stage table for debugging.
- **`GET /ws/:workstationId`** — workstation link WSS upgrade. Bearer check at the Worker
  (`Authorization: Bearer <LINK_SHARED_SECRET>`), then routed to the workstation DO
  (`idFromName(workstationId)`) where `acceptWebSocket(server, ["link"])` gives
  hibernation-compatible handling (`webSocketMessage/Close/Error/Hibernation` + `alarm`).
- **hello validation interface** — first message must be `hello` with canonical
  numeric `protocol_version === 1`, non-empty bounded workstation/boot/link identity,
  and `workstationId` must equal the route key. The current epoch-2 identity is
  accepted; the immediately previous frozen epoch-1 pair is accepted only to keep
  supervised migration/rollback continuity. Other contract pairs fail closed.
- **heartbeat / last_seen** — persisted into DO storage (throttled re-writes); staleness
  (`LINK_STALE_AFTER_MS`) drives online/offline in `/status/:workstationId`.
  Rely on Cloudflare WS auto-response for protocol pings so routine pongs never wake the DO.
- **request_id correlation + bounded pending map** — `PendingRequestRegistry`
  (256 pending / 512 completed / 10 min TTL) persisted to storage; `request_id`
  in logs; idempotency-key dedup for mutating ops.
- **forwarding over WSS w/ timeout/error mapping** — MCP `tools/call` is routed
  through the DO to the link socket; deadlines are enforced by both an
  in-session timeout and a Durable Object alarm (timers may not fire while hibernated).
  Link error codes are mapped to the edge taxonomy (`mapLinkErrorCode`).
- **MCP HTTP protocol negotiation** — `server/discover` advertises SDK wire
  `2025-11-25` first, with legacy `2025-06-18` through `2024-10-07`. ChatGPT /
  `openai-mcp` clients also see probe version `2026-07-28`; initialize negotiates
  unknown future versions down to `2025-11-25` (wire-compatible with the frozen
  epoch-2 contract).
- **offline / reconnecting semantics** — `workstation_offline` (no link),
  `workstation_reconnecting` (queued/undelivered), `delivery_uncertain`
  (sent but dropped; reads retryable, mutating NOT — never blind replay).
- **payload-size checks** — 1 MiB edge frame/body budget (well below the 32 MiB DO
  limit); applied to inbound frames, forward bodies and args, and outbound orders.
- **no secrets logged** — `logger.ts` sanitizes token-like keys, omits body-shaped
  values, and log fields are built from allowlists only (requestId/workstationId/op/status...).

## The Relay Protocol v1 boundary (ownership rule)

All wire envelopes, encode/decode, protocol-version checks and internal↔wire mapping
live in **`src/relay-adapter.ts`** (canonical type/validation port in
`src/canonical-imports.ts`). A separate worker owns `src/relay/**` (and
`src/link/**`); nothing in this edge imports them and you must not edit them from
here. When the link side lands, the two sides agree on the envelopes/versions in
`relay-adapter.ts` — single point of reconciliation.

The wire is **canonical Relay Protocol v1** (mirrors `src/relay/protocol.ts`):
- `protocol_version` is the **number `1`** on every frame.
- Kinds: `hello`, `hello_ack`, `heartbeat`, `status`, `tool_request`, `tool_result`,
  `tool_error`, `cancel`, `cancel_ack`.
- `workstation_id` on every frame; `request_id` only on correlated kinds
  (`tool_request`/`tool_result`/`tool_error`/`cancel`/`cancel_ack`).
- Snake_case field names. Old provisional kinds (`request`/`response`/`drain`/
  `runtime_status`/`upgrade_status`/`error`/`resume`) are REJECTED at decode.
- Edge → link sends: `hello_ack`, `tool_request`, `cancel`, `status` (query=true).
- Link → edge sends: `hello`, `heartbeat`, `status`, `tool_result`, `tool_error`,
  `cancel_ack`.
- Draining/upgrade state lives as local DO state only (no wire kind for them).

`canonical-imports.ts` is a wire-identical port of the canonical validation for
this isolated Worker build; it can be swapped for
`export * from "../../src/relay/protocol.js"` once the trees share a build.

## Pre-hello handshake

- A freshly accepted socket starts `inactive` (serialized attachment).
- The FIRST frame MUST be a canonical `hello` with matching `workstation_id`.
- Any other frame before hello → WS close 1008 (no non-canonical "error" kind).
- On valid hello, exactly one active link per workstation is chosen; older
  sockets are closed/retired and their close events cannot take the session
  offline.

## Cloudflare assumptions (recorded as required by the plan)

1. Hibernation only applies to server-accepted WebSockets — matches the
   workstation-initiated link WSS. We never open outbound sockets from the DO.
2. `setTimeout` is unreliable during hibernation → request deadlines also use DO
   alarms. Storage is authoritative; in-memory registry/resolvers are caches.
3. A pending DO `fetch` keeps the DO awake and prevents hibernation. Cloudflare's
   default 30 s limit is **active CPU time**, not wall-clock waiting time; HTTP/DO
   requests may wait on I/O without a fixed wall-time ceiling while the caller remains
   connected. The Edge still clamps ordinary relay calls to ≤ 60 s (default 30 s)
   as an application-level MCP/Herdr resource and ambiguity bound — see `limits.ts`.
   Long local commands use explicit start/read/kill semantics (runtime-owned, plan §11).
4. DO SQLite backend via `new_sqlite_classes` migration (local miniflare + wrangler
   support this; check `wrangler types` output for your account's DO backend).
5. Worker deployment does not imply hostname cutover. Both dev and production
   candidates are validated on `workers.dev` first. An optional Custom Domain is
   attached only after an independent preflight; open-source users may remain on
   `workers.dev` permanently.
6. Link WSS uses a fail-closed shared secret in the current epoch. MCP accepts the
   migrated OAuth path plus an operator-only static bearer for production smoke
   tests. Secret rotation stays outside Git and normal command output.

## Quickstart (local / dev only)

Prerequisites: Node ≥ 22.

```sh
# 1) Dev type-check + compile (needs @cloudflare/workers-types for the DO types)
#    Installed without saving so root package.json/lockfile are untouched:
npm install --no-save @cloudflare/workers-types
npx tsc -p edge/cloudflare/tsconfig.json          # emits edge/cloudflare/dist/

# 2) Pure-logic unit tests (no wrangler needed)
node --test edge/cloudflare/tests/*.test.mjs

# 3) Optional: full local run with miniflare (wrangler dev, local mode — never deploy)
cd edge/cloudflare
cp .dev.vars.example .dev.vars                    # then CHANGE the secret
npm install --no-save wrangler                    # or npx wrangler@latest
# Remote deploy only: create the private R2 bucket if needed, then deploy.
# node provision-r2.mjs --config wrangler.toml
# npx wrangler deploy --config wrangler.toml
npx wrangler dev                                   # binds http://127.0.0.1:8787
```

Smoke against local dev:

```sh
curl -s localhost:8787/health | jq .
curl -s localhost:8787/info | jq .
curl -s localhost:8787/status/dev-ws1 | jq .        # DO presence (offline)
# MCP transport checks (add the configured local dev bearer when auth is enabled)
curl -s -X POST localhost:8787/mcp -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize"}' | jq .
curl -s -X POST localhost:8787/mcp -H 'content-type: application/json' \
  -H 'x-herdr-workstation: dev-ws1' \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"herdr_inspect"}}' | jq .  # offline error (link not connected)

# Full relay demo: one terminal runs wrangler dev; another runs the fake link:
node tests/manual/dev-link-smoke.mjs
```

`npm test` at the repo root still runs only the repo suite; the edge suite is run
explicitly as above so root `package.json` stays untouched.

## Deliberate non-goals / next work

- **No implicit contract drift.** Epoch 2 is the production public ABI: 18 tools,
  including `herdr_skill`. Any later public tool/metadata change requires another
  frozen contract epoch and a supervised migration; routine runtime upgrades stay
  inside epoch 2.
- **No public inbound path to the workstation.** The Edge terminates public MCP;
  the workstation still connects outward through `herdr-link` only.
- **No mandatory custom domain.** `workers.dev` remains a supported installation
  target. Custom Domain is a routing preference, not a product prerequisite.
- **No automatic DNS deletion in the domain controller.** Existing CNAME/Tunnel
  migrations require explicit rollback evidence before the conflicting DNS record
  is removed.
- **No second relay implementation.** Protocol ownership remains canonical and the
  Edge adapter must stay wire-compatible with `src/relay/**`.
- **Runtime A/B does not launch arbitrary candidate processes.** Candidate process
  creation remains a deployment/upgrade step; `herdr-link` owns validation,
  activation, drain and rollback. See [`../../docs/i18n/en/runtime-self-upgrade.md`](../../docs/i18n/en/runtime-self-upgrade.md).

## Security posture

- Link secret: `LINK_SHARED_SECRET`, missing/empty → fail closed. Production values
  live in Cloudflare secrets and workstation Keychain, never in tracked config.
- MCP auth: production OAuth is authoritative; a separate static bearer exists only
  for bounded operator smoke tests and is also stored outside Git.
- Logs: allowlist fields + key redaction + body omission; never log args/prompts.
- Idempotency keys: mutating ops should carry one; dedup is bounded (10 min TTL).
- Fail-closed remains the default for authorization, malformed input and unsupported boundaries.
