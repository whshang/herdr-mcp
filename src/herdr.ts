/**
 * herdr Unix-socket client (newline-delimited JSON). Protocol version is
 * whatever the installed daemon speaks — reflect it via `herdr api schema --json`,
 * never hardcode it here.
 * Every call opens a fresh socket (no stale connection reuse).
 *
 * Reconnect/retry is a documented client obligation under live handoff
 * ("clients should reconnect and retry"), but ONLY for idempotent methods:
 * a transport timeout may have already applied a side effect, so
 * NON_IDEMPOTENT_METHODS are never transparently retried — the error carries
 * retryable:true and the caller decides.
 */
import * as net from "node:net";
import * as path from "node:path";
import { homedir } from "node:os";

export interface HerdrErrorDetail {
  code: string;
  message: string;
  errno?: string;
  socket_path?: string;
  method?: string;
  retryable: boolean;
}

export class HerdrError extends Error {
  readonly code: string;
  readonly errno?: string;
  readonly socketPath?: string;
  readonly method?: string;
  readonly retryable: boolean;

  constructor(code: string, message: string, detail: Partial<HerdrErrorDetail> = {}) {
    super(`${code}: ${message}`);
    this.code = code;
    this.errno = detail.errno;
    this.socketPath = detail.socket_path;
    this.method = detail.method;
    this.retryable = detail.retryable ?? ["socket_error", "timeout", "connection_refused", "socket_missing"].includes(code);
  }

  toDetail(): HerdrErrorDetail {
    return {
      code: this.code,
      message: this.message,
      ...(this.errno ? { errno: this.errno } : {}),
      ...(this.socketPath ? { socket_path: this.socketPath } : {}),
      ...(this.method ? { method: this.method } : {}),
      retryable: this.retryable,
    };
  }
}

export type HerdrResult = Record<string, unknown>;
export type HerdrEvent = Record<string, unknown>;
export interface Subscription { type: string; pane_id?: string; [k: string]: unknown }

function defaultSocketPath(): string {
  return process.env.HERDR_SOCKET_PATH
    ?? path.join(homedir(), ".config", "herdr", "herdr.sock");
}

function socketError(e: NodeJS.ErrnoException, sockPath: string, method?: string): HerdrError {
  let code = "socket_error";
  if (e.code === "ECONNREFUSED") code = "connection_refused";
  else if (e.code === "ENOENT") code = "socket_missing";
  return new HerdrError(code, e.message, {
    errno: e.code,
    socket_path: sockPath,
    method,
    retryable: true,
  });
}

function connect(sockPath: string, timeoutMs: number, method?: string): Promise<net.Socket> {
  const { promise, resolve, reject } = Promise.withResolvers<net.Socket>();
  const s = net.createConnection(sockPath);
  const timer = setTimeout(() => {
    s.destroy();
    reject(new HerdrError("timeout", `connect ${sockPath}`, { socket_path: sockPath, method, retryable: true }));
  }, timeoutMs);
  s.once("connect", () => { clearTimeout(timer); resolve(s); });
  s.once("error", (e: NodeJS.ErrnoException) => { clearTimeout(timer); reject(socketError(e, sockPath, method)); });
  return promise;
}

/**
 * Side-effecting socket methods (method names reflected from `herdr api schema
 * --json`; set maintained by hand — verify against herdr_methods when the
 * method list changes). A transport timeout may already have applied these,
 * so they are never transparently retried.
 */
export const NON_IDEMPOTENT_METHODS = new Set([
  "agent.focus", "agent.prompt", "agent.rename", "agent.send_keys", "agent.start",
  "agent.view.clear", "agent.view.set",
  "client.window_title.clear", "client.window_title.set",
  "integration.install", "integration.uninstall",
  "layout.apply", "layout.set_split_ratio",
  "notification.show",
  "pane.clear_agent_authority", "pane.close", "pane.focus", "pane.focus_direction",
  "pane.graphics.clear", "pane.graphics.set", "pane.input.set", "pane.move", "pane.rename",
  "pane.release_agent", "pane.report_agent", "pane.report_agent_session", "pane.report_metadata",
  "pane.resize", "pane.send_input", "pane.send_keys", "pane.send_text", "pane.split",
  "pane.swap", "pane.zoom",
  "plugin.action.invoke", "plugin.disable", "plugin.enable", "plugin.link", "plugin.unlink",
  "plugin.pane.close", "plugin.pane.focus", "plugin.pane.open",
  "popup.close",
  "server.reload_agent_manifests", "server.reload_config", "server.stop",
  "tab.close", "tab.create", "tab.focus", "tab.move", "tab.rename",
  "workspace.close", "workspace.create", "workspace.focus", "workspace.move",
  "workspace.move_block", "workspace.rename", "workspace.report_metadata",
  "worktree.create", "worktree.open", "worktree.remove",
  "events.subscribe", // opening a second stream on retry is not free
]);


function parseEnvelope(line: string): { id?: string; result?: unknown; error?: { code?: string; message?: string }; event?: unknown } {
  return JSON.parse(line) as { id?: string; result?: unknown; error?: { code?: string; message?: string }; event?: unknown };
}

export class HerdrClient {
  private readonly sockPath: string;
  private readonly defaultTimeoutMs: number;
  private nextId = 0;

  constructor(socketPath?: string, defaultTimeoutMs = 30000) {
    this.sockPath = socketPath ?? defaultSocketPath();
    this.defaultTimeoutMs = defaultTimeoutMs;
  }

  /** Public lightweight diagnostic — no workspace/session dependency. */
  async health(): Promise<Record<string, unknown>> {
    try {
      const pong = await this.call("ping", {}, 3000, false);
      return { ok: true, socket_path: this.sockPath, pong };
    } catch (e) {
      const err = e instanceof HerdrError ? e : new HerdrError("unknown", String(e));
      return { ok: false, ...err.toDetail() };
    }
  }

  /**
   * One request → one response. Each attempt uses a NEW socket.
   *
   * Retry policy (live-handoff client obligation): idempotent methods get ONE
   * transparent retry on transport errors (fresh socket + jitter). Methods in
   * NON_IDEMPOTENT_METHODS are never auto-retried — a timeout may have already
   * applied the side effect — the thrown error keeps retryable:true so the
   * caller can decide (typically: verify state first, then re-send).
   */
  async call(
    method: string,
    params?: Record<string, unknown>,
    timeoutMs?: number,
    retry?: boolean,
  ): Promise<HerdrResult> {
    const allowRetry = retry === undefined ? !NON_IDEMPOTENT_METHODS.has(method) : retry;
    try {
      return await this.callOnce(method, params ?? {}, timeoutMs ?? this.defaultTimeoutMs);
    } catch (e) {
      const err = e instanceof HerdrError ? e : new HerdrError("unknown", String(e), { method });
      if (allowRetry && err.retryable) {
        // Short jitter lets a just-restarted herdr daemon recreate its socket.
        await new Promise<void>((resolve) => setTimeout(resolve, 100));
        return this.callOnce(method, params ?? {}, timeoutMs ?? this.defaultTimeoutMs);
      }
      throw err;
    }
  }

  private async callOnce(method: string, params: Record<string, unknown>, timeoutMs: number): Promise<HerdrResult> {
    const id = `c${++this.nextId}`;
    const sock = await connect(this.sockPath, timeoutMs, method);
    const { promise, resolve, reject } = Promise.withResolvers<HerdrResult>();
    let buf = "";

    const onData = (chunk: Buffer) => {
      buf += chunk.toString("utf-8");
      const nl = buf.indexOf("\n");
      if (nl < 0) return;
      cleanup();
      try {
        const env = parseEnvelope(buf.slice(0, nl));
        if (env.error) {
          reject(new HerdrError(env.error.code ?? "error", env.error.message ?? "", { method, retryable: false }));
        } else {
          resolve((env.result ?? {}) as HerdrResult);
        }
      } catch (e) {
        reject(new HerdrError("parse_error", e instanceof Error ? e.message : "unknown", { method, retryable: false }));
      }
    };
    const onError = (e: NodeJS.ErrnoException) => { cleanup(); reject(socketError(e, this.sockPath, method)); };
    const onTimeout = () => { cleanup(); reject(new HerdrError("timeout", method, { socket_path: this.sockPath, method, retryable: true })); };
    const cleanup = () => {
      sock.off("data", onData);
      sock.off("error", onError);
      sock.off("timeout", onTimeout);
      sock.destroy();
    };

    sock.on("data", onData);
    sock.on("error", onError);
    sock.on("timeout", onTimeout);
    sock.write(JSON.stringify({ id, method, params }) + "\n");
    return promise;
  }

  async ping(): Promise<HerdrResult> {
    return this.call("ping", {}, 5000);
  }

  async snapshot(): Promise<HerdrResult> {
    const r = await this.call("session.snapshot", {}, 10000);
    return (r.snapshot ?? {}) as HerdrResult;
  }

  subscribe(subscriptions: Subscription[], timeoutSec: number): AsyncIterable<HerdrEvent> {
    const sockPath = this.sockPath;
    const id = `s${++this.nextId}`;
    return {
      [Symbol.asyncIterator]() {
        let sock: net.Socket | null = null;
        let buf = "";
        const queue: HerdrEvent[] = [];
        let closed = false;
        const deadline = Date.now() + timeoutSec * 1000;
        const { promise: started, resolve: resolveStarted } = Promise.withResolvers<void>();

        (async () => {
          try {
            sock = await connect(sockPath, timeoutSec * 1000, "events.subscribe");
            sock.setTimeout(1000);
            sock.on("data", (chunk: Buffer) => {
              buf += chunk.toString("utf-8");
              let nl: number;
              while ((nl = buf.indexOf("\n")) >= 0) {
                const line = buf.slice(0, nl); buf = buf.slice(nl + 1);
                if (!line.trim()) continue;
                try {
                  const env = parseEnvelope(line);
                  if (env.error) { closed = true; sock?.destroy(); return; }
                  // Live envelopes are {"event":"pane_updated","data":{...}} where
                  // `event` is a STRING; older shape may carry an object. Unify to
                  // the data payload (or the object itself) so subscribers see the
                  // event body. (A-2: snapshot+events single source of truth.)
                  if (env.event !== undefined && env.event !== null) {
                    const data = (env as { data?: unknown }).data;
                    if (typeof env.event === "object") {
                      queue.push(env.event as HerdrEvent);
                    } else if (typeof data === "object" && data !== null) {
                      const payload = data as Record<string, unknown>;
                      queue.push({ ...payload, event: env.event });
                    }
                  }
                } catch { /* skip */ }
              }
            });
            sock.on("close", () => { closed = true; });
            sock.on("error", () => { closed = true; sock?.destroy(); });
            sock.on("timeout", () => { if (Date.now() >= deadline) { closed = true; sock?.destroy(); } });
            sock.write(JSON.stringify({ id, method: "events.subscribe", params: { subscriptions } }) + "\n");
            resolveStarted();
          } catch { closed = true; resolveStarted(); }
        })();

        return {
          async next(): Promise<IteratorResult<HerdrEvent>> {
            await started;
            while (queue.length === 0 && !closed) {
              await new Promise<void>((resolve) => {
                const check = setInterval(() => {
                  if (queue.length > 0 || closed || Date.now() >= deadline) {
                    clearInterval(check); if (Date.now() >= deadline) { closed = true; sock?.destroy(); } resolve();
                  }
                }, 50);
              });
            }
            return queue.length > 0
              ? { value: queue.shift()!, done: false }
              : { value: undefined, done: true };
          },
          async return(): Promise<IteratorResult<HerdrEvent>> {
            sock?.destroy();
            return { value: undefined, done: true };
          },
        };
      },
    };
  }
}
