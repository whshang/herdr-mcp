// A-1 validation smoke: exercise validateMethodParams against real schema.
import { validateMethodParams, getMethodParamsSchema, listMethods } from "../../dist/schema.js";

const cases = [
  ["pane.split", { direction: "right" }],                    // OK (required satisfied)
  ["pane.split", {}],                                        // missing required direction -> error
  ["pane.split", { direction: "sideways" }],                 // enum violation -> error
  ["pane.split", { direction: "right", bogus_param: 1 }],    // unknown param -> warning, still ok
  ["agent.read", { target: "wH:p1", source: "recent_unwrapped" }], // OK
  ["agent.read", { target: "wH:p1" }],                       // missing source -> error
  ["agent.prompt", { target: "wH:p1", text: "hi" }],         // OK
  ["agent.prompt", { text: "hi" }],                          // missing target -> error
  ["session.snapshot", {}],                                  // empty params OK
  ["totally.bogus.method", {}],                              // unknown method -> warning pass-through
];

let fail = 0;
for (const [method, params] of cases) {
  const v = validateMethodParams(method, params);
  const flag = v.ok ? "OK " : "ERR";
  console.log(`${flag} ${method} ${JSON.stringify(params)}`);
  for (const e of v.errors) console.log(`      ERROR ${e.name}: ${e.message}`);
  for (const w of v.warnings) console.log(`      warn  ${w.name}: ${w.message}`);
  if (method === "pane.split" && params.direction === "right" && !v.ok) fail++;
  if (method === "pane.split" && JSON.stringify(params) === "{}" && v.ok) fail++;
  if (method === "pane.split" && params.direction === "sideways" && v.ok) fail++;
  if (method === "agent.read" && !("source" in params) && v.ok) fail++;
}
const methods = listMethods();
console.log(`\nlistMethods count: ${methods.length}`);
const ps = getMethodParamsSchema("pane.split");
console.log("pane.split schema required:", ps?.required, "props:", Object.keys(ps?.properties ?? {}));
console.log(fail === 0 ? "VALIDATION SMOKE OK" : `VALIDATION SMOKE FAIL (${fail})`);
process.exit(fail === 0 ? 0 : 1);
