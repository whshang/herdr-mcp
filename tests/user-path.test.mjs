import test from "node:test";
import assert from "node:assert/strict";
import { delimiter, join } from "node:path";
import { enrichedUserPath, enrichedUserEnv } from "../dist/user-path.js";

test("non-interactive user PATH includes common CLI install locations and de-duplicates entries", () => {
  const home = "/tmp/herdr-user-path";
  const inherited = ["/usr/bin", "/bin", join(home, ".local", "bin")].join(delimiter);
  const value = enrichedUserPath({ HOME: home, PATH: inherited });
  const parts = value.split(delimiter);
  assert.deepEqual(parts.slice(0, 5), [
    join(home, ".local", "bin"),
    join(home, ".npm-global", "bin"),
    join(home, ".cargo", "bin"),
    join(home, ".opencode", "bin"),
    join(home, ".grok", "bin"),
  ]);
  assert.equal(parts.filter((p) => p === join(home, ".local", "bin")).length, 1);
  assert.ok(parts.includes("/opt/homebrew/bin"));
  assert.ok(parts.includes("/usr/local/bin"));
  assert.ok(parts.includes("/usr/bin"));
  assert.ok(parts.includes("/bin"));
});

test("enrichedUserEnv preserves unrelated environment variables", () => {
  const env = enrichedUserEnv({ HOME: "/tmp/u", PATH: "/bin", KEEP_ME: "yes" });
  assert.equal(env.KEEP_ME, "yes");
  assert.match(env.PATH, /\.npm-global/);
});
