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
import { readBodyBounded } from "./payload.js";

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
};

type SemanticRouteResult =
  | { ok: true; payload: Record<string, unknown> }
  | { ok: false; fatal: boolean; status?: number; code: string };

const SEMANTIC_ROUTE_LIMIT = 8;
const SEMANTIC_ATTEMPT_LIMIT = 3;
const SEMANTIC_BUDGET_MS = 3_000;
const SEMANTIC_CHAT_BUDGET_MS = 15_000;
let semanticRouteCursor = 0;
const semanticRouteCooldowns = new Map<string, number>();

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
      const allowed = new Set(["name", "protocol", "url", "model", "api_key"]);
      if (Object.keys(value).some((key) => !allowed.has(key))) return { ok: false };
      const name = typeof value.name === "string" ? value.name.trim() : "";
      if (!/^[A-Za-z0-9_-]{1,64}$/.test(name) || names.has(name)) return { ok: false };

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
      routes.push({ name, protocol, url, model, apiKey });
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
  return { model: route.model, answers };
}

function semanticCooldownMs(status?: number): number {
  if (status === 401 || status === 403) return 5 * 60_000;
  if (status === 408 || status === 409 || status === 429 || (status !== undefined && status >= 500)) {
    return 30_000;
  }
  return 30_000;
}

async function callSemanticRoute(
  route: EdgeSemanticRoute,
  state: unknown,
  questions: Record<string, unknown>,
  timeoutMs: number,
): Promise<SemanticRouteResult> {
  const routeQuestions = semanticQuestionsForRoute(route, questions);
  if (!routeQuestions) return { ok: false, fatal: true, code: "semantic_request_invalid" };

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

  try {
    const upstream = await fetch(route.url, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(Math.max(1, timeoutMs)),
    });
    if (!upstream.ok) {
      return {
        ok: false,
        fatal: upstream.status === 400 || upstream.status === 422,
        status: upstream.status,
        code: "semantic_provider_unavailable",
      };
    }
    const text = await upstream.text();
    if (text.length > 256 * 1024) {
      return { ok: false, fatal: false, code: "semantic_provider_invalid_response" };
    }
    const normalized = normalizeSemanticPayload(route, JSON.parse(text));
    if (!normalized) {
      return { ok: false, fatal: false, code: "semantic_provider_invalid_response" };
    }
    return { ok: true, payload: normalized };
  } catch {
    return { ok: false, fatal: false, code: "semantic_provider_unavailable" };
  }
}

async function callSemanticChatRoute(
  route: EdgeSemanticRoute,
  messages: unknown,
  timeoutMs: number,
): Promise<SemanticRouteResult> {
  if (
    route.protocol !== "openai-chat"
    || !Array.isArray(messages)
    || messages.length < 1
    || messages.length > 32
  ) {
    return { ok: false, fatal: true, code: "semantic_request_invalid" };
  }
  for (const message of messages) {
    if (
      !isRecord(message)
      || Object.keys(message).some((key) => key !== "role" && key !== "content")
      || !["system", "user", "assistant"].includes(String(message.role ?? ""))
      || !validSemanticValue(message.content, 64 * 1024)
    ) {
      return { ok: false, fatal: true, code: "semantic_request_invalid" };
    }
  }
  try {
    const upstream = await fetch(route.url, {
      method: "POST",
      headers: {
        authorization: "Bearer " + route.apiKey,
        "content-type": "application/json",
      },
      body: JSON.stringify({
        model: route.model,
        messages,
        temperature: 0,
        stream: false,
      }),
      signal: AbortSignal.timeout(Math.max(1, timeoutMs)),
    });
    if (!upstream.ok) {
      return {
        ok: false,
        fatal: false,
        status: upstream.status,
        code: "semantic_provider_unavailable",
      };
    }
    const text = await upstream.text();
    if (text.length > 256 * 1024) {
      return { ok: false, fatal: false, code: "semantic_provider_invalid_response" };
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
      return { ok: false, fatal: false, code: "semantic_provider_invalid_response" };
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
    return { ok: false, fatal: false, code: "semantic_provider_unavailable" };
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
  } else if (
    Object.keys(parsed.value).some((key) => key !== "messages")
    || !Array.isArray(parsed.value.messages)
  ) {
    return noStoreJsonResponse({ ok: false, code: "bad_request" }, 400);
  }

  const eligibleRoutes = routes.filter((route) => semanticModeForProtocol(route.protocol) === mode);
  if (eligibleRoutes.length === 0) {
    return noStoreJsonResponse({ ok: false, code: "semantic_provider_unavailable" }, 503);
  }
  const now = Date.now();
  for (const [name, until] of semanticRouteCooldowns) {
    if (until <= now) semanticRouteCooldowns.delete(name);
  }
  const start = semanticRouteCursor++ % eligibleRoutes.length;
  const deadline = Date.now() + (mode === "chat" ? SEMANTIC_CHAT_BUDGET_MS : SEMANTIC_BUDGET_MS);
  let attempted = 0;
  let lastCode = "semantic_provider_unavailable";
  for (let offset = 0; offset < eligibleRoutes.length && attempted < SEMANTIC_ATTEMPT_LIMIT; offset++) {
    const route = eligibleRoutes[(start + offset) % eligibleRoutes.length];
    if ((semanticRouteCooldowns.get(route.name) ?? 0) > Date.now()) continue;
    const remaining = deadline - Date.now();
    if (remaining <= 0) break;
    attempted++;
    const result = mode === "chat"
      ? await callSemanticChatRoute(route, parsed.value.messages, remaining)
      : await callSemanticRoute(route, parsed.value.state, parsed.value.questions as Record<string, unknown>, remaining);
    if (result.ok) {
      semanticRouteCooldowns.delete(route.name);
      return noStoreJsonResponse(result.payload);
    }
    lastCode = result.code;
    if (result.fatal) return noStoreJsonResponse({ ok: false, code: result.code }, 400);
    semanticRouteCooldowns.set(route.name, Date.now() + semanticCooldownMs(result.status));
  }
  return noStoreJsonResponse({ ok: false, code: lastCode }, 502);
}
