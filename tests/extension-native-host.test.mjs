import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile, chmod } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(__dirname, "..");
const hostPath = path.join(root, "bin", "herdr-extension-host");

async function withFakeRust(fn) {
  const dir = await mkdtemp(path.join(os.tmpdir(), "herdr-extension-host-delegate-"));
  const fake = path.join(dir, "herdr-mcp");
  const capture = path.join(dir, "args.txt");
  await writeFile(
    fake,
    `#!/bin/sh\nprintf '%s\\n' "$@" > "$HERDR_DELEGATE_CAPTURE"\n`,
    "utf8",
  );
  await chmod(fake, 0o700);
  try {
    await fn({ fake, capture });
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

async function runCompat(args, fake, capture) {
  const child = spawn(process.execPath, [hostPath, ...args], {
    env: {
      ...process.env,
      HERDR_MCP_BIN: fake,
      HERDR_DELEGATE_CAPTURE: capture,
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stderr = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  const code = await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", resolve);
  });
  assert.equal(code, 0, stderr);
  return (await readFile(capture, "utf8")).trim().split("\n").filter(Boolean);
}

test("compat entrypoint delegates install/status lifecycle to Rust native-host", async () => {
  await withFakeRust(async ({ fake, capture }) => {
    assert.deepEqual(await runCompat(["install"], fake, capture), ["native-host", "install"]);
    assert.deepEqual(await runCompat(["status"], fake, capture), ["native-host", "status"]);
    assert.deepEqual(await runCompat(["rollback"], fake, capture), ["native-host", "rollback"]);
  });
});

test("compat entrypoint delegates Chrome invocation to Rust extension-host", async () => {
  await withFakeRust(async ({ fake, capture }) => {
    assert.deepEqual(await runCompat([], fake, capture), ["extension-host"]);
    const origin = "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/";
    assert.deepEqual(await runCompat([origin], fake, capture), ["extension-host", origin]);
  });
});

test("compat entrypoint contains no legacy extension identity or manifest owner", async () => {
  const source = await readFile(hostPath, "utf8");
  assert.match(source, /spawnSync/);
  assert.match(source, /native-host/);
  assert.match(source, /extension-host/);
  assert.doesNotMatch(source, /chromiumIdForPath|EXTENSION_PATH|allowed_origins|NativeMessagingHosts|writeFileSync/);
});
