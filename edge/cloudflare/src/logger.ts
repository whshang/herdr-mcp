/**
 * logger.ts — redacting structured logger.
 *
 * Contract for edge logging:
 *  - only explicitly enumerated scalar fields may be logged (never raw maps
 *    of user/request data);
 *  - authorization material, secrets, prompts, tool arguments, file contents
 *    and frame bodies are structurally impossible to log through normal calls
 *    (callers pass allowlist fields only);
 *  - `sanitize()` is a belt-and-suspenders filter for any generic field map.
 *
 * Pure module so redaction rules are unit-testable.
 */

export type LogLevel = "info" | "warn" | "error";

const SENSITIVE_KEY_PATTERN =
  /(authorization|bearer|token|secret|password|passwd|credential|cookie|api[_-]?key|session|enrollment|pairing|pepper)/i;

const FORBIDDEN_VALUE_KEYS = /(body|args|arguments|prompt|result|output|content|file|data|message)/i;

/** A bare six-digit `code` value is a pairing code; error codes are strings. */
const SIX_DIGIT_CODE = /^[0-9]{6}$/;

/** Redact key names that look sensitive; drop payload-shaped values. */
export function sanitize(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sanitize);
  if (value !== null && typeof value === "object") {
    const out: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(value as Record<string, unknown>)) {
      if (SENSITIVE_KEY_PATTERN.test(k)) {
        out[k] = "[redacted]";
      } else if (k === "code" && typeof v === "string" && SIX_DIGIT_CODE.test(v)) {
        out[k] = "[redacted]";
      } else if (FORBIDDEN_VALUE_KEYS.test(k)) {
        out[k] = "[omitted]";
      } else {
        out[k] = sanitize(v);
      }
    }
    return out;
  }
  return value;
}

export interface LogEntry {
  tsMs: number;
  level: LogLevel;
  scope: string;
  event: string;
  fields: Record<string, unknown>;
}

export interface EdgeLogger {
  info(event: string, fields?: Record<string, unknown>): void;
  warn(event: string, fields?: Record<string, unknown>): void;
  error(event: string, fields?: Record<string, unknown>): void;
}

export interface LoggerSink {
  write(line: string): void;
}

const consoleSink: LoggerSink = { write: (line) => console.log(line) };

/**
 * Create a per-scope logger. `emit` is injectable for tests; default writes a
 * compact single-line JSON record to console.
 */
export function createLogger(scope: string, opts: { sink?: LoggerSink } = {}): EdgeLogger {
  const sink = opts.sink ?? consoleSink;
  const emit = (level: LogLevel, event: string, fields: Record<string, unknown> | undefined) => {
    const entry: LogEntry = { tsMs: Date.now(), level, scope, event, fields: sanitize(fields ?? {}) as Record<string, unknown> };
    sink.write(JSON.stringify(entry));
  };
  return {
    info: (event, fields) => emit("info", event, fields),
    warn: (event, fields) => emit("warn", event, fields),
    error: (event, fields) => emit("error", event, fields),
  };
}