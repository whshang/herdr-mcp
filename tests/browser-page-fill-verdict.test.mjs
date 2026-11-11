import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const source = readFileSync(new URL("../extension/background.js", import.meta.url), "utf8");
const start = source.indexOf("function browserPageFillVerdict(");
const body = source.slice(start, source.indexOf("\n}\n", start) + 2);
const browserPageFillVerdict = new Function(`${body}; return browserPageFillVerdict;`)();

test("user fills a Claude composer that restored a draft | Given the before-text already equals the request | When the fill step did not prove a clear | Then fill is reported unverifiable instead of applied", () => {
  const want = "seed OK";
  const same = { ok: true, text: want };
  assert.deepEqual(
    browserPageFillVerdict({ before: same, after: same, want, result: { ok: true, committed: true } }),
    { applied: false, error: "fill_unverifiable_same_text", beforeMatched: true },
  );
  assert.equal(browserPageFillVerdict({ before: same, after: same, want, result: { ok: true, cleared: true } }).applied, true);
});

test("user fills a rich-text composer | Given before/after editor readbacks | When the after-text differs or the step failed | Then fill reports not applied", () => {
  const want = "hello";
  const empty = { ok: true, text: "" };
  assert.equal(browserPageFillVerdict({ before: empty, after: { ok: true, text: "hello" }, want, result: { ok: true } }).applied, true);
  assert.equal(browserPageFillVerdict({ before: empty, after: empty, want, result: { ok: true } }).error, "fill_not_applied");
  assert.equal(browserPageFillVerdict({ before: empty, after: empty, want, result: { ok: false, error: "fill_clear_not_applied" } }).error, "fill_clear_not_applied");
});

test("user cannot reuse stale refs after MAIN-world rich-text fill | Given browserPageMainWorldFill completes in background.js | When the ISOLATED cleanup runs | Then it strips data-h2w-fill-token and invalidates the page-assist kernel generation", async () => {
  const normStart = source.indexOf("function browserPageFillNormalize(");
  const normBody = source.slice(normStart, source.indexOf("\n}\n", normStart) + 2);
  const mainStart = source.indexOf("async function browserPageMainWorldFill(");
  const mainBody = source.slice(mainStart, source.indexOf("\n}\n", mainStart) + 2);

  let invalidated = 0;
  let removedToken = false;
  const fakeChrome = {
    scripting: {
      executeScript: async ({ world, func, args }) => {
        if (world === "ISOLATED") {
          const fakeGlobal = {
            document: {
              querySelectorAll: () => [{
                removeAttribute(attr) {
                  if (attr === "data-h2w-fill-token") removedToken = true;
                },
              }],
            },
            window: {
              H2W_BROWSER_PAGE_KERNEL: {
                invalidateGeneration() {
                  invalidated += 1;
                },
              },
            },
          };
          const fnSrc = func.toString();
          const runner = new Function("document", "window", `return (${fnSrc});`)(
            fakeGlobal.document,
            fakeGlobal.window,
          );
          runner(...(args || []));
          return [{ result: null }];
        }
        return [{ result: { ok: true, committed: true } }];
      },
    },
  };
  let probeCall = 0;
  const browserPageFillProbe = async () => {
    probeCall += 1;
    return { ok: true, text: probeCall === 1 ? "" : "hello rich editor" };
  };
  const browserPageTrustedFillAllowed = () => false;
  const h2wMainWorldInsertText = () => {};

  const browserPageMainWorldFill = new Function(
    "chrome",
    "browserPageFillProbe",
    "browserPageTrustedFillAllowed",
    "browserPageTrustedFill",
    "h2wMainWorldInsertText",
    `${normBody}\n${body}\n${mainBody}\nreturn browserPageMainWorldFill;`,
  )(
    fakeChrome,
    browserPageFillProbe,
    browserPageTrustedFillAllowed,
    async () => ({ ok: false }),
    h2wMainWorldInsertText,
  );

  const out = await browserPageMainWorldFill(7, {
    ok: true,
    ref: "e1",
    generation: "gen_1",
    main_world_fill_token: "f123abc",
    value: "hello rich editor",
  });
  assert.equal(out.ok, true);
  assert.equal(out.generation, "gen_1");
  assert.equal(out.main_world_fill_token, undefined);
  assert.equal(removedToken, true);
  assert.equal(invalidated, 1);
});
