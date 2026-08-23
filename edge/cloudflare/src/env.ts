/**
 * env.ts — typed Cloudflare bindings for the dev edge.
 *
 * Bindings are declared in edge/cloudflare/wrangler.toml; secret overrides
 * come from `.dev.vars` (or `wrangler secret put` for remote dev). These names
 * are dev-only — production bindings will be introduced at cutover and must
 * NOT be added here without updating the README + plan.
 */

export interface Env {
  /** Durable Object binding (class WorkstationDO). */
  WORKSTATION_DO: DurableObjectNamespace;
  /** Global OAuth state Durable Object binding (class OAuthStoreDO). */
  OAUTH_STORE_DO: DurableObjectNamespace;
  /** Dev-only shared link secret. Fail closed when absent. */
  LINK_SHARED_SECRET?: string;
  /** Temporary Phase-3 bearer for public dev /mcp + /status. Replaced by OAuth in Phase 4. */
  DEV_MCP_BEARER_SECRET?: string;
  /** Optional static bearer compatibility with the current localhost runtime. */
  STATIC_MCP_BEARER_SECRET?: string;
  /** Exact production OAuth issuer used to validate already-issued access JWTs. */
  OAUTH_ISSUER?: string;
  /** Existing production OAuth RS256 public key, supplied as a Worker secret. */
  OAUTH_JWT_PUBLIC_PEM?: string;
  /** Temporary one-time admin bearer used only while importing OAuth state. */
  OAUTH_IMPORT_SECRET?: string;
  /** Edge environment label surfaced on /health and /info. */
  EDGE_ENV?: string;
  /** Deployment/project identity surfaced in diagnostics. */
  EDGE_PROJECT?: string;
  /** Edge version shown on /health (overridable for local builds). */
  EDGE_VERSION?: string;
  /** Optional string overrides for limits (see limits.ts makeLimits). */
  EDGE_MAX_FRAME_BYTES?: string;
  DEFAULT_REQUEST_TIMEOUT_MS?: string;
  LINK_STALE_AFTER_MS?: string;
  /** Dev-only default workstation id for the tools/call demo route. */
  DEMO_WORKSTATION_ID?: string;
}