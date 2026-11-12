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

test("user coalesces post-mutation observation in one browser_page.action call | Given an applied mutation envelope and includeObservation enabled | When maybeAttachBrowserPagePostObservation runs across ready, reinjected, and failing content scripts | Then applied mutations return fresh post_observation or preserve applied delivery_state on observation failure", async () => {
  const idStart = source.indexOf("function withBrowserPageIdentity(");
  const idBody = source.slice(idStart, source.indexOf("\n}\n", idStart) + 2);
  const postStart = source.indexOf("async function maybeAttachBrowserPagePostObservation(");
  const postBody = source.slice(postStart, source.indexOf("\n}\n", postStart) + 2);

  let mode = "direct";
  let injected = 0;
  const fakeChrome = {
    tabs: {
      sendMessage: async (_tabId, payload) => {
        assert.equal(payload.type, "h2w_page_assist");
        assert.equal(payload.action, "observe");
        if (mode === "direct") {
          return { ok: true, generation: "pa_gen_after_1", url: "https://example.com/a", elements: [{ ref: "ref_pa_gen_after_1_0" }] };
        }
        if (mode === "reinject_once") {
          if (injected === 0) throw new Error("Receiving end does not exist.");
          return { ok: true, generation: "pa_gen_after_nav", url: "https://example.com/b", elements: [] };
        }
        throw new Error("tab navigated cross-origin");
      },
    },
    scripting: {
      executeScript: async ({ files }) => {
        assert.deepEqual(files, ["content/browser-page-kernel.js", "content/page-assist.js"]);
        injected += 1;
      },
    },
  };

  const maybeAttach = new Function(
    "chrome",
    `${idBody}\n${postBody}\nreturn maybeAttachBrowserPagePostObservation;`,
  )(fakeChrome);

  const page = {
    page_ref: "bp_" + "a".repeat(64),
    origin: "https://example.com",
    canonical_url: "https://example.com/a",
    ownership: "owned",
    page_generation: 2,
  };
  const applied = { ok: true, delivery_state: "applied", page_ref: page.page_ref };

  // 1. Disabled -> untouched
  const untouched = await maybeAttach(applied, page, 11, false);
  assert.deepEqual(untouched, applied);

  // 2. Direct observe -> attaches post_observation with fresh generation
  const withObs = await maybeAttach(applied, page, 11, true);
  assert.equal(withObs.ok, true);
  assert.equal(withObs.delivery_state, "applied");
  assert.equal(withObs.post_observation.generation, "pa_gen_after_1");
  assert.equal(withObs.post_observation.page_ref, page.page_ref);

  // 3. Content script absent after same-origin nav -> injects once and observes
  mode = "reinject_once";
  const afterNav = await maybeAttach(applied, page, 11, true);
  assert.equal(injected, 1);
  assert.equal(afterNav.post_observation.generation, "pa_gen_after_nav");

  // 4. Observation fails -> mutation verdict stays ok:true / delivery_state:applied
  mode = "fail";
  const degraded = await maybeAttach(applied, page, 11, true);
  assert.equal(degraded.ok, true);
  assert.equal(degraded.delivery_state, "applied");
  assert.equal(degraded.post_observation, null);
  assert.match(degraded.post_observation_error, /cross-origin/);
});
