import { chmod, mkdir, readFile, rename, stat, writeFile } from "node:fs/promises";
import { dirname } from "node:path";
import {
  RUNTIME_GENERATION_SCHEMA_VERSION,
  RuntimeGenerationManager,
  type RuntimeGenerationSpec,
  validateRuntimeGenerationSpec,
} from "./runtime-generation.js";

const MAX_CONTROL_BYTES = 128 * 1024;

export interface RuntimeControlDocument {
  schema_version: 1;
  revision: number;
  desired_active: string;
  generations: RuntimeGenerationSpec[];
  observation?: {
    checks?: number;
    interval_ms?: number;
  };
}

export interface RuntimeControlStatusDocument {
  schema_version: 1;
  processed_revision: number;
  desired_active: string;
  outcome: string;
  updated_at_ms: number;
  manager: ReturnType<RuntimeGenerationManager["getStatus"]>;
}

export interface RuntimeControlLoopOptions {
  manager: RuntimeGenerationManager;
  base: RuntimeGenerationSpec;
  controlPath: string;
  statusPath: string;
  pollIntervalMs?: number;
  clock?: () => number;
  onStatus?: (status: RuntimeControlStatusDocument) => void;
}

function safeInteger(value: unknown, fallback: number, min: number, max: number): number {
  const n = Number(value);
  return Number.isInteger(n) && n >= min && n <= max ? n : fallback;
}

function sameSpec(a: RuntimeGenerationSpec | null, b: RuntimeGenerationSpec): boolean {
  return !!a &&
    a.generation === b.generation &&
    a.endpoint === b.endpoint &&
    (a.expected_runtime_version ?? null) === (b.expected_runtime_version ?? null) &&
    (a.runtime_commit ?? null) === (b.runtime_commit ?? null);
}

export function validateRuntimeControlDocument(value: unknown): RuntimeControlDocument {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new TypeError("runtime-control: document must be an object");
  const raw = value as Record<string, unknown>;
  if (raw.schema_version !== RUNTIME_GENERATION_SCHEMA_VERSION) throw new RangeError("runtime-control: schema_version must be 1");
  const revision = Number(raw.revision);
  if (!Number.isInteger(revision) || revision < 1) throw new RangeError("runtime-control: revision must be a positive integer");
  if (typeof raw.desired_active !== "string" || raw.desired_active.length === 0) throw new TypeError("runtime-control: desired_active is required");
  if (!Array.isArray(raw.generations) || raw.generations.length < 1 || raw.generations.length > 8) {
    throw new RangeError("runtime-control: generations must contain 1..8 entries");
  }
  const seen = new Set<string>();
  const generations = raw.generations.map((item) => {
    const spec = item as RuntimeGenerationSpec;
    validateRuntimeGenerationSpec(spec);
    if (seen.has(spec.generation)) throw new Error("runtime-control: duplicate generation id");
    seen.add(spec.generation);
    return { ...spec };
  });
  if (!seen.has(raw.desired_active)) throw new Error("runtime-control: desired_active must reference a registered generation");
  let observation: RuntimeControlDocument["observation"];
  if (raw.observation !== undefined) {
    if (!raw.observation || typeof raw.observation !== "object" || Array.isArray(raw.observation)) {
      throw new TypeError("runtime-control: observation must be an object");
    }
    const o = raw.observation as Record<string, unknown>;
    observation = {
      checks: safeInteger(o.checks, 3, 1, 20),
      interval_ms: safeInteger(o.interval_ms, 500, 0, 10_000),
    };
  }
  return {
    schema_version: 1,
    revision,
    desired_active: raw.desired_active,
    generations,
    ...(observation ? { observation } : {}),
  };
}

async function atomicJson(path: string, value: unknown): Promise<void> {
  await mkdir(dirname(path), { recursive: true, mode: 0o700 });
  const tmp = `${path}.tmp-${process.pid}`;
  await writeFile(tmp, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
  await chmod(tmp, 0o600);
  await rename(tmp, path);
  await chmod(path, 0o600);
}

async function readJson(path: string): Promise<unknown> {
  const info = await stat(path);
  if (info.size > MAX_CONTROL_BYTES) throw new RangeError("runtime-control: file exceeds size limit");
  return JSON.parse(await readFile(path, "utf8"));
}

export class RuntimeControlLoop {
  private readonly manager: RuntimeGenerationManager;
  private readonly base: RuntimeGenerationSpec;
  readonly controlPath: string;
  readonly statusPath: string;
  private readonly pollIntervalMs: number;
  private readonly now: () => number;
  private readonly onStatus?: (status: RuntimeControlStatusDocument) => void;
  private timer: NodeJS.Timeout | null = null;
  private ticking = false;
  private processedRevision = 0;

  constructor(options: RuntimeControlLoopOptions) {
    this.manager = options.manager;
    this.base = { ...options.base };
    this.controlPath = options.controlPath;
    this.statusPath = options.statusPath;
    this.pollIntervalMs = safeInteger(options.pollIntervalMs, 1000, 100, 60_000);
    this.now = options.clock ?? Date.now;
    this.onStatus = options.onStatus;
  }

  async initialize(): Promise<void> {
    await mkdir(dirname(this.controlPath), { recursive: true, mode: 0o700 });
    try {
      await stat(this.controlPath);
    } catch {
      const initial: RuntimeControlDocument = {
        schema_version: 1,
        revision: 1,
        desired_active: this.base.generation,
        generations: [{ ...this.base }],
        observation: { checks: 3, interval_ms: 500 },
      };
      await atomicJson(this.controlPath, initial);
    }
    await this.tick();
  }

  start(): void {
    if (this.timer) return;
    this.timer = setInterval(() => { void this.tick(); }, this.pollIntervalMs);
    this.timer.unref?.();
  }

  close(): void {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
  }

  async tick(): Promise<RuntimeControlStatusDocument | null> {
    if (this.ticking) return null;
    this.ticking = true;
    try {
      let doc: RuntimeControlDocument;
      try {
        doc = validateRuntimeControlDocument(await readJson(this.controlPath));
      } catch (error) {
        const status = this.status(this.processedRevision, this.manager.activeGenerationId, `control_invalid:${error instanceof Error ? error.message : "invalid"}`);
        await atomicJson(this.statusPath, status);
        this.onStatus?.(status);
        return status;
      }
      if (doc.revision <= this.processedRevision) return null;

      let outcome = "validated";
      const desired = doc.desired_active;
      for (const spec of doc.generations) {
        const current = this.manager.getGenerationSpec(spec.generation);
        if (sameSpec(current, spec)) {
          if (spec.generation !== this.manager.activeGenerationId) {
            const validation = await this.manager.validateGeneration(spec.generation);
            if (!validation.ok && spec.generation === desired) outcome = `candidate_rejected:${validation.code}`;
          }
          continue;
        }
        const validation = await this.manager.registerGeneration(spec);
        if (!validation.ok && spec.generation === desired) outcome = `candidate_rejected:${validation.code}`;
      }

      if (!outcome.startsWith("candidate_rejected") && desired !== this.manager.activeGenerationId) {
        const activated = await this.manager.activateGeneration(desired, {
          checks: doc.observation?.checks,
          intervalMs: doc.observation?.interval_ms,
        });
        outcome = activated.ok ? "activated" : `${activated.rolled_back ? "rolled_back" : "activation_blocked"}:${activated.code}`;
      } else if (desired === this.manager.activeGenerationId) {
        outcome = outcome === "validated" ? "active_unchanged" : outcome;
      }

      const wanted = new Set(doc.generations.map((spec) => spec.generation));
      for (const state of this.manager.getStatus().generations) {
        if (!wanted.has(state.generation) && state.generation !== this.manager.activeGenerationId) {
          this.manager.removeGeneration(state.generation);
        }
      }

      this.processedRevision = doc.revision;
      const status = this.status(doc.revision, desired, outcome);
      await atomicJson(this.statusPath, status);
      this.onStatus?.(status);
      return status;
    } finally {
      this.ticking = false;
    }
  }

  private status(revision: number, desired: string, outcome: string): RuntimeControlStatusDocument {
    return {
      schema_version: 1,
      processed_revision: revision,
      desired_active: desired,
      outcome,
      updated_at_ms: this.now(),
      manager: this.manager.getStatus(),
    };
  }
}
