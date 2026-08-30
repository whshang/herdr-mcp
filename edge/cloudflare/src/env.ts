/**
 * env.ts — typed Cloudflare bindings for Herdr Edge.
 *
 * Bindings are declared by the dev/prod wrangler configs; local secret
 * overrides come from `.dev.vars`, while remote deployments use Worker secrets.
 */

export interface Env {
  /** Durable Object binding (class WorkstationDO). */
  WORKSTATION_DO: DurableObjectNamespace;
  /** Global OAuth state Durable Object binding (class OAuthStoreDO). */
  OAUTH_STORE_DO: DurableObjectNamespace;
  /**
   * Private R2 bucket for short-lived generic artifact relay. Absent binding
   * fails closed; the bucket is never a public r2.dev asset store.
   */
  ARTIFACT_BUCKET?: R2Bucket;
  /** Shared workstation-link secret. Fail closed when absent. */
  LINK_SHARED_SECRET?: string;
  /** Optional development-only static MCP bearer fallback. */
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
  /** Default workstation target when a request does not carry an explicit id. */
  DEFAULT_WORKSTATION_ID?: string;
}