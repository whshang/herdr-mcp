// #835: a non-terminal manual ChatGPT Project handoff transfer reuses its source
// tab and navigates it to the Project home. Tab ids outlive reloads, SPA
// navigation and new conversations, so a persisted transfer must be proven to
// still own the tab before any resume navigates or seeds into it. These tests
// drive the real decision helper and the real `launchHandoffTarget` region of
// extension/background.js with fakes at the chrome.tabs boundary.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import {
  HANDOFF_TAB_RESUME_TTL_MS,
  handoffSourceNavigationBlock,
  handoffStatusIsActive,
} from "../extension/continuity-core.js";

const backgroundSource = readFileSync(new URL("../extension/background.js", import.meta.url), "utf8");

const PROJECT = "g-p-0123456789abcdef0123456789abcdef";
const SOURCE_CONV = `https://chatgpt.com/g/${PROJECT}/c/old-conversation`;
const OTHER_CONV = `https://chatgpt.com/g/${PROJECT}/c/new-conversation`;
const PROJECT_HOME = `https://chatgpt.com/g/${PROJECT}/project`;
const OTHER_PROJECT_CONV = "https://chatgpt.com/g/g-p-ffffffffffffffffffffffffffffffff/c/other";

function transfer(overrides = {}) {
  const now = Date.now();
  return {
    id: "ht:test:1",
    version: 1,
    site: "chatgpt",
    trigger: "manual",
    status: "summary_ready",
    source_conv_key: SOURCE_CONV,
    source_tab_id: 7,
    source_url: SOURCE_CONV,
    project_id: PROJECT,
    project_key: `https://chatgpt.com/g/${PROJECT}`,
    project_launch_url: PROJECT_HOME,
    handoff_launch_url: PROJECT_HOME,
    handoff_text: `<<<HERDR_HANDOFF_V1 id=ht:test:1>>>\nwork state\n<<<END_HERDR_HANDOFF_V1>>>`,
    canonical_handoff: null,
    continuity_id: "hc:test:1",
    target_tab_id: null,
    target_conv_key: null,
    created_at: now,
    updated_at: now,
    ...overrides,
  };
}

function slice(startMarker, endMarker, what) {
  const start = backgroundSource.indexOf(startMarker);
  const end = backgroundSource.indexOf(endMarker, start);
  assert.ok(start >= 0 && end > start, `${what} must remain extractable from extension/background.js`);
  return backgroundSource.slice(start, end);
}

// Drives the real launchHandoffTarget with chrome.tabs and transfer storage faked.
function launchHarness({ transfers, tabUrl }) {
  const manualSource = slice(
    "function manualChatGptProjectHandoff(",
    "async function launchHandoffTarget(",
    "manualChatGptProjectHandoff",
  );
  const launchSource = slice(
    "async function launchHandoffTarget(",
    "async function resumeUncertainHandoff(",
    "launchHandoffTarget",
  );
  const tabs = { 7: { id: 7, url: tabUrl, status: "complete" } };
  const state = { navigations: [], marks: [], seeded: [] };
  const api = new Function(
    "handoffSourceNavigationBlock",
    "transfers",
    "tabs",
    "state",
    `
    const chrome = {
      tabs: {
        get: async (id) => {
          const tab = tabs[id];
          if (!tab) throw new Error("no such tab");
          return tab;
        },
        update: async (id, patch) => {
          state.navigations.push({ id, ...patch });
          return { id };
        },
        create: async (patch) => {
          state.navigations.push({ created: true, ...patch });
          return { id: 99 };
        },
      },
    };
    const loadHandoffTransfers = async () => transfers;
    const durableChainWindow = async () => ({ durable: true, turn_count: 3 });
    const automationRuntimeGate = async () => ({ ok: true });
    const markTransfer = async (id, patch) => {
      state.marks.push({ id, ...patch });
      Object.assign(transfers[id], patch, { updated_at: Date.now() });
      return transfers[id];
    };
    const tabStillExists = async (id) => Boolean(tabs[id]);
    const waitForManualProjectHome = async (id) => tabs[id] || null;
    const waitForTabComplete = async (id) => tabs[id] || null;
    const sleep = async () => {};
    const seedHandoffIntoTarget = async (id, tabId) => {
      state.seeded.push({ id, tabId });
      return { ok: true };
    };
    ${manualSource}
    ${launchSource}
    return { launchHandoffTarget };
    `,
  )(handoffSourceNavigationBlock, transfers, tabs, state);
  return { api, state };
}

test("user handoff resume keeps the tab on the conversation it is handing off | Given a manual Project transfer whose source tab still shows that conversation | When launchHandoffTarget navigates the reused tab | Then it navigates to the Project home and seeds the packet", async () => {
  const row = transfer();
  const { api, state } = launchHarness({ transfers: { [row.id]: row }, tabUrl: SOURCE_CONV });
  const result = await api.launchHandoffTarget(row.id);
  assert.deepEqual(state.navigations, [{ id: 7, url: PROJECT_HOME, active: true }]);
  assert.deepEqual(state.seeded, [{ id: row.id, tabId: 7 }]);
  assert.equal(result.ok, true);
});

test("user new conversation is never navigated away by a stale handoff transfer | Given a persisted manual Project transfer whose tab id now shows a different conversation in the same Project | When launchHandoffTarget runs | Then the tab is not navigated, nothing is seeded, and the transfer stops being resumable", async () => {
  const row = transfer();
  const { api, state } = launchHarness({ transfers: { [row.id]: row }, tabUrl: OTHER_CONV });
  const result = await api.launchHandoffTarget(row.id);
  assert.deepEqual(state.navigations, [], "the tab must not be navigated");
  assert.deepEqual(state.seeded, [], "no handoff may be seeded into an unrelated conversation");
  assert.equal(result.ok, false);
  assert.equal(result.error, "stale_handoff_conversation_mismatch");
  assert.deepEqual(state.marks.at(-1), {
    id: row.id,
    status: "failed",
    error: "stale_handoff_conversation_mismatch",
  });
});

test("user abandoned handoff transfer expires instead of navigating later | Given a manual Project transfer last touched longer than the resume TTL ago on its own source conversation | When launchHandoffTarget runs | Then the tab stays put and the transfer is failed as expired", async () => {
  const row = transfer({ updated_at: Date.now() - HANDOFF_TAB_RESUME_TTL_MS - 1000 });
  const { api, state } = launchHarness({ transfers: { [row.id]: row }, tabUrl: SOURCE_CONV });
  const result = await api.launchHandoffTarget(row.id);
  assert.deepEqual(state.navigations, []);
  assert.equal(result.error, "stale_handoff_expired");
  assert.equal(state.marks.at(-1).error, "stale_handoff_expired");
  assert.equal(state.marks.at(-1).status, "failed");
});

test("user handoff source tab ownership is decided from the live URL and the transfer clock | Given the source conversation, another conversation, another Project and an aged transfer | When handoffSourceNavigationBlock evaluates each case | Then only the owning pages are allowed", () => {
  const fresh = transfer();
  assert.equal(handoffSourceNavigationBlock({ transfer: fresh, liveUrl: SOURCE_CONV }), null);
  assert.equal(
    handoffSourceNavigationBlock({ transfer: fresh, liveUrl: OTHER_CONV }),
    "stale_handoff_conversation_mismatch",
  );
  assert.equal(
    handoffSourceNavigationBlock({ transfer: fresh, liveUrl: OTHER_PROJECT_CONV }),
    "stale_handoff_conversation_mismatch",
  );
  assert.equal(
    handoffSourceNavigationBlock({ transfer: fresh, liveUrl: "https://chatgpt.com/" }),
    "stale_handoff_conversation_mismatch",
  );
  assert.equal(
    handoffSourceNavigationBlock({ transfer: fresh, liveUrl: "https://example.com/c/1" }),
    "stale_handoff_conversation_mismatch",
  );
  // Already moved to the transfer's recorded target conversation.
  assert.equal(
    handoffSourceNavigationBlock({
      transfer: transfer({ status: "seed_uncertain", target_conv_key: OTHER_CONV }),
      liveUrl: OTHER_CONV,
    }),
    null,
  );
  // Same conversation, but the transfer was abandoned long ago.
  assert.equal(
    handoffSourceNavigationBlock({
      transfer: transfer({ updated_at: Date.now() - HANDOFF_TAB_RESUME_TTL_MS - 1 }),
      liveUrl: SOURCE_CONV,
    }),
    "stale_handoff_expired",
  );
  assert.equal(
    handoffSourceNavigationBlock({ transfer: transfer({ updated_at: null, created_at: null }), liveUrl: SOURCE_CONV }),
    "stale_handoff_expired",
  );
  assert.equal(handoffSourceNavigationBlock({ transfer: null, liveUrl: SOURCE_CONV }), "stale_handoff_missing");
});

test("user Project home only continues an in-flight handoff | Given the transfer's own Project home is showing | When the transfer status is an in-progress continuation versus an unstarted one | Then only the continuation statuses are allowed to act on the tab", () => {
  for (const status of ["target_opening", "seed_submitting", "seed_uncertain"]) {
    assert.equal(
      handoffSourceNavigationBlock({ transfer: transfer({ status }), liveUrl: PROJECT_HOME }),
      null,
      `${status} must be able to finish on the navigated Project home`,
    );
  }
  for (const status of ["summary_requested", "summary_ready"]) {
    assert.equal(
      handoffSourceNavigationBlock({ transfer: transfer({ status }), liveUrl: PROJECT_HOME }),
      "stale_handoff_conversation_mismatch",
      `${status} must not claim a Project home it never navigated to`,
    );
  }
  // The Project home of a different Project is never ours.
  assert.equal(
    handoffSourceNavigationBlock({
      transfer: transfer({ status: "seed_submitting" }),
      liveUrl: "https://chatgpt.com/g/g-p-ffffffffffffffffffffffffffffffff/project",
    }),
    "stale_handoff_conversation_mismatch",
  );
});

// Drives the real per-tab resume entry point (h2w_handoff_start on a Project tab).
function resumeHarness({ transfers, tabUrl, seedConfirmed = false }) {
  const manualSource = slice(
    "function manualChatGptProjectHandoff(",
    "async function launchHandoffTarget(",
    "manualChatGptProjectHandoff",
  );
  const resumeSource = slice(
    "// A seed can only be present in the page",
    "async function startHandoffForTab(",
    "resumeHandoffForCurrentTab",
  );
  const state = { resumed: [], failed: [] };
  const api = new Function(
    "handoffSourceNavigationBlock",
    "handoffStatusIsActive",
    "transfers",
    "tabUrl",
    "probeConfirmed",
    "state",
    `
    const chrome = {
      tabs: {
        get: async () => ({ id: 7, url: tabUrl, status: "complete" }),
        sendMessage: async () => ({ ok: true, seedConfirmed: probeConfirmed }),
      },
    };
    const loadHandoffTransfers = async () => transfers;
    const markTransfer = async (id, patch) => {
      state.failed.push({ id, ...patch });
      Object.assign(transfers[id], patch);
      return transfers[id];
    };
    const tabStillExists = async () => true;
    const handoffView = (row) => ({ id: row?.id, status: row?.status });
    const resumeUncertainHandoff = async (row) => { state.resumed.push({ how: "uncertain", id: row.id }); return { ok: true }; };
    const resumeSummaryRequested = async (row) => { state.resumed.push({ how: "summary", id: row.id }); return { ok: true }; };
    const seedHandoffIntoTarget = async (id) => { state.resumed.push({ how: "seed", id }); return { ok: true }; };
    const launchHandoffTarget = async (id) => { state.resumed.push({ how: "launch", id }); return { ok: true }; };
    ${manualSource}
    ${resumeSource}
    return { resumeHandoffForCurrentTab };
    `,
  )(handoffSourceNavigationBlock, handoffStatusIsActive, transfers, tabUrl, seedConfirmed, state);
  return { api, state };
}

test("user sending a message in a new conversation is never hijacked by an old tab-scoped handoff | Given an old manual Project transfer recorded on the same tab id that now shows a newer conversation in that Project | When the tab asks for a handoff resume | Then nothing is resumed or navigated and the stale transfer is failed as a conversation mismatch", async () => {
  const row = transfer({ status: "seed_uncertain", target_tab_id: 7 });
  const { api, state } = resumeHarness({ transfers: { [row.id]: row }, tabUrl: OTHER_CONV });
  const result = await api.resumeHandoffForCurrentTab(7);
  assert.equal(result, null, "a foreign tab must fall through to a fresh handoff");
  assert.deepEqual(state.resumed, [], "no resume path may run on a conversation the transfer does not own");
  assert.deepEqual(state.failed, [
    { id: row.id, status: "failed", error: "stale_handoff_conversation_mismatch" },
  ]);
});

test("user uncertain handoff whose seed already landed is adopted instead of restarted | Given an old manual Project transfer without a recorded target conversation and a tab on that Project's conversation that carries the transfer marker | When the tab asks for a handoff resume | Then the transfer resumes on that conversation", async () => {
  const row = transfer({ status: "seed_uncertain", target_tab_id: 7 });
  const { api, state } = resumeHarness({
    transfers: { [row.id]: row },
    tabUrl: OTHER_CONV,
    seedConfirmed: true,
  });
  const result = await api.resumeHandoffForCurrentTab(7);
  assert.equal(result?.ok, true);
  assert.deepEqual(state.resumed, [{ how: "uncertain", id: row.id }]);
  assert.deepEqual(state.failed, [], "a page-confirmed target is not treated as foreign");
});

test("user tab parked on its own Project home still resumes the in-flight handoff | Given an in-progress manual Project transfer whose tab is on that Project's home | When the tab asks for a handoff resume | Then the handoff continues on the same tab", async () => {
  const row = transfer({ status: "target_opening", target_tab_id: 7, updated_at: Date.now() - 130000 });
  const { api, state } = resumeHarness({ transfers: { [row.id]: row }, tabUrl: PROJECT_HOME });
  const result = await api.resumeHandoffForCurrentTab(7);
  assert.equal(result?.ok, true);
  assert.deepEqual(state.resumed, [{ how: "seed", id: row.id }]);
});
