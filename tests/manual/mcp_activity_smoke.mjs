#!/usr/bin/env node
/**
 * Unit smoke for mcp-activity ring buffer + query filter.
 */
import {
  recordMcpToolCall,
  queryMcpActivity,
  resetMcpActivityForTests,
} from "../../dist/mcp-activity.js";

let failures = 0;
function ok(cond, label) {
  if (cond) console.log(`  ✅ ${label}`);
  else { failures++; console.error(`  ❌ ${label}`); }
}

resetMcpActivityForTests();
const t0 = 1_700_000_000_000;
recordMcpToolCall({ at: t0 + 1000, tool: "herdr_exec", call: null, ua: "openai-mcp/1.0", status: 200 });
recordMcpToolCall({ at: t0 + 2000, tool: "herdr_fs_read", call: null, ua: "Python-urllib/3.12", status: 200 });
recordMcpToolCall({ at: t0 + 3000, tool: "herdr_inspect", call: null, ua: "openai-mcp", status: 200 });

const allOpenAi = queryMcpActivity({ since_ms: t0, until_ms: t0 + 4000, ua_includes: "openai-mcp" });
ok(allOpenAi.count === 2, "openai-mcp filter counts 2");
ok(allOpenAi.tools.every((x) => /openai-mcp/i.test(x.ua)), "all hits match ua");

const empty = queryMcpActivity({ since_ms: t0 + 5000, until_ms: t0 + 6000 });
ok(empty.count === 0, "empty window");

const py = queryMcpActivity({ since_ms: t0, until_ms: t0 + 4000, ua_includes: "Python-urllib" });
ok(py.count === 1 && py.tools[0].tool === "herdr_fs_read", "alt ua filter");

console.log(`\n=== ${failures === 0 ? "MCP ACTIVITY PASS" : failures + " FAILURES"} ===`);
process.exit(failures === 0 ? 0 : 1);
