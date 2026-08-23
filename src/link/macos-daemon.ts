import { execFileSync } from "node:child_process";
import { homedir, userInfo } from "node:os";
import { join } from "node:path";
import { readLinkDaemonConfig, runLinkDaemon, type LinkDaemonConfig } from "./daemon.js";

export const MACOS_LINK_KEYCHAIN_SERVICE = "herdr-edge-dev-link-secret";
export const MACOS_DEFAULT_EDGE_URL = "wss://herdr-edge-dev.whshang.workers.dev/ws";
export const MACOS_DEFAULT_WORKSTATION_ID = "dev-real-runtime";

function commandText(file: string, args: string[], label: string): string {
  try {
    return execFileSync(file, args, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim();
  } catch {
    throw new Error(`herdr-link macOS: unable to load ${label}`);
  }
}

export function loadMacOsLinkConfig(env: NodeJS.ProcessEnv = process.env): LinkDaemonConfig {
  const username = env.USER?.trim() || userInfo().username;
  const home = env.HOME?.trim() || homedir();
  const linkToken =
    env.HERDR_LINK_TOKEN?.trim() ||
    commandText(
      "/usr/bin/security",
      [
        "find-generic-password",
        "-a",
        username,
        "-s",
        env.HERDR_LINK_KEYCHAIN_SERVICE?.trim() || MACOS_LINK_KEYCHAIN_SERVICE,
        "-w",
      ],
      "workstation link credential",
    );
  const runtimeToken =
    env.HERDR_MCP_TOKEN?.trim() ||
    commandText(
      "/usr/libexec/PlistBuddy",
      ["-c", "Print :EnvironmentVariables:HERDR_MCP_TOKEN", join(home, "Library/LaunchAgents/dev.herdr-mcp.server.plist")],
      "local MCP credential",
    );

  return readLinkDaemonConfig({
    ...env,
    HERDR_EDGE_URL: env.HERDR_EDGE_URL?.trim() || MACOS_DEFAULT_EDGE_URL,
    HERDR_WORKSTATION_ID: env.HERDR_WORKSTATION_ID?.trim() || MACOS_DEFAULT_WORKSTATION_ID,
    HERDR_LINK_TOKEN: linkToken,
    HERDR_MCP_TOKEN: runtimeToken,
  });
}

if (import.meta.url === `file://${process.argv[1]}`) {
  try {
    process.exitCode = await runLinkDaemon(loadMacOsLinkConfig());
  } catch (error) {
    const message = error instanceof Error ? error.message : "unknown macOS daemon startup error";
    process.stderr.write(`[herdr-link-macos] error ${message}\n`);
    process.exitCode = 2;
  }
}
