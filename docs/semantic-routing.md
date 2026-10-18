# Semantic routing (jev decision protocol)

Last reviewed: 2026-10-18 21:30 +08:00

Two code paths share one set of routing semantics for semantic judgment calls:

| Path | Code | Route source |
| --- | --- | --- |
| Cloudflare Worker proxy | `edge/cloudflare/src/semantic-proxy.ts` | `HERDR_SEMANTIC_ROUTES` secret (JSON array) |
| Local runtime | `crates/herdr-mcp/src/semantic.rs` (+ `config.rs`) | `config.json` `semantic.routes` |

Change one and you must change the other. Each file points at the other in a comment next to its routing constants. `work_compactor.rs` reuses the same local `SemanticService`, so it inherits all of the behavior below.

## Route fields

```json
[{
  "name": "primary",
  "protocol": "decision",
  "url": "https://api.typesafe.ai/v1/systemone",
  "model": "jev-2026-05-01",
  "api_key": "<secret>",
  "max_requests_per_second": 80,
  "max_tokens_per_second": 100000
}]
```

- `name`, `protocol` (`decision` | `decision-vercel` | `openai-chat`), `url`, `model`, `api_key` are required.
- `max_requests_per_second` (default 80) and `max_tokens_per_second` (default 100000) are optional positive integers. A secret/`config.json` written before they existed parses and behaves exactly as before. The Worker rejects a non-integer or non-positive budget as `semantic_provider_misconfigured`; the local runtime falls back to the default, so a stale `0` cannot silently disable a route.
- Unknown keys are rejected (Worker: `semantic_provider_misconfigured`; Rust: `deny_unknown_fields`).
- Credentials never appear in status JSON, traces, logs, errors, `Debug` output, or handoff/memory files. Only the route name and model are ever reported.

## Load balancing

Each pool keeps a start cursor and begins one route further along on every call (round-robin), so the configured first route is not the only one used. Routes inside their cooldown window are skipped. The existing attempt limit (3 attempts, plus the per-tier deadline) still bounds a single call.

## Cooldown and backoff

- Retryable failures — `429`, `5xx`, `408`, timeout, network error, invalid provider response — cool the route down with exponential backoff: 1s base, doubling per consecutive failure, capped at 60s. A success resets the streak.
- `401`/`403` (Worker) and additionally `400`/`404`/`422` (local) use a fixed 300s cooldown; a quick retry cannot help those.
- A route skipped by its own rate budget is **not** cooled down — the pool simply tries the next route.

## Rate budget

Each route has a 1-second fixed window counting requests and estimated tokens (`ceil(serialized request body bytes / 4)`). Before an upstream call, if that call would exceed either budget the route is skipped and the next route is tried, with no cooldown and no provider request. Defaults are deliberately generous; they exist to stop one route from draining a provider quota, not to shape normal traffic.

## Model pinning

A route's `model` is *pinned* when it names a concrete version: it contains a digit and does not end with the `latest` alias (case-insensitive). Unpinned models are **accepted** (existing production config keeps working) but are reported as `pinned: false` per route in `GET /semantic/status` and in the Rust `capability_json` (`routes` / `chat_routes`), with one warning per model per process/isolate. Every successful evaluate call logs the route name, requested model, returned model, and provider `usage` when present (Worker: one JSON `console.log` line; Rust: the semantic trace and its log line).

## Batching and state size

Decision callers already batch every question into one request; that stays. Before sending, the runtime guards the batch: state + all questions ≤ 64k characters and state + longest question ≤ 32k characters. An oversized state is trimmed deterministically in code (arrays keep their most recent entries, objects keep their trailing serialization order, strings keep their most recent tail behind a `[...trimmed...]` marker). If a trimmed state still cannot fit, the call returns `state_too_large` and the caller falls back to its deterministic rules. The Worker rejects such a request with HTTP 413 and code `semantic_state_too_large`.

## Confidence thresholds

All decision thresholds live in exactly one reviewable file: `crates/herdr-mcp/src/semantic_thresholds.rs` — `CHOICE_MIN_CONFIDENCE`, `SCORE_MIN_CONFIDENCE`, `NOUL_TRUE_THRESHOLD`, `NOUL_FALSE_THRESHOLD`, `DESTRUCTIVE_ACTION_MIN_CONFIDENCE`, plus the `gate_choice` / `gate_destructive` helpers. `DEFAULT_DECISION_THRESHOLD` and `DEFAULT_NEGATIVE_DECISION_THRESHOLD` are re-exported from `semantic.rs` as compatibility aliases of the Noul thresholds.

A Noul probability threshold must never gate a Choice answer, and cleanup — a destructive recommendation — requires the destructive gate (0.9). Below a gate the caller keeps its deterministic/rule result.

## Hostile state stance

Model input state (page text, branch names, task descriptions, history) is **data**. It is never an instruction source, and it cannot add questions, candidates, answers, or confidence:

- question ids, candidate lists, counts, dates, and arithmetic are computed in code; they are never asked of the model,
- injected text such as "IGNORE PREVIOUS INSTRUCTIONS" or fake JSON answers changes nothing beyond the gates above,
- a model answer below its threshold falls back to the deterministic path; a destructive action additionally requires the destructive gate,
- deterministic guards (delivery evidence, permissions, cleanup `safe_to_delete`, idempotency) remain authoritative regardless of any semantic answer.
