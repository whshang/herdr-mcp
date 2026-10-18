/**
 * semantic-proxy.ts — optional enrolled-device semantic provider proxy.
 *
 * Owner of the Worker-wide semantic route pool and its HTTP surface:
 * `GET /semantic/status` plus `POST /semantic/systemone|chat`. The Worker holds
 * the provider credentials from the `HERDR_SEMANTIC_ROUTES` secret; enrolled
 * workstations authenticate with their device credential and receive only the
 * bounded semantic result. index.ts stays the Worker composition/router and
 * dispatches these paths here.
 */

import { extractLinkCredential } from "./auth.js";
import { authenticateDeviceCredential } from "./device-directory.js";
import type { Env } from "./env.js";
import { byteLengthOf, readBodyBounded } from "./payload.js";

// Local per-module helpers, deliberately duplicated instead of promoted into a
// shared HTTP utility module: index.ts keeps its own copies for its own routes
// and cannot be imported from here without a cycle, and this Worker already
// keeps per-module `jsonResponse`/`isRecord` copies (artifact-relay.ts,
// mcp-handler.ts, device-directory.ts). A shared layer would be broader churn
// than the eight-line response writer it would replace.
function noStoreJsonResponse(payload: unknown, status = 200): Response {
  return new Response(JSON.stringify(payload), {
    status,
    headers: {
      "content-type": "application/json",
      "cache-control": "no-store",
      pragma: "no-cache",
    },
  });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

type SemanticProtocol = "decision" | "decision-vercel" | "openai-chat";

type EdgeSemanticRoute = {
  name: string;
  protocol: SemanticProtocol;
  url: string;
  model: string;
  apiKey: string;
  maxRequestsPerSecond: number;
  maxTokensPerSecond: number;
};

type SemanticRetryClass = "backoff" | "fixed" | "none";

type SemanticRouteResult =
  | { ok: true; payload: Record<string, unknown> }
  | {
    ok: false;
    fatal: boolean;
    status?: number;
    code: string;
    // How the pool should cool this route down. "none" means the failure is
    // not the provider's fault (local rate-budget skip), so the route keeps
    // serving the next request.
    retry: SemanticRetryClass;
  };

const SEMANTIC_ROUTE_LIMIT = 8;
const SEMANTIC_ATTEMPT_LIMIT = 3;
const SEMANTIC_BUDGET_MS = 3_000;
const SEMANTIC_CHAT_BUDGET_MS = 15_000;
const SEMANTIC_BACKGROUND_CHAT_BUDGET_MS = 60_000;
const SEMANTIC_CHAT_MAX_TOKENS = 32_768;
// Route-pool cooldown for retryable failures: 1s base, doubling per consecutive
// failure, capped at 60s. Mirrors `COOLDOWN_BASE` / `COOLDOWN_CAP` in
// crates/herdr-mcp/src/semantic.rs.
const SEMANTIC_COOLDOWN_BASE_MS = 1_000;
const SEMANTIC_COOLDOWN_MAX_MS = 60_000;
// Fixed cooldown for credential/request-shape failures (401/403).
const SEMANTIC_COOLDOWN_AUTH_MS = 5 * 60_000;
// Per-route rate budgets, enforced with a 1s fixed window. Mirrors
// `DEFAULT_MAX_REQUESTS_PER_SECOND` / `DEFAULT_MAX_TOKENS_PER_SECOND` in
// crates/herdr-mcp/src/semantic.rs.
const SEMANTIC_DEFAULT_MAX_REQUESTS_PER_SECOND = 80;
const SEMANTIC_DEFAULT_MAX_TOKENS_PER_SECOND = 100_000;
// Batch-size guard (characters). Mirrors `MAX_REQUEST_CHARS` /
// `MAX_STATE_PLUS_QUESTION_CHARS` in crates/herdr-mcp/src/semantic.rs.
const SEMANTIC_MAX_REQUEST_CHARS = 64 * 1024;
const SEMANTIC_MAX_STATE_PLUS_QUESTION_CHARS = 32 * 1024;

type SemanticChatOptions = {
  tier: "foreground" | "background";
  maxTokens?: number;
  jsonResponse: boolean;
};

const SEMANTIC_CHAT_KEYS = new Set(["messages", "max_tokens", "response_format", "tier"]);

/**
 * Parses the optional chat options. `max_tokens` is an integer in
 * 1..=32768; `response_format` is "text" | "json" | "json_object" or
 * `{type: "text" | "json_object"}`; `tier` is "foreground" | "background".
 * Returns null for any invalid value so the request fails closed.
 */
export function parseSemanticChatOptions(body: Record<string, unknown>): SemanticChatOptions | null {
  const options: SemanticChatOptions = { tier: "foreground", jsonResponse: false };
  const tier = body.tier;
  if (tier !== undefined && tier !== null) {
    if (tier !== "foreground" && tier !== "background") return null;
    options.tier = tier;
  }
  const maxTokens = body.max_tokens;
  if (maxTokens !== undefined && maxTokens !== null) {
    if (
      typeof maxTokens !== "number"
      || !Number.isInteger(maxTokens)
      || maxTokens < 1
      || maxTokens > SEMANTIC_CHAT_MAX_TOKENS
    ) return null;
    options.maxTokens = maxTokens;
  }
  const format = body.response_format;
  if (format !== undefined && format !== null) {
    const type = typeof format === "string"
      ? format
      : isRecord(format) && Object.keys(format).every((key) => key === "type") && typeof format.type === "string"
        ? format.type
        : null;
    if (type === "text") options.jsonResponse = false;
    else if (type === "json_object" || (type === "json" && typeof format === "string")) options.jsonResponse = true;
    else return null;
  }
  return options;
}
let semanticRouteCursor = 0;
// Per-route pool health (consecutive retryable failures and the cooldown
// deadline they earned) in one record per route name, so an out-of-order
// completion can never shorten a cooldown a stricter failure already recorded.
const semanticRouteHealth = new Map<string, { streak: number; cooldownUntil: number }>();
const semanticRouteWindows = new Map<string, { second: number; requests: number; tokens: number }>();
const semanticWarnedModels = new Set<string>();

function semanticProtocol(value: unknown): SemanticProtocol | null {
  return value === "decision"
    || value === "decision-vercel"
    || value === "openai-chat"
    ? value
    : null;
}

function semanticModeForProtocol(protocol: SemanticProtocol): "evaluate" | "chat" {
  return protocol === "openai-chat" ? "chat" : "evaluate";
}

function validSemanticValue(value: unknown, max: number): value is string {
  return typeof value === "string"
    && value.trim().length > 0
    && value.length <= max
    && !/[\u0000-\u001f\u007f]/.test(value);
}

function semanticUrl(value: string): string | null {
  try {
    const url = new URL(value);
    if (
      url.protocol !== "https:"
      || url.username
      || url.password
      || url.search
      || url.hash
    ) return null;
    return url.toString();
  } catch {
    return null;
  }
}

function semanticRoutes(env: Env): { ok: true; routes: EdgeSemanticRoute[] } | { ok: false } {
  const raw = env.HERDR_SEMANTIC_ROUTES?.trim() ?? "";
  if (!raw) return { ok: true, routes: [] };

  try {
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed) || parsed.length < 1 || parsed.length > SEMANTIC_ROUTE_LIMIT) {
      return { ok: false };
    }
    const names = new Set<string>();
    const routes: EdgeSemanticRoute[] = [];
    for (const value of parsed) {
      if (!isRecord(value)) return { ok: false };
      // `max_requests_per_second` / `max_tokens_per_second` are optional:
      // secrets written before they existed must parse identically.
      const allowed = new Set([
        "name",
        "protocol",
        "url",
        "model",
        "api_key",
        "max_requests_per_second",
        "max_tokens_per_second",
      ]);
      if (Object.keys(value).some((key) => !allowed.has(key))) return { ok: false };
      const name = typeof value.name === "string" ? value.name.trim() : "";
      if (!/^[A-Za-z0-9_-]{1,64}$/.test(name) || names.has(name)) return { ok: false };
      const budget = (raw: unknown, fallback: number): number | null => {
        if (raw === undefined || raw === null) return fallback;
        return typeof raw === "number" && Number.isInteger(raw) && raw > 0 ? raw : null;
      };
      const maxRequestsPerSecond = budget(
        value.max_requests_per_second,
        SEMANTIC_DEFAULT_MAX_REQUESTS_PER_SECOND,
      );
      const maxTokensPerSecond = budget(
        value.max_tokens_per_second,
        SEMANTIC_DEFAULT_MAX_TOKENS_PER_SECOND,
      );
      if (maxRequestsPerSecond === null || maxTokensPerSecond === null) return { ok: false };

      const protocol = semanticProtocol(value.protocol);
      if (!protocol) return { ok: false };

      const rawUrl = value.url;
      const model = value.model;
      const apiKey = value.api_key;
      if (
        !validSemanticValue(rawUrl, 2048)
        || !validSemanticValue(model, 256)
        || !validSemanticValue(apiKey, 4096)
      ) return { ok: false };

      const url = semanticUrl(rawUrl);
      if (!url) return { ok: false };
      names.add(name);
      if (!semanticModelPinned(model) && !semanticWarnedModels.has(model)) {
        semanticWarnedModels.add(model);
        // Route name and model only; the credential never appears here.
        console.warn(
          `herdr-mcp semantic route ${name} uses unpinned model ${model}; a versioned model id is recommended`,
        );
      }
      routes.push({ name, protocol, url, model, apiKey, maxRequestsPerSecond, maxTokensPerSecond });
    }
    return { ok: true, routes };
  } catch {
    return { ok: false };
  }
}

function semanticQuestionsForRoute(
  route: EdgeSemanticRoute,
  questions: Record<string, unknown>,
): Record<string, unknown> | null {
  const converted: Record<string, unknown> = {};
  if (semanticModeForProtocol(route.protocol) !== "evaluate") return null;
  for (const [id, raw] of Object.entries(questions)) {
    if (!isRecord(raw)) return null;
    const type = raw.type;
    if (type !== "noul" && type !== "choice" && type !== "score") return null;
    if (route.protocol === "decision-vercel" && type === "noul") {
      converted[id] = { ...raw, type: "boolean" };
    } else {
      converted[id] = raw;
    }
  }
  return converted;
}

function normalizeSemanticPayload(
  route: EdgeSemanticRoute,
  raw: unknown,
): Record<string, unknown> | null {
  if (!isRecord(raw) || !isRecord(raw.answers)) return null;
  if (route.protocol !== "decision-vercel") {
    if (typeof raw.model !== "string") return null;
    return raw;
  }
  const answers: Record<string, unknown> = {};
  for (const [id, value] of Object.entries(raw.answers)) {
    if (!isRecord(value) || typeof value.type !== "string") return null;
    if (value.type === "boolean") {
      if (typeof value.probability !== "number") return null;
      answers[id] = { type: "noul", noul: value.probability };
    } else if (value.type === "choice" || value.type === "score") {
      answers[id] = value;
    } else {
      return null;
    }
  }
  return {
    model: route.model,
    answers,
    ...(raw.usage !== undefined ? { usage: raw.usage } : {}),
  };
}

/**
 * Counts Unicode code points instead of UTF-16 code units, mirroring Rust
 * `str::chars().count()` so both sides of the semantic contract measure an
 * emoji or other astral character as one character.
 */
function semanticCharCount(value: string): number {
  let count = 0;
  for (const _ of value) count += 1;
  return count;
}

/**
 * A model is *pinned* when the route names a concrete version: it carries at
 * least one digit and does not end with the floating `latest` alias. Unpinned
 * models keep working and are reported per route (and warned about once).
 */
export function semanticModelPinned(model: string): boolean {
  const normalized = model.trim().toLowerCase();
  return normalized.length > 0 && !normalized.endsWith("latest") && /\d/.test(normalized);
}

/**
 * Exponential backoff for `streak` consecutive retryable failures: 1s, 2s, 4s,
 * ... capped at 60s. Identical to `backoff_duration` in
 * crates/herdr-mcp/src/semantic.rs.
 */
export function semanticBackoffMs(streak: number): number {
  const doublings = Math.min(Math.max(streak - 1, 0), 6);
  return Math.min(SEMANTIC_COOLDOWN_BASE_MS * 2 ** doublings, SEMANTIC_COOLDOWN_MAX_MS);
}

/**
 * Reserve one call and its estimated tokens in the route's current 1s window.
 * Returns false when this call would exceed the route budget; the pool then
 * moves to the next route without cooling this one down and without spending
 * one of the attempt-limit attempts.
 */
function semanticBudgetAllows(route: EdgeSemanticRoute, estimatedTokens: number): boolean {
  const second = Math.floor(Date.now() / 1000);
  let window = semanticRouteWindows.get(route.name);
  if (!window || window.second !== second) {
    window = { second, requests: 0, tokens: 0 };
    semanticRouteWindows.set(route.name, window);
  }
  if (window.requests + 1 > route.maxRequestsPerSecond) return false;
  if (window.tokens + estimatedTokens > route.maxTokensPerSecond) return false;
  window.requests += 1;
  window.tokens += estimatedTokens;
  return true;
}

/** Estimated token cost of one request body: 4 bytes per token, rounded up. */
function semanticEstimatedTokens(body: string): number {
  return Math.ceil(byteLengthOf(body) / 4);
}

/** Pool state for one route name; both fields move together. */
function semanticRouteState(name: string): { streak: number; cooldownUntil: number } {
  let state = semanticRouteHealth.get(name);
  if (!state) {
    state = { streak: 0, cooldownUntil: 0 };
    semanticRouteHealth.set(name, state);
  }
  return state;
}

/**
 * Record a cooldown deadline, keeping the later of the existing and new
 * deadline: overlapping failures can complete out of order, so a shorter
 * backoff must never overwrite a longer cooldown already in force.
 */
function semanticSetRouteCooldown(name: string, durationMs: number): void {
  const state = semanticRouteState(name);
  state.cooldownUntil = Math.max(state.cooldownUntil, Date.now() + durationMs);
}

/** A prepared outbound provider request: headers plus serialized body. */
type SemanticRouteRequest = { headers: Record<string, string>; body: string };

/**
 * Read-only view of one route's pool health in this isolate: consecutive
 * retryable failures and the cooldown still in force. The pool owns every
 * write; this only reports state (never a credential).
 */
export function semanticRouteHealthSnapshot(name: string): {
  consecutive_failures: number;
  cooldown_remaining_ms: number;
} {
  const state = semanticRouteHealth.get(name);
  return {
    consecutive_failures: state?.streak ?? 0,
    cooldown_remaining_ms: Math.max(0, (state?.cooldownUntil ?? 0) - Date.now()),
  };
}

function semanticRetryClassForStatus(status: number): SemanticRetryClass {
  return status === 401 || status === 403 ? "fixed" : "backoff";
}

/**
 * Builds the typed-decision provider request. Kept separate from the call so
 * the pool can charge the route's rate budget (which needs the serialized body)
 * before it counts an attempt. Returns null for an invalid question set.
 */
function semanticEvaluateRequest(
  route: EdgeSemanticRoute,
  state: unknown,
  questions: Record<string, unknown>,
): SemanticRouteRequest | null {
  const routeQuestions = semanticQuestionsForRoute(route, questions);
  if (!routeQuestions) return null;

  const headers: Record<string, string> = {
    authorization: "Bearer " + route.apiKey,
    "content-type": "application/json",
  };
  let body: Record<string, unknown>;
  if (route.protocol === "decision-vercel") {
    headers["ai-evaluation-model-specification-version"] = "4";
    headers["ai-model-id"] = route.model;
    body = { state, questions: routeQuestions };
  } else {
    body = { state, questions: routeQuestions, model: route.model };
  }
  return { headers, body: JSON.stringify(body) };
}

async function callSemanticRoute(
  route: EdgeSemanticRoute,
  prepared: SemanticRouteRequest,
  timeoutMs: number,
): Promise<SemanticRouteResult> {
  try {
    const upstream = await fetch(route.url, {
      method: "POST",
      headers: prepared.headers,
      body: prepared.body,
      signal: AbortSignal.timeout(Math.max(1, timeoutMs)),
    });
    if (!upstream.ok) {
      return {
        ok: false,
        fatal: upstream.status === 400 || upstream.status === 422,
        status: upstream.status,
        code: "semantic_provider_unavailable",
        retry: semanticRetryClassForStatus(upstream.status),
      };
    }
    const text = await upstream.text();
    if (text.length > 256 * 1024) {
      return {
        ok: false,
        fatal: false,
        code: "semantic_provider_invalid_response",
        retry: "backoff",
      };
    }
    const normalized = normalizeSemanticPayload(route, JSON.parse(text));
    if (!normalized) {
      return {
        ok: false,
        fatal: false,
        code: "semantic_provider_invalid_response",
        retry: "backoff",
      };
    }
    return { ok: true, payload: normalized };
  } catch {
    return { ok: false, fatal: false, code: "semantic_provider_unavailable", retry: "backoff" };
  }
}

/**
 * Builds the chat provider request. Kept separate from the call so the pool can
 * charge the route's rate budget before it counts an attempt. Returns null for
 * an invalid chat request.
 */
function semanticChatRequest(
  route: EdgeSemanticRoute,
  messages: unknown,
  options: SemanticChatOptions = { tier: "foreground", jsonResponse: false },
): SemanticRouteRequest | null {
  if (
    route.protocol !== "openai-chat"
    || !Array.isArray(messages)
    || messages.length < 1
    || messages.length > 32
  ) return null;
  for (const message of messages) {
    if (
      !isRecord(message)
      || Object.keys(message).some((key) => key !== "role" && key !== "content")
      || !["system", "user", "assistant"].includes(String(message.role ?? ""))
      || !validSemanticValue(message.content, 64 * 1024)
    ) return null;
  }
  return {
    headers: {
      authorization: "Bearer " + route.apiKey,
      "content-type": "application/json",
    },
    body: JSON.stringify({
      model: route.model,
      messages,
      temperature: 0,
      stream: false,
      ...(options.maxTokens !== undefined ? { max_tokens: options.maxTokens } : {}),
      ...(options.jsonResponse ? { response_format: { type: "json_object" } } : {}),
    }),
  };
}

async function callSemanticChatRoute(
  route: EdgeSemanticRoute,
  prepared: SemanticRouteRequest,
  timeoutMs: number,
): Promise<SemanticRouteResult> {
  try {
    const upstream = await fetch(route.url, {
      method: "POST",
      headers: prepared.headers,
      body: prepared.body,
      signal: AbortSignal.timeout(Math.max(1, timeoutMs)),
    });
    if (!upstream.ok) {
      return {
        ok: false,
        fatal: false,
        status: upstream.status,
        code: "semantic_provider_unavailable",
        retry: semanticRetryClassForStatus(upstream.status),
      };
    }
    const text = await upstream.text();
    if (text.length > 256 * 1024) {
      return {
        ok: false,
        fatal: false,
        code: "semantic_provider_invalid_response",
        retry: "backoff",
      };
    }
    const payload = JSON.parse(text);
    const content = isRecord(payload)
      && Array.isArray(payload.choices)
      && isRecord(payload.choices[0])
      && isRecord(payload.choices[0].message)
      && typeof payload.choices[0].message.content === "string"
      ? payload.choices[0].message.content.trim()
      : "";
    if (!content) {
      return {
        ok: false,
        fatal: false,
        code: "semantic_provider_invalid_response",
        retry: "backoff",
      };
    }
    return {
      ok: true,
      payload: {
        model: route.model,
        content,
        ...(isRecord(payload) && payload.usage !== undefined ? { usage: payload.usage } : {}),
      },
    };
  } catch {
    return { ok: false, fatal: false, code: "semantic_provider_unavailable", retry: "backoff" };
  }
}

export async function handleSemanticProxy(
  request: Request,
  env: Env,
  mode: "evaluate" | "chat" = "evaluate",
): Promise<Response> {
  const workstationId = request.headers.get("x-herdr-workstation")?.trim() ?? "";
  if (!/^[A-Za-z0-9_.-]{1,64}$/.test(workstationId)) {
    return noStoreJsonResponse({ ok: false, code: "bad_request" }, 400);
  }
  const extracted = extractLinkCredential(request);
  if (!extracted.ok) {
    return noStoreJsonResponse({ ok: false, code: "link_auth_failed" }, 401);
  }
  const registry = env.DEVICE_REGISTRY_DO.get(env.DEVICE_REGISTRY_DO.idFromName("devices-v1"));
  const authenticated = await authenticateDeviceCredential(registry, workstationId, extracted.credential);
  if (!authenticated.ok) {
    return noStoreJsonResponse({ ok: false, code: authenticated.code }, 401);
  }

  const configured = semanticRoutes(env);
  if (!configured.ok) {
    return noStoreJsonResponse({ ok: false, code: "semantic_provider_misconfigured" }, 503);
  }
  const routes = configured.routes;
  if (request.method === "GET") {
    const evaluateAvailable = routes.some((route) => semanticModeForProtocol(route.protocol) === "evaluate");
    const chatAvailable = routes.some((route) => semanticModeForProtocol(route.protocol) === "chat");
    return noStoreJsonResponse({
      ok: true,
      available: evaluateAvailable || chatAvailable,
      evaluate_available: evaluateAvailable,
      chat_available: chatAvailable,
      routes: routes.map((route) => ({
        name: route.name,
        protocol: route.protocol,
        model: route.model,
        pinned: semanticModelPinned(route.model),
      })),
    });
  }
  if (routes.length === 0) {
    return noStoreJsonResponse({ ok: false, code: "semantic_provider_unavailable" }, 503);
  }

  const parsed = await readBodyBounded(request, 96 * 1024);
  if (!parsed.ok || !isRecord(parsed.value)) {
    const code = parsed.ok ? "bad_request" : parsed.code;
    return noStoreJsonResponse(
      { ok: false, code },
      !parsed.ok && parsed.code === "payload_too_large" ? 413 : 400,
    );
  }
  if (mode === "evaluate") {
    const keys = Object.keys(parsed.value);
    if (
      keys.some((key) => key !== "state" && key !== "questions")
      || !Object.prototype.hasOwnProperty.call(parsed.value, "state")
      || !isRecord(parsed.value.questions)
    ) {
      return noStoreJsonResponse({ ok: false, code: "bad_request" }, 400);
    }
    const questionCount = Object.keys(parsed.value.questions).length;
    if (questionCount < 1 || questionCount > 32) {
      return noStoreJsonResponse({ ok: false, code: "bad_request" }, 400);
    }
    // Batch-size guard. The local runtime trims its state before sending, so
    // anything still over budget here fails closed with a stable code instead
    // of reaching a provider. Counted in Unicode code points, not UTF-16 code
    // units, to match Rust `str::chars().count()`: an emoji costs one character
    // on both sides.
    const stateChars = semanticCharCount(JSON.stringify(parsed.value.state));
    let questionsChars = 0;
    let longestQuestionChars = 0;
    for (const [id, question] of Object.entries(parsed.value.questions)) {
      const chars = semanticCharCount(id) + semanticCharCount(JSON.stringify(question));
      questionsChars += chars;
      longestQuestionChars = Math.max(longestQuestionChars, chars);
    }
    if (
      stateChars + questionsChars > SEMANTIC_MAX_REQUEST_CHARS
      || stateChars + longestQuestionChars > SEMANTIC_MAX_STATE_PLUS_QUESTION_CHARS
    ) {
      return noStoreJsonResponse({ ok: false, code: "semantic_state_too_large" }, 413);
    }
  } else if (
    Object.keys(parsed.value).some((key) => !SEMANTIC_CHAT_KEYS.has(key))
    || !Array.isArray(parsed.value.messages)
  ) {
    return noStoreJsonResponse({ ok: false, code: "bad_request" }, 400);
  }
  const chatOptions = mode === "chat" ? parseSemanticChatOptions(parsed.value) : null;
  if (mode === "chat" && !chatOptions) {
    return noStoreJsonResponse({ ok: false, code: "bad_request" }, 400);
  }

  const eligibleRoutes = routes.filter((route) => semanticModeForProtocol(route.protocol) === mode);
  if (eligibleRoutes.length === 0) {
    return noStoreJsonResponse({ ok: false, code: "semantic_provider_unavailable" }, 503);
  }
  const now = Date.now();
  for (const [name, state] of semanticRouteHealth) {
    if (state.cooldownUntil <= now) {
      // Clear the expired deadline but keep the streak, so a flapping route
      // backs off further on its next failure.
      state.cooldownUntil = 0;
      if (state.streak === 0) semanticRouteHealth.delete(name);
    }
  }
  const start = semanticRouteCursor++ % eligibleRoutes.length;
  const budgetMs = mode !== "chat"
    ? SEMANTIC_BUDGET_MS
    : chatOptions?.tier === "background"
      ? SEMANTIC_BACKGROUND_CHAT_BUDGET_MS
      : SEMANTIC_CHAT_BUDGET_MS;
  const deadline = Date.now() + budgetMs;
  let attempted = 0;
  let lastCode = "semantic_provider_unavailable";
  for (let offset = 0; offset < eligibleRoutes.length && attempted < SEMANTIC_ATTEMPT_LIMIT; offset++) {
    const route = eligibleRoutes[(start + offset) % eligibleRoutes.length];
    if ((semanticRouteHealth.get(route.name)?.cooldownUntil ?? 0) > Date.now()) continue;
    // Prepared before the attempt is counted: an over-budget route must not
    // spend one of the attempt-limit attempts, or it would starve every healthy
    // route behind it.
    const prepared = mode === "chat"
      ? semanticChatRequest(route, parsed.value.messages, chatOptions ?? undefined)
      : semanticEvaluateRequest(
        route,
        parsed.value.state,
        parsed.value.questions as Record<string, unknown>,
      );
    if (!prepared) {
      return noStoreJsonResponse({ ok: false, code: "semantic_request_invalid" }, 400);
    }
    if (!semanticBudgetAllows(route, semanticEstimatedTokens(prepared.body))) {
      lastCode = "semantic_route_rate_limited";
      continue;
    }
    const remaining = deadline - Date.now();
    if (remaining <= 0) break;
    attempted++;
    const result = mode === "chat"
      ? await callSemanticChatRoute(route, prepared, remaining)
      : await callSemanticRoute(route, prepared, remaining);
    if (result.ok) {
      semanticRouteHealth.delete(route.name);
      if (mode === "evaluate") {
        // One structured line per successful evaluate call: route name,
        // requested model, returned model, and provider usage. Never a key.
        console.log(JSON.stringify({
          event: "semantic_evaluate",
          route: route.name,
          requested_model: route.model,
          returned_model: typeof result.payload.model === "string" ? result.payload.model : null,
          usage: result.payload.usage ?? null,
        }));
      }
      return noStoreJsonResponse(result.payload);
    }
    lastCode = result.code;
    if (result.fatal) return noStoreJsonResponse({ ok: false, code: result.code }, 400);
    if (result.retry === "fixed") {
      semanticSetRouteCooldown(route.name, SEMANTIC_COOLDOWN_AUTH_MS);
    } else if (result.retry === "backoff") {
      const state = semanticRouteState(route.name);
      state.streak += 1;
      semanticSetRouteCooldown(route.name, semanticBackoffMs(state.streak));
    }
  }
  // `lastCode` already carries the local budget code when the pool skipped a
  // route for its own budget (see above).
  return noStoreJsonResponse({ ok: false, code: lastCode }, 502);
}
