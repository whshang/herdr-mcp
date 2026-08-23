import { homedir } from "node:os";
import { join } from "node:path";
import { HerdrLink } from "./client.js";
import { RuntimeControlLoop } from "./runtime-control.js";
import { RuntimeGenerationManager } from "./runtime-generation.js";

export const EPOCH1_CONTRACT_HASH =
  "sha256:3f23083ae31b977dad21b1ec9d6919c49e1067a27f7b7eea7bdd021b54770c0d";

export interface LinkDaemonConfig {
  edgeUrl: string;
  workstationId: string;
  linkToken: string;
  runtimeToken: string;
  runtimeEndpoint: string;
  runtimeGeneration: string;
  contractHash: string;
  runtimeControlPath: string;
  runtimeStatusPath: string;
  runtimeControlPollMs: number;
}

function required(env: NodeJS.ProcessEnv, name: string): string {
  const value = env[name];
  if (!value || value.trim().length === 0) throw new Error(`herdr-link daemon: ${name} is required`);
  return value;
}

export function readLinkDaemonConfig(env: NodeJS.ProcessEnv = process.env): LinkDaemonConfig {
  const edgeUrl = required(env, "HERDR_EDGE_URL");
  const workstationId = required(env, "HERDR_WORKSTATION_ID");
  const linkToken = required(env, "HERDR_LINK_TOKEN");
  const runtimeToken = required(env, "HERDR_MCP_TOKEN");
  const runtimeEndpoint = env.HERDR_MCP_ENDPOINT?.trim() || "http://127.0.0.1:8772/mcp";
  const runtimeGeneration = env.HERDR_RUNTIME_GENERATION?.trim() || "local-mcp-active";
  const contractHash = env.HERDR_CONTRACT_HASH?.trim() || EPOCH1_CONTRACT_HASH;
  const runtimeControlDir = env.HERDR_RUNTIME_CONTROL_DIR?.trim() || join(homedir(), ".config", "herdr-mcp");
  const runtimeControlPath = env.HERDR_RUNTIME_CONTROL_PATH?.trim() || join(runtimeControlDir, "runtime-control.json");
  const runtimeStatusPath = env.HERDR_RUNTIME_STATUS_PATH?.trim() || join(runtimeControlDir, "runtime-status.json");
  const pollRaw = Number(env.HERDR_RUNTIME_CONTROL_POLL_MS ?? "1000");
  const runtimeControlPollMs = Number.isInteger(pollRaw) && pollRaw >= 100 && pollRaw <= 60_000 ? pollRaw : 1000;

  const edge = new URL(edgeUrl);
  if (edge.protocol !== "wss:" && edge.protocol !== "ws:") {
    throw new Error("herdr-link daemon: HERDR_EDGE_URL must use wss:// or ws://");
  }
  if (!/^[A-Za-z0-9_.-]{1,64}$/.test(workstationId)) {
    throw new Error("herdr-link daemon: HERDR_WORKSTATION_ID is invalid");
  }
  if (!/^[A-Za-z0-9_.-]{1,64}$/.test(runtimeGeneration)) {
    throw new Error("herdr-link daemon: HERDR_RUNTIME_GENERATION is invalid");
  }
  if (contractHash !== EPOCH1_CONTRACT_HASH) {
    throw new Error("herdr-link daemon: contract hash differs from frozen epoch 1");
  }
  return {
    edgeUrl,
    workstationId,
    linkToken,
    runtimeToken,
    runtimeEndpoint,
    runtimeGeneration,
    contractHash,
    runtimeControlPath,
    runtimeStatusPath,
    runtimeControlPollMs,
  };
}

function safeLogger() {
  const write = (level: string, message: string, extra?: unknown) => {
    const suffix = extra && typeof extra === "object" ? ` ${JSON.stringify(extra)}` : "";
    process.stderr.write(`[herdr-link-daemon] ${level} ${message}${suffix}\n`);
  };
  return {
    debug: (message: string, extra?: unknown) => write("debug", message, extra),
    info: (message: string, extra?: unknown) => write("info", message, extra),
    warn: (message: string, extra?: unknown) => write("warn", message, extra),
    error: (message: string, extra?: unknown) => write("error", message, extra),
  };
}

export async function runLinkDaemon(config: LinkDaemonConfig): Promise<number> {
  const baseGeneration = {
    generation: config.runtimeGeneration,
    endpoint: config.runtimeEndpoint,
  };
  const transport = new RuntimeGenerationManager({
    base: baseGeneration,
    bearerToken: config.runtimeToken,
    contractHash: config.contractHash,
    contractEpoch: 1,
    defaultTimeoutMs: 30_000,
    maxTimeoutMs: 60_000,
    observationChecks: 3,
    observationIntervalMs: 500,
  });
  const runtimeControl = new RuntimeControlLoop({
    manager: transport,
    base: baseGeneration,
    controlPath: config.runtimeControlPath,
    statusPath: config.runtimeStatusPath,
    pollIntervalMs: config.runtimeControlPollMs,
    onStatus: (status) => {
      process.stderr.write(`[herdr-link-daemon] info runtime-control revision=${status.processed_revision} outcome=${status.outcome} active=${status.manager.active_generation}\n`);
    },
  });
  await runtimeControl.initialize();
  runtimeControl.start();

  // Populate the runtime version cache before hello when the local runtime is
  // already available. A temporary local outage must not prevent the link
  // process from starting; HerdrLink remains connected/reconnecting and later
  // status probes will report runtime health.
  const initialHealth = await transport.getHealth().catch(() => ({ healthy: false }));
  if (!initialHealth.healthy) process.stderr.write("[herdr-link-daemon] warn local runtime health probe failed at startup\n");

  const link = new HerdrLink({
    workstationId: config.workstationId,
    edgeUrl: config.edgeUrl,
    linkToken: config.linkToken,
    transport,
    logger: safeLogger(),
    heartbeatMs: 15_000,
    maxSilenceMs: 60_000,
    handshakeTimeoutMs: 10_000,
    requestTimeoutMs: 60_000,
  });

  let stopping = false;
  const stop = (signal: string) => {
    if (stopping) return;
    stopping = true;
    void link.close({ reason: signal, drainMs: 5_000 });
  };
  const onSigterm = () => stop("SIGTERM");
  const onSigint = () => stop("SIGINT");
  process.once("SIGTERM", onSigterm);
  process.once("SIGINT", onSigint);

  try {
    const exit = await link.connect();
    process.stderr.write(`[herdr-link-daemon] info exit kind=${exit.kind}\n`);
    // Deliberate fencing/auth/contract stops must stay stopped. launchd is
    // configured to restart only unsuccessful exits, so return 0 here.
    if (
      exit.kind === "stopped" ||
      exit.kind === "superseded" ||
      exit.kind === "auth_rejected" ||
      exit.kind === "contract_rejected"
    ) return 0;
    return 1;
  } finally {
    runtimeControl.close();
    process.removeListener("SIGTERM", onSigterm);
    process.removeListener("SIGINT", onSigint);
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  try {
    const config = readLinkDaemonConfig();
    process.exitCode = await runLinkDaemon(config);
  } catch (error) {
    const message = error instanceof Error ? error.message : "unknown daemon startup error";
    process.stderr.write(`[herdr-link-daemon] error ${message}\n`);
    process.exitCode = 2;
  }
}
