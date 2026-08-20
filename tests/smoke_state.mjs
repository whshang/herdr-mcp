/**
 * A-2 acceptance smoke test: SnapshotCache against the REAL herdr socket (read-only).
 *
 * Starts the shared cache, waits for >=3 live events to be applied, then prints
 * agents[] including last_activity_at. Exit code 0 on success.
 *
 * Run: npx tsx tests/smoke_state.mjs   (or node dist/tests/... after build)
 */
import { HerdrClient } from "../dist/herdr.js";
import { getSnapshotCache } from "../dist/state.js";

const c = new HerdrClient();
const cache = getSnapshotCache(c);

// Wait until the cache has applied >=3 events (ambient agent activity generates
// pane_updated events from the running daemon).
const deadline = Date.now() + 40_000;
while (cache.eventCount < 3 && Date.now() < deadline) {
  await new Promise((r) => setTimeout(r, 500));
}

const agents = cache.agentViews();
console.log("eventCount:", cache.eventCount);
console.log("agentsWithActivity:", agents.filter((a) => a.last_activity_at).length);
for (const a of agents.slice(0, 12)) {
  console.log(`  ${a.name}@${a.pane} status=${a.status} started=${a.started_at ?? "-"} last_activity=${a.last_activity_at ?? "-"}`);
}

if (cache.eventCount < 3) {
  console.error("FAIL: fewer than 3 events received in 40s (daemon quiet?)");
  process.exit(1);
}
const withActivity = agents.filter((a) => a.last_activity_at);
if (withActivity.length === 0) {
  console.error("FAIL: no agent has last_activity_at");
  process.exit(1);
}
console.log("SMOKE OK");
process.exit(0);
