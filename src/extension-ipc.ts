import type { Request } from "express";
import type { Socket } from "node:net";
import { homedir } from "node:os";
import { join } from "node:path";

const EXTENSION_IPC_MARK = Symbol.for("herdr-mcp.extension-ipc");

type MarkedSocket = Socket & { [EXTENSION_IPC_MARK]?: boolean };

export function extensionIpcSocketPath(env: NodeJS.ProcessEnv = process.env): string {
  const configured = String(env.HERDR_EXTENSION_IPC_SOCKET || "").trim();
  if (configured) return configured;
  return join(homedir(), ".config", "herdr-mcp", "extension.sock");
}

export function markExtensionIpcSocket(socket: Socket): void {
  (socket as MarkedSocket)[EXTENSION_IPC_MARK] = true;
}

export function isTrustedExtensionIpcRequest(req: Request): boolean {
  return (req.socket as MarkedSocket)[EXTENSION_IPC_MARK] === true;
}
